//! LilNotes backend.
//!
//! Module map (filled in milestone by milestone):
//! - `commands`    — Tauri IPC commands (thin wrappers)
//! - `audio`       — dual-source capture engine (M2)
//! - `permissions` — TCC status/request helpers (M2)
//! - `asr`         — whisper-rs transcription (M3)
//! - `diarize`     — sherpa-onnx speaker diarization (M4)
//! - `transcript`  — merge + speaker mapping (M4)
//! - `db`          — SQLite persistence (M5)
//! - `summary`     — Ollama client (M6)
//! - `models`      — ML/Ollama model management (M3/M6)

mod audio;
mod commands;
mod permissions;

use audio::CaptureEngine;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(CaptureEngine::default())
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::start_recording,
            commands::stop_recording,
            commands::recording_status,
            commands::mic_permission_status,
            commands::request_mic_permission,
            commands::probe_system_audio_permission,
            commands::open_privacy_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
