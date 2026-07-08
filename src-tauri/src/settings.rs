//! App settings, stored as JSON in the app data dir.
//!
//! Interim solution for M3 — migrates into the SQLite `settings` table in
//! milestone 5. Keep the shape flat and serde-friendly.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// Whisper model id (see `models::WHISPER_MODELS`).
    pub asr_model: String,
    /// Transcribe in near-live chunks during recording (vs. after stop).
    pub live_transcription: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            asr_model: "large-v3-turbo".into(),
            live_transcription: true,
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
    current: Mutex<AppSettings>,
}

impl SettingsStore {
    pub fn load(app_data_dir: PathBuf) -> Self {
        let path = app_data_dir.join("settings.json");
        let current = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            current: Mutex::new(current),
        }
    }

    pub fn get(&self) -> AppSettings {
        self.current.lock().unwrap().clone()
    }

    pub fn set(&self, settings: AppSettings) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| e.to_string())?;
        *self.current.lock().unwrap() = settings;
        Ok(())
    }
}
