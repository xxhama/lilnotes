//! Tauri IPC commands.
//!
//! Every command exposed to the webview lives here. Keep command bodies
//! thin: parse/validate input, call into the relevant module, map errors
//! to strings.

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};

use crate::asr::{chunker, AsrEngine, Segment};
use crate::audio::{CaptureEngine, StartedRecording, StoppedRecording};
use crate::diarize::DiarizeEngine;
use crate::models::{self, DownloadManager};
use crate::transcript;
use crate::permissions::{self, PermissionStatus};
use crate::settings::{AppSettings, SettingsStore};

/// Join handle of the live transcription worker for the active session.
#[derive(Default)]
pub struct AsrSession(
    pub Mutex<Option<std::thread::JoinHandle<Result<Vec<Segment>, String>>>>,
);

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRecordingResponse {
    #[serde(flatten)]
    pub started: StartedRecording,
    /// Whether a live transcription worker is attached to this session.
    pub live_transcription: bool,
    /// Set when live transcription was requested but couldn't start.
    pub live_transcription_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopRecordingResponse {
    #[serde(flatten)]
    pub stopped: StoppedRecording,
    /// Present when live transcription ran (possibly empty for a silent
    /// meeting); None when it was off — use `transcribe_session` then.
    pub segments: Option<Vec<Segment>>,
    pub transcription_error: Option<String>,
}

/// Load the configured whisper model if present on disk.
fn ensure_asr_model(
    app: &AppHandle,
    asr: &AsrEngine,
    settings: &AppSettings,
) -> Result<(), String> {
    let path = models::whisper_model_path(app, &settings.asr_model)?;
    if !path.exists() {
        return Err(format!(
            "transcription model \"{}\" is not downloaded yet — get it in Settings",
            settings.asr_model
        ));
    }
    asr.ensure_loaded(&settings.asr_model, &path.to_string_lossy())
}

#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    engine: State<'_, CaptureEngine>,
    asr: State<'_, Arc<AsrEngine>>,
    asr_session: State<'_, AsrSession>,
    settings: State<'_, SettingsStore>,
) -> Result<StartRecordingResponse, String> {
    let dir = session_dir(&app)?;
    let cfg = settings.get();

    // Prepare live transcription if enabled and the model is available.
    // Model loading takes seconds — do it off the async runtime.
    let mut live_error: Option<String> = None;
    let live_tx = if cfg.live_transcription {
        let app2 = app.clone();
        let asr2 = asr.inner().clone();
        let cfg2 = cfg.clone();
        let load: Result<(), String> = tauri::async_runtime::spawn_blocking(move || {
            ensure_asr_model(&app2, &asr2, &cfg2)
        })
        .await
        .map_err(|e| e.to_string())?;
        match load {
            Ok(()) => {
                let (tx, rx) = crossbeam_channel::bounded(1024);
                let started = engine.start(app.clone(), dir, Some(tx))?;
                let handle = chunker::spawn_live_worker(
                    app.clone(),
                    asr.inner().clone(),
                    rx,
                    started.session_id.clone(),
                );
                *asr_session.0.lock().unwrap() = Some(handle);
                return Ok(StartRecordingResponse {
                    started,
                    live_transcription: true,
                    live_transcription_error: None,
                });
            }
            Err(e) => {
                live_error = Some(e);
                None
            }
        }
    } else {
        None
    };

    let started = engine.start(app.clone(), dir, live_tx)?;
    Ok(StartRecordingResponse {
        started,
        live_transcription: false,
        live_transcription_error: live_error,
    })
}

#[tauri::command]
pub async fn stop_recording(
    engine: State<'_, CaptureEngine>,
    asr_session: State<'_, AsrSession>,
) -> Result<StopRecordingResponse, String> {
    // Stopping capture ends the pipelines, which drops the live senders and
    // lets the ASR worker flush its remainder and exit.
    let stopped = engine.stop()?;

    let handle = asr_session.0.lock().unwrap().take();
    let (segments, transcription_error) = match handle {
        None => (None, None),
        Some(h) => {
            // The worker may still be transcribing the tail — join off the
            // async runtime.
            let joined = tauri::async_runtime::spawn_blocking(move || h.join())
                .await
                .map_err(|e| e.to_string())?;
            match joined {
                Ok(Ok(segments)) => (Some(segments), None),
                Ok(Err(e)) => (None, Some(e)),
                Err(_) => (None, Some("transcription worker panicked".into())),
            }
        }
    };

    Ok(StopRecordingResponse {
        stopped,
        segments,
        transcription_error,
    })
}

