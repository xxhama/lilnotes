//! Streaming mono resampler: arbitrary input rate -> 16 kHz.
//!
//! Band-limited interpolation with a Kaiser-windowed sinc, evaluated through
//! a precomputed polyphase table (128 phases x 32 taps, linear interpolation
//! between phases). For speech headed to a 16 kHz ASR model this is
//! transparent, dependency-free, and allocation-free in the steady state.
//!
//! Design notes:
//! - The filter cutoff scales with the resampling ratio so it acts as the
//!   anti-alias filter when downsampling (the common case: 48 kHz -> 16 kHz).
//! - State (history tail + fractional read position) is kept across calls,
//!   so feeding a stream chunk-by-chunk is seamless.

pub const TARGET_RATE: u32 = 16_000;

const TAPS: usize = 32; // filter taps per output sample (even)
const PHASES: usize = 128; // table resolution; linear interp between rows
const HALF: usize = TAPS / 2;

pub struct StreamingResampler {
    /// input samples per output sample (e.g. 3.0 for 48k -> 16k)
    step: f64,
    /// fractional read position within `buf`, in input samples
    pos: f64,
    /// carried input history + pending samples
    buf: Vec<f32>,
    /// polyphase filter table, PHASES+1 rows (extra row eases interpolation)
    table: Vec<[f32; TAPS]>,
    /// reusable output buffer
    out: Vec<f32>,
}

impl StreamingResampler {
    pub fn new(in_rate: u32) -> Self {
        let step = in_rate as f64 / TARGET_RATE as f64;
        // Cutoff (as a fraction of the *input* Nyquist): when downsampling,
        // limit to the output Nyquist with a little margin for the
        // transition band; when upsampling, just below input Nyquist.
        let cutoff = if step > 1.0 { 0.92 / step } else { 0.92 };
        let table = build_table(cutoff);
        Self {
            step,
            pos: 0.0,
            buf: Vec::with_capacity(8192),
            table,
            out: Vec::with_capacity(4096),
        }
    }

    /// Feed a chunk of mono input samples; returns the resampled output
    /// produced so far. The returned slice is only valid until `process`
    /// is called again.
    pub fn process(&mut self, input: &[f32]) -> &[f32] {
        self.buf.extend_from_slice(input);
        self.out.clear();

        // We can emit an output sample while the filter window
        // [pos-HALF+1, pos+HALF] lies inside the buffered input.
        loop {
            let center = self.pos;
            let last_needed = center.floor() as isize + HALF as isize;
            if last_needed >= self.buf.len() as isize {
                break;
            }
            self.out.push(self.interpolate(center));
            self.pos += self.step;
        }

        // Drop input we'll never need again, keeping HALF-1 history samples
        // before the current read position.
        let keep_from = (self.pos.floor() as isize - HALF as isize + 1).max(0) as usize;
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }

        &self.out
    }

    fn interpolate(&self, center: f64) -> f32 {
        let idx = center.floor() as isize;
        let frac = center - idx as f64; // in [0, 1)
        let phase_f = frac * PHASES as f64;
        let phase = phase_f.floor() as usize; // in [0, PHASES]
        let phase_frac = (phase_f - phase as f64) as f32;

        let row_a = &self.table[phase.min(PHASES)];
        let row_b = &self.table[(phase + 1).min(PHASES)];

        let mut acc = 0.0f32;
        for t in 0..TAPS {
            // tap t corresponds to input sample idx - HALF + 1 + t
            let s_idx = idx - HALF as isize + 1 + t as isize;
            let s = if s_idx < 0 {
                0.0
            } else {
                *self.buf.get(s_idx as usize).unwrap_or(&0.0)
            };
            let coeff = row_a[t] + (row_b[t] - row_a[t]) * phase_frac;
            acc += s * coeff;
        }
        acc
    }
}

