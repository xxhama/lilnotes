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

use std::sync::Mutex;

use serde::Serialize;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// A transcribed segment, timestamped on the meeting's shared timeline.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// "mic" (labeled Me) or "system" (diarized in M4).
    pub source: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

pub struct AsrEngine {
    /// (model id, loaded context)
    ctx: Mutex<Option<(String, WhisperContext)>>,
}

impl Default for AsrEngine {
    fn default() -> Self {
        Self {
            ctx: Mutex::new(None),
        }
    }
}

impl AsrEngine {
    /// Load `model_id` from `model_path` if it isn't the active context yet.
    pub fn ensure_loaded(&self, model_id: &str, model_path: &str) -> Result<(), String> {
        let mut guard = self.ctx.lock().unwrap();
        if let Some((loaded, _)) = guard.as_ref() {
            if loaded == model_id {
                return Ok(());
            }
        }
        *guard = None; // free the old model before loading the new one
        let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
            .map_err(|e| format!("failed to load whisper model {model_id}: {e}"))?;
        *guard = Some((model_id.to_string(), ctx));
        Ok(())
    }

    pub fn is_loaded(&self) -> bool {
        self.ctx.lock().unwrap().is_some()
    }

    /// Transcribe a 16 kHz mono chunk. Returned timestamps are relative to
    /// the chunk start; the caller adds the absolute offset.
    ///
    /// Runs on the caller's thread and holds the context lock for the
    /// duration (serializes GPU work, which is what we want).
    pub fn transcribe_chunk(
        &self,
        pcm: &[f32],
        initial_prompt: Option<&str>,
    ) -> Result<Vec<(u64, u64, String)>, String> {
        let guard = self.ctx.lock().unwrap();
        let (_, ctx) = guard.as_ref().ok_or("no whisper model loaded")?;

        let mut state = ctx
            .create_state()
            .map_err(|e| format!("whisper state failed: {e}"))?;

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

        state
            .full(params, pcm)
            .map_err(|e| format!("transcription failed: {e}"))?;

        let mut out = Vec::new();
        for seg in state.as_iter() {
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
