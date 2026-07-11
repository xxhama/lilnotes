//! Acoustic Echo Cancellation (AEC) via WebRTC AudioProcessing.
//!
//! On a desktop with loud speakers, the mic picks up the system audio (remote
//! speakers), so remote voices leak into the "Me"/mic channel. Apple's
//! voice-processing AEC can't help here: it cancels echo using *lilnotes' own
//! playback* as the reference, and lilnotes never plays the system audio — it
//! taps *other apps'* audio. The reference is silent, so the echo goes
//! uncancelled.
//!
//! The fix: feed the captured system-audio stream (the process tap = what the
//! speakers are actually playing) as the **render/reference** signal to
//! WebRTC's AudioProcessing, and process the mic as the **capture/forward**
//! signal. AEC3 subtracts the echo of the render from the capture, leaving the
//! near-end (user's) voice. Both signals run at 16 kHz mono, framed at 10 ms
//! (160 samples) — the rate APM expects.
//!
//! The `Processor` is `Send + Sync` and shared via `Arc`, so the render path
//! (system capture thread) and capture path (mic capture thread) both reach
//! the same underlying APM without cross-thread synchronization: APM has an
//! internal delay estimator + render jitter buffer, so frames can be fed as
//! they arrive and APM aligns them.

use std::sync::Arc;

use webrtc_audio_processing::config::{
    AdaptiveDigital, EchoCanceller, FixedDigital, GainController, GainController2, HighPassFilter,
    NoiseSuppression,
};
use webrtc_audio_processing::{Config, Processor, Stats};

/// Samples per 10 ms frame at 16 kHz.
pub const FRAME_SAMPLES: usize = 160;

/// Buffers variable-size 16 kHz sample chunks into fixed 160-sample frames.
/// The resampler emits variable-length output, but APM consumes fixed frames.
pub struct FrameAssembler {
    buf: Vec<f32>,
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(FRAME_SAMPLES * 2),
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);
    }

    /// Drains one full 160-sample frame if enough samples are buffered.
    pub fn take_frame(&mut self) -> Option<Vec<f32>> {
        if self.buf.len() >= FRAME_SAMPLES {
            let frame: Vec<f32> = self.buf.drain(..FRAME_SAMPLES).collect();
            Some(frame)
        } else {
            None
        }
    }

    /// Zero-pads any remaining samples (< 160) into a final frame so the tail
    /// of the stream isn't lost. Returns `None` if nothing is buffered.
    pub fn flush(&mut self) -> Option<Vec<f32>> {
        if self.buf.is_empty() {
            return None;
        }
        let mut frame = vec![0.0f32; FRAME_SAMPLES];
        frame[..self.buf.len()].copy_from_slice(&self.buf);
        self.buf.clear();
        Some(frame)
    }
}

impl Default for FrameAssembler {
    fn default() -> Self {
        Self::new()
    }
}

/// Capture-side AEC: processes mic frames (in-place echo cancellation + NS +
/// high-pass). Shares its `Processor` with the matching `AecRenderFeeder`.
pub struct AecProcessor {
    processor: Arc<Processor>,
    capture_asm: FrameAssembler,
}

/// Render-side feeder: analyzes system-audio frames (reference signal,
/// non-mutating). Shares the same inner `Processor` as the capture side.
pub struct AecRenderFeeder {
    processor: Arc<Processor>,
    render_asm: FrameAssembler,
}

/// Builds a capture/render pair sharing one APM, configured for AEC3 Full
/// (auto delay estimation) + high-pass filter + noise suppression at
/// `sample_rate_hz` (16000). AGC2 (adaptive-digital) is enabled on the capture
/// path so quieter speech is boosted to a consistent level — the gain is
/// applied after AEC and NS, with a limiter preventing clipping. A noise floor
/// (`max_output_noise_level_dbfs`) stops silence from being amplified.
pub fn new_aec_pair(sample_rate_hz: u32) -> Result<(AecProcessor, AecRenderFeeder), String> {
    let processor =
        Arc::new(Processor::new(sample_rate_hz).map_err(|e| format!("APM init failed: {e}"))?);
    let config = Config {
        echo_canceller: Some(EchoCanceller::Full {
            stream_delay_ms: None,
        }),
        high_pass_filter: Some(HighPassFilter::default()),
        noise_suppression: Some(NoiseSuppression::default()),
        gain_controller: Some(GainController::GainController2(GainController2 {
            // We capture at a fixed digital level (no analog gain slider to
            // drive), so the input-volume controller is off; the adaptive
            // digital controller does the leveling instead.
            input_volume_controller_enabled: false,
            adaptive_digital: Some(AdaptiveDigital {
                // Defaults from WebRTC, slightly tamed: cap the max gain so
                // very quiet/noise-only frames don't get pumped up excessively,
                // and slow the ramp a touch to avoid pumping on pause boundaries.
                headroom_db: 5.0,
                max_gain_db: 30.0,
                initial_gain_db: 8.0,
                max_gain_change_db_per_second: 6.0,
                max_output_noise_level_dbfs: -50.0,
            }),
            fixed_digital: FixedDigital::default(),
        })),
        ..Default::default()
    };
    processor.set_config(config);

    Ok((
        AecProcessor {
            processor: Arc::clone(&processor),
            capture_asm: FrameAssembler::new(),
        },
        AecRenderFeeder {
            processor,
            render_asm: FrameAssembler::new(),
        },
    ))
}

