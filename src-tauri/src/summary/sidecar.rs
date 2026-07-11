//! Native LLM inference via a bundled sidecar process.
//!
//! `llama-cpp-2` (Metal, ggml 0.13.1) and `whisper-rs` (Metal, ggml 0.9.5)
//! cannot coexist in one binary — their vendored ggml-metal static libs
//! collide at the symbol level. So the LLM runs in a separate process
//! (`llm-sidecar`), spawned via `tauri-plugin-shell`'s sidecar mechanism.
//! IPC is JSON-lines over stdio.
//!
//! The sidecar owns the model + a 5-min auto-unload timer. The main app
//! spawns it lazily on first use and keeps the process alive across requests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tauri::async_runtime::{channel, Mutex as AsyncMutex};
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

static REQ_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_id() -> String {
    format!("req-{}", REQ_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// What the reader task sends to a waiting caller.
enum SidecarEvent {
    Done(String),
    Error(String),
    Status(bool),
}

#[allow(clippy::type_complexity)]
struct Pending {
    on_token: Option<Box<dyn FnMut(&str, bool) + Send>>,
    done_tx: tauri::async_runtime::Sender<SidecarEvent>,
}

/// Managed Tauri state. Lazily spawns the `llm-sidecar` process and
/// communicates via JSON-lines over stdio. One generate at a time (the
/// `generating` async mutex serializes calls client-side; the sidecar's
/// single-threaded read loop serializes server-side too).
pub struct SidecarLlmClient {
    /// The spawned sidecar child (for writing to stdin). Shared with the
    /// reader task so it can clear it on process exit.
    child: Arc<Mutex<Option<CommandChild>>>,
    /// Per-request registry: id → token callback + done channel sender.
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    /// Serializes generate calls (one at a time).
    generating: AsyncMutex<()>,
}

impl Default for SidecarLlmClient {
    fn default() -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            generating: AsyncMutex::new(()),
        }
    }
}

/// Parsed sidecar stdout response.
#[derive(Deserialize)]
struct SidecarResponse {
    #[serde(rename = "type")]
    type_: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    is_thinking: Option<bool>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    loaded: Option<bool>,
}

