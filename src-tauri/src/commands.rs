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
use crate::audio::{offline_aec, playback, CaptureEngine, StartedRecording};
use crate::db::{
    CustomerDetail, CustomerRollupRow, CustomerSearchResult, CustomerSummary, LazyDb,
    MeetingDetail, MeetingSummary, Persona,
};
use crate::diarize::DiarizeEngine;
use crate::mcp::{self, McpServer, McpStatus};
use crate::models::{self, DownloadManager};
use crate::permissions::{self, PermissionStatus};
use crate::personas;
use crate::settings::AppSettings;
use crate::summary::{self, ollama, sidecar::SidecarLlmClient, SummaryBackend};
use crate::transcript;
use crate::voiceprint::VoiceprintEngine;

/// Prefix of the placeholder title stamped at recording start
/// (`create_meeting_at_start`). Used to detect meetings that still have the
/// default title and may be auto-renamed when a summary is generated — a
/// manually-renamed title never starts with this, so it's preserved.
const DEFAULT_TITLE_PREFIX: &str = "Meeting — ";

/// Join handle of the live transcription worker for the active session.
#[derive(Default)]
#[allow(clippy::type_complexity)]
pub struct AsrSession(pub Mutex<Option<std::thread::JoinHandle<Result<Vec<Segment>, String>>>>);

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

/// Check whether the encrypted database file already exists on disk.
/// Used by the frontend to decide whether to show onboarding (new user)
/// or proceed directly to the app (returning user). Does NOT touch the
/// Keychain — safe to call before `init_db`.
#[tauri::command]
pub fn db_exists(app: AppHandle) -> bool {
    match app.path().app_data_dir() {
        Ok(dir) => crate::db::db_path(&dir).exists(),
        Err(_) => false,
    }
}

/// Open the encrypted database (retrieving the Keychain key). For new
/// users this triggers the macOS Keychain prompt; for returning users
/// the access is silent. Called from the onboarding wizard's "Security"
/// step.
#[tauri::command]
pub async fn init_db(app: AppHandle, db: State<'_, Arc<LazyDb>>) -> Result<(), String> {
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || db.init())
        .await
        .map_err(|e| format!("database init failed: {e}"))??;
    // The MCP server waits for an unlocked DB; a returning user who enabled
    // it gets it started here (setup skipped it while the DB was locked).
    mcp::start_if_enabled(&app);
    Ok(())
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

/// Insert the meeting row the moment capture starts so notes taken during
/// the live recording can be saved against it. Title is stamped at start
/// time (more accurate than stop time for a meeting's "when").
fn create_meeting_at_start(
    db: &State<'_, Arc<LazyDb>>,
    started: &StartedRecording,
) -> Result<i64, String> {
    let title = format!(
        "{}{}",
        DEFAULT_TITLE_PREFIX,
        chrono::Local::now().format("%b %-d, %Y %-I:%M %p")
    );
    db.insert_meeting_started(&started.session_id, &title, started.started_at_ms as i64)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRecordingResponse {
    #[serde(flatten)]
    pub started: StartedRecording,
    /// The meeting row created at recording start so notes can be saved
    /// against it during the live recording.
    pub meeting_id: i64,
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

/// Playback cache root: always under the app data dir (inside the asset
/// protocol scope), whatever `settings.storage_dir` says.
fn playback_cache_root(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?;
    Ok(playback::cache_root(&data_dir))
}

/// Load `model_id` if present on disk. The model-id-override path used by
/// re-transcription (per-meeting model) and the echo-clean re-transcribe. The
/// settings-bound [`ensure_asr_model`] delegates here with the global model.
fn ensure_asr_model_id(app: &AppHandle, asr: &AsrEngine, model_id: &str) -> Result<(), String> {
    let path = models::whisper_model_path(app, model_id)?;
    if !path.exists() {
        return Err(format!(
            "transcription model \"{model_id}\" is not downloaded yet — get it in Settings"
        ));
    }
    asr.ensure_loaded(model_id, &path.to_string_lossy())
}

/// Load the configured (global) whisper model if present on disk. Live and
/// first-pass transcription use this so they follow the Settings picker.
fn ensure_asr_model(
    app: &AppHandle,
    asr: &AsrEngine,
    settings: &AppSettings,
) -> Result<(), String> {
    ensure_asr_model_id(app, asr, &settings.asr_model)
}

/// Resolve which model a meeting should be transcribed with: the model it was
/// originally transcribed with (`meetings.asr_model`), falling back to the
/// current global setting for meetings transcribed before that column existed.
/// Echo-clean re-transcribes use this so they keep the meeting's recorded
/// model instead of jumping to whatever the global picker is on now.
fn meeting_asr_model(db: &crate::db::LazyDb, meeting_id: i64) -> Result<String, String> {
    let meeting = db.get_meeting(meeting_id)?;
    Ok(meeting
        .asr_model
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| db.get_settings().asr_model))
}

#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    engine: State<'_, CaptureEngine>,
    asr: State<'_, Arc<AsrEngine>>,
    asr_session: State<'_, AsrSession>,
    db: State<'_, Arc<LazyDb>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
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
        let vp2 = voiceprint.inner().clone();
        let db2 = db.inner().clone();
        let load: Result<Option<Arc<chunker::LiveVoiceprint>>, String> =
            tauri::async_runtime::spawn_blocking(move || {
                ensure_asr_model(&app2, &asr2, &cfg2)?;
                // Load the persona gallery and pre-warm the voiceprint model
                // so the first system chunk doesn't pay the load cost. If the
                // gallery is empty or the model isn't available, live ID is
                // silently skipped — system segments stay speaker: None.
                let live_vp = {
                    let gallery = db2.list_personas_with_voiceprints().unwrap_or_default();
                    let threshold = cfg2.persona_live_threshold;
                    if gallery.is_empty() || threshold >= 1.0 {
                        None
                    } else {
                        let _ = vp2.ensure_loaded(&app2);
                        Some(Arc::new(chunker::LiveVoiceprint::new(
                            app2.clone(),
                            vp2,
                            gallery,
                            threshold,
                        )))
                    }
                };
                Ok(live_vp)
            })
            .await
            .map_err(|e| e.to_string())?;
        match load {
            Ok(live_vp) => {
                let (tx, rx) = crossbeam_channel::bounded(1024);
                let started = engine.start(
                    app.clone(),
                    dir,
                    Some(tx),
                    cfg.aec_enabled,
                    cfg.aec_aggressiveness,
                )?;
                let handle = chunker::spawn_live_worker(
                    app.clone(),
                    asr.inner().clone(),
                    rx,
                    started.session_id.clone(),
                    live_vp,
                );
                *asr_session.0.lock().unwrap() = Some(handle);
                let meeting_id = create_meeting_at_start(&db, &started)?;
                return Ok(StartRecordingResponse {
                    started,
                    meeting_id,
                    live_transcription: true,
                    live_transcription_error: None,
                });
            }
            Err(e) => live_error = Some(e),
        }
    }

    let started = engine.start(
        app.clone(),
        dir,
        None,
        cfg.aec_enabled,
        cfg.aec_aggressiveness,
    )?;
    let meeting_id = create_meeting_at_start(&db, &started)?;
    Ok(StartRecordingResponse {
        started,
        meeting_id,
        live_transcription: false,
        live_transcription_error: live_error,
    })
}

