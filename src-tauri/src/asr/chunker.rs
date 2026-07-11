//! Near-live chunking: turn a stream of 16 kHz mono audio into transcribed
//! segments as the meeting happens.
//!
//! Strategy per channel:
//! - Buffer incoming audio.
//! - Flush when the buffer hits `MAX_CHUNK_S`, or when it has at least
//!   `MIN_CHUNK_S` and ends in `SILENCE_TAIL_MS` of silence (cutting at
//!   pauses avoids splitting words).
//! - A buffer that is entirely silence is discarded without touching the GPU
//!   (the system channel is often quiet for long stretches).
//! - Each flush passes the tail of the previous transcript as the initial
//!   prompt so casing/terminology stay consistent across chunk boundaries.
//!
//! The same machinery powers batch mode: `feed` the whole WAV, then `finish`.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::{AsrEngine, Segment};
use crate::audio::pipeline::{LiveChunk, Source};
use crate::audio::resampler::TARGET_RATE;
use crate::personas::{rank_personas, PersonaWithEmbeddings};
use crate::voiceprint::VoiceprintEngine;

const MAX_CHUNK_S: usize = 12;
const MIN_CHUNK_S: usize = 4;
const SILENCE_TAIL_MS: usize = 600;
const SILENCE_RMS: f32 = 0.006;
/// Max characters of trailing transcript used as the next chunk's prompt.
const PROMPT_TAIL_CHARS: usize = 200;

/// Live known-persona identification state. Created at recording start when
/// the persona gallery is non-empty; the system `ChannelBuffer` calls
/// `identify` on each flushed chunk to tag segments with a persona name.
pub struct LiveVoiceprint {
    app: AppHandle,
    engine: Arc<VoiceprintEngine>,
    gallery: Vec<PersonaWithEmbeddings>,
    threshold: f32,
}

impl LiveVoiceprint {
    pub fn new(
        app: AppHandle,
        engine: Arc<VoiceprintEngine>,
        gallery: Vec<PersonaWithEmbeddings>,
        threshold: f32,
    ) -> Self {
        Self {
            app,
            engine,
            gallery,
            threshold,
        }
    }

    /// Compute an embedding from `samples`, match against the gallery, and
    /// return the persona display name if the best cosine score clears the
    /// threshold. Returns `None` on any error or below-threshold match so
    /// the recording is never affected by voiceprint failures.
    fn identify(&self, samples: &[f32]) -> Option<String> {
        let emb = self
            .engine
            .embed_samples(&self.app, samples, TARGET_RATE)
            .ok()??;
        let scores = rank_personas(&self.gallery, &emb.vec);
        let best = scores.first()?;
        if best.score >= self.threshold {
            Some(best.display_name.clone())
        } else {
            None
        }
    }
}

/// Payload of the `asr:segment` event.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SegmentEvent {
    pub session_id: String,
    #[serde(flatten)]
    pub segment: Segment,
}

struct ChannelBuffer {
    source: Source,
    buf: Vec<f32>,
    /// Samples already consumed (flushed or discarded) — the absolute offset
    /// of `buf[0]` on the meeting timeline.
    consumed: u64,
    prompt: String,
}

impl ChannelBuffer {
    fn new(source: Source) -> Self {
        Self {
            source,
            buf: Vec::with_capacity(TARGET_RATE as usize * (MAX_CHUNK_S + 2)),
            consumed: 0,
            prompt: String::new(),
        }
    }

