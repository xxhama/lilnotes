//! llm-sidecar: native LLM inference in a separate process.
//!
//! Runs llama-cpp-2 (Metal) in its own binary so the main app's whisper-rs
//! (Metal, ggml 0.9.5) doesn't collide with llama-cpp-sys-2's ggml 0.13.1 at
//! link time. Communication is JSON-lines over stdio:
//!
//! Request  (stdin,  one JSON per line):
//!   { "type":"load",    "id":"<req>", "model_path":"<abs>.gguf" }
//!   { "type":"generate","id":"<req>", "model_path":"<abs>.gguf", "prompt":"..." }
//!   { "type":"unload" }
//!   { "type":"status" }
//!
//! Response (stdout, one JSON per line):
//!   { "type":"loaded",  "id":"<req>" }
//!   { "type":"error",   "id":"<req>", "message":"..." }
//!   { "type":"token",   "id":"<req>", "text":"...", "is_thinking":bool }
//!   { "type":"done",    "id":"<req>", "content":"..." }
//!   { "type":"unloaded" }
//!   { "type":"status",  "loaded":bool }

use std::io::{BufRead, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::params::LlamaModelParams,
    model::{AddBos, LlamaChatMessage, LlamaModel},
    sampling::LlamaSampler,
};
use serde::{Deserialize, Serialize};

/// Auto-unload after this many minutes of inactivity (mirrors Ollama's
/// default keep-alive). Frees GPU memory when the user isn't summarizing.
const UNLOAD_AFTER: Duration = Duration::from_secs(5 * 60);
/// Poll interval for the auto-unload checker.
const UNLOAD_POLL: Duration = Duration::from_secs(30);
/// Context window size (tokens). Qwen3.5 models support up to 262,144 but
/// setting N_CTX that high allocates a huge KV cache (~18 GB for the 4B
/// model). 32,768 covers meetings up to ~3-4 hours of speech + the summary
/// output, while keeping GPU memory usage practical (~5 GB total).
const N_CTX: u32 = 32768;

// ---------------------------------------------------------------------------
// Engine — ported verbatim from src-tauri/src/summary/native.rs (minus the
// Tauri/AppHandle dependency; it's a plain struct now).
// ---------------------------------------------------------------------------

struct Engine {
    /// Initialized lazily on first `load()` call.
    backend: Mutex<Option<LlamaBackend>>,
    model: Mutex<Option<LoadedModel>>,
    last_used: Mutex<Option<Instant>>,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            backend: Mutex::new(None),
            model: Mutex::new(None),
            last_used: Mutex::new(None),
        }
    }
}

struct LoadedModel {
    /// The llama.cpp model handle (weights offloaded to Metal at load time).
    model: LlamaModel,
    path: PathBuf,
}

impl Engine {
    /// Ensure a model is loaded from `gguf_path`. If a different model is
    /// already loaded, drops it first. If the same model is loaded, no-op.
    fn load(&self, gguf_path: &Path) -> Result<(), String> {
        // Already loaded the same model?
        {
            let guard = self.model.lock().unwrap();
            if let Some(loaded) = guard.as_ref() {
                if loaded.path == gguf_path {
                    *self.last_used.lock().unwrap() = Some(Instant::now());
                    return Ok(());
                }
            }
        }

        // Init backend if needed, then load the model with all layers
        // offloaded to Metal.
        let params = LlamaModelParams::default().with_n_gpu_layers(u32::MAX);
        let model = {
            let mut backend_guard = self.backend.lock().unwrap();
            if backend_guard.is_none() {
                *backend_guard = Some(
                    LlamaBackend::init()
                        .map_err(|e| format!("failed to init llama backend: {e}"))?,
                );
            }
            let backend = backend_guard.as_ref().unwrap();
            LlamaModel::load_from_file(backend, gguf_path, &params)
                .map_err(|e| format!("failed to load GGUF model: {e}"))?
        };

        *self.model.lock().unwrap() = Some(LoadedModel {
            model,
            path: gguf_path.to_path_buf(),
        });
        *self.last_used.lock().unwrap() = Some(Instant::now());
        Ok(())
    }

