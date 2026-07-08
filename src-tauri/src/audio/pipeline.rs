//! Per-channel processing pipeline: mono input at native rate ->
//! resample to 16 kHz -> soft limiter -> WAV on disk (+ level meters,
//! + optional live feed for the ASR stage in milestone 3).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crossbeam_channel::Sender;

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
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    path: PathBuf,
    meters: Arc<ChannelMeters>,
    source: Source,
    live_tx: Option<Sender<LiveChunk>>,
    frames_written: u64,
}

impl ChannelPipeline {
    pub fn new(
        path: &Path,
        in_rate: u32,
        source: Source,
        meters: Arc<ChannelMeters>,
        live_tx: Option<Sender<LiveChunk>>,
    ) -> Result<Self, String> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, spec)
            .map_err(|e| format!("failed to create {}: {e}", path.display()))?;
        Ok(Self {
            resampler: StreamingResampler::new(in_rate),
            writer,
            path: path.to_path_buf(),
            meters,
            source,
            live_tx,
            frames_written: 0,
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

        // Gentle soft limiter: keeps occasional hot samples from hard-clipping
        // in the 16-bit file while leaving normal levels untouched. The two
        // channels are limited independently and stay in separate files.
        let limited: Vec<f32> = out.iter().map(|&s| soft_limit(s)).collect();

        for &s in &limited {
            let v = (s * i16::MAX as f32) as i16;
            self.writer
                .write_sample(v)
                .map_err(|e| format!("wav write failed: {e}"))?;
        }
        self.frames_written += limited.len() as u64;

        if let Some(tx) = &self.live_tx {
            // Best-effort: drop chunks if no consumer is keeping up (M2 has
            // no consumer; M3's ASR stage attaches here).
            let _ = tx.try_send(LiveChunk {
                source: self.source,
                samples: Arc::new(limited),
            });
        }
        Ok(())
    }

    /// Duration of audio written so far, in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.frames_written * 1000 / TARGET_RATE as u64
    }

    pub fn finalize(self) -> Result<PathBuf, String> {
        self.writer
            .finalize()
            .map_err(|e| format!("wav finalize failed: {e}"))?;
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
            assert!(soft_limit(x).abs() < 1.0);
        }
    }

    #[test]
    fn downmix_averages() {
        let stereo = [1.0f32, 0.0, 0.5, 0.5, -1.0, 1.0];
        assert_eq!(downmix_interleaved(&stereo, 2), vec![0.5, 0.5, 0.0]);
    }
}