#[tauri::command]
pub async fn stop_recording(
    engine: State<'_, CaptureEngine>,
    asr_session: State<'_, AsrSession>,
    db: State<'_, Arc<LazyDb>>,
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

    // Finalize the meeting row created at recording start with the end time
    // and WAV paths.
    let meeting_id = db
        .meeting_id_by_session(&stopped.session_id)?
        .ok_or_else(|| "no meeting row for this session".to_string())?;
    db.finalize_meeting(
        meeting_id,
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

/// Paths the audio player should load for a meeting: the echo-cleaned mic
/// when there is one (same preference as re-transcription), else the raw
/// mic, plus the system channel — decoded from FLAC into constant-bitrate
/// WAVs in the playback cache so the webview seeks exactly (see
/// `audio::playback`). Cheap on a cache hit; a legacy `.wav` passes through.
#[tauri::command]
pub async fn prepare_playback_audio(
    app: AppHandle,
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
) -> Result<playback::PlaybackAudio, String> {
    let db = db.inner().clone();
    let root = playback_cache_root(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let m = db.get_meeting(meeting_id)?;
        let (mic, system) = match (m.mic_cleaned_wav.or(m.mic_wav), m.system_wav) {
            (Some(mic), Some(system)) => (mic, system),
            _ => return Err("this meeting's audio files have been deleted".into()),
        };
        playback::prepare(&root, meeting_id, &mic, &system)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Batch transcription of a persisted meeting's WAVs. Emits `asr:segment`
/// events as it goes and saves the result.
#[tauri::command]
pub async fn transcribe_meeting(
    app: AppHandle,
    asr: State<'_, Arc<AsrEngine>>,
    db: State<'_, Arc<LazyDb>>,
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
        let mut segments = chunker::transcribe_wavs(
            app.clone(),
            &asr,
            format!("meeting-{meeting_id}"),
            &mic,
            &system,
        )?;
        // A meeting that was already diarized keeps its speaker labels (the
        // key for renames/personas/voiceprints): treat the previous system
        // segments as turns so the fresh transcript inherits them. A
        // following `diarize_meeting` then remaps against these.
        let old_turns: Vec<crate::diarize::Turn> =
            turns_from_segments(&db.meeting_segments_all(meeting_id)?)
                .into_iter()
                .filter(|t| t.speaker.starts_with(transcript::RAW_LABEL_PREFIX))
                .collect();
        if !old_turns.is_empty() {
            transcript::assign_speakers(&mut segments, &old_turns);
        }
        // Preserve echo/delete marks across the re-transcribe: replace_segments
        // DELETEs all rows, so snapshot the marks first and re-apply them to the
        // fresh rows by (source, start_ms ± 250ms).
        let marks = db.snapshot_marks(meeting_id)?;
        db.replace_segments(meeting_id, &segments)?;
        db.reapply_marks(meeting_id, &marks)?;
        // Record which model produced this meeting's transcript (first pass).
        db.set_meeting_asr_model(meeting_id, &cfg.asr_model)?;
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
    /// True if the audio files were removed per the delete-audio setting.
    pub audio_deleted: bool,
}

/// Payload of the `offline_aec:progress` event — drives the "Clean echo"
/// progress bar in Meeting Detail.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OfflineAecProgress {
    pub meeting_id: i64,
    /// 0..=1 fraction of the offline AEC pass completed.
    pub pct: f32,
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
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
    num_speakers: Option<i32>,
) -> Result<DiarizedTranscript, String> {
    let diarizer = diarizer.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (mic_wav, system_wav) = db.meeting_wavs(meeting_id)?;
        let system_wav = system_wav.ok_or("this meeting's audio files have been deleted")?;
        // The previous run's labels are the identities renames/personas are
        // keyed on; capture them before the pipeline rewrites the segments.
        let old = db.meeting_segments_all(meeting_id)?;
        let speaker_count = diarize_and_persist(
            &app,
            &diarizer,
            &voiceprint,
            &db,
            meeting_id,
            &system_wav,
            num_speakers,
            &old,
        )?;

        // Transcript + speakers are safely stored; drop the audio if asked
        // (mic, system, and any echo-cleaned mic).
        let mut audio_deleted = false;
        if db.get_settings().delete_audio_after_transcription {
            let cleaned = db.get_meeting(meeting_id)?.mic_cleaned_wav;
            for file in [mic_wav, Some(system_wav), cleaned].into_iter().flatten() {
                let _ = std::fs::remove_file(&file);
            }
            db.clear_audio_paths(meeting_id)?;
            if let Ok(root) = playback_cache_root(&app) {
                playback::invalidate(&root, meeting_id);
            }
            audio_deleted = true;
        }

        Ok(DiarizedTranscript {
            // Return the visible set (echo/deleted hidden) — the frontend
            // renders from `get_meeting`, which applies the same filter.
            segments: db.meeting_segments(meeting_id)?,
            speaker_count,
            audio_deleted,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Diarize the system channel and persist the result while keeping every
/// voice's existing `SPEAKER_xx` label. Those labels are the key for
/// per-meeting renames, persona links, and enrolled voiceprints, so a fresh
/// diarization must not re-mint them: new clusters are matched to the labels
/// in `old_segments` by time overlap (`transcript::remap_labels`), the
/// relabeled turns are assigned to the segments, and
/// `Db::reconcile_speakers` drops the link/rename/voiceprint of any label
/// that genuinely vanished. Shared by `diarize_meeting` and
/// `retranscribe_and_rediarize`.
///
/// `old_segments` must be captured by the caller BEFORE anything rewrites
/// the meeting's segments (a re-transcribe wipes speaker labels first).
/// Returns the distinct system-speaker count.
#[allow(clippy::too_many_arguments)]
fn diarize_and_persist(
    app: &AppHandle,
    diarizer: &DiarizeEngine,
    voiceprint: &VoiceprintEngine,
    db: &crate::db::LazyDb,
    meeting_id: i64,
    system_wav: &str,
    num_speakers: Option<i32>,
    old_segments: &[Segment],
) -> Result<usize, String> {
    // Load ALL segments (including echo-marked + soft-deleted) so the replace
    // below preserves their kind/deleted flags — diarization only relabels
    // system speakers; mic/echo/deleted rows pass through.
    let mut segments = db.meeting_segments_all(meeting_id)?;
    if segments.is_empty() {
        return Err("transcribe the meeting before identifying speakers".into());
    }

    let mut turns = diarizer.diarize_wav(app, system_wav, num_speakers)?;
    let map = transcript::remap_labels(old_segments, &turns);
    transcript::relabel_turns(&mut turns, &map);
    let speaker_count = transcript::assign_speakers(&mut segments, &turns);
    segments.sort_by_key(|s| s.start_ms);

    // Belt-and-suspenders: the rows already carry their marks (loaded via
    // _all), but snapshot/reapply keeps them safe if timestamp shifting ever
    // changes row identity.
    let marks = db.snapshot_marks(meeting_id)?;
    db.replace_segments(meeting_id, &segments)?;
    db.reapply_marks(meeting_id, &marks)?;

    db.reconcile_speakers(meeting_id, &raw_labels(&segments))?;
    db.set_diarize_num_speakers(meeting_id, num_speakers)?;

    // Identity layer (additive). Runs after speakers are persisted; never
    // blocks the diarize result on failure.
    let settings = db.get_settings();
    if let Err(e) = personas::identify_and_persist(
        db, voiceprint, app, meeting_id, system_wav, &turns, &settings,
    )
    .map(|m| {
        let _ = app.emit_to("main", "speakers:identified", m);
    }) {
        eprintln!("identify_speakers failed (non-fatal): {e}");
    }
    Ok(speaker_count)
}

/// Distinct diarizer labels (`SPEAKER_xx`) present in `segments`, sorted.
fn raw_labels(segments: &[Segment]) -> Vec<String> {
    let mut labels: Vec<String> = segments
        .iter()
        .filter_map(|s| s.speaker.clone())
        .filter(|s| s.starts_with(transcript::RAW_LABEL_PREFIX))
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

// ---------------------------------------------------------------------------
// Meetings CRUD (milestone 5)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_meetings(
    db: State<'_, Arc<LazyDb>>,
    search: Option<String>,
) -> Result<Vec<MeetingSummary>, String> {
    db.list_meetings(search.as_deref())
}

#[tauri::command]
pub fn get_meeting(db: State<'_, Arc<LazyDb>>, meeting_id: i64) -> Result<MeetingDetail, String> {
    db.get_meeting(meeting_id)
}

// ---------------------------------------------------------------------------
// Segment marks: mark-as-echo + soft-delete (Tier 2)
// ---------------------------------------------------------------------------

/// Every segment for a meeting including echo-marked + soft-deleted ones, for
/// the "show hidden" transcript toggle.
#[tauri::command]
pub fn list_hidden_segments(
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
) -> Result<Vec<Segment>, String> {
    db.meeting_segments_all(meeting_id)
}

/// Mark a mic segment the ASR mis-attributed to "Me" as echo. Echo segments are
/// hidden from the transcript + summary but retained for offline echo
/// re-processing (Tier 3 uses them as labeled echo windows).
#[tauri::command]
pub fn mark_segment_echo(db: State<'_, Arc<LazyDb>>, segment_id: i64) -> Result<(), String> {
    db.set_segment_kind(segment_id, "echo")
}

/// Revert an echo mark back to normal speech.
#[tauri::command]
pub fn unmark_segment_echo(db: State<'_, Arc<LazyDb>>, segment_id: i64) -> Result<(), String> {
    db.set_segment_kind(segment_id, "speech")
}

/// Soft-delete a segment (hide from transcript + summary, recoverable).
#[tauri::command]
pub fn delete_segment(db: State<'_, Arc<LazyDb>>, segment_id: i64) -> Result<(), String> {
    db.set_segment_deleted(segment_id, true)
}

/// Restore a soft-deleted segment.
#[tauri::command]
pub fn restore_segment(db: State<'_, Arc<LazyDb>>, segment_id: i64) -> Result<(), String> {
    db.set_segment_deleted(segment_id, false)
}

// ---------------------------------------------------------------------------
// Offline echo re-processing (Tier 3)
// ---------------------------------------------------------------------------

/// Re-transcribe a meeting's mic (from `mic_path`) + system recording and re-run
/// diarization on the system channel, keeping speaker labels — and with them
/// renames, persona links, and voiceprints — attached to the same voices.
/// Shared by `clean_echo` (mic_path = the cleaned file), `revert_echo_clean`
/// (mic_path = the original mic recording), and `retranscribe_meeting` (mic_path =
/// cleaned-or-original, `model_id` = the user's per-meeting pick). Echo/delete
/// marks are snapshotted and re-applied across both `replace_segments` calls so
/// they survive the rebuild. The meeting's `asr_model` is recorded as
/// `model_id` so the UI keeps showing which model produced the transcript.
#[allow(clippy::too_many_arguments)]
fn retranscribe_and_rediarize(
    app: &AppHandle,
    asr: &AsrEngine,
    diarizer: &DiarizeEngine,
    voiceprint: &VoiceprintEngine,
    db: &crate::db::LazyDb,
    meeting_id: i64,
    mic_path: &str,
    system_wav: &str,
    model_id: &str,
) -> Result<DiarizedTranscript, String> {
    ensure_asr_model_id(app, asr, model_id)?;
    // Snapshot the previous speaker labels BEFORE the transcribe rewrite
    // below wipes them; the diarize step needs them to keep each voice's
    // label (and everything keyed on it).
    let old = db.meeting_segments_all(meeting_id)?;
    let segments = chunker::transcribe_wavs(
        app.clone(),
        asr,
        format!("meeting-{meeting_id}"),
        mic_path,
        system_wav,
    )?;
    // Preserve echo/delete marks across the re-transcribe.
    let marks = db.snapshot_marks(meeting_id)?;
    db.replace_segments(meeting_id, &segments)?;
    db.reapply_marks(meeting_id, &marks)?;
    // Record which model produced this transcript.
    db.set_meeting_asr_model(meeting_id, model_id)?;

    // Re-diarize the (unchanged) system channel with the same cluster count
    // as the original run: diarization is deterministic, so identical
    // input + k reproduces the original clustering and labels. The remap in
    // `diarize_and_persist` covers whatever still shifts.
    let num_speakers = db.diarize_num_speakers(meeting_id)?;
    let speaker_count = diarize_and_persist(
        app,
        diarizer,
        voiceprint,
        db,
        meeting_id,
        system_wav,
        num_speakers,
        &old,
    )?;

    Ok(DiarizedTranscript {
        segments: db.meeting_segments(meeting_id)?,
        speaker_count,
        audio_deleted: false,
    })
}

/// Mic segments the user marked as echo (`source == "mic" && kind == "echo"`).
/// These are the supervised learning regions for the offline AEC filter — the
/// mic is pure echo there (near-end silent), so the adaptation error is a clean
/// echo residual. Shared by `clean_echo` and `clean_echo_segment`.
fn collect_echo_windows(
    db: &crate::db::LazyDb,
    meeting_id: i64,
) -> Result<Vec<offline_aec::EchoWindow>, String> {
    Ok(db
        .meeting_segments_all(meeting_id)?
        .iter()
        .filter(|s| s.source == "mic" && s.kind == "echo")
        .map(|s| offline_aec::EchoWindow {
            start_ms: s.start_ms,
            end_ms: s.end_ms,
        })
        .collect())
}

/// Mic speech segments (`source == "mic" && kind == "speech" && !deleted`) as
/// apply ranges — the regions the whole-meeting clean should de-echo. Pure-echo
/// regions and untouched audio are left alone (learned from, not mangled),
/// which is the fix for the "added echo to everything" failure.
fn speech_apply_ranges(
    db: &crate::db::LazyDb,
    meeting_id: i64,
) -> Result<Vec<offline_aec::EchoWindow>, String> {
    Ok(db
        .meeting_segments_all(meeting_id)?
        .iter()
        .filter(|s| s.source == "mic" && s.kind == "speech" && !s.deleted)
        .map(|s| offline_aec::EchoWindow {
            start_ms: s.start_ms,
            end_ms: s.end_ms,
        })
        .collect())
}

/// Shared body for `clean_echo` (whole-meeting) and `clean_echo_segment`
/// (per-segment): run offline AEC with the given learning `windows` + apply
/// `scope`, persist `mic_cleaned_wav`, then re-transcribe + re-diarize. Emits
/// `offline_aec:progress` events (0..=1) keyed by `meeting_id`.
#[allow(clippy::too_many_arguments)]
fn run_offline_clean(
    app: &AppHandle,
    asr: &AsrEngine,
    diarizer: &DiarizeEngine,
    voiceprint: &VoiceprintEngine,
    db: &crate::db::LazyDb,
    meeting_id: i64,
    mic_wav: &str,
    system_wav: &str,
    windows: &[offline_aec::EchoWindow],
    scope: offline_aec::ApplyScope,
) -> Result<DiarizedTranscript, String> {
    // Write mic_cleaned.flac next to the mic recording. A previous clean may
    // have left a differently named file (legacy `mic_cleaned.wav`); drop it
    // so overwriting the pointer below doesn't orphan it.
    let out_path = crate::audio::codec::cleaned_mic_path(mic_wav);
    let out_str = out_path.to_string_lossy().into_owned();
    if let Some(old) = db.get_meeting(meeting_id)?.mic_cleaned_wav {
        if old != out_str {
            let _ = std::fs::remove_file(&old);
        }
    }

    // Run the offline filter with progress events. If no echo windows were
    // marked, the filter falls back to unsupervised adaptation — but the apply
    // scope bounds the blast radius to the requested region(s), so a wrong `W`
    // can never again corrupt the whole recording.
    {
        let app_prog = app.clone();
        let id_prog = meeting_id;
        let progress = move |p: f32| {
            let _ = app_prog.emit_to(
                "main",
                "offline_aec:progress",
                OfflineAecProgress {
                    meeting_id: id_prog,
                    pct: p,
                },
            );
        };
        offline_aec::clean(mic_wav, system_wav, windows, &scope, &out_str, progress)?;
    }
    db.set_mic_cleaned_wav(meeting_id, &out_str)?;

    // Keep the meeting's recorded model (not the current global one) so an
    // echo-clean doesn't silently swap the transcript to a different model.
    let model_id = meeting_asr_model(db, meeting_id)?;
    retranscribe_and_rediarize(
        app, asr, diarizer, voiceprint, db, meeting_id, &out_str, system_wav, &model_id,
    )
}

/// Run offline AEC on a meeting's mic recording using the system recording as
/// the exact echo reference, seeded by the user's echo-marked mic segments,
/// then re-transcribe the cleaned mic and re-diarize. The apply scope is the
/// mic **speech** segments only (learn from marked echo regions, apply to
/// speech) — so pure-echo regions and untouched audio are not mangled. With no
/// speech segments this is a no-op (no `mic_cleaned_wav` written, transcript
/// unchanged). The original mic file is preserved; the cleaned path is stored
/// in `meetings.mic_cleaned_wav` so the action is revertible via
/// `revert_echo_clean`. Emits `offline_aec:progress` events (0..=1).
#[tauri::command]
pub async fn clean_echo(
    app: AppHandle,
    asr: State<'_, Arc<AsrEngine>>,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
) -> Result<DiarizedTranscript, String> {
    let asr = asr.inner().clone();
    let diarizer = diarizer.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (mic_wav, system_wav) = db.meeting_wavs(meeting_id)?;
        let (mic_wav, system_wav) = match (mic_wav, system_wav) {
            (Some(m), Some(s)) => (m, s),
            _ => return Err("this meeting's audio files have been deleted".into()),
        };

        let windows = collect_echo_windows(&db, meeting_id)?;
        let apply = speech_apply_ranges(&db, meeting_id)?;
        eprintln!(
            "[clean_echo] meeting {meeting_id}: {} marked echo window(s), {} speech apply region(s)",
            windows.len(),
            apply.len()
        );

        // No speech to de-echo → true no-op (don't blast the whole track).
        // Drop any stale cleaned file + pointer and return the current transcript.
        if apply.is_empty() {
            if let Some(cleaned) = db.clear_mic_cleaned_wav(meeting_id)? {
                let _ = std::fs::remove_file(&cleaned);
            }
            return Ok(DiarizedTranscript {
                segments: db.meeting_segments(meeting_id)?,
                speaker_count: db.get_meeting(meeting_id)?.speaker_count as usize,
                audio_deleted: false,
            });
        }

        run_offline_clean(
            &app,
            &asr,
            &diarizer,
            &voiceprint,
            &db,
            meeting_id,
            &mic_wav,
            &system_wav,
            &windows,
            offline_aec::ApplyScope::Ranges(apply),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Per-segment offline echo clean: learn the echo path from the user's marked
/// echo regions (same as `clean_echo`), but apply the clean to a single
/// `[start_ms, end_ms]` region only — the segment the user selected. Audio
/// outside that region is left bit-identical, so any filter imperfection is
/// confined to the requested segment (no "added echo to everything"). Reuses
/// `run_offline_clean` + `retranscribe_and_rediarize`; emits the same
/// `offline_aec:progress` events so the MeetingDetail progress bar works
/// unchanged.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn clean_echo_segment(
    app: AppHandle,
    asr: State<'_, Arc<AsrEngine>>,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
    start_ms: u64,
    end_ms: u64,
) -> Result<DiarizedTranscript, String> {
    let asr = asr.inner().clone();
    let diarizer = diarizer.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (mic_wav, system_wav) = db.meeting_wavs(meeting_id)?;
        let (mic_wav, system_wav) = match (mic_wav, system_wav) {
            (Some(m), Some(s)) => (m, s),
            _ => return Err("this meeting's audio files have been deleted".into()),
        };

        let windows = collect_echo_windows(&db, meeting_id)?;
        let apply = vec![offline_aec::EchoWindow { start_ms, end_ms }];
        eprintln!(
            "[clean_echo_segment] meeting {meeting_id}: region {start_ms}..{end_ms} ms, {} marked echo window(s) for learning",
            windows.len()
        );

        run_offline_clean(
            &app,
            &asr,
            &diarizer,
            &voiceprint,
            &db,
            meeting_id,
            &mic_wav,
            &system_wav,
            &windows,
            offline_aec::ApplyScope::Ranges(apply),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Revert an offline echo clean: drop the `mic_cleaned_wav` pointer and
/// re-transcribe from the original mic recording. Marks are snapshot/reapplied so
/// echo/delete marks survive the rebuild.
#[tauri::command]
pub async fn revert_echo_clean(
    app: AppHandle,
    asr: State<'_, Arc<AsrEngine>>,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
) -> Result<DiarizedTranscript, String> {
    let asr = asr.inner().clone();
    let diarizer = diarizer.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (mic_wav, system_wav) = db.meeting_wavs(meeting_id)?;
        let (mic_wav, system_wav) = match (mic_wav, system_wav) {
            (Some(m), Some(s)) => (m, s),
            _ => return Err("this meeting's audio files have been deleted".into()),
        };
        // Best effort: remove the cleaned file from disk so we don't
        // accumulate stale copies if the user re-runs a clean later.
        if let Some(cleaned) = db.clear_mic_cleaned_wav(meeting_id)? {
            let _ = std::fs::remove_file(&cleaned);
        }
        let model_id = meeting_asr_model(&db, meeting_id)?;
        retranscribe_and_rediarize(
            &app,
            &asr,
            &diarizer,
            &voiceprint,
            &db,
            meeting_id,
            &mic_wav,
            &system_wav,
            &model_id,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Re-transcribe an already-transcribed meeting with a different whisper model
/// (the user's per-meeting pick from the dropdown), then re-diarize. Does NOT
/// touch the global/live `settings.asr_model` — future recordings keep using
/// that. The mic source is the echo-cleaned mic if one exists (so an echo clean
/// survives a model swap), otherwise the original mic recording. Echo/delete marks
/// are snapshot/reapplied so they survive the rebuild, and the meeting's
/// `asr_model` is recorded as `model_id` so the UI reflects the new model.
#[tauri::command]
pub async fn retranscribe_meeting(
    app: AppHandle,
    asr: State<'_, Arc<AsrEngine>>,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
    model_id: String,
) -> Result<DiarizedTranscript, String> {
    let asr = asr.inner().clone();
    let diarizer = diarizer.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let detail = db.get_meeting(meeting_id)?;
        let (mic_wav, system_wav) = match (detail.mic_wav.clone(), detail.system_wav.clone()) {
            (Some(m), Some(s)) => (m, s),
            _ => return Err("this meeting's audio files have been deleted".into()),
        };
        // Prefer the echo-cleaned mic if present so a clean survives a model swap.
        let mic_path = detail.mic_cleaned_wav.unwrap_or(mic_wav);
        retranscribe_and_rediarize(
            &app,
            &asr,
            &diarizer,
            &voiceprint,
            &db,
            meeting_id,
            &mic_path,
            &system_wav,
            &model_id,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn update_meeting_title(
    db: State<'_, Arc<LazyDb>>,
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
pub fn update_meeting_notes(
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
    notes: String,
) -> Result<(), String> {
    db.update_notes(meeting_id, &notes)
}

#[tauri::command]
pub fn rename_speaker(
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
    raw_label: String,
    display_name: Option<String>,
) -> Result<(), String> {
    let name = display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    db.rename_speaker(meeting_id, &raw_label, name)
}

// ---------------------------------------------------------------------------
// Personas + voiceprints (milestone 9)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_personas(db: State<'_, Arc<LazyDb>>) -> Result<Vec<Persona>, String> {
    db.list_personas()
}

#[tauri::command]
pub fn create_persona(db: State<'_, Arc<LazyDb>>, display_name: String) -> Result<i64, String> {
    db.create_persona(&display_name)
}

#[tauri::command]
pub fn rename_persona(
    db: State<'_, Arc<LazyDb>>,
    persona_id: i64,
    display_name: String,
) -> Result<(), String> {
    db.rename_persona(persona_id, &display_name)
}

#[tauri::command]
pub fn delete_persona(db: State<'_, Arc<LazyDb>>, persona_id: i64) -> Result<(), String> {
    db.delete_persona(persona_id)
}

#[tauri::command]
pub fn delete_all_voiceprints(db: State<'_, Arc<LazyDb>>) -> Result<(), String> {
    db.delete_all_voiceprints()
}

// ---------------------------------------------------------------------------
// Customers (accounts)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_customers(db: State<'_, Arc<LazyDb>>) -> Result<Vec<CustomerSummary>, String> {
    db.list_customers()
}

#[tauri::command]
pub fn create_customer(
    db: State<'_, Arc<LazyDb>>,
    name: String,
    notes: Option<String>,
) -> Result<i64, String> {
    db.create_customer(&name, notes.as_deref())
}

#[tauri::command]
pub fn rename_customer(
    db: State<'_, Arc<LazyDb>>,
    customer_id: i64,
    name: String,
) -> Result<(), String> {
    db.rename_customer(customer_id, &name)
}

#[tauri::command]
pub fn update_customer_notes(
    db: State<'_, Arc<LazyDb>>,
    customer_id: i64,
    notes: Option<String>,
) -> Result<(), String> {
    db.update_customer_notes(customer_id, notes.as_deref())
}

#[tauri::command]
pub fn get_customer(
    db: State<'_, Arc<LazyDb>>,
    customer_id: i64,
) -> Result<CustomerDetail, String> {
    db.get_customer(customer_id)
}

/// Delete a customer. Its meetings become unassigned (FK ON DELETE SET NULL);
/// personas are global and untouched.
#[tauri::command]
pub fn delete_customer(db: State<'_, Arc<LazyDb>>, customer_id: i64) -> Result<(), String> {
    db.delete_customer(customer_id)
}

/// Reassign a meeting to a different customer (or unassign with null).
#[tauri::command]
pub fn set_meeting_customer(
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
    customer_id: Option<i64>,
) -> Result<(), String> {
    db.set_meeting_customer(meeting_id, customer_id)
}

/// Merge `source_id` into `target_id` (irreversible): reassigns all of
/// source's meetings to target, then deletes source. Personas need no change
/// — target's derived roster naturally reflects the union.
#[tauri::command]
pub fn merge_customers(
    db: State<'_, Arc<LazyDb>>,
    source_id: i64,
    target_id: i64,
) -> Result<(), String> {
    db.merge_customers(source_id, target_id)
}

#[tauri::command]
pub fn search_customer_meetings(
    db: State<'_, Arc<LazyDb>>,
    customer_id: i64,
    query: String,
) -> Result<Vec<CustomerSearchResult>, String> {
    db.search_customer_meetings(customer_id, &query)
}

#[tauri::command]
pub fn list_customer_summaries(
    db: State<'_, Arc<LazyDb>>,
    customer_id: i64,
) -> Result<Vec<CustomerRollupRow>, String> {
    db.list_customer_summaries(customer_id)
}

/// Reconstruct diarization turns from persisted system-channel segments.
/// Avoids re-diarizing on identify/confirm, which could produce different
/// `SPEAKER_xx` labels than the original run when a declared speaker count
/// was used. Segments with no speaker label (pre-diarization) are skipped.
fn turns_from_segments(segments: &[Segment]) -> Vec<crate::diarize::Turn> {
    segments
        .iter()
        .filter(|s| s.source == "system")
        .filter_map(|s| {
            s.speaker.clone().map(|sp| crate::diarize::Turn {
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                speaker: sp,
            })
        })
        .collect()
}

/// Re-run embedding + matching for a meeting's diarized speakers; persists
/// suggestions (confirmed links are frozen). Useful to re-match after the
/// gallery grew. Fails gracefully if the audio has been deleted. Turns are
/// rebuilt from persisted segments (no re-diarization), so labels stay
/// stable; per-label rows for labels no longer in the transcript are
/// reconciled away first.
#[tauri::command]
pub async fn identify_speakers(
    app: AppHandle,
    db: State<'_, Arc<LazyDb>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    meeting_id: i64,
) -> Result<Vec<personas::SpeakerMatch>, String> {
    let db = db.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (_, system_wav) = db.meeting_wavs(meeting_id)?;
        let system_wav = system_wav.ok_or("this meeting's audio files have been deleted")?;
        db.reconcile_speakers(
            meeting_id,
            &raw_labels(&db.meeting_segments_all(meeting_id)?),
        )?;
        let turns = turns_from_segments(&db.meeting_segments(meeting_id)?);
        let settings = db.get_settings();
        let matches = personas::identify_and_persist(
            &db,
            &voiceprint,
            &app,
            meeting_id,
            &system_wav,
            &turns,
            &settings,
        )?;
        let _ = app.emit_to("main", "speakers:identified", matches.clone());
        Ok(matches)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Relevance data for the speaker picker: per-label voice matches (scored
/// from stored embeddings, no audio needed) and the meeting's customer
/// roster. Read-only; never touches links.
#[tauri::command]
pub async fn speaker_persona_candidates(
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
) -> Result<personas::SpeakerCandidates, String> {
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        personas::speaker_candidates(&db, meeting_id, &db.get_settings())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Confirm that `raw_label` in a meeting is `persona_id`. In one transaction:
/// marks the link confirmed, drops any voiceprint previously enrolled from
/// this speaker (so a changed mind doesn't leave the voice under the old
/// persona), applies the persona name via the per-meeting display mapping,
/// and enrolls the embedding stored on the speaker row at identify time —
/// so both persona counts change together, instantly. Only when no stored
/// embedding exists (meeting identified before embeddings were persisted)
/// does it fall back to a detached audio-based enrollment (slow — CAM++
/// over the system WAV). A `voiceprints:enrolled` event fires either way so
/// persona counts can refresh.
#[tauri::command]
pub async fn confirm_speaker_persona(
    app: AppHandle,
    db: State<'_, Arc<LazyDb>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    meeting_id: i64,
    raw_label: String,
    persona_id: i64,
) -> Result<(), String> {
    let db2 = db.inner().clone();
    let raw = raw_label.clone();
    // Everything the chip and persona counts reflect runs synchronously so
    // the UI is consistent as soon as the command resolves.
    let enrolled = tauri::async_runtime::spawn_blocking(move || {
        let name = db2
            .list_personas()?
            .into_iter()
            .find(|p| p.id == persona_id)
            .map(|p| p.display_name)
            .ok_or("persona not found")?;
        let cap = db2.get_settings().voiceprint_gallery_cap;
        // Link confirmed + stale voiceprint dropped + display name applied
        // + stored embedding enrolled, atomically.
        db2.confirm_link(meeting_id, &raw, persona_id, &name, cap)
    })
    .await
    .map_err(|e| e.to_string())??;
    if enrolled {
        let _ = app.emit_to("main", "voiceprints:enrolled", ());
        return Ok(());
    }

    // Fallback: no stored embedding for this speaker. Enroll from audio,
    // detached — extraction takes a few seconds and must not block the chip
    // update. Emit when done so the frontend can refresh persona counts.
    let db3 = db.inner().clone();
    let voiceprint2 = voiceprint.inner().clone();
    let raw2 = raw_label.clone();
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Ok((_, Some(system_wav))) = db3.meeting_wavs(meeting_id) {
            if let Ok(segments) = db3.meeting_segments(meeting_id) {
                let turns = turns_from_segments(&segments);
                let cap = db3.get_settings().voiceprint_gallery_cap;
                let _ = personas::enroll(
                    &db3,
                    &voiceprint2,
                    &app2,
                    persona_id,
                    meeting_id,
                    &raw2,
                    Some(&system_wav),
                    &turns,
                    cap,
                );
            }
        }
        let _ = app2.emit_to("main", "voiceprints:enrolled", ());
    });
    Ok(())
}

/// Remove the persona link for a raw label, the voiceprint enrolled from it,
/// and any display rename applied by a prior confirm (revert to the raw
/// `SPEAKER_xx` label). All run in one transaction so a partial failure can't
/// leave the link removed but the persona's name or voice still around.
#[tauri::command]
pub fn unlink_speaker_persona(
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
    raw_label: String,
) -> Result<(), String> {
    db.unlink_and_clear_rename(meeting_id, &raw_label)?;
    Ok(())
}

/// Delete a meeting row; also removes its audio files (mic, system, and the
/// echo-cleaned mic if any) from disk, plus its playback cache.
#[tauri::command]
pub fn delete_meeting(
    app: AppHandle,
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
) -> Result<(), String> {
    let paths = db.delete_meeting(meeting_id)?;
    for file in paths.iter() {
        let _ = std::fs::remove_file(file);
    }
    if let Ok(root) = playback_cache_root(&app) {
        playback::invalidate(&root, meeting_id);
    }
    // Remove the (now likely empty) session directory. Non-recursive on
    // purpose: anything we don't know about stays.
    if let Some(dir) = paths
        .iter()
        .next()
        .and_then(|p| std::path::Path::new(p).parent())
    {
        let _ = std::fs::remove_dir(dir);
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
    tauri::async_runtime::spawn_blocking(|| crate::audio::system_tap::probe_access().map(|_| true))
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
pub fn get_settings(db: State<'_, Arc<LazyDb>>) -> AppSettings {
    db.get_settings()
}

/// Persist settings and reconcile the MCP server with them. Returns the
/// stored settings, which may differ from the input: enabling MCP for the
/// first time generates the bearer token server-side.
#[tauri::command]
pub fn update_settings(
    db: State<'_, Arc<LazyDb>>,
    mcp_server: State<'_, McpServer>,
    new_settings: AppSettings,
) -> Result<AppSettings, String> {
    let mut s = new_settings;
    if s.mcp_enabled && s.mcp_port < 1024 {
        return Err("MCP port must be 1024 or higher".into());
    }
    if s.mcp_enabled && s.mcp_token.as_deref().is_none_or(str::is_empty) {
        s.mcp_token = Some(mcp::generate_token());
    }
    db.set_settings(&s)?;
    // Start/stop failures (e.g. port in use) are surfaced via `mcp_status`,
    // never by failing the settings save.
    mcp_server.apply_settings(db.inner().clone(), &s);
    Ok(s)
}

// ---------------------------------------------------------------------------
// MCP server (read-only, localhost)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn mcp_status(mcp_server: State<'_, McpServer>) -> McpStatus {
    mcp_server.status()
}

/// Rotate the MCP bearer token (invalidates every configured client) and
/// restart the server with it. Returns the stored settings.
#[tauri::command]
pub fn regenerate_mcp_token(
    db: State<'_, Arc<LazyDb>>,
    mcp_server: State<'_, McpServer>,
) -> Result<AppSettings, String> {
    let mut s = db.get_settings();
    s.mcp_token = Some(mcp::generate_token());
    db.set_settings(&s)?;
    mcp_server.apply_settings(db.inner().clone(), &s);
    Ok(s)
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
    db: State<'_, Arc<LazyDb>>,
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
// Built-in LLM models (Qwen3.5 GGUF via llama.cpp)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeLlmModelInfo {
    pub id: String,
    pub label: String,
    pub approx_bytes: u64,
    pub note: String,
    pub downloaded: bool,
    pub active: bool,
}

#[tauri::command]
pub fn list_native_models(
    app: AppHandle,
    db: State<'_, Arc<LazyDb>>,
) -> Result<Vec<NativeLlmModelInfo>, String> {
    let settings_model = db.get_settings().summary_model;
    Ok(models::NATIVE_LLM_MODELS
        .iter()
        .map(|m| {
            let path = models::native_llm_model_path(&app, m.id).unwrap_or_default();
            NativeLlmModelInfo {
                id: m.id.into(),
                label: m.label.into(),
                approx_bytes: m.approx_bytes,
                note: m.note.into(),
                downloaded: path.exists(),
                active: settings_model.as_deref() == Some(m.id),
            }
        })
        .collect())
}

#[tauri::command]
pub async fn download_native_model(
    app: AppHandle,
    downloads: State<'_, DownloadManager>,
    id: String,
) -> Result<(), String> {
    let model = models::native_llm_model(&id).ok_or_else(|| format!("unknown LLM model: {id}"))?;
    let dest = models::native_llm_model_path(&app, &id)?;
    if dest.exists() {
        return Ok(());
    }
    // Ensure the llm/ subdirectory exists before the download writes into it.
    models::native_llm_dir(&app)?;
    let key = format!("llm:{id}");
    let cancel = downloads.begin(&key)?;
    let url = model.url;
    let id2 = id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        models::download_with_progress(&app, &id2, url, &dest, &cancel)
    })
    .await
    .map_err(|e| e.to_string())?;
    downloads.finish(&key);
    result
}

#[tauri::command]
pub fn cancel_native_model_download(downloads: State<'_, DownloadManager>, id: String) -> bool {
    downloads.cancel(&format!("llm:{id}"))
}

#[tauri::command]
pub async fn native_model_status(
    app: AppHandle,
    client: State<'_, SidecarLlmClient>,
) -> Result<bool, String> {
    Ok(client.status(&app).await)
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
pub async fn list_ollama_models(db: State<'_, Arc<LazyDb>>) -> Result<OllamaModels, String> {
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
    /// Short AI-generated title produced from the summary, when the meeting's
    /// title was still the default placeholder at summary time. `None`/null
    /// means the title was left untouched (manually renamed, or generation
    /// failed / produced nothing usable).
    pub title: Option<String>,
}

/// Generate a summary for a meeting, streaming tokens via `summary:token`
/// events, and persist it. `model` overrides the configured/auto-picked one.
/// Dispatches to the built-in llama.cpp engine or Ollama based on
/// `settings.summary_backend`.
#[tauri::command]
pub async fn summarize_meeting(
    app: AppHandle,
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
    model: Option<String>,
) -> Result<SummaryResult, String> {
    let meeting = db.get_meeting(meeting_id)?;
    if meeting.segments.is_empty() {
        return Err("transcribe the meeting before summarizing".into());
    }

    let settings = db.get_settings();

    let template = settings
        .summary_template
        .clone()
        .unwrap_or_else(|| summary::DEFAULT_TEMPLATE.to_string());
    let transcript = summary::transcript_text(&meeting.segments, &meeting.renames);
    let prompt = summary::build_prompt(&template, &meeting.title, &transcript);

    let app_for_tokens = app.clone();
    let app_for_title = app.clone();
    let settings_for_title = settings.clone();
    let (content, model_id) = dispatch_summary(
        app,
        settings,
        prompt,
        model.clone(),
        Box::new(move |token, is_thinking| {
            let _ = app_for_tokens.emit_to(
                "main",
                "summary:token",
                summary::SummaryToken {
                    meeting_id,
                    token: token.to_string(),
                    is_thinking,
                },
            );
        }),
    )
    .await?;

    let summary_id = db.insert_summary(meeting_id, &model_id, &template, &content)?;

    // Best-effort: if the title is still the recording-start placeholder,
    // ask the model for a short title derived from the summary we just
    // generated and persist it. Non-fatal — a failure or empty result
    // leaves the existing title untouched and the summary still succeeds.
    let generated_title = if meeting.title.starts_with(DEFAULT_TITLE_PREFIX) {
        generate_meeting_title(app_for_title, settings_for_title, &content, model)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    if let Some(t) = &generated_title {
        let _ = db.update_title(meeting_id, t);
    }

    Ok(SummaryResult {
        summary_id,
        model: model_id,
        content,
        title: generated_title,
    })
}

/// Ask the model for a short meeting title derived from `summary`. Uses the
/// same dispatch/model as the summary itself, with a no-op token callback
/// (the title is short and not streamed — it appears at the end). Returns
/// `Ok(None)` when there's nothing to title from.
async fn generate_meeting_title(
    app: AppHandle,
    settings: AppSettings,
    summary: &str,
    model: Option<String>,
) -> Result<Option<String>, String> {
    if summary.trim().is_empty() {
        return Ok(None);
    }
    let prompt = summary::build_title_prompt(summary);
    let (raw, _model_id) =
        dispatch_summary(app, settings, prompt, model, Box::new(|_t, _i| {})).await?;
    Ok(clean_title(&raw))
}

/// Normalize a model-produced title: trim, strip one layer of matching
/// surrounding quotes, drop a trailing period, collapse internal whitespace,
/// cap the length. Returns `None` if nothing usable remains.
fn clean_title(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_string();
    // Strip one layer of matching surrounding quotes (" or ').
    if s.len() >= 2 {
        let first = s.chars().next().unwrap();
        let last = s.chars().last().unwrap();
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            s = s[1..s.len() - 1].trim().to_string();
        }
    }
    // Drop a single trailing period.
    if s.ends_with('.') {
        s.pop();
        s = s.trim().to_string();
    }
    // Collapse internal whitespace runs to single spaces.
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    // Cap length (keep whole words).
    const MAX: usize = 100;
    if s.chars().count() > MAX {
        s = s.chars().take(MAX).collect();
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Shared LLM dispatch for per-meeting summaries and customer rollups. Picks
/// the backend from `settings.summary_backend`, resolves the model (explicit,
/// then settings, then auto-pick), streams tokens through `on_token`, and
/// returns `(content, model_id)`. The caller owns event emission (closes over
/// the right event name + entity id).
#[allow(clippy::type_complexity)]
async fn dispatch_summary(
    app: AppHandle,
    settings: AppSettings,
    prompt: String,
    model: Option<String>,
    on_token: Box<dyn FnMut(&str, bool) + Send>,
) -> Result<(String, String), String> {
    let backend = SummaryBackend::from_str(&settings.summary_backend);
    match backend {
        SummaryBackend::Native => {
            // Resolve model id: explicit > settings.summary_model > first downloaded.
            let model_id = match model.clone().or(settings.summary_model.clone()) {
                Some(m) => m,
                None => {
                    let downloaded: Vec<&str> = models::NATIVE_LLM_MODELS
                        .iter()
                        .filter(|m| {
                            models::native_llm_model_path(&app, m.id)
                                .map(|p| p.exists())
                                .unwrap_or(false)
                        })
                        .map(|m| m.id)
                        .collect();
                    downloaded
                        .first()
                        .copied()
                        .ok_or(
                            "no built-in model downloaded — download one in Settings → Summaries",
                        )?
                        .to_string()
                }
            };

            let gguf_path = models::native_llm_model_path(&app, &model_id)?;
            if !gguf_path.exists() {
                return Err(format!(
                    "model file not found for {model_id} — re-download in Settings → Summaries"
                ));
            }

            let client = app.state::<SidecarLlmClient>();
            let mut on_token = on_token;
            let content = client
                .generate(
                    &app,
                    &gguf_path.to_string_lossy(),
                    &prompt,
                    move |token, is_thinking| {
                        on_token(token, is_thinking);
                    },
                )
                .await?;

            Ok((content, model_id))
        }
        SummaryBackend::Ollama => {
            // Resolve model: explicit > settings > curated auto-pick.
            let model_id = match model.clone().or(settings.summary_model.clone()) {
                Some(m) => m,
                None => {
                    let installed: Vec<String> = ollama::installed_models()
                        .await?
                        .into_iter()
                        .map(|m| m.name)
                        .collect();
                    summary::pick_default_model(&installed)
                        .ok_or("no Ollama model installed — pull one in Settings → Summaries")?
                }
            };

            let mut on_token = on_token;
            let content = ollama::chat_stream(&model_id, &prompt, |token| {
                on_token(token, false);
            })
            .await?;

            Ok((content, model_id))
        }
    }
}

#[tauri::command]
pub fn list_summaries(
    db: State<'_, Arc<LazyDb>>,
    meeting_id: i64,
) -> Result<Vec<crate::db::SummaryRow>, String> {
    db.list_summaries(meeting_id)
}

/// Result of a customer rollup generation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerSummaryResult {
    pub summary_id: i64,
    pub model: String,
    pub content: String,
    pub meeting_count: i64,
}

/// Generate a "customer at a glance" rollup: condenses the latest summaries
/// of the customer's recent meetings into a short topics overview, streamed
/// via `customer-summary:token` events and persisted to `customer_summaries`.
/// Requires at least 2 meetings with a saved summary.
#[tauri::command]
pub async fn summarize_customer(
    app: AppHandle,
    db: State<'_, Arc<LazyDb>>,
    customer_id: i64,
    model: Option<String>,
) -> Result<CustomerSummaryResult, String> {
    let customer = db.get_customer(customer_id)?;
    // Gather the last 6 meetings (reverse-chrono) that have a saved summary.
    let rows = db.customer_meetings_with_latest_summary(customer_id, 6)?;
    if rows.len() < 2 {
        return Err("generate summaries on at least two of this customer's meetings first".into());
    }
    let meeting_ids: Vec<i64> = rows.iter().map(|(id, _, _, _)| *id).collect();
    let entries: Vec<(String, String, String)> = rows
        .iter()
        .map(|(_, title, started, content)| {
            let date = chrono::DateTime::from_timestamp_millis(*started)
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| started.to_string());
            (title.clone(), date, content.clone())
        })
        .collect();
    let summaries_text = summary::format_rollup_summaries(&entries);
    let template = summary::DEFAULT_CUSTOMER_ROLLUP_TEMPLATE;
    let prompt = summary::build_customer_rollup_prompt(template, &customer.name, &summaries_text);

    let settings = db.get_settings();
    let app_for_tokens = app.clone();
    let (content, model_id) = dispatch_summary(
        app,
        settings,
        prompt,
        model,
        Box::new(move |token, is_thinking| {
            let _ = app_for_tokens.emit_to(
                "main",
                "customer-summary:token",
                summary::CustomerSummaryToken {
                    customer_id,
                    token: token.to_string(),
                    is_thinking,
                },
            );
        }),
    )
    .await?;

    let summary_id = db.insert_customer_summary(customer_id, &model_id, &content, &meeting_ids)?;
    Ok(CustomerSummaryResult {
        summary_id,
        model: model_id,
        content,
        meeting_count: meeting_ids.len() as i64,
    })
}
