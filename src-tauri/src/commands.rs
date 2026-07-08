//! Tauri IPC commands.
//!
//! Every command exposed to the webview lives here. Keep command bodies
//! thin: parse/validate input, call into the relevant module, map errors
//! to strings.

use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};

use crate::audio::{CaptureEngine, StartedRecording, StoppedRecording};
use crate::permissions::{self, PermissionStatus};

// ---------------------------------------------------------------------------
// Health check (milestone 1)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Recording (milestone 2)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub recording: bool,
    pub session_id: Option<String>,
    pub elapsed_ms: Option<u64>,
}

/// Directory where a session's WAVs live:
/// `<app data>/recordings/<session timestamp>/`.
fn session_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    Ok(base.join("recordings").join(stamp))
}

#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    engine: State<'_, CaptureEngine>,
) -> Result<StartedRecording, String> {
    let dir = session_dir(&app)?;
    engine.start(app.clone(), dir)
}

#[tauri::command]
pub async fn stop_recording(
    engine: State<'_, CaptureEngine>,
) -> Result<StoppedRecording, String> {
    engine.stop()
}

#[tauri::command]
pub fn recording_status(engine: State<'_, CaptureEngine>) -> RecordingStatus {
    match engine.status() {
        Some((session_id, elapsed_ms)) => RecordingStatus {
            recording: true,
            session_id: Some(session_id),
            elapsed_ms: Some(elapsed_ms),
        },
        None => RecordingStatus {
            recording: false,
            session_id: None,
            elapsed_ms: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Permissions (milestone 2)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn mic_permission_status() -> PermissionStatus {
    permissions::mic_status()
}

/// Shows the mic TCC prompt if undetermined; resolves when the user answers.
#[tauri::command]
pub async fn request_mic_permission() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(permissions::request_mic_access)
        .await
        .map_err(|e| format!("permission request failed: {e}"))
}

/// Probe system-audio access by creating (and immediately destroying) a
/// process tap. Surfaces the TCC prompt on first use.
#[tauri::command]
pub async fn probe_system_audio_permission() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(|| {
        crate::audio::system_tap::probe_access().map(|_| true)
    })
    .await
    .map_err(|e| format!("probe failed: {e}"))?
}

#[tauri::command]
pub fn open_privacy_settings(section: String) -> Result<(), String> {
    permissions::open_privacy_settings(&section)
}
