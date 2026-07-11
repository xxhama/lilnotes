//! Native LLM inference via llama-server (llama.cpp's HTTP server).
//!
//! llama-server runs in its own process (spawned via tauri-plugin-shell's
//! sidecar mechanism) to avoid the ggml-metal symbol collision between
//! llama.cpp and whisper-rs. Communication is HTTP/SSE: the main app POSTs
//! to /v1/chat/completions and parses the Server-Sent Events stream.
//!
//! llama-server provides `chat_template_kwargs: {"enable_thinking": false}`
//! — the model-agnostic way to disable thinking mode. Works across
//! Qwen3.5, DeepSeek, Gemma, etc. without per-model hardcoded token IDs.
//! The model's own Jinja template handles it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rand::RngCore;
use serde::Deserialize;
use serde_json::json;
use tauri::async_runtime::Mutex as AsyncMutex;
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// Auto-unload after this many minutes of inactivity (mirrors Ollama's
/// default keep-alive). Kills the llama-server process to free GPU memory.
const UNLOAD_AFTER: Duration = Duration::from_secs(5 * 60);
/// Poll interval for the auto-unload checker.
const UNLOAD_POLL: Duration = Duration::from_secs(30);
/// Context window size (tokens). Matches the previous sidecar's N_CTX.
const N_CTX: u32 = 32768;
/// How long to wait for llama-server to report its port before giving up.
/// Model loading is usually <1s on Apple Silicon, but 120s gives headroom for
/// large models (9B) on cold disk cache.
const PORT_WAIT: Duration = Duration::from_secs(120);
/// Poll interval while waiting for the port to appear.
const PORT_POLL: Duration = Duration::from_millis(200);
/// Max lines of server output to retain for error diagnostics.
const RECENT_LINES_MAX: usize = 30;

/// Managed Tauri state. Lazily spawns the `llama-server` process and
/// communicates via HTTP. One generate at a time (the `generating` async
/// mutex serializes calls).
pub struct SidecarLlmClient {
    /// The spawned llama-server child (for killing on unload/switch).
    child: Arc<Mutex<Option<CommandChild>>>,
    /// Port the server is listening on (parsed from stderr after startup).
    port: Arc<Mutex<Option<u16>>>,
    /// Model path currently loaded in the server.
    current_model: Arc<Mutex<Option<String>>>,
    /// Last activity time (for auto-unload).
    last_used: Arc<Mutex<Option<Instant>>>,
    /// Serializes generate calls (one at a time).
    generating: AsyncMutex<()>,
    /// Ensures the auto-unload thread is started only once.
    auto_unload_started: AtomicBool,
    /// Recent server output lines for error diagnostics (FIFO, max RECENT_LINES_MAX).
    recent_lines: Arc<Mutex<Vec<String>>>,
    /// Set if the server prints a fatal error during startup.
    startup_error: Arc<Mutex<Option<String>>>,
    /// Ephemeral API key for the running llama-server (regenerated each spawn).
    /// Sent as `Authorization: Bearer <key>`; rejects rogue local processes and
    /// DNS-rebinding browser requests.
    api_key: Arc<Mutex<Option<String>>>,
}

impl Default for SidecarLlmClient {
    fn default() -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            port: Arc::new(Mutex::new(None)),
            current_model: Arc::new(Mutex::new(None)),
            last_used: Arc::new(Mutex::new(None)),
            generating: AsyncMutex::new(()),
            auto_unload_started: AtomicBool::new(false),
            recent_lines: Arc::new(Mutex::new(Vec::new())),
            startup_error: Arc::new(Mutex::new(None)),
            api_key: Arc::new(Mutex::new(None)),
        }
    }
}

// ---------------------------------------------------------------------------
// SSE response parsing
// ---------------------------------------------------------------------------

#[derive(Default, Deserialize)]
struct SseDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Default, Deserialize)]
struct SseChoice {
    #[serde(default)]
    delta: SseDelta,
}

