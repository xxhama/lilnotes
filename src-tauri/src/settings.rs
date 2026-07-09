//! App settings shape. Stored in the SQLite `settings` table (see `db`);
//! a pre-M5 `settings.json` is imported once at startup if present.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// Whisper model id (see `models::WHISPER_MODELS`).
    pub asr_model: String,
    /// Transcribe in near-live chunks during recording (vs. after stop).
    pub live_transcription: bool,
    /// Where session recordings are stored; None = app data dir.
    pub storage_dir: Option<String>,
    /// Delete the WAVs once a meeting is transcribed + diarized.
    pub delete_audio_after_transcription: bool,
    /// Ollama model tag for summaries; None = auto-pick from installed.
    pub summary_model: Option<String>,
    /// Custom summary prompt template; None = built-in default.
    pub summary_template: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            asr_model: "large-v3-turbo".into(),
            live_transcription: true,
            storage_dir: None,
            delete_audio_after_transcription: false,
            summary_model: None,
            summary_template: None,
        }
    }
}

/// One-time import of the milestone-3 JSON settings file into SQLite.
pub fn migrate_json_settings(app_data_dir: &std::path::Path, db: &crate::db::Db) {
    let json_path = app_data_dir.join("settings.json");
    if !json_path.exists() {
        return;
    }
    if let Ok(text) = std::fs::read_to_string(&json_path) {
        if let Ok(settings) = serde_json::from_str::<AppSettings>(&text) {
            let _ = db.set_settings(&settings);
        }
    }
    let _ = std::fs::rename(&json_path, app_data_dir.join("settings.json.bak"));
}
