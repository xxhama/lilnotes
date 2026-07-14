//! App settings shape. Stored in the SQLite `settings` table (see `db`);
//! a pre-M5 `settings.json` is imported once at startup if present.

use serde::{Deserialize, Serialize};

/// Live AEC aggressiveness preset. Stronger suppression removes more speaker
/// echo from the mic (the loud-speaker / quiet-room case) at the cost of
/// possibly dulling the user's own voice. Only meaningful when `aec_enabled`.
/// Tuned in `audio::aec::new_aec_pair` — see the preset table there.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum AecAggressiveness {
    Balanced,
    #[default]
    Strong,
    Maximum,
}

impl AecAggressiveness {
    /// String key persisted in the `settings` table.
    pub fn as_str(self) -> &'static str {
        match self {
            AecAggressiveness::Balanced => "balanced",
            AecAggressiveness::Strong => "strong",
            AecAggressiveness::Maximum => "maximum",
        }
    }

    /// Parse a persisted key, falling back to the default on anything unknown.
    pub fn parse(s: &str) -> Self {
        match s {
            "balanced" => AecAggressiveness::Balanced,
            "maximum" => AecAggressiveness::Maximum,
            _ => AecAggressiveness::Strong,
        }
    }
}

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
    /// Summary backend: "native" (built-in llama.cpp) or "ollama".
    pub summary_backend: String,
    /// Model tag/id for summaries within the active backend; None = auto-pick.
    pub summary_model: Option<String>,
    /// Custom summary prompt template; None = built-in default.
    pub summary_template: Option<String>,
    /// Cosine score at/above which a persona is a strong (pre-filled) suggestion.
    pub persona_auto_threshold: f32,
    /// Cosine score at/above which a persona is a tentative suggestion.
    pub persona_suggest_threshold: f32,
    /// Cosine score at/above which a known persona is auto-identified live
    /// during recording (no user confirmation). Stricter than the post-
    /// diarization auto threshold. 1.0 = effectively disabled.
    pub persona_live_threshold: f32,
    /// Max voiceprints kept per persona (oldest pruned on enroll). 0 = unlimited.
    pub voiceprint_gallery_cap: i32,
    /// Software acoustic echo cancellation: feeds the captured system audio
    /// (what the speakers play) as the reference to WebRTC APM and subtracts
    /// its echo from the mic. Disables Apple's voice-processing AEC (which
    /// references lilnotes' own silent playback) and replaces its NS with
    /// WebRTC's. On by default — disable only when using headphones (no echo
    /// to cancel) or if you prefer the raw mic.
    pub aec_enabled: bool,
    /// AEC aggressiveness preset (Balanced/Strong/Maximum). Stronger = less
    /// speaker echo but may slightly dull the user's own voice. Default Strong.
    pub aec_aggressiveness: AecAggressiveness,
    /// Whether the first-launch onboarding wizard has been completed.
    pub onboarding_complete: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            asr_model: "large-v3-turbo".into(),
            live_transcription: true,
            storage_dir: None,
            delete_audio_after_transcription: false,
            summary_backend: "native".into(),
            summary_model: None,
            summary_template: None,
            persona_auto_threshold: 0.65,
            persona_suggest_threshold: 0.45,
            persona_live_threshold: 0.72,
            voiceprint_gallery_cap: 150,
            aec_enabled: true,
            aec_aggressiveness: AecAggressiveness::default(),
            onboarding_complete: false,
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