    /// Stream a chat completion, calling `on_token` for each generated token.
    /// Returns the full text on completion. Ensures the model is loaded
    /// from `gguf_path` before inference.
    fn chat_stream(
        &self,
        gguf_path: &Path,
        prompt: &str,
        mut on_token: impl FnMut(&str, bool),
    ) -> Result<String, String> {
        self.load(gguf_path)?;

        let guard = self.model.lock().unwrap();
        let loaded = guard.as_ref().ok_or("model not loaded")?;

        // Apply the model's built-in chat template (Qwen3.5 uses ChatML,
        // which is embedded in the GGUF metadata).
        let template = loaded
            .model
            .chat_template(None)
            .map_err(|e| format!("chat template error: {e}"))?;
        let messages = vec![
            LlamaChatMessage::new("user".to_string(), prompt.to_string())
                .map_err(|e| format!("chat message error: {e}"))?,
        ];
        let formatted = loaded
            .model
            .apply_chat_template(&template, &messages, true)
            .map_err(|e| format!("template apply error: {e}"))?;

        // Create an inference context with an 8K-token window.
        let backend = self.backend.lock().unwrap();
        let backend = backend.as_ref().ok_or("backend not initialized")?;
        let mut ctx = loaded
            .model
            .new_context(
                backend,
                LlamaContextParams::default().with_n_ctx(NonZeroU32::new(N_CTX)),
            )
            .map_err(|e| format!("context creation: {e}"))?;

        // Tokenize the formatted prompt.
        let tokens = loaded
            .model
            .str_to_token(&formatted, AddBos::Always)
            .map_err(|e| format!("tokenize error: {e}"))?;

        // Feed all prompt tokens into the batch and decode (prefill).
        // The batch must be large enough to hold the entire prompt — meeting
        // transcripts are often thousands of tokens, so size to the prompt.
        let mut batch = LlamaBatch::new(tokens.len().max(512), 1);
        for (i, token) in tokens.iter().enumerate() {
            let is_last = i == tokens.len() - 1;
            batch
                .add(*token, i as i32, &[0], is_last)
                .map_err(|e| format!("batch add error: {e}"))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| format!("decode error: {e}"))?;

        // Sampler chain: top_p → temperature → fixed-seed distribution
        // for deterministic summaries.
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::top_p(0.8, 1),
            LlamaSampler::temp(0.6),
            LlamaSampler::dist(42),
        ]);

        // Token-by-token generation loop.
        let mut n_cur = tokens.len() as i32;
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut out = String::new();
        let mut in_thinking = false;

        loop {
            let token = sampler.sample(&ctx, batch.n_tokens() as i32 - 1);
            sampler.accept(token);
            if token == loaded.model.token_eos() {
                break;
            }

            // `special=true` so thinking delimiters (<think>/</think>) decode
            // as their text form instead of empty strings — lets us filter.
            let piece = loaded
                .model
                .token_to_piece(token, &mut decoder, true, None)
                .map_err(|e| format!("token to piece error: {e}"))?;

            if piece.contains("<think>") {
                in_thinking = true;
            } else if piece.contains("</think>") {
                in_thinking = false;
            } else if in_thinking {
                on_token(&piece, true);
            } else {
                on_token(&piece, false);
                out.push_str(&piece);
            }

            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|e| format!("batch add error: {e}"))?;
            ctx.decode(&mut batch)
                .map_err(|e| format!("decode error: {e}"))?;
            n_cur += 1;
            if n_cur >= N_CTX as i32 {
                break; // context limit
            }
        }

        *self.last_used.lock().unwrap() = Some(Instant::now());
        Ok(out.trim_start().to_string())
    }

    fn is_loaded(&self) -> bool {
        self.model.lock().unwrap().is_some()
    }

    /// Drop the model from memory, freeing GPU resources.
    fn unload(&self) {
        *self.model.lock().unwrap() = None;
        *self.last_used.lock().unwrap() = None;
    }
}