impl SidecarLlmClient {
    /// Spawn the sidecar if it isn't running yet. Idempotent and safe to
    /// call from sync or async context (the spawn itself is sync).
    fn ensure_spawned(&self, app: &AppHandle) -> Result<(), String> {
        let mut guard = self.child.lock().unwrap();
        if guard.is_some() {
            return Ok(());
        }

        let (mut rx, child) = app
            .shell()
            .sidecar("llm-sidecar")
            .map_err(|e| format!("failed to create sidecar command: {e}"))?
            .spawn()
            .map_err(|e| format!("failed to spawn llm-sidecar: {e}"))?;

        // Start the background reader task that dispatches stdout events
        // to per-request handlers.
        let child_ref = self.child.clone();
        let pending = self.pending.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) => {
                        let line = String::from_utf8_lossy(&bytes);
                        let resp: SidecarResponse = match serde_json::from_str(line.trim()) {
                            Ok(r) => r,
                            Err(e) => {
                                eprintln!("[sidecar] bad JSON from sidecar: {e}: {line}");
                                continue;
                            }
                        };
                        match resp.type_.as_str() {
                            "token" => {
                                if let Some(id) = &resp.id {
                                    let mut guard = pending.lock().unwrap();
                                    if let Some(p) = guard.get_mut(id) {
                                        if let Some(cb) = p.on_token.as_mut() {
                                            cb(
                                                resp.text.as_deref().unwrap_or(""),
                                                resp.is_thinking.unwrap_or(false),
                                            );
                                        }
                                    }
                                }
                            }
                            "done" => {
                                if let Some(id) = resp.id.clone() {
                                    let entry = {
                                        let mut guard = pending.lock().unwrap();
                                        guard.remove(&id)
                                    };
                                    if let Some(p) = entry {
                                        let _ = p.done_tx.try_send(SidecarEvent::Done(
                                            resp.content.unwrap_or_default(),
                                        ));
                                    }
                                }
                            }
                            "error" => {
                                if let Some(id) = resp.id.clone() {
                                    let entry = {
                                        let mut guard = pending.lock().unwrap();
                                        guard.remove(&id)
                                    };
                                    if let Some(p) = entry {
                                        let _ = p.done_tx.try_send(SidecarEvent::Error(
                                            resp.message.unwrap_or_else(|| "unknown error".into()),
                                        ));
                                    }
                                }
                            }
                            "status" => {
                                if let Some(id) = resp.id.clone() {
                                    let entry = {
                                        let mut guard = pending.lock().unwrap();
                                        guard.remove(&id)
                                    };
                                    if let Some(p) = entry {
                                        let _ = p.done_tx.try_send(SidecarEvent::Status(
                                            resp.loaded.unwrap_or(false),
                                        ));
                                    }
                                }
                            }
                            "loaded" | "unloaded" => {
                                // informational — no pending request to wake
                            }
                            other => {
                                eprintln!("[sidecar] unknown response type: {other}");
                            }
                        }
                    }
                    CommandEvent::Stderr(bytes) => {
                        eprintln!("[llm-sidecar] {}", String::from_utf8_lossy(&bytes));
                    }
                    CommandEvent::Terminated(_) => {
                        eprintln!("[sidecar] llm-sidecar process exited");
                        // Clear the child so the next call re-spawns.
                        *child_ref.lock().unwrap() = None;
                        // Wake all pending requests with an error.
                        let mut guard = pending.lock().unwrap();
                        for (_, p) in guard.drain() {
                            let _ = p.done_tx.try_send(SidecarEvent::Error(
                                "llm-sidecar process exited unexpectedly".into(),
                            ));
                        }
                        break;
                    }
                    CommandEvent::Error(err) => {
                        eprintln!("[sidecar] process error: {}", err);
                    }
                    _ => {}
                }
            }
        });

        *guard = Some(child);
        Ok(())
    }

    /// Stream a chat completion, calling `on_token` for each generated token.
    /// Returns the full text on completion. One generate at a time.
    pub async fn generate(
        &self,
        app: &AppHandle,
        model_path: &str,
        prompt: &str,
        on_token: impl FnMut(&str, bool) + Send + 'static,
    ) -> Result<String, String> {
        let _guard = self.generating.lock().await;
        self.ensure_spawned(app)?;

        let id = next_id();
        let (tx, mut rx) = channel::<SidecarEvent>(1);

        {
            let mut guard = self.pending.lock().unwrap();
            guard.insert(
                id.clone(),
                Pending {
                    on_token: Some(Box::new(on_token)),
                    done_tx: tx,
                },
            );
        }

        let request = serde_json::json!({
            "type": "generate",
            "id": id,
            "model_path": model_path,
            "prompt": prompt,
        });
        self.write_stdin(&request)?;

        match rx.recv().await {
            Some(SidecarEvent::Done(content)) => Ok(content),
            Some(SidecarEvent::Error(e)) => Err(e),
            Some(_) => Err("unexpected sidecar response".into()),
            None => Err("llm-sidecar closed unexpectedly".into()),
        }
    }

    /// Query whether a model is currently loaded in the sidecar.
    pub async fn status(&self, app: &AppHandle) -> bool {
        if self.child.lock().unwrap().is_none() {
            return false; // not spawned = not loaded
        }
        if self.ensure_spawned(app).is_err() {
            return false;
        }
        let id = next_id();
        let (tx, mut rx) = channel::<SidecarEvent>(1);
        {
            let mut guard = self.pending.lock().unwrap();
            guard.insert(
                id.clone(),
                Pending {
                    on_token: None,
                    done_tx: tx,
                },
            );
        }
        let request = serde_json::json!({ "type": "status", "id": id });
        if self.write_stdin(&request).is_err() {
            return false;
        }
        match rx.recv().await {
            Some(SidecarEvent::Status(loaded)) => loaded,
            _ => false,
        }
    }

    /// Ask the sidecar to unload the model (frees GPU memory). Fire-and-forget.
    #[allow(dead_code)]
    pub fn unload(&self, app: &AppHandle) -> Result<(), String> {
        self.ensure_spawned(app)?;
        let request = serde_json::json!({ "type": "unload" });
        self.write_stdin(&request)
    }

    fn write_stdin(&self, request: &serde_json::Value) -> Result<(), String> {
        let line = format!("{}\n", request);
        let mut guard = self.child.lock().unwrap();
        let child = guard.as_mut().ok_or("sidecar not spawned")?;
        child
            .write(line.as_bytes())
            .map_err(|e| format!("failed to write to sidecar stdin: {e}"))
    }
}
