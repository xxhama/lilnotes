//! Async Ollama HTTP client (localhost only).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter};

const BASE: &str = "http://localhost:11434";

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        // No overall timeout: chat generation and pulls run for minutes.
        .build()
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Health + installed models
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OllamaStatus {
    pub reachable: bool,
    pub version: Option<String>,
}

pub async fn status() -> OllamaStatus {
    let Ok(client) = client() else {
        return OllamaStatus {
            reachable: false,
            version: None,
        };
    };
    let resp = client
        .get(format!("{BASE}/api/version"))
        .timeout(Duration::from_secs(3))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            #[derive(Deserialize)]
            struct V {
                version: String,
            }
            let version = r.json::<V>().await.ok().map(|v| v.version);
            OllamaStatus {
                reachable: true,
                version,
            }
        }
        _ => OllamaStatus {
            reachable: false,
            version: None,
        },
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InstalledModel {
    pub name: String,
    pub size_bytes: u64,
    pub parameter_size: Option<String>,
    pub family: Option<String>,
}

pub async fn installed_models() -> Result<Vec<InstalledModel>, String> {
    #[derive(Deserialize)]
    struct Details {
        parameter_size: Option<String>,
        family: Option<String>,
    }
    #[derive(Deserialize)]
    struct Model {
        name: String,
        size: u64,
        details: Option<Details>,
    }
    #[derive(Deserialize)]
    struct Tags {
        models: Vec<Model>,
    }
    let resp = client()?
        .get(format!("{BASE}/api/tags"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| "Ollama is not reachable at localhost:11434".to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let tags: Tags = resp.json().await.map_err(|e| e.to_string())?;
    Ok(tags
        .models
        .into_iter()
        .map(|m| InstalledModel {
            name: m.name,
            size_bytes: m.size,
            parameter_size: m.details.as_ref().and_then(|d| d.parameter_size.clone()),
            family: m.details.as_ref().and_then(|d| d.family.clone()),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Pull with aggregated progress
// ---------------------------------------------------------------------------

/// Payload of the `ollama:pull` event.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PullProgress {
    pub model: String,
    pub status: String,
    pub completed: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// POST /api/pull with `stream:true`. Progress arrives as NDJSON lines —
/// one `downloading` line per layer digest with (completed, total); a pull
/// spans several digests, so we track each and aggregate for the overall
/// figure. Cancellation just drops the connection: Ollama resumes a
/// cancelled pull from where it left off on the next attempt.
pub async fn pull(app: &AppHandle, model: &str, cancel: &AtomicBool) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Line {
        status: Option<String>,
        error: Option<String>,
        digest: Option<String>,
        total: Option<u64>,
        completed: Option<u64>,
    }

    let emit = |status: &str, completed: u64, total: u64, done: bool, error: Option<String>| {
        let _ = app.emit_to(
            "main",
            "ollama:pull",
            PullProgress {
                model: model.to_string(),
                status: status.to_string(),
                completed,
                total,
                done,
                error,
            },
        );
    };

    let resp = client()?
        .post(format!("{BASE}/api/pull"))
        .json(&json!({ "model": model, "stream": true }))
        .send()
        .await
        .map_err(|_| "Ollama is not reachable at localhost:11434".to_string())?
        .error_for_status()
        .map_err(|e| format!("pull failed: {e}"))?;

    let mut digests: HashMap<String, (u64, u64)> = HashMap::new();
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();

    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            emit("cancelled", 0, 0, true, Some("cancelled".into()));
            return Err("cancelled".into());
        }
        let chunk = chunk.map_err(|e| format!("pull stream error: {e}"))?;
        buf.extend_from_slice(&chunk);

        // NDJSON: process complete lines, keep the remainder buffered.
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let Ok(parsed) = serde_json::from_slice::<Line>(&line) else {
                continue;
            };
            if let Some(err) = parsed.error {
                emit("error", 0, 0, true, Some(err.clone()));
                return Err(err);
            }
            let status = parsed.status.unwrap_or_default();
            if let (Some(digest), Some(total)) = (parsed.digest.clone(), parsed.total) {
                digests.insert(digest, (parsed.completed.unwrap_or(0), total));
            }
            let completed: u64 = digests.values().map(|(c, _)| c).sum();
            let total: u64 = digests.values().map(|(_, t)| t).sum();
            if status == "success" {
                emit("success", total, total, true, None);
                return Ok(());
            }
            emit(&status, completed, total, false, None);
        }
    }
    // Stream ended without an explicit success line.
    emit("error", 0, 0, true, Some("pull ended unexpectedly".into()));
    Err("pull ended unexpectedly".into())
}

// ---------------------------------------------------------------------------
// Streaming chat
// ---------------------------------------------------------------------------

/// POST /api/chat with `stream:true`; calls `on_token` per content token
/// and returns the full response text.
pub async fn chat_stream(
    model: &str,
    prompt: &str,
    mut on_token: impl FnMut(&str),
) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Msg {
        content: Option<String>,
    }
    #[derive(Deserialize)]
    struct Line {
        message: Option<Msg>,
        error: Option<String>,
        done: Option<bool>,
    }

    let resp = client()?
        .post(format!("{BASE}/api/chat"))
        .json(&json!({
            "model": model,
            "stream": true,
            "messages": [ { "role": "user", "content": prompt } ]
        }))
        .send()
        .await
        .map_err(|_| "Ollama is not reachable at localhost:11434".to_string())?
        .error_for_status()
        .map_err(|e| format!("chat failed: {e}"))?;

    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    let mut content = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("chat stream error: {e}"))?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let Ok(parsed) = serde_json::from_slice::<Line>(&line) else {
                continue;
            };
            if let Some(err) = parsed.error {
                return Err(err);
            }
            if let Some(tok) = parsed.message.and_then(|m| m.content) {
                if !tok.is_empty() {
                    on_token(&tok);
                    content.push_str(&tok);
                }
            }
            if parsed.done == Some(true) {
                return Ok(content);
            }
        }
    }
    if content.is_empty() {
        Err("chat ended without a response".into())
    } else {
        Ok(content)
    }
}