// ---------------------------------------------------------------------------
// IPC protocol (JSON-lines over stdio)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Request {
    #[serde(rename = "type")]
    type_: String,
    id: Option<String>,
    model_path: Option<String>,
    prompt: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    #[serde(rename = "type")]
    type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loaded: Option<bool>,
}

impl Response {
    fn loaded(id: &str) -> Self {
        Self {
            type_: "loaded".into(),
            id: Some(id.into()),
            message: None,
            text: None,
            is_thinking: None,
            content: None,
            loaded: None,
        }
    }
    fn error(id: &str, msg: &str) -> Self {
        Self {
            type_: "error".into(),
            id: Some(id.into()),
            message: Some(msg.into()),
            text: None,
            is_thinking: None,
            content: None,
            loaded: None,
        }
    }
    fn token(id: &str, text: &str, is_thinking: bool) -> Self {
        Self {
            type_: "token".into(),
            id: Some(id.into()),
            message: None,
            text: Some(text.into()),
            is_thinking: Some(is_thinking),
            content: None,
            loaded: None,
        }
    }
    fn done(id: &str, content: &str) -> Self {
        Self {
            type_: "done".into(),
            id: Some(id.into()),
            message: None,
            text: None,
            is_thinking: None,
            content: Some(content.into()),
            loaded: None,
        }
    }
    fn unloaded() -> Self {
        Self {
            type_: "unloaded".into(),
            id: None,
            message: None,
            text: None,
            is_thinking: None,
            content: None,
            loaded: None,
        }
    }
    fn status(loaded: bool) -> Self {
        Self {
            type_: "status".into(),
            id: None,
            message: None,
            text: None,
            is_thinking: None,
            content: None,
            loaded: Some(loaded),
        }
    }
}

/// Emit one JSON response line to stdout and flush.
fn emit(resp: &Response) {
    let mut stdout = std::io::stdout();
    if let Ok(json) = serde_json::to_string(resp) {
        let _ = writeln!(stdout, "{json}");
        let _ = stdout.flush();
    }
}

fn main() {
    let engine = Arc::new(Engine::default());

    // Auto-unload background thread (mirrors NativeLlmEngine::spawn_auto_unload).
    {
        let engine = engine.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(UNLOAD_POLL);
            let should_unload = {
                let last = engine.last_used.lock().unwrap();
                match *last {
                    Some(t) => t.elapsed() >= UNLOAD_AFTER,
                    None => false, // nothing loaded
                }
            };
            if should_unload {
                let was_loaded = engine.is_loaded();
                engine.unload();
                if was_loaded {
                    eprintln!(
                        "[llm-sidecar] auto-unloaded model after {} min idle",
                        UNLOAD_AFTER.as_secs() / 60
                    );
                }
            }
        });
    }

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // stdin closed — parent exited
        };
        if line.trim().is_empty() {
            continue;
        }

        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[llm-sidecar] bad request: {e}");
                continue;
            }
        };

        match req.type_.as_str() {
            "load" => {
                let path = req.model_path.as_deref().unwrap_or("");
                let id = req.id.clone().unwrap_or_default();
                match engine.load(Path::new(path)) {
                    Ok(()) => emit(&Response::loaded(&id)),
                    Err(e) => emit(&Response::error(&id, &e)),
                }
            }
            "generate" => {
                let path = req.model_path.as_deref().unwrap_or("");
                let prompt = req.prompt.as_deref().unwrap_or("");
                let id = req.id.clone().unwrap_or_default();
                let id_for_token = id.clone();
                match engine.chat_stream(Path::new(path), prompt, |text, is_thinking| {
                    emit(&Response::token(&id_for_token, text, is_thinking));
                }) {
                    Ok(content) => emit(&Response::done(&id, &content)),
                    Err(e) => emit(&Response::error(&id, &e)),
                }
            }
            "unload" => {
                engine.unload();
                let mut resp = Response::unloaded();
                resp.id = req.id.clone();
                emit(&resp);
            }
            "status" => {
                let mut resp = Response::status(engine.is_loaded());
                resp.id = req.id.clone();
                emit(&resp);
            }
            other => {
                eprintln!("[llm-sidecar] unknown request type: {other}");
            }
        }
    }
}