/// Kaiser-windowed sinc, evaluated for PHASES+1 fractional offsets.
/// Row `p` holds the filter for fractional position p/PHASES; tap `t`
/// weighs input sample at relative index (t - HALF + 1) - frac.
fn build_table(cutoff: f64) -> Vec<[f32; TAPS]> {
    let beta = 8.0; // Kaiser beta: ~80 dB stopband
    let denom = bessel_i0(beta);
    let mut table = Vec::with_capacity(PHASES + 1);
    for p in 0..=PHASES {
        let frac = p as f64 / PHASES as f64;
        let mut row = [0.0f32; TAPS];
        let mut sum = 0.0f64;
        for (t, slot) in row.iter_mut().enumerate() {
            let x = (t as isize - HALF as isize + 1) as f64 - frac; // distance from center
            let sinc = if x.abs() < 1e-9 {
                cutoff
            } else {
                (std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            // Kaiser window over the span [-HALF, HALF]
            let w_arg = 1.0 - (x / HALF as f64).powi(2);
            let window = if w_arg <= 0.0 {
                0.0
            } else {
                bessel_i0(beta * w_arg.sqrt()) / denom
            };
            let c = sinc * window;
            *slot = c as f32;
            sum += c;
        }
        // Normalize each phase to unity DC gain to avoid amplitude ripple.
        if sum.abs() > 1e-12 {
            for c in row.iter_mut() {
                *c = (*c as f64 / sum) as f32;
            }
        }
        table.push(row);
    }
    table
}

/// Zeroth-order modified Bessel function of the first kind (series expansion).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half_x = x / 2.0;
    for k in 1..32 {
        term *= (half_x / k as f64).powi(2);
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, seconds: f64) -> Vec<f32> {
        let n = (rate as f64 * seconds) as usize;
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    #[test]
    fn preserves_tone_amplitude_48k() {
        let input = sine(48_000, 440.0, 1.0);
        let mut rs = StreamingResampler::new(48_000);
        let mut out = Vec::new();
        for chunk in input.chunks(480) {
            out.extend_from_slice(rs.process(chunk));
        }
        // ~16000 output samples, RMS of a unit sine is ~0.707
        assert!((out.len() as i64 - 16_000).unsigned_abs() < 100, "len={}", out.len());
        let r = rms(&out[800..out.len() - 800]);
        assert!((r - 0.707).abs() < 0.02, "rms={r}");
    }

    #[test]
    fn preserves_tone_amplitude_44k1() {
        let input = sine(44_100, 1000.0, 1.0);
        let mut rs = StreamingResampler::new(44_100);
        let mut out = Vec::new();
        for chunk in input.chunks(441) {
            out.extend_from_slice(rs.process(chunk));
        }
        assert!((out.len() as i64 - 16_000).unsigned_abs() < 100, "len={}", out.len());
        let r = rms(&out[800..out.len() - 800]);
        assert!((r - 0.707).abs() < 0.02, "rms={r}");
    }

    #[test]
    fn attenuates_above_output_nyquist() {
        // 12 kHz tone at 48 kHz input must be strongly attenuated at 16 kHz out.
        let input = sine(48_000, 12_000.0, 1.0);
        let mut rs = StreamingResampler::new(48_000);
        let mut out = Vec::new();
        for chunk in input.chunks(480) {
            out.extend_from_slice(rs.process(chunk));
        }
        let r = rms(&out[800..out.len() - 800]);
        assert!(r < 0.02, "aliased energy rms={r}");
    }

    #[test]
    fn chunk_size_independent() {
        let input = sine(48_000, 440.0, 0.5);
        let run = |sizes: &[usize]| {
            let mut rs = StreamingResampler::new(48_000);
            let mut out = Vec::new();
            let mut i = 0;
            let mut s = 0;
            while i < input.len() {
                let n = sizes[s % sizes.len()].min(input.len() - i);
                out.extend_from_slice(rs.process(&input[i..i + n]));
                i += n;
                s += 1;
            }
            out
        };
        let a = run(&[480]);
        let b = run(&[7, 1024, 33, 256]);
        let n = a.len().min(b.len());
        for i in 0..n {
            assert!((a[i] - b[i]).abs() < 1e-6, "diverged at {i}");
        }
    }
}