    fn source_str(&self) -> &'static str {
        match self.source {
            Source::Mic => "mic",
            Source::System => "system",
        }
    }

    fn len_s(&self) -> f32 {
        self.buf.len() as f32 / TARGET_RATE as f32
    }

    fn tail_is_silent(&self) -> bool {
        let tail = TARGET_RATE as usize * SILENCE_TAIL_MS / 1000;
        if self.buf.len() < tail {
            return false;
        }
        rms(&self.buf[self.buf.len() - tail..]) < SILENCE_RMS
    }

    fn all_silent(&self) -> bool {
        rms(&self.buf) < SILENCE_RMS
    }

    fn should_flush(&self) -> bool {
        let s = self.len_s();
        s >= MAX_CHUNK_S as f32 || (s >= MIN_CHUNK_S as f32 && self.tail_is_silent())
    }

    /// Transcribe and clear the buffer; returns absolute-timestamped segments.
    /// `vp` is the live voiceprint identifier (system channel only); when
    /// present, the speaker is identified from the chunk audio before it's
    /// cleared.
    fn flush(
        &mut self,
        engine: &AsrEngine,
        vp: Option<&LiveVoiceprint>,
    ) -> Result<Vec<Segment>, String> {
        if self.buf.is_empty() {
            return Ok(Vec::new());
        }
        let offset_ms = self.consumed * 1000 / TARGET_RATE as u64;
        self.consumed += self.buf.len() as u64;

        if self.all_silent() {
            self.buf.clear();
            return Ok(Vec::new());
        }

        let prompt = if self.prompt.is_empty() {
            None
        } else {
            Some(self.prompt.as_str())
        };
        let raw = engine.transcribe_chunk(&self.buf, prompt)?;

        // Identify the speaker from the chunk audio before clearing the
        // buffer. Only the system channel is identified — mic is always "Me".
        let identified = match self.source {
            Source::System => vp.and_then(|v| v.identify(&self.buf)),
            Source::Mic => None,
        };

        self.buf.clear();

        let mut segments = Vec::with_capacity(raw.len());
        for (start, end, text) in raw {
            self.prompt.push(' ');
            self.prompt.push_str(&text);
            segments.push(Segment {
                source: self.source_str().into(),
                start_ms: offset_ms + start,
                end_ms: offset_ms + end,
                text,
                speaker: match self.source {
                    Source::Mic => Some("Me".into()),
                    Source::System => identified.clone(), // persona name or None
                },
            });
        }
        // Keep only the tail of the running prompt.
        if self.prompt.len() > PROMPT_TAIL_CHARS {
            let cut = self.prompt.len() - PROMPT_TAIL_CHARS;
            // Don't split a UTF-8 codepoint.
            let cut = (cut..self.prompt.len())
                .find(|&i| self.prompt.is_char_boundary(i))
                .unwrap_or(0);
            self.prompt = self.prompt[cut..].to_string();
        }
        Ok(segments)
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Streams segments for both channels of a session; used live (fed from the
/// capture pipelines) and in batch (fed from WAV files).
pub struct SessionChunker {
    app: AppHandle,
    session_id: String,
    mic: ChannelBuffer,
    system: ChannelBuffer,
    collected: Vec<Segment>,
    live_vp: Option<Arc<LiveVoiceprint>>,
}

impl SessionChunker {
    pub fn new(app: AppHandle, session_id: String, live_vp: Option<Arc<LiveVoiceprint>>) -> Self {
        Self {
            app,
            session_id,
            mic: ChannelBuffer::new(Source::Mic),
            system: ChannelBuffer::new(Source::System),
            collected: Vec::new(),
            live_vp,
        }
    }

    pub fn feed(&mut self, chunk: &LiveChunk, engine: &AsrEngine) -> Result<(), String> {
        // Clone the Arc upfront so we don't borrow self while mutating the
        // channel buffer.
        let vp = self.live_vp.clone();
        let ch = match chunk.source {
            Source::Mic => &mut self.mic,
            Source::System => &mut self.system,
        };
        ch.buf.extend_from_slice(&chunk.samples);
        if ch.should_flush() {
            let segments = ch.flush(engine, vp.as_deref())?;
            self.emit(segments);
        }
        Ok(())
    }

    /// Flush both channels and return everything transcribed this session,
    /// ordered by start time. Remainders are flushed without live ID —
    /// they're typically too short for a reliable embedding.
    pub fn finish(mut self, engine: &AsrEngine) -> Result<Vec<Segment>, String> {
        let mic = self.mic.flush(engine, None)?;
        self.emit(mic);
        let system = self.system.flush(engine, None)?;
        self.emit(system);
        self.collected.sort_by_key(|s| s.start_ms);
        Ok(self.collected)
    }

    fn emit(&mut self, segments: Vec<Segment>) {
        for segment in segments {
            let _ = self.app.emit_to(
                "main",
                "asr:segment",
                SegmentEvent {
                    session_id: self.session_id.clone(),
                    segment: segment.clone(),
                },
            );
            self.collected.push(segment);
        }
    }
}

/// Live worker: consumes capture chunks until the senders drop (recording
/// stopped), then flushes the remainder and emits `asr:done`.
pub fn spawn_live_worker(
    app: AppHandle,
    engine: Arc<AsrEngine>,
    rx: crossbeam_channel::Receiver<LiveChunk>,
    session_id: String,
    live_vp: Option<Arc<LiveVoiceprint>>,
) -> std::thread::JoinHandle<Result<Vec<Segment>, String>> {
    std::thread::Builder::new()
        .name("asr-live".into())
        .spawn(move || {
            let mut chunker = SessionChunker::new(app.clone(), session_id.clone(), live_vp);
            let mut worker_err: Option<String> = None;
            while let Ok(chunk) = rx.recv() {
                if let Err(e) = chunker.feed(&chunk, &engine) {
                    // Keep consuming so capture never blocks; report at end.
                    eprintln!("live transcription error: {e}");
                    worker_err.get_or_insert(e);
                }
            }
            let result = match chunker.finish(&engine) {
                Ok(segments) => match worker_err {
                    None => Ok(segments),
                    Some(e) => Err(e),
                },
                Err(e) => Err(e),
            };
            let _ = app.emit_to(
                "main",
                "asr:done",
                serde_json::json!({ "sessionId": session_id }),
            );
            result
        })
        .expect("failed to spawn asr worker")
}

/// Batch: transcribe two finished WAVs through the same chunker, emitting
/// `asr:segment` events along the way.
pub fn transcribe_wavs(
    app: AppHandle,
    engine: &AsrEngine,
    session_id: String,
    mic_wav: &str,
    system_wav: &str,
) -> Result<Vec<Segment>, String> {
    let mut chunker = SessionChunker::new(app.clone(), session_id.clone(), None);
    for (path, source) in [(mic_wav, Source::Mic), (system_wav, Source::System)] {
        let mut reader =
            hound::WavReader::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("cannot read {path}: {e}"))?;
        eprintln!(
            "[asr] transcribe_wavs {}: {} samples ({:.1}s)",
            path,
            samples.len(),
            samples.len() as f32 / TARGET_RATE as f32
        );
        // Feed in ~2s slices so flushes happen at the same cadence as live.
        for slice in samples.chunks(TARGET_RATE as usize * 2) {
            chunker.feed(
                &LiveChunk {
                    source,
                    samples: Arc::new(slice.to_vec()),
                },
                engine,
            )?;
        }
        // Force a flush between channels by finishing each buffer naturally
        // via `finish` at the end (buffers are per-channel, so interleaving
        // feeds is fine).
    }
    let segments = chunker.finish(engine)?;
    let _ = app.emit_to(
        "main",
        "asr:done",
        serde_json::json!({ "sessionId": session_id }),
    );
    Ok(segments)
}
