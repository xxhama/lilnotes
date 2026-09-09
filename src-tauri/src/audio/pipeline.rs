//! Per-channel processing pipeline: mono input at native rate ->
//! resample to 16 kHz -> soft limiter -> FLAC on disk (+ level meters,
//! + optional live feed for the ASR stage in milestone 3).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crossbeam_channel::Sender;

use super::aec::{AecProcessor, AecRenderFeeder};
use super::codec::MonoWriter;
use super::resampler::{StreamingResampler, TARGET_RATE};

/// Which capture source a chunk came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Mic,
    System,
}

/// 16 kHz mono chunk fanned out to live consumers (ASR in M3).
#[derive(Clone)]
pub struct LiveChunk {
    pub source: Source,
    pub samples: Arc<Vec<f32>>,
}

/// Lock-free level meters shared with the event-emitter thread.
/// Values are f32 bit patterns in AtomicU32.
#[derive(Default)]
pub struct ChannelMeters {
    rms: AtomicU32,
    peak: AtomicU32,
}

impl ChannelMeters {
    pub fn update(&self, rms: f32, peak: f32) {
        self.rms.store(rms.to_bits(), Ordering::Relaxed);
        // Peak is monotonic until read+reset by the emitter.
        let prev = f32::from_bits(self.peak.load(Ordering::Relaxed));
        if peak > prev {
            self.peak.store(peak.to_bits(), Ordering::Relaxed);
        }
    }

    /// Read current rms and take (reset) the accumulated peak.
    pub fn read_and_reset_peak(&self) -> (f32, f32) {
        let rms = f32::from_bits(self.rms.load(Ordering::Relaxed));
        let peak = f32::from_bits(self.peak.swap(0f32.to_bits(), Ordering::Relaxed));
        (rms, peak)
    }
}

pub struct ChannelPipeline {
    resampler: StreamingResampler,
    writer: MonoWriter,
    path: PathBuf,
    meters: Arc<ChannelMeters>,
    source: Source,
    live_tx: Option<Sender<LiveChunk>>,
    frames_written: u64,
    /// Capture-side AEC (mic path only): in-place echo cancellation + NS.
    aec_capture: Option<AecProcessor>,
    /// Render-side AEC feeder (system path only): feeds the reference signal
    /// to the shared APM. The mic and system pipelines share one inner
    /// `Processor` via Arc.
    aec_render: Option<AecRenderFeeder>,
    /// Set if the APM returned an error mid-recording. Once true, subsequent
    /// frames bypass the APM and pass through (soft-limited only) so the
    /// recording survives an unexpected APM failure.
    aec_failed: bool,
}