#[derive(Default, Deserialize)]
struct SseChunk {
    #[serde(default)]
    choices: Option<Vec<SseChoice>>,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl SidecarLlmClient {
    /// Stream a chat completion, calling `on_token` for each generated token.
    /// Returns the full content text on completion. One generate at a time.
    pub async fn generate(
        &self,
        app: &AppHandle,
        model_path: &str,
        prompt: &str,
        mut on_token: impl FnMut(&str, bool) + Send + 'static,
    ) -> Result<String, String> {
        let _guard = self.generating.lock().await;
        self.ensure_running_with_model(app, model_path).await?;

        let port = self
            .port
            .lock()
            .unwrap()
            .ok_or("llama-server port not available")?;

        let api_key = self
            .api_key
            .lock()
            .unwrap()
            .clone()
            .ok_or("llama-server api key not available")?;

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| format!("HTTP client error: {e}"))?;

        let resp = client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .bearer_auth(&api_key)
            .json(&json!({
                "model": "local",
                "messages": [{"role": "user", "content": prompt}],
                "chat_template_kwargs": {"enable_thinking": false},
                "stream": true,
                "max_tokens": -1,
                "temperature": 0.6,
                "top_p": 0.8,
                "seed": 42
            }))
            .send()
            .await
            .map_err(|e| format!("llama-server request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("llama-server error: {e}"))?;

        let mut stream = resp.bytes_stream();
        let mut buf = Vec::new();
        let mut content = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("SSE stream error: {e}"))?;
            buf.extend_from_slice(&chunk);

            // SSE: process complete lines, keep the remainder buffered.
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line).trim().to_string();

                if line.is_empty() || !line.starts_with("data:") {
                    continue;
                }

                let data = line[5..].trim_start(); // strip "data:" + spaces
                if data == "[DONE]" {
                    *self.last_used.lock().unwrap() = Some(Instant::now());
                    return Ok(content.trim_start().to_string());
                }

                let chunk: SseChunk = match serde_json::from_str(data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                if let Some(choices) = chunk.choices {
                    if let Some(choice) = choices.first() {
                        if let Some(text) = &choice.delta.content {
                            if !text.is_empty() {
                                on_token(text, false);
                                content.push_str(text);
                            }
                        }
                        if let Some(text) = &choice.delta.reasoning_content {
                            if !text.is_empty() {
                                on_token(text, true);
                            }
                        }
                    }
                }
            }
        }

