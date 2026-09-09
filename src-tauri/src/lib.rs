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
//! - `summary`     — summarization: built-in llama.cpp + Ollama (M6)

mod asr;
pub mod audio; // pub so the offline AEC example (separate crate) can reach it
mod commands;
mod db;
mod diarize;
mod keystore;
mod mcp;
mod models;
mod permissions;
mod personas;
mod settings;
// Re-exported so the offline AEC example (separate crate) can construct a pair.
pub use settings::AecAggressiveness;
mod shutdown;
mod summary;
mod transcript;
mod tray;
mod voiceprint;

use std::sync::{Arc, OnceLock};

use tauri::Manager;

/// Global AppHandle for the panic hook to access Tauri state (kill the
/// sidecar) when a panic occurs. Set once in `setup`.
static PANIC_APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

use asr::AsrEngine;
use audio::CaptureEngine;
use commands::AsrSession;
use diarize::DiarizeEngine;
use models::DownloadManager;
use summary::sidecar::SidecarLlmClient;
use voiceprint::VoiceprintEngine;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Panic hook: kill the llama-server sidecar so a panic doesn't orphan
    // the child process (which holds GPU memory). Does NOT stop a recording
    // — joining capture threads from a panic hook risks a deadlock. The
    // startup orphan cleanup catches any lingering sidecar.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("[panic] {info}");
        if let Some(handle) = PANIC_APP_HANDLE.get() {
            if let Some(sidecar) = handle.try_state::<SidecarLlmClient>() {
                let _ = sidecar.unload(handle);
            }
        }
        (default_hook)(info);
    }));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(CaptureEngine::default())
        .manage(Arc::new(AsrEngine::default()))
        .manage(Arc::new(DiarizeEngine::default()))
        .manage(Arc::new(VoiceprintEngine::default()))
        .manage(AsrSession::default())
        .manage(DownloadManager::default())
        .manage(SidecarLlmClient::default())
        .manage(mcp::McpServer::default())
        .setup(|app| {
            // Store the AppHandle for the panic hook.
            let _ = PANIC_APP_HANDLE.set(app.handle().clone());

            // Kill orphaned llama-server processes from a previous
            // crashed/killed session. Two patterns cover both builds:
            //   Dev:  .../src-tauri/binaries/llama-server-<target-triple>
            //   Prod: .../LilNotes.app/Contents/MacOS/llama-server
            // Both are specific to our app — an independently-run
            // llama-server (different path) is not affected.
            let _ = std::process::Command::new("pkill")
                .args(["-f", "binaries/llama-server-"])
                .output();
            let _ = std::process::Command::new("pkill")
                .args(["-f", "LilNotes.app/Contents/MacOS/llama-server"])
                .output();

            let data_dir = app.path().app_data_dir()?;

            // Lazy DB: for returning users (DB file exists) we open eagerly
            // so the keychain access is silent. For new users we defer to
            // the onboarding wizard's "Security" step via `init_db`.
            let lazy_db = Arc::new(db::LazyDb::new(data_dir.clone()));
            if db::db_path(&data_dir).exists() {
                lazy_db
                    .init()
                    .map_err(|e| std::io::Error::other(format!("cannot unlock database: {e}")))?;
                // Recordings from before 0.3 are WAV; convert them to FLAC in
                // the background. New users have nothing to convert.
                audio::migrate::spawn(app.handle().clone(), lazy_db.clone());
            }
            app.manage(lazy_db);
            // Read-only MCP server for local AI agents — only if the user
            // enabled it and the DB is already unlocked (returning users).
            // New users get it started from `init_db` after onboarding.
            mcp::start_if_enabled(app.handle());
            tray::setup(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Hide to the menu bar instead of quitting — the app stays
                // alive so recording can continue in the background.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::db_exists,
            commands::init_db,
            commands::start_recording,
            commands::stop_recording,
            commands::recording_status,
            commands::transcribe_meeting,
            commands::diarize_meeting,
            commands::list_meetings,
            commands::get_meeting,
            commands::list_hidden_segments,
            commands::mark_segment_echo,
            commands::unmark_segment_echo,
            commands::delete_segment,
            commands::restore_segment,
            commands::clean_echo,
            commands::clean_echo_segment,
            commands::revert_echo_clean,
            commands::retranscribe_meeting,
            commands::update_meeting_title,
            commands::update_meeting_notes,
            commands::rename_speaker,
            commands::list_personas,
            commands::create_persona,
            commands::rename_persona,
            commands::delete_persona,
            commands::delete_all_voiceprints,
            commands::identify_speakers,
            commands::confirm_speaker_persona,
            commands::unlink_speaker_persona,
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
            commands::list_native_models,
            commands::download_native_model,
            commands::cancel_native_model_download,
            commands::native_model_status,
            commands::ollama_status,
            commands::list_ollama_models,
            commands::suggested_ollama_models,
            commands::pull_ollama_model,
            commands::cancel_ollama_pull,
            commands::default_summary_template,
            commands::summarize_meeting,
            commands::list_summaries,
            commands::list_customers,
            commands::create_customer,
            commands::rename_customer,
            commands::update_customer_notes,
            commands::get_customer,
            commands::delete_customer,
            commands::set_meeting_customer,
            commands::merge_customers,
            commands::search_customer_meetings,
            commands::summarize_customer,
            commands::list_customer_summaries,
            commands::mcp_status,
            commands::regenerate_mcp_token,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |app_handle, event| {
            shutdown::on_run_event(app_handle, event);
        });
}