impl ChannelPipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: &Path,
        in_rate: u32,
        source: Source,
        meters: Arc<ChannelMeters>,
        live_tx: Option<Sender<LiveChunk>>,
        aec_capture: Option<AecProcessor>,
        aec_render: Option<AecRenderFeeder>,
    ) -> Result<Self, String> {
        let writer = MonoWriter::create(path)?;
        Ok(Self {
            resampler: StreamingResampler::new(in_rate),
            writer,
            path: path.to_path_buf(),
            meters,
            source,
            live_tx,
            frames_written: 0,
            aec_capture,
            aec_render,
            aec_failed: false,
        })
    }

    /// Push a chunk of mono samples at the native input rate.
    pub fn push(&mut self, mono: &[f32]) -> Result<(), String> {
        if mono.is_empty() {
            return Ok(());
        }

        // Meter on the raw input so the UI reacts even before resampling.
        let mut sum_sq = 0.0f32;
        let mut peak = 0.0f32;
        for &s in mono {
            sum_sq += s * s;
            let a = s.abs();
            if a > peak {
                peak = a;
            }
        }
        self.meters
            .update((sum_sq / mono.len() as f32).sqrt(), peak);

        let out = self.resampler.process(mono);
        if out.is_empty() {
            return Ok(());
        }

        // AEC integration:
        // - System path (render/reference): feed the raw 16 kHz samples to the
        //   shared APM's render path, then soft-limit + write as usual. The
        //   system recording/ASR are unchanged — AEC only affects the mic.
        // - Mic path (capture/forward): run the 16 kHz samples through the
        //   APM capture path (echo cancellation + NS + HPF), then soft-limit +
        //   write the cleaned output. The output length may differ from the
        //   input because APM buffers partial 160-sample frames internally.
        //
        // If the APM errors mid-recording (unexpected — it only fails on bad
        // frame sizes, which our framing prevents), we log once and fall back
        // to passthrough so the recording survives instead of being killed.
        let passthrough: Vec<f32> = out.iter().map(|&s| soft_limit(s)).collect();
        let processed: Vec<f32> = if self.aec_failed {
            passthrough
        } else if let Some(render) = &mut self.aec_render {
            match render.feed_render(out) {
                Ok(()) => passthrough,
                Err(e) => {
                    self.aec_failed = true;
                    eprintln!("[aec] render error, falling back to passthrough: {e}");
                    passthrough
                }
            }
        } else if let Some(capture) = &mut self.aec_capture {
            match capture.process_capture(out) {
                Ok(cleaned) => cleaned.iter().map(|&s| soft_limit(s)).collect(),
                Err(e) => {
                    self.aec_failed = true;
                    eprintln!("[aec] capture error, falling back to passthrough: {e}");
                    passthrough
                }
            }
        } else {
            passthrough
        };

        let pcm: Vec<i16> = processed
            .iter()
            .map(|&s| (s * i16::MAX as f32) as i16)
            .collect();
        self.writer.write_samples(&pcm)?;
        self.frames_written += processed.len() as u64;

        if let Some(tx) = &self.live_tx {
            // Best-effort: drop chunks if no consumer is keeping up (M2 has
            // no consumer; M3's ASR stage attaches here).
            let _ = tx.try_send(LiveChunk {
                source: self.source,
                samples: Arc::new(processed),
            });
        }
        Ok(())
    }

    /// Duration of audio written so far, in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.frames_written * 1000 / TARGET_RATE as u64
    }

    pub fn finalize(mut self) -> Result<PathBuf, String> {
        // Flush the AEC so the buffered partial frame (< 160 samples) is
        // zero-padded and processed — otherwise the tail of the mic would be
        // lost. The render side is flushed too (no output, just drains).
        // Flush errors are logged, not fatal — losing the tail (< 10 ms) is
        // better than failing to finalize the recording.
        if let Some(capture) = &mut self.aec_capture {
            if !self.aec_failed {
                match capture.flush() {
                    Ok(tail) => {
                        let pcm: Vec<i16> = tail
                            .iter()
                            .map(|&s| (soft_limit(s) * i16::MAX as f32) as i16)
                            .collect();
                        self.writer.write_samples(&pcm)?;
                        self.frames_written += tail.len() as u64;
                    }
                    Err(e) => eprintln!("[aec] capture flush error (tail lost): {e}"),
                }
            }
            // Log AEC stats so the user can verify it worked on each recording.
            let stats = capture.stats();
            eprintln!(
                "[aec] mic path finalized: ERLE={:.1} dB, delay={:?} ms, residual={:.3}",
                stats.echo_return_loss_enhancement.unwrap_or(0.0),
                stats.delay_ms,
                stats.residual_echo_likelihood.unwrap_or(0.0),
            );
        }
        if let Some(render) = &mut self.aec_render {
            if !self.aec_failed {
                if let Err(e) = render.flush() {
                    eprintln!("[aec] render flush error: {e}");
                }
            }
        }
        self.writer.finalize()?;
        Ok(self.path)
    }
}

/// Soft-knee limiter: transparent below the threshold, tanh-shaped above it,
/// asymptotically approaching 1.0.
fn soft_limit(x: f32) -> f32 {
    const T: f32 = 0.89;
    let a = x.abs();
    if a <= T {
        x
    } else {
        let y = T + (1.0 - T) * ((a - T) / (1.0 - T)).tanh();
        y.copysign(x)
    }
}

/// Downmix an interleaved buffer to mono by averaging channels.
pub fn downmix_interleaved(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    let frames = data.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut acc = 0.0f32;
        for c in 0..channels {
            acc += data[f * channels + c];
        }
        mono.push(acc / channels as f32);
    }
    mono
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_limit_is_transparent_below_threshold() {
        for &x in &[0.0f32, 0.3, -0.5, 0.88, -0.88] {
            assert_eq!(soft_limit(x), x);
        }
    }

    #[test]
    fn soft_limit_never_exceeds_unity() {
        for &x in &[0.9f32, 1.0, 2.0, 10.0, -3.0] {
            assert!(soft_limit(x).abs() <= 1.0);
        }
    }

    #[test]
    fn downmix_averages() {
        let stereo = [1.0f32, 0.0, 0.5, 0.5, -1.0, 1.0];
        assert_eq!(downmix_interleaved(&stereo, 2), vec![0.5, 0.5, 0.0]);
    }
}
