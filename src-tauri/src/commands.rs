//! Tauri IPC commands.
//!
//! Every command exposed to the webview lives here. Keep command bodies
//! thin: parse/validate input, call into the relevant module, map errors
//! to strings.

use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// Response of the `ping` health-check command (milestone 1 IPC round-trip).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResponse {
    /// Echo of the message that was sent.
    pub echo: String,
    /// Backend crate version.
    pub version: String,
    /// Unix epoch milliseconds when the backend handled the call.
    pub handled_at_ms: u64,
}

/// Round-trip a message from the webview through Rust and back.
#[tauri::command]
pub fn ping(message: String) -> PingResponse {
    let handled_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    PingResponse {
        echo: message,
        version: env!("CARGO_PKG_VERSION").to_string(),
        handled_at_ms,
    }
}
