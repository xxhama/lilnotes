//! Tauri IPC commands.
//!
//! Every command exposed to the webview lives here. Keep command bodies
//! thin: parse/validate input, call into the relevant module, map errors
//! to strings.

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::asr::{chunker, AsrEngine, Segment};
use crate::audio::{CaptureEngine, StartedRecording};
use crate::db::{Db, MeetingDetail, MeetingSummary, Persona};
use crate::diarize::DiarizeEngine;
use crate::models::{self, DownloadManager};
use crate::permissions::{self, PermissionStatus};
use crate::personas;
use crate::settings::AppSettings;
use crate::summary::{self, ollama};
use crate::transcript;
use crate::voiceprint::VoiceprintEngine;

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
    pub echo: String,
    pub version: String,
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
// Recording (milestone 2/3/5)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub recording: bool,
    pub session_id: Option<String>,
    pub elapsed_ms: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRecordingResponse {
    #[serde(flatten)]
    pub started: StartedRecording,
    pub live_transcription: bool,
    pub live_transcription_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopRecordingResponse {
    /// The persisted meeting row created for this session.
    pub meeting_id: i64,
    pub session_id: String,
    pub duration_ms: u64,
    /// Segments captured live (empty when live transcription was off).
    pub segments: Vec<Segment>,
    pub transcription_error: Option<String>,
}

/// Recordings go to `<storage dir>/recordings/<session timestamp>/`.
fn session_dir(app: &AppHandle, settings: &AppSettings) -> Result<std::path::PathBuf, String> {
    let base = match &settings.storage_dir {
        Some(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => app
            .path()
            .app_data_dir()
            .map_err(|e| format!("no app data dir: {e}"))?,
    };
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    Ok(base.join("recordings").join(stamp))
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
    db: State<'_, Arc<Db>>,
) -> Result<StartRecordingResponse, String> {
    let cfg = db.get_settings();
    let dir = session_dir(&app, &cfg)?;

    // Prepare live transcription if enabled and the model is available.
    // Model loading takes seconds — do it off the async runtime.
    let mut live_error: Option<String> = None;
    if cfg.live_transcription {
        let app2 = app.clone();
        let asr2 = asr.inner().clone();
        let cfg2 = cfg.clone();
        let load: Result<(), String> =
            tauri::async_runtime::spawn_blocking(move || ensure_asr_model(&app2, &asr2, &cfg2))
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
            Err(e) => live_error = Some(e),
        }
    }

    let started = engine.start(app.clone(), dir, None)?;
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
    db: State<'_, Arc<Db>>,
) -> Result<StopRecordingResponse, String> {
    // Stopping capture ends the pipelines, which drops the live senders and
    // lets the ASR worker flush its remainder and exit.
    let stopped = engine.stop()?;

    let handle = asr_session.0.lock().unwrap().take();
    let (segments, transcription_error) = match handle {
        None => (Vec::new(), None),
        Some(h) => {
            let joined = tauri::async_runtime::spawn_blocking(move || h.join())
                .await
                .map_err(|e| e.to_string())?;
            match joined {
                Ok(Ok(segments)) => (segments, None),
                Ok(Err(e)) => (Vec::new(), Some(e)),
                Err(_) => (Vec::new(), Some("transcription worker panicked".into())),
            }
        }
    };

    // Persist the meeting.
    let title = chrono::Local::now().format("Meeting — %b %-d, %Y %-I:%M %p").to_string();
    let meeting_id = db.insert_meeting(
        &stopped.session_id,
        &title,
        stopped.started_at_ms as i64,
        (stopped.started_at_ms + stopped.duration_ms) as i64,
        &stopped.mic_wav,
        &stopped.system_wav,
    )?;
    if !segments.is_empty() {
        db.replace_segments(meeting_id, &segments)?;
    }

    Ok(StopRecordingResponse {
        meeting_id,
        session_id: stopped.session_id,
        duration_ms: stopped.duration_ms,
        segments,
        transcription_error,
    })
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

/// Batch transcription of a persisted meeting's WAVs. Emits `asr:segment`
/// events as it goes and saves the result.
#[tauri::command]
pub async fn transcribe_meeting(
    app: AppHandle,
    asr: State<'_, Arc<AsrEngine>>,
    db: State<'_, Arc<Db>>,
    meeting_id: i64,
) -> Result<Vec<Segment>, String> {
    let asr = asr.inner().clone();
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = db.get_settings();
        let (mic, system) = db.meeting_wavs(meeting_id)?;
        let (mic, system) = match (mic, system) {
            (Some(m), Some(s)) => (m, s),
            _ => return Err("this meeting's audio files have been deleted".into()),
        };
        ensure_asr_model(&app, &asr, &cfg)?;
        let segments =
            chunker::transcribe_wavs(app.clone(), &asr, format!("meeting-{meeting_id}"), &mic, &system)?;
        db.replace_segments(meeting_id, &segments)?;
        Ok(segments)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Diarization (milestone 4/5)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizedTranscript {
    pub segments: Vec<Segment>,
    pub speaker_count: usize,
    /// True if the WAVs were removed per the delete-audio setting.
    pub audio_deleted: bool,
}

/// Diarize a meeting's system channel, persist the labeled segments, and
/// (optionally, per settings) delete the audio files afterwards.
/// `num_speakers`: exact remote-speaker count if the user declared one
/// (much more reliable than automatic estimation).
#[tauri::command]
pub async fn diarize_meeting(
    app: AppHandle,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    db: State<'_, Arc<Db>>,
    meeting_id: i64,
    num_speakers: Option<i32>,
) -> Result<DiarizedTranscript, String> {
    let diarizer = diarizer.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (mic_wav, system_wav) = db.meeting_wavs(meeting_id)?;
        let system_wav = system_wav.ok_or("this meeting's audio files have been deleted")?;
        let mut segments = db.meeting_segments(meeting_id)?;
        if segments.is_empty() {
            return Err("transcribe the meeting before identifying speakers".into());
        }

        let turns = diarizer.diarize_wav(&app, &system_wav, num_speakers)?;
        let speaker_count = transcript::assign_speakers(&mut segments, &turns);
        segments.sort_by_key(|s| s.start_ms);

        db.replace_segments(meeting_id, &segments)?;
        let mut labels: Vec<String> = segments
            .iter()
            .filter_map(|s| s.speaker.clone())
            .filter(|s| s.starts_with("SPEAKER_"))
            .collect();
        labels.sort();
        labels.dedup();
        db.ensure_speakers(meeting_id, &labels)?;

        // Milestone 9: identity layer (additive). Runs after speakers are
        // persisted; never blocks the diarize result on failure.
        let settings = db.get_settings();
        if let Err(e) = personas::identify_and_persist(
            &db, &voiceprint, &app, meeting_id, &system_wav, &turns, &settings,
        )
        .map(|m| { let _ = app.emit_to("main", "speakers:identified", m); })
        {
            eprintln!("identify_speakers failed (non-fatal): {e}");
        }

        // Transcript + speakers are safely stored; drop the audio if asked.
        let mut audio_deleted = false;
        if db.get_settings().delete_audio_after_transcription {
            if let Some(mic) = mic_wav {
                let _ = std::fs::remove_file(&mic);
            }
            let _ = std::fs::remove_file(&system_wav);
            db.clear_audio_paths(meeting_id)?;
            audio_deleted = true;
        }

        Ok(DiarizedTranscript {
            segments,
            speaker_count,
            audio_deleted,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Meetings CRUD (milestone 5)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_meetings(
    db: State<'_, Arc<Db>>,
    search: Option<String>,
) -> Result<Vec<MeetingSummary>, String> {
    db.list_meetings(search.as_deref())
}

#[tauri::command]
pub fn get_meeting(db: State<'_, Arc<Db>>, meeting_id: i64) -> Result<MeetingDetail, String> {
    db.get_meeting(meeting_id)
}

#[tauri::command]
pub fn update_meeting_title(
    db: State<'_, Arc<Db>>,
    meeting_id: i64,
    title: String,
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("title cannot be empty".into());
    }
    db.update_title(meeting_id, title)
}

#[tauri::command]
pub fn rename_speaker(
    db: State<'_, Arc<Db>>,
    meeting_id: i64,
    raw_label: String,
    display_name: Option<String>,
) -> Result<(), String> {
    let name = display_name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    db.rename_speaker(meeting_id, &raw_label, name)
}

// ---------------------------------------------------------------------------
// Personas + voiceprints (milestone 9)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_personas(db: State<'_, Arc<Db>>) -> Result<Vec<Persona>, String> {
    db.list_personas()
}

#[tauri::command]
pub fn create_persona(db: State<'_, Arc<Db>>, display_name: String) -> Result<i64, String> {
    db.create_persona(&display_name)
}

#[tauri::command]
pub fn rename_persona(
    db: State<'_, Arc<Db>>,
    persona_id: i64,
    display_name: String,
) -> Result<(), String> {
    db.rename_persona(persona_id, &display_name)
}

#[tauri::command]
pub fn delete_persona(db: State<'_, Arc<Db>>, persona_id: i64) -> Result<(), String> {
    db.delete_persona(persona_id)
}

#[tauri::command]
pub fn delete_all_voiceprints(db: State<'_, Arc<Db>>) -> Result<(), String> {
    db.delete_all_voiceprints()
}

/// Re-run embedding + matching for a meeting's diarized speakers; persists
/// suggestions (confirmed preserved). Useful to re-match after the gallery
/// grew. Fails gracefully if the audio has been deleted.
#[tauri::command]
pub async fn identify_speakers(
    app: AppHandle,
    db: State<'_, Arc<Db>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    meeting_id: i64,
) -> Result<Vec<personas::SpeakerMatch>, String> {
    let db = db.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let diarizer = diarizer.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (_, system_wav) = db.meeting_wavs(meeting_id)?;
        let system_wav = system_wav.ok_or("this meeting's audio files have been deleted")?;
        let turns = diarizer.diarize_wav(&app, &system_wav, None)?;
        let settings = db.get_settings();
        let matches = personas::identify_and_persist(
            &db, &voiceprint, &app, meeting_id, &system_wav, &turns, &settings,
        )?;
        let _ = app.emit_to("main", "speakers:identified", matches.clone());
        Ok(matches)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Confirm that `raw_label` in a meeting is `persona_id`. Marks the link
/// confirmed AND enrolls that speaker's embedding (if audio is available).
/// Also applies the persona name via the per-meeting display mapping so the
/// transcript shows the name.
#[tauri::command]
pub async fn confirm_speaker_persona(
    app: AppHandle,
    db: State<'_, Arc<Db>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    meeting_id: i64,
    raw_label: String,
    persona_id: i64,
) -> Result<(), String> {
    let db2 = db.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let diarizer = diarizer.inner().clone();
    let raw = raw_label.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // 1. Mark the link confirmed.
        db2.set_link_confirmed(meeting_id, &raw, persona_id)?;
        // 2. Apply the persona name to the per-meeting display mapping.
        let name = db2
            .list_personas()?
            .into_iter()
            .find(|p| p.id == persona_id)
            .map(|p| p.display_name)
            .ok_or("persona not found")?;
        db2.rename_speaker(meeting_id, &raw, Some(&name))?;
        // 3. Enroll the embedding (best-effort if audio still present).
        let (_, system) = db2.meeting_wavs(meeting_id)?;
        if let Some(system_wav) = system {
            let turns = diarizer.diarize_wav(&app, &system_wav, None)?;
            let cap = db2.get_settings().voiceprint_gallery_cap;
            let _ = personas::enroll(
                &db2, &voiceprint, &app, persona_id, meeting_id, &raw,
                Some(&system_wav), &turns, cap,
            );
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove the persona link for a raw label and clear any display rename
/// applied by a prior confirm (revert to the raw `SPEAKER_xx` label).
#[tauri::command]
pub fn unlink_speaker_persona(
    db: State<'_, Arc<Db>>,
    meeting_id: i64,
    raw_label: String,
) -> Result<(), String> {
    db.unlink_speaker(meeting_id, &raw_label)?;
    db.rename_speaker(meeting_id, &raw_label, None)?;
    Ok(())
}

/// Delete a meeting row; also removes its WAVs from disk.
#[tauri::command]
pub fn delete_meeting(db: State<'_, Arc<Db>>, meeting_id: i64) -> Result<(), String> {
    let (mic, system) = db.delete_meeting(meeting_id)?;
    for wav in [mic, system].into_iter().flatten() {
        let path = std::path::PathBuf::from(&wav);
        let _ = std::fs::remove_file(&path);
        // Remove the (now likely empty) session directory.
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
    Ok(())
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
// Settings + ASR model management (milestone 3/5)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_settings(db: State<'_, Arc<Db>>) -> AppSettings {
    db.get_settings()
}

#[tauri::command]
pub fn update_settings(db: State<'_, Arc<Db>>, new_settings: AppSettings) -> Result<(), String> {
    db.set_settings(&new_settings)
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
    db: State<'_, Arc<Db>>,
) -> Result<Vec<AsrModelInfo>, String> {
    let active = db.get_settings().asr_model;
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

// ---------------------------------------------------------------------------
// Ollama: status, models, pull (milestone 6)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn ollama_status() -> Result<ollama::OllamaStatus, String> {
    Ok(ollama::status().await)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaModels {
    pub installed: Vec<ollama::InstalledModel>,
    /// The model summaries will use (settings override or curated auto-pick).
    pub active: Option<String>,
}

#[tauri::command]
pub async fn list_ollama_models(db: State<'_, Arc<Db>>) -> Result<OllamaModels, String> {
    let installed = ollama::installed_models().await?;
    let names: Vec<String> = installed.iter().map(|m| m.name.clone()).collect();
    let settings_model = db.get_settings().summary_model;
    let active = settings_model
        .filter(|m| names.iter().any(|n| n == m))
        .or_else(|| summary::pick_default_model(&names));
    Ok(OllamaModels { installed, active })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedModel {
    pub tag: String,
    pub tier: String,
    pub approx_download: String,
    pub note: String,
    pub installed: bool,
    /// Set when the matching `-mlx` (Apple MLX runtime) variant is installed.
    pub mlx_installed: bool,
}

#[tauri::command]
pub async fn suggested_ollama_models() -> Result<Vec<SuggestedModel>, String> {
    let installed: Vec<String> = ollama::installed_models()
        .await
        .map(|ms| ms.into_iter().map(|m| m.name).collect())
        .unwrap_or_default();
    Ok(summary::CURATED_MODELS
        .iter()
        .map(|c| SuggestedModel {
            tag: c.tag.into(),
            tier: c.tier.into(),
            approx_download: c.approx_download.into(),
            note: c.note.into(),
            installed: installed.iter().any(|m| m == c.tag),
            mlx_installed: installed.iter().any(|m| m == &format!("{}-mlx", c.tag)),
        })
        .collect())
}

/// Pull a model with live `ollama:pull` progress events. Cancellable;
/// Ollama resumes cancelled pulls on the next attempt.
#[tauri::command]
pub async fn pull_ollama_model(
    app: AppHandle,
    downloads: State<'_, DownloadManager>,
    model: String,
) -> Result<(), String> {
    let key = format!("ollama:{model}");
    let cancel = downloads.begin(&key)?;
    let result = ollama::pull(&app, &model, &cancel).await;
    downloads.finish(&key);
    result
}

#[tauri::command]
pub fn cancel_ollama_pull(downloads: State<'_, DownloadManager>, model: String) -> bool {
    downloads.cancel(&format!("ollama:{model}"))
}

// ---------------------------------------------------------------------------
// Summaries (milestone 6)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn default_summary_template() -> String {
    summary::DEFAULT_TEMPLATE.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryResult {
    pub summary_id: i64,
    pub model: String,
    pub content: String,
}

/// Generate a summary for a meeting, streaming tokens via `summary:token`
/// events, and persist it. `model` overrides the configured/auto-picked one.
#[tauri::command]
pub async fn summarize_meeting(
    app: AppHandle,
    db: State<'_, Arc<Db>>,
    meeting_id: i64,
    model: Option<String>,
) -> Result<SummaryResult, String> {
    let meeting = db.get_meeting(meeting_id)?;
    if meeting.segments.is_empty() {
        return Err("transcribe the meeting before summarizing".into());
    }

    // Resolve model: explicit > settings > curated auto-pick.
    let settings = db.get_settings();
    let model = match model.or(settings.summary_model) {
        Some(m) => m,
        None => {
            let installed: Vec<String> = ollama::installed_models()
                .await?
                .into_iter()
                .map(|m| m.name)
                .collect();
            summary::pick_default_model(&installed).ok_or(
                "no Ollama model installed — pull one in Settings → Summaries",
            )?
        }
    };

    let template = settings
        .summary_template
        .unwrap_or_else(|| summary::DEFAULT_TEMPLATE.to_string());
    let transcript = summary::transcript_text(&meeting.segments, &meeting.renames);
    let prompt = summary::build_prompt(&template, &meeting.title, &transcript);

    let app2 = app.clone();
    let content = ollama::chat_stream(&model, &prompt, |token| {
        let _ = app2.emit_to(
            "main",
            "summary:token",
            summary::SummaryToken {
                meeting_id,
                token: token.to_string(),
            },
        );
    })
    .await?;

    let summary_id = db.insert_summary(meeting_id, &model, &template, &content)?;
    Ok(SummaryResult {
        summary_id,
        model,
        content,
    })
}

#[tauri::command]
pub fn list_summaries(
    db: State<'_, Arc<Db>>,
    meeting_id: i64,
) -> Result<Vec<crate::db::SummaryRow>, String> {
    db.list_summaries(meeting_id)
}
