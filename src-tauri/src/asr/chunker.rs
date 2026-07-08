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

const MAX_CHUNK_S: usize = 12;
const MIN_CHUNK_S: usize = 4;
const SILENCE_TAIL_MS: usize = 600;
const SILENCE_RMS: f32 = 0.006;
/// Max characters of trailing transcript used as the next chunk's prompt.
const PROMPT_TAIL_CHARS: usize = 200;

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
    fn flush(&mut self, engine: &AsrEngine) -> Result<Vec<Segment>, String> {
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
}

impl SessionChunker {
    pub fn new(app: AppHandle, session_id: String) -> Self {
        Self {
            app,
            session_id,
            mic: ChannelBuffer::new(Source::Mic),
            system: ChannelBuffer::new(Source::System),
            collected: Vec::new(),
        }
    }

    pub fn feed(&mut self, chunk: &LiveChunk, engine: &AsrEngine) -> Result<(), String> {
        let ch = match chunk.source {
            Source::Mic => &mut self.mic,
            Source::System => &mut self.system,
        };
        ch.buf.extend_from_slice(&chunk.samples);
        if ch.should_flush() {
            let segments = ch.flush(engine)?;
            self.emit(segments);
        }
        Ok(())
    }

    /// Flush both channels and return everything transcribed this session,
    /// ordered by start time.
    pub fn finish(mut self, engine: &AsrEngine) -> Result<Vec<Segment>, String> {
        let mic = self.mic.flush(engine)?;
        self.emit(mic);
        let system = self.system.flush(engine)?;
        self.emit(system);
        self.collected.sort_by_key(|s| s.start_ms);
        Ok(self.collected)
    }

    fn emit(&mut self, segments: Vec<Segment>) {
        for segment in segments {
            let _ = self.app.emit(
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
) -> std::thread::JoinHandle<Result<Vec<Segment>, String>> {
    std::thread::Builder::new()
        .name("asr-live".into())
        .spawn(move || {
            let mut chunker = SessionChunker::new(app.clone(), session_id.clone());
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
            let _ = app.emit("asr:done", serde_json::json!({ "sessionId": session_id }));
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
    let mut chunker = SessionChunker::new(app.clone(), session_id.clone());
    for (path, source) in [(mic_wav, Source::Mic), (system_wav, Source::System)] {
        let mut reader =
            hound::WavReader::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("cannot read {path}: {e}"))?;
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
    let _ = app.emit("asr:done", serde_json::json!({ "sessionId": session_id }));
    Ok(segments)
}
