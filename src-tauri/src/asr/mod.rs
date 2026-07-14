//! Transcription via whisper.cpp (whisper-rs, Metal backend).
//!
//! - One `WhisperContext` is kept loaded (models take seconds to load) and
//!   swapped when the user picks a different model.
//! - `chunker` implements the near-live strategy: buffer 16 kHz audio per
//!   channel, cut at silence (or a max window), transcribe with the previous
//!   chunk's text as the initial prompt for continuity.
//! - The same chunker handles batch mode (whole WAV in one pass, chunked
//!   internally) so live and batch produce consistent output.

pub mod chunker;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

/// Idle window before the loaded model is freed. Mirrors the llama-server
/// sidecar's keep-alive (`summary/sidecar.rs`) and Ollama's default: keep the
/// model resident across back-to-back transcriptions, then drop it (and the
/// Metal backend) to reclaim GPU memory once the user goes idle.
const UNLOAD_AFTER: Duration = Duration::from_secs(5 * 60); // 5 min idle
const UNLOAD_POLL: Duration = Duration::from_secs(30);

/// A transcribed segment, timestamped on the meeting's shared timeline.
#[derive(Serialize, serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// DB row id. 0 for live/ephemeral segments before they're persisted (the
    /// DB assigns the real id on insert); the frontend uses it to address
    /// mark-as-echo / delete mutations on persisted segments.
    #[serde(default)]
    pub id: i64,
    /// "mic" (labeled Me) or "system" (diarized).
    pub source: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// "Me" for mic; "SPEAKER_xx" for system after diarization; None until
    /// then (the UI shows a generic "Speaker" chip).
    #[serde(default)]
    pub speaker: Option<String>,
    /// `'speech'` (normal) or `'echo'` — a mic region the user marked as echo
    /// (the ASR mis-attributed the speaker echo to "Me"). Echo segments are
    /// hidden from the transcript + summary but retained for the offline echo
    /// re-processing path, which uses them as labeled echo windows.
    #[serde(default = "default_segment_kind")]
    pub kind: String,
    /// Soft-delete flag — hidden from the transcript + summary, recoverable.
    #[serde(default)]
    pub deleted: bool,
}

/// Serde default for `Segment::kind` (and the struct `Default`).
fn default_segment_kind() -> String {
    "speech".to_string()
}

impl Default for Segment {
    fn default() -> Self {
        Self {
            id: 0,
            source: String::new(),
            start_ms: 0,
            end_ms: 0,
            text: String::new(),
            speaker: None,
            kind: default_segment_kind(),
            deleted: false,
        }
    }
}

/// A loaded model: the whisper context (weights) plus its long-lived
/// `WhisperState`. The state owns the KV cache, compute buffers, and the
/// Metal backend — ~700 MB that must be allocated once and reused across
/// every chunk, not recreated per chunk (which would tear down and spin the
/// Metal backend on every silence cut). `whisper_full_with_state` clears the
/// KV cache at the start of each call, so reusing one state across chunks is
/// safe; we set `no_context(true)` and thread cross-chunk context via the
/// initial prompt anyway.
struct LoadedModel {
    id: String,
    // `WhisperState` holds a raw pointer into the context's model tensors, so
    // the context must outlive the state. Never read directly — kept here to
    // own the weights for the state's lifetime.
    #[allow(dead_code)]
    ctx: WhisperContext,
    state: WhisperState,
}

pub struct AsrEngine {
    model: Arc<Mutex<Option<LoadedModel>>>,
    /// Last time the model was used (loaded or transcribed). Fed to the
    /// idle-unload watcher so the ~700 MB state is freed after `UNLOAD_AFTER`
    /// of inactivity. `Arc`-wrapped so the watcher thread can outlive `&self`.
    last_used: Arc<Mutex<Option<Instant>>>,
    auto_unload_started: AtomicBool,
}

impl Default for AsrEngine {
    fn default() -> Self {
        Self {
            model: Arc::new(Mutex::new(None)),
            last_used: Arc::new(Mutex::new(None)),
            auto_unload_started: AtomicBool::new(false),
        }
    }
}

