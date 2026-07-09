//! LilNotes backend.
//!
//! Module map:
//! - `commands`    — Tauri IPC commands (thin wrappers)
//! - `audio`       — dual-source capture engine (M2)
//! - `permissions` — TCC status/request helpers (M2)
//! - `asr`         — whisper-rs transcription (M3)
//! - `models`      — ML model registry + downloader (M3/M4)
//! - `settings`    — settings shape (stored in SQLite)
//! - `diarize`     — sherpa-onnx speaker diarization (M4)
//! - `personas`   — named identity layer + voiceprint matching (M9)
//! - `voiceprint`  — CAM++ speaker embeddings for cross-meeting personas (M9)
//! - `transcript`  — merge + speaker mapping (M4)
//! - `db`          — SQLite persistence (M5)
//! - `keystore`    — macOS Keychain key for SQLCipher (M9)
//! - `summary`     — Ollama client (M6)

mod asr;
mod audio;
mod commands;
mod keystore;
mod db;
mod diarize;
mod models;
mod personas;
mod permissions;
mod settings;
mod summary;
mod transcript;
mod voiceprint;

use std::sync::Arc;

use tauri::Manager;

use asr::AsrEngine;
use audio::CaptureEngine;
use commands::AsrSession;
use db::Db;
use diarize::DiarizeEngine;
use models::DownloadManager;
use voiceprint::VoiceprintEngine;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(CaptureEngine::default())
        .manage(Arc::new(AsrEngine::default()))
        .manage(Arc::new(DiarizeEngine::default()))
        .manage(Arc::new(VoiceprintEngine::default()))
        .manage(AsrSession::default())
        .manage(DownloadManager::default())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let key = keystore::db_key()
                .map_err(|e| std::io::Error::other(format!("cannot unlock database: {e}")))?;
            let db = Arc::new(
                Db::open(&db::db_path(&data_dir), &key)
                    .map_err(std::io::Error::other)?,
            );
            settings::migrate_json_settings(&data_dir, &db);
            app.manage(db);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::start_recording,
            commands::stop_recording,
            commands::recording_status,
            commands::transcribe_meeting,
            commands::diarize_meeting,
            commands::list_meetings,
            commands::get_meeting,
            commands::update_meeting_title,
            commands::rename_speaker,
            commands::delete_meeting,
            commands::mic_permission_status,
            commands::request_mic_permission,
            commands::probe_system_audio_permission,
            commands::open_privacy_settings,
            commands::get_settings,
            commands::update_settings,
            commands::list_asr_models,
            commands::download_asr_model,
            commands::cancel_model_download,
            commands::ollama_status,
            commands::list_ollama_models,
            commands::suggested_ollama_models,
            commands::pull_ollama_model,
            commands::cancel_ollama_pull,
            commands::default_summary_template,
            commands::summarize_meeting,
            commands::list_summaries,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