impl AecProcessor {
    /// Feeds mic samples through the APM capture path and returns the cleaned
    /// samples. Variable-length input is buffered into 160-sample frames; any
    /// partial frame stays buffered for the next call. Call `flush()` at
    /// end-of-stream to recover the (zero-padded) tail.
    pub fn process_capture(&mut self, samples: &[f32]) -> Result<Vec<f32>, String> {
        self.capture_asm.push(samples);
        let mut out = Vec::with_capacity(samples.len() + FRAME_SAMPLES);
        while let Some(mut frame) = self.capture_asm.take_frame() {
            // APM expects non-interleaved channels: one Vec<f32> per channel.
            // Mono = a single channel.
            let channels = vec![frame.as_mut_slice()];
            self.processor
                .process_capture_frame(channels)
                .map_err(|e| format!("APM capture failed: {e}"))?;
            out.extend_from_slice(&frame);
        }
        Ok(out)
    }

    /// Processes any buffered tail (zero-padded) so no mic samples are lost.
    pub fn flush(&mut self) -> Result<Vec<f32>, String> {
        let mut out = Vec::new();
        if let Some(mut frame) = self.capture_asm.flush() {
            let channels = vec![frame.as_mut_slice()];
            self.processor
                .process_capture_frame(channels)
                .map_err(|e| format!("APM capture flush failed: {e}"))?;
            out.extend_from_slice(&frame);
        }
        Ok(out)
    }

    pub fn stats(&self) -> Stats {
        self.processor.get_stats()
    }
}

impl AecRenderFeeder {
    /// Feeds system-audio samples to the APM render/reference path. Best-effort
    /// and non-blocking: `analyze_render_frame` is read-only on the frame.
    /// Partial frames are buffered; call `flush()` at end-of-stream.
    pub fn feed_render(&mut self, samples: &[f32]) -> Result<(), String> {
        self.render_asm.push(samples);
        while let Some(frame) = self.render_asm.take_frame() {
            let channels = vec![frame.as_slice()];
            self.processor
                .analyze_render_frame(channels)
                .map_err(|e| format!("APM render failed: {e}"))?;
        }
        Ok(())
    }

    /// Flushes any buffered render tail (zero-padded).
    pub fn flush(&mut self) -> Result<(), String> {
        if let Some(frame) = self.render_asm.flush() {
            let channels = vec![frame.as_slice()];
            self.processor
                .analyze_render_frame(channels)
                .map_err(|e| format!("APM render flush failed: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_assembler_drains_exact_frames() {
        let mut asm = FrameAssembler::new();
        // 160 → one frame, 0 remainder.
        asm.push(&vec![0.5; 160]);
        assert!(asm.take_frame().is_some());
        assert!(asm.take_frame().is_none());

        // 320 → two frames.
        asm.push(&vec![0.3; 320]);
        assert!(asm.take_frame().is_some());
        assert!(asm.take_frame().is_some());
        assert!(asm.take_frame().is_none());
    }

    #[test]
    fn frame_assembler_buffers_partial() {
        let mut asm = FrameAssembler::new();
        asm.push(&vec![0.1; 100]);
        assert!(asm.take_frame().is_none());
        asm.push(&vec![0.2; 60]);
        let frame = asm.take_frame().expect("100 + 60 = 160");
        assert_eq!(frame.len(), 160);
        assert!(frame[..100].iter().all(|&s| s == 0.1));
        assert!(frame[100..].iter().all(|&s| s == 0.2));
    }

    #[test]
    fn frame_assembler_flush_pads_tail() {
        let mut asm = FrameAssembler::new();
        asm.push(&vec![0.7; 50]);
        let frame = asm.flush().expect("tail padded to 160");
        assert_eq!(frame.len(), 160);
        assert!(frame[..50].iter().all(|&s| s == 0.7));
        assert!(frame[50..].iter().all(|&s| s == 0.0));
        assert!(asm.flush().is_none());
    }

    #[test]
    fn new_aec_pair_shares_processor() {
        // Just verify construction works at 16 kHz — the C++ APM must init.
        let (mut aec, mut render) = new_aec_pair(16000).expect("APM init");

        // Feed a render frame (system) then a capture frame (mic). A clean
        // (zero) render + zero capture should produce ~zero output and no
        // error — smoke test that the plumbing works end to end.
        let render_frame = vec![0.0f32; FRAME_SAMPLES];
        render.feed_render(&render_frame).expect("render");

        let capture = vec![0.0f32; FRAME_SAMPLES];
        let out = aec.process_capture(&capture).expect("capture");
        assert_eq!(out.len(), FRAME_SAMPLES);
        assert!(out.iter().all(|&s| s.abs() < 1e-6));
    }
}
