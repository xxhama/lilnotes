//! LilNotes backend.
//!
//! Module map (filled in milestone by milestone):
//! - `commands`    — Tauri IPC commands (thin wrappers)
//! - `audio`       — dual-source capture engine (M2)
//! - `permissions` — TCC status/request helpers (M2)
//! - `asr`         — whisper-rs transcription (M3)
//! - `models`      — ML model registry + downloader (M3)
//! - `settings`    — JSON settings store (M3; SQLite in M5)
//! - `diarize`     — sherpa-onnx speaker diarization (M4)
//! - `transcript`  — merge + speaker mapping (M4)
//! - `db`          — SQLite persistence (M5)
//! - `summary`     — Ollama client (M6)

mod asr;
mod audio;
mod commands;
mod models;
mod permissions;
mod settings;

use std::sync::Arc;

use tauri::Manager;

use asr::AsrEngine;
use audio::CaptureEngine;
use commands::AsrSession;
use models::DownloadManager;
use settings::SettingsStore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(CaptureEngine::default())
        .manage(Arc::new(AsrEngine::default()))
        .manage(AsrSession::default())
        .manage(DownloadManager::default())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            app.manage(SettingsStore::load(data_dir));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::start_recording,
            commands::stop_recording,
            commands::recording_status,
            commands::transcribe_session,
            commands::mic_permission_status,
            commands::request_mic_permission,
            commands::probe_system_audio_permission,
            commands::open_privacy_settings,
            commands::get_settings,
            commands::update_settings,
            commands::list_asr_models,
            commands::download_asr_model,
            commands::cancel_model_download,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
