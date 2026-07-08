//! LilNotes backend.
//!
//! Module map (filled in milestone by milestone):
//! - `commands`   — Tauri IPC commands (thin wrappers)
//! - `audio`      — dual-source capture engine (M2)
//! - `asr`        — whisper-rs transcription (M3)
//! - `diarize`    — sherpa-onnx speaker diarization (M4)
//! - `transcript` — merge + speaker mapping (M4)
//! - `db`         — SQLite persistence (M5)
//! - `summary`    — Ollama client (M6)
//! - `models`     — ML/Ollama model management (M3/M6)

mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![commands::ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