/// Batch transcription of a finished session's WAVs (used when live mode is
/// off, or to re-run transcription). Emits `asr:segment` events as it goes.
#[tauri::command]
pub async fn transcribe_session(
    app: AppHandle,
    asr: State<'_, Arc<AsrEngine>>,
    settings: State<'_, SettingsStore>,
    session_id: String,
    mic_wav: String,
    system_wav: String,
) -> Result<Vec<Segment>, String> {
    let asr = asr.inner().clone();
    let cfg = settings.get();
    tauri::async_runtime::spawn_blocking(move || {
        ensure_asr_model(&app, &asr, &cfg)?;
        chunker::transcribe_wavs(app.clone(), &asr, session_id, &mic_wav, &system_wav)
    })
    .await
    .map_err(|e| e.to_string())?
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
// Diarization (milestone 4)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizedTranscript {
    pub segments: Vec<Segment>,
    /// Number of distinct speakers found on the system channel.
    pub speaker_count: usize,
}

/// Diarize a session's system channel and label the given segments.
/// Downloads the (small) diarization models on first use, emitting the
/// usual `model:progress` events; emits `diarize:progress` while running.
#[tauri::command]
pub async fn diarize_session(
    app: AppHandle,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    system_wav: String,
    mut segments: Vec<Segment>,
) -> Result<DiarizedTranscript, String> {
    let diarizer = diarizer.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let turns = diarizer.diarize_wav(&app, &system_wav)?;
        let speaker_count = transcript::assign_speakers(&mut segments, &turns);
        segments.sort_by_key(|s| s.start_ms);
        Ok(DiarizedTranscript {
            segments,
            speaker_count,
        })
    })
    .await
    .map_err(|e| e.to_string())?
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

// ---------------------------------------------------------------------------
// Settings + ASR model management (milestone 3)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_settings(settings: State<'_, SettingsStore>) -> AppSettings {
    settings.get()
}

#[tauri::command]
pub fn update_settings(
    settings: State<'_, SettingsStore>,
    new_settings: AppSettings,
) -> Result<(), String> {
    settings.set(new_settings)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrModelInfo {
    pub id: String,
    pub label: String,
    pub approx_bytes: u64,
    pub note: String,
    pub downloaded: bool,
    pub active: bool,
}

#[tauri::command]
pub fn list_asr_models(
    app: AppHandle,
    settings: State<'_, SettingsStore>,
) -> Result<Vec<AsrModelInfo>, String> {
    let active = settings.get().asr_model;
    models::WHISPER_MODELS
        .iter()
        .map(|m| {
            let path = models::whisper_model_path(&app, m.id)?;
            Ok(AsrModelInfo {
                id: m.id.into(),
                label: m.label.into(),
                approx_bytes: m.approx_bytes,
                note: m.note.into(),
                downloaded: path.exists(),
                active: m.id == active,
            })
        })
        .collect()
}

/// Download a whisper model with `model:progress` events. Resolves when the
/// download completes (or fails/cancels).
#[tauri::command]
pub async fn download_asr_model(
    app: AppHandle,
    downloads: State<'_, DownloadManager>,
    id: String,
) -> Result<(), String> {
    let model = models::whisper_model(&id).ok_or_else(|| format!("unknown model: {id}"))?;
    let dest = models::whisper_model_path(&app, &id)?;
    if dest.exists() {
        return Ok(());
    }
    let cancel = downloads.begin(&id)?;
    let url = model.url;
    let id2 = id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        models::download_with_progress(&app, &id2, url, &dest, &cancel)
    })
    .await
    .map_err(|e| e.to_string())?;
    downloads.finish(&id);
    result
}

#[tauri::command]
pub fn cancel_model_download(downloads: State<'_, DownloadManager>, id: String) -> bool {
    downloads.cancel(&id)
}