        // Stream ended without [DONE] — likely a server crash or network drop.
        *self.last_used.lock().unwrap() = Some(Instant::now());
        if content.is_empty() {
            Err("llama-server stream ended unexpectedly".into())
        } else {
            eprintln!("[sidecar] stream ended without [DONE] — returning partial content");
            Ok(content.trim_start().to_string())
        }
    }

    /// Whether a model is currently loaded (process alive).
    pub async fn status(&self, _app: &AppHandle) -> bool {
        self.child.lock().unwrap().is_some()
    }

    /// Kill the server process, freeing GPU memory. Fire-and-forget.
    #[allow(dead_code)]
    pub fn unload(&self, _app: &AppHandle) -> Result<(), String> {
        self.kill_server();
        Ok(())
    }

    // -- internal helpers -- //

    fn kill_server(&self) {
        if let Some(child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
        }
        *self.port.lock().unwrap() = None;
        *self.current_model.lock().unwrap() = None;
        *self.last_used.lock().unwrap() = None;
        *self.api_key.lock().unwrap() = None;
    }

    /// Ensure llama-server is running with the given model loaded. If a
    /// different model is loaded (or no server is running), kills and
    /// respawns. Awaits until the server reports its port.
    async fn ensure_running_with_model(
        &self,
        app: &AppHandle,
        model_path: &str,
    ) -> Result<(), String> {
        // Start the auto-unload watcher once.
        if !self.auto_unload_started.swap(true, Ordering::Relaxed) {
            self.spawn_auto_unload();
        }

        // Check if we need to (re)start: no child, or model mismatch.
        let needs_restart = {
            let child_guard = self.child.lock().unwrap();
            let model_guard = self.current_model.lock().unwrap();
            child_guard.is_none() || model_guard.as_deref() != Some(model_path)
        };

        if needs_restart {
            self.kill_server();
            self.spawn_server(app, model_path)?;
        }

        // Wait for the port to appear. Using tokio::time::sleep (not
        // std::thread::sleep) so the event reader task that processes
        // stderr can run on the same tokio worker thread.
        let deadline = Instant::now() + PORT_WAIT;
        loop {
            if let Some(port) = *self.port.lock().unwrap() {
                eprintln!("[sidecar] llama-server ready on port {port}");
                *self.last_used.lock().unwrap() = Some(Instant::now());
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.kill_server();
                let lines = self.recent_lines.lock().unwrap();
                let diag = if lines.is_empty() {
                    "no output captured".to_string()
                } else {
                    lines.join("\n")
                };
                return Err(format!(
                    "timed out waiting for llama-server to start\n\
                     --- llama-server output ---\n{diag}"
                ));
            }
            if let Some(err) = self.startup_error.lock().unwrap().take() {
                self.kill_server();
                return Err(format!("llama-server failed to start: {err}"));
            }
            if needs_restart && self.child.lock().unwrap().is_none() {
                let lines = self.recent_lines.lock().unwrap();
                let diag = if lines.is_empty() {
                    "no output captured".to_string()
                } else {
                    lines.join("\n")
                };
                return Err(format!(
                    "llama-server process exited during startup\n\
                     --- llama-server output ---\n{diag}"
                ));
            }
            tokio::time::sleep(PORT_POLL).await;
        }
    }

    fn spawn_server(&self, app: &AppHandle, model_path: &str) -> Result<(), String> {
        let n_ctx_str = N_CTX.to_string();
        // Ephemeral API key: 32 random bytes as hex. Regenerated each spawn,
        // never persisted. Rejects requests lacking the matching Bearer token.
        let api_key = generate_api_key();
        let cmd = app
            .shell()
            .sidecar("llama-server")
            .map_err(|e| format!("failed to create sidecar command: {e}"))?;

        let (mut rx, child) = cmd
            .args([
                "--model",
                model_path,
                "--port",
                "0",
                "--host",
                "127.0.0.1",
                "--jinja",
                "--n-gpu-layers",
                "-1",
                "--ctx-size",
                n_ctx_str.as_str(),
                "--api-key",
                api_key.as_str(),
                "--no-slots",
            ])
            .spawn()
            .map_err(|e| format!("failed to spawn llama-server: {e}"))?;

        *self.child.lock().unwrap() = Some(child);
        *self.current_model.lock().unwrap() = Some(model_path.to_string());
        *self.port.lock().unwrap() = None;
        *self.api_key.lock().unwrap() = Some(api_key);
        self.recent_lines.lock().unwrap().clear();
        self.startup_error.lock().unwrap().take();

        // Background reader: parse stderr/stdout for the port, log everything,
        // capture lines for diagnostics, detect errors, and clear state on
        // termination.
        let port_ref = self.port.clone();
        let child_ref = self.child.clone();
        let model_ref = self.current_model.clone();
        let recent_lines_ref = self.recent_lines.clone();
        let startup_error_ref = self.startup_error.clone();
        let api_key_ref = self.api_key.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stderr(bytes) => {
                        let line = String::from_utf8_lossy(&bytes).trim().to_string();
                        eprintln!("[llama-server] {}", line);
                        push_recent_line(&recent_lines_ref, &line);
                        if let Some(port) = parse_port(&line) {
                            *port_ref.lock().unwrap() = Some(port);
                        }
                        if is_error_line(&line) {
                            *startup_error_ref.lock().unwrap() = Some(line.clone());
                        }
                    }
                    CommandEvent::Stdout(bytes) => {
                        let line = String::from_utf8_lossy(&bytes).trim().to_string();
                        eprintln!("[llama-server] {}", line);
                        push_recent_line(&recent_lines_ref, &line);
                        if let Some(port) = parse_port(&line) {
                            *port_ref.lock().unwrap() = Some(port);
                        }
                    }
                    CommandEvent::Terminated(_) => {
                        eprintln!("[sidecar] llama-server process exited");
                        *child_ref.lock().unwrap() = None;
                        *port_ref.lock().unwrap() = None;
                        *model_ref.lock().unwrap() = None;
                        *api_key_ref.lock().unwrap() = None;
                        break;
                    }
                    CommandEvent::Error(err) => {
                        eprintln!("[sidecar] process error: {}", err);
                        *startup_error_ref.lock().unwrap() = Some(err.to_string());
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }

    fn spawn_auto_unload(&self) {
        let child = self.child.clone();
        let port = self.port.clone();
        let current_model = self.current_model.clone();
        let last_used = self.last_used.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(UNLOAD_POLL);
            let should_unload = {
                let last = last_used.lock().unwrap();
                match *last {
                    Some(t) => t.elapsed() >= UNLOAD_AFTER,
                    None => false,
                }
            };
            if should_unload {
                let was_loaded = child.lock().unwrap().is_some();
                if let Some(c) = child.lock().unwrap().take() {
                    let _ = c.kill();
                }
                *port.lock().unwrap() = None;
                *current_model.lock().unwrap() = None;
                *last_used.lock().unwrap() = None;
                if was_loaded {
                    eprintln!(
                        "[sidecar] auto-unloaded llama-server after {} min idle",
                        UNLOAD_AFTER.as_secs() / 60
                    );
                }
            }
        });
    }
}

/// Parse a port number from a llama-server stderr line.
/// Matches "127.0.0.1:PORT" or "localhost:PORT" patterns.
fn parse_port(line: &str) -> Option<u16> {
    for prefix in &["127.0.0.1:", "localhost:"] {
        if let Some(pos) = line.find(prefix) {
            let rest = &line[pos + prefix.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(port) = digits.parse::<u16>() {
                if port > 0 {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// Push a line into the recent-lines buffer (FIFO, max RECENT_LINES_MAX).
fn push_recent_line(buf: &Arc<Mutex<Vec<String>>>, line: &str) {
    let mut buf = buf.lock().unwrap();
    if buf.len() >= RECENT_LINES_MAX {
        buf.remove(0);
    }
    buf.push(line.to_string());
}

/// Check if a stderr line indicates a fatal error during startup.
fn is_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("error")
        || lower.contains("failed")
        || lower.contains("fatal")
        || lower.contains("cannot")
        || lower.contains("unable to")
        || lower.contains("aborting")
}

/// Generate a random 32-byte hex string for use as the llama-server API key.
/// Uses `rand::OsRng` (cryptographically secure) via the `rand` crate.
fn generate_api_key() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