impl AsrEngine {
    /// Load `model_id` from `model_path` if it isn't the active model yet.
    /// Creates the `WhisperState` once alongside the context so it can be
    /// reused for every subsequent chunk.
    pub fn ensure_loaded(&self, model_id: &str, model_path: &str) -> Result<(), String> {
        // Start the idle-unload watcher once (mirrors the llama-server
        // sidecar). Started before the id-match short-circuit so it's armed
        // on the very first load.
        if !self.auto_unload_started.swap(true, Ordering::Relaxed) {
            self.spawn_auto_unload();
        }

        let mut guard = self.model.lock().unwrap();
        if let Some(loaded) = guard.as_ref() {
            if loaded.id == model_id {
                // Still used recently enough: refresh the idle timer so a
                // transcribe of the same model resets the unload countdown.
                *self.last_used.lock().unwrap() = Some(Instant::now());
                return Ok(());
            }
        }
        *guard = None; // free the old model (context + state) before loading the new one
        let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
            .map_err(|e| format!("failed to load whisper model {model_id}: {e}"))?;
        let state = ctx
            .create_state()
            .map_err(|e| format!("whisper state failed: {e}"))?;
        *guard = Some(LoadedModel {
            id: model_id.to_string(),
            ctx,
            state,
        });
        *self.last_used.lock().unwrap() = Some(Instant::now());
        Ok(())
    }

    /// Drop the loaded model (context + state) and clear the idle timer, if
    /// any. Called from the shutdown path so app exit frees GPU memory
    /// promptly instead of waiting for the idle watcher. Best-effort, infallible.
    pub fn unload(&self) {
        *self.model.lock().unwrap() = None; // drops LoadedModel -> ggml_metal_free
        *self.last_used.lock().unwrap() = None;
    }

    /// Background thread that polls the idle timer and frees the model after
    /// `UNLOAD_AFTER` of inactivity. Lives for the engine's lifetime (one
    /// thread per process, gated by `auto_unload_started`). Mirrors
    /// `summary/sidecar.rs::spawn_auto_unload`.
    fn spawn_auto_unload(&self) {
        let model = Arc::clone(&self.model);
        let last_used = Arc::clone(&self.last_used);
        std::thread::Builder::new()
            .name("asr-idle-unload".into())
            .spawn(move || loop {
                std::thread::sleep(UNLOAD_POLL);
                let should_unload = {
                    let g = last_used.lock().unwrap();
                    matches!(*g, Some(t) if t.elapsed() >= UNLOAD_AFTER)
                };
                if should_unload {
                    // Drops the LoadedModel (ctx + state), releasing the KV
                    // cache, compute buffers, and Metal backend (~700 MB).
                    *model.lock().unwrap() = None;
                    *last_used.lock().unwrap() = None;
                }
            })
            .expect("failed to spawn asr idle-unload watcher");
    }

    /// Transcribe a 16 kHz mono chunk. Returned timestamps are relative to
    /// the chunk start; the caller adds the absolute offset.
    ///
    /// Runs on the caller's thread and holds the model lock for the
    /// duration (serializes GPU work, which is what we want). Reuses the
    /// long-lived `WhisperState` instead of allocating a fresh one per chunk.
    pub fn transcribe_chunk(
        &self,
        pcm: &[f32],
        initial_prompt: Option<&str>,
    ) -> Result<Vec<(u64, u64, String)>, String> {
        let mut guard = self.model.lock().unwrap();
        let model = guard.as_mut().ok_or("no whisper model loaded")?;

        // Mark activity so the idle-unload watcher doesn't fire mid-session.
        // During a live recording this stamps at least every ~12 s (per flush),
        // well under the 5-min window, so the model stays resident for the
        // whole recording and is freed only after it stops.
        *self.last_used.lock().unwrap() = Some(Instant::now());

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4) as i32;
        params.set_n_threads(threads);
        params.set_language(Some("auto"));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        // We manage cross-chunk context ourselves via the initial prompt.
        params.set_no_context(true);
        if let Some(prompt) = initial_prompt {
            params.set_initial_prompt(prompt);
        }

        model
            .state
            .full(params, pcm)
            .map_err(|e| format!("transcription failed: {e}"))?;

        let mut out = Vec::new();
        for seg in model.state.as_iter() {
            let text = match seg.to_str_lossy() {
                Ok(t) => t.trim().to_string(),
                Err(_) => continue,
            };
            if text.is_empty() || is_non_speech(&text) {
                continue;
            }
            // Whisper hallucinates fillers on near-silence; drop segments it
            // is fairly sure contain no speech.
            if seg.no_speech_probability() > 0.75 {
                continue;
            }
            let start_ms = (seg.start_timestamp().max(0) as u64) * 10;
            let end_ms = (seg.end_timestamp().max(0) as u64) * 10;
            out.push((start_ms, end_ms, text));
        }
        Ok(out)
    }
}

/// Filter pure sound-effect/markup segments like "[BLANK_AUDIO]", "(music)".
fn is_non_speech(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('(') && t.ends_with(')'))
        || (t.starts_with('*') && t.ends_with('*'))
        || t.chars().all(|c| !c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::is_non_speech;

    #[test]
    fn filters_markup() {
        assert!(is_non_speech("[BLANK_AUDIO]"));
        assert!(is_non_speech("(upbeat music)"));
        assert!(is_non_speech("♪ ♪"));
        assert!(!is_non_speech("Hello there."));
    }
}
