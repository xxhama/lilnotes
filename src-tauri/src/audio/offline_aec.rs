//! Offline acoustic echo cancellation (Tier 3).
//!
//! The live WebRTC AEC3 (`aec.rs`) suppresses echo in real time but can't fully
//! track a loud-speaker path at a high echo-to-near ratio. lilnotes also
//! records `system.wav` — the *exact* signal the speakers played — so we can
//! do better after the fact: run an adaptive filter offline, where the delay
//! can be found globally and the filter can iterate to convergence.
//!
//! Pipeline:
//! 1. Estimate the global acoustic delay by normalized cross-correlation
//!    (FFT-based) of the mic against the system, searching 0–500 ms. The user's
//!    marked echo windows (mic regions confirmed to be pure echo) are the best
//!    delay-estimation target — there the mic is the system, time-shifted.
//! 2. Delay-align the reference (`ref[n] = system[n - delay]`).
//! 3. Two passes of a partitioned-block frequency-domain NLMS filter
//!    (PB-FDAF, overlap-save):
//!      - Pass 1 adapts the echo-path filter `W` across the whole track, with
//!        double-talk freeze (skip adaptation when the near-end voice
//!        dominates) and forced adaptation inside echo windows (near ≈ 0
//!        there, so the error signal is a clean echo residual).
//!      - Pass 2 applies the converged `W` (no adaptation) to produce the
//!        cleaned mic = mic − estimated_echo throughout, including the
//!        early frames that pass 1 hadn't yet converged on.
//!
//!    Two passes avoid the early-convergence echo leak a single pass would
//!    leave at the start of the recording.
//! 4. A simple per-20 ms residual-echo suppressor gain masks any leftover
//!    echo: `gain = e_power / (e_power + β·echo_power)` per frame.
//!
//! The output is written to `mic_cleaned.wav` (16 kHz mono i16), preserving the
//! original `mic.wav` so the action is revertible. The caller then re-transcribes
//! the mic from the cleaned WAV.
//!
//! Scope (v1): linear echo path + residual mask. Nonlinear/saturating-speaker
//! echo and finer PB-FDAF efficiency tweaks are later improvements, not blocking
//! — finding the delay globally and converging offline already clearly beats the
//! live AEC on the loud-speaker case.

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Sample rate everything here operates at (the pipeline's capture rate).
const RATE: u32 = 16_000;

/// PB-FDAF block hop. 512 samples = 32 ms — small enough for a smooth residual
/// mask, large enough to keep the per-block FFT cost low.
const N: usize = 512;
/// FFT length for overlap-save (2× block hop).
const FFT: usize = 2 * N;
/// Number of filter partitions. `PARTS * N` samples = 4096 = 256 ms echo tail,
/// which covers the longest realistic room/speaker echo decay.
const PARTS: usize = 8;
/// NLMS step size. The constrained (overlap-save) gradient is purer than an
/// unconstrained one — energy no longer leaks into the circular-alias half — so
/// the effective adaptation is stronger and a smaller μ stays stable. Pair with
/// the per-bin step clamp (`MAX_STEP`) so a single ill-conditioned bin can't
/// diverge the filter to NaN.
const MU: f32 = 0.02;
/// Per-bin maximum |ΔW| per block — a safety clamp on top of the NLMS
/// normalization so an ill-conditioned bin (low reference power) can't blow W
/// up. Generous enough not to slow normal convergence.
const MAX_STEP: f32 = 0.5;
/// Per-bin maximum |W| — backstop against unbounded filter growth.
const MAX_W: f32 = 8.0;
/// Number of adaptation passes over the track before the final apply pass.
/// A user-marked echo window may be short (a second or two) — too few NLMS
/// iterations for a 4096-tap filter to converge in one pass. Replaying the
/// track (W persists between passes) gives the window enough iterations to
/// converge, like having a longer window. Each pass replays from t=0.
const N_ADAPT_PASSES: usize = 4;
/// NLMS per-bin regularization, as a fraction of the mean reference bin power.
/// A relative floor (not a tiny fixed constant) keeps the per-bin step μ/px
/// bounded on low-energy bins, where a fixed 1e-6 floor would leave the step
/// effectively un-normalized and blow W up to NaN.
const DELTA: f32 = 1e-2;
/// Largest echo delay we search for (samples / ms at 16 kHz).
const MAX_DELAY_SAMPLES: usize = RATE as usize * 500 / 1000; // 8000
/// Double-talk freeze: when the mic (near + echo) is more than `DT_FACTOR`× the
/// reference energy, the near-end voice dominates → freeze adaptation so we
/// don't unlearn the echo path.
const DT_FACTOR: f32 = 3.0;
/// Residual suppressor strength. Larger = more aggressive ducking of frames
/// where the estimated echo still dominates the residual.
const RES_BETA: f32 = 2.0;
/// Residual suppressor frame (20 ms).
const RES_FRAME: usize = RATE as usize * 20 / 1000; // 320
/// Floor for the residual-suppressor gain so we never bit-crush into pure
/// silence (avoids musical-noise artifacts on borderline frames).
const RES_GAIN_FLOOR: f32 = 0.03;

/// A user-confirmed echo window (a mic region that is pure echo, near-end
/// silent) — `start_ms`/`end_ms` on the meeting timeline. Used as the preferred
/// delay-estimation target and as forced-adaptation regions in pass 1.
#[derive(Clone, Copy, Debug)]
pub struct EchoWindow {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Bulk pre-delay applied to the reference (the reference is *advanced* by this
/// many samples — `ref_sig[n] = system[n + PRE_DELAY]`). This moves the echo
/// impulse from tap `delay` to tap `delay + PRE_DELAY`, keeping it off the
/// overlap-save block boundary (tap 0) for *all* delays — including the
/// near-zero-delay loud-speaker case, where the impulse would otherwise sit at
/// tap 0 and the constrained gradient converges to a sign-flipped echo path
/// (`error = mic − (−echo) = near + 2·echo`, doubling the echo instead of
/// cancelling it). The old `align` guard only shifted the reference for delays
/// ≥ 256 ms, the opposite of the loud-speaker case; this fixed pre-delay covers
/// every delay. The 4096-tap filter has ample headroom for 256 samples, at the
/// cost of a ~16 ms uncancelled window at the very start (and end) of the
/// track — negligible on real minute-scale recordings.
const PRE_DELAY: usize = N / 2; // 256 samples ≈ 16 ms

/// Where the cleaned signal is written. The adaptation passes always learn the
/// echo path globally (using `windows` for forced adaptation); this only
/// controls the **apply** pass:
/// - `WholeTrack` — write `error = mic − echo_est` everywhere (classic
///   behavior; the existing tests use this).
/// - `Ranges` — inside the listed regions write the cleaned `error` (with the
///   residual mask + a short crossfade at each boundary); outside, write the
///   original `mic` **bit-identical**. This bounds the blast radius: a
///   mis-converged `W` can only corrupt the requested region(s), never the
///   whole recording. Empty `Ranges` → output equals the input mic (nothing to
///   clean); callers should handle that case before calling.
#[derive(Clone, Debug)]
pub enum ApplyScope {
    WholeTrack,
    Ranges(Vec<EchoWindow>),
}

/// Read a WAV as mono f32 in [-1, 1]. Multi-channel files downmix to channel 0
/// (the capture path is already mono; this just defensive). Returns the spec
/// so the caller can sanity-check the sample rate.
fn read_wav_mono_f32(path: &str) -> Result<(WavSpec, Vec<f32>), String> {
    let mut reader = WavReader::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0).unwrap_or(0.0))
            .collect(),
        SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
    };
    let mono = if ch == 1 {
        raw
    } else {
        raw.into_iter().step_by(ch).collect()
    };
    Ok((spec, mono))
}

/// Write 16 kHz mono i16 PCM WAV (the format the rest of the pipeline expects).
fn write_wav_mono_i16(path: &str, samples: &[f32]) -> Result<(), String> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec).map_err(|e| format!("create {path}: {e}"))?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        writer
            .write_sample(v)
            .map_err(|e| format!("write {path}: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("finalize {path}: {e}"))?;
    Ok(())
}

fn next_pow2(n: usize) -> usize {
    let mut p = 1;
    while p < n {
        p *= 2;
    }
    p
}

/// `complex(a * b)`.
#[inline]
fn cmul(a: Complex32, b: Complex32) -> Complex32 {
    Complex32::new(a.re * b.re - a.im * b.im, a.re * b.im + a.im * b.re)
}

/// Mean square of a slice (energy / len). Used for double-talk + mask decisions.
fn msq(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32
}

/// Estimate the global acoustic delay (in samples) of `system` echoed into
/// `mic`, via FFT cross-correlation over a representative segment. The segment
/// is the first marked echo window if any (mic is pure echo there → the
/// correlation peaks sharply at the true delay), else the first 10 s.
/// Returns 0 if the signals are too short to estimate.
fn estimate_delay(mic: &[f32], system: &[f32], windows: &[EchoWindow]) -> usize {
    // Pick the correlation segment [seg_s, seg_e) on the mic timeline.
    let (seg_s, seg_e) = if let Some(w) = windows.iter().min_by_key(|w| w.start_ms) {
        let s = (w.start_ms as usize * RATE as usize / 1000).min(mic.len());
        let e = (w.end_ms as usize * RATE as usize / 1000).min(mic.len());
        if e > s + RES_FRAME {
            (s, e)
        } else {
            (0, mic.len().min(RATE as usize * 10))
        }
    } else {
        (0, mic.len().min(RATE as usize * 10))
    };
    let seg_len = seg_e - seg_s;
    if seg_len < RES_FRAME || mic.len() < RES_FRAME || system.is_empty() {
        return 0;
    }

    // The reference segment must extend past the mic segment by the max delay
    // so that every candidate lag has a full overlap.
    let ref_s = seg_s;
    let ref_e = (seg_e + MAX_DELAY_SAMPLES).min(system.len());
    if ref_e <= ref_s + seg_len {
        return 0;
    }
    let m = next_pow2(seg_len + MAX_DELAY_SAMPLES);

    let mut mic_pad = vec![0f32; m];
    mic_pad[..seg_len].copy_from_slice(&mic[seg_s..seg_e]);
    let mut sys_pad = vec![0f32; m];
    let sys_len = ref_e - ref_s;
    sys_pad[..sys_len].copy_from_slice(&system[ref_s..ref_e]);

    let mut planner = RealFftPlanner::<f32>::new();
    let r2c: Arc<dyn RealToComplex<f32>> = planner.plan_fft_forward(m);
    let c2r: Arc<dyn ComplexToReal<f32>> = planner.plan_fft_inverse(m);
    let mut mic_spec = r2c.make_output_vec();
    let mut sys_spec = r2c.make_output_vec();
    if r2c.process(&mut mic_pad, &mut mic_spec).is_err() {
        return 0;
    }
    if r2c.process(&mut sys_pad, &mut sys_spec).is_err() {
        return 0;
    }

    // Cross-correlation: R = IFFT(conj(SYS) · MIC). R[k] ≈ Σ mic[n]·system[n−k],
    // peaking at k = echo delay.
    let mut cross = sys_spec
        .iter()
        .map(|s| Complex32::new(s.re, -s.im)) // conj(system)
        .collect::<Vec<_>>();
    for i in 0..cross.len() {
        cross[i] = cmul(cross[i], mic_spec[i]);
    }
    let mut corr = c2r.make_output_vec();
    if c2r.process(&mut cross, &mut corr).is_err() {
        return 0;
    }
    let scale = 1.0 / m as f32;

    // Search [0, MAX_DELAY] for the peak (the echo delay is small + positive).
    let end = MAX_DELAY_SAMPLES.min(corr.len());
    let mut best_k = 0usize;
    let mut best_v = f32::MIN;
    let mut sum_abs = 0f32;
    for (k, &val) in corr.iter().take(end).enumerate() {
        let v = val * scale;
        sum_abs += v.abs();
        if v > best_v {
            best_v = v;
            best_k = k;
        }
    }
    // Quality gate: the peak must stand out from the correlation floor. On real
    // loud-speaker audio a spurious peak (near-end speech that happens to
    // correlate with the system, or a reverberant autocorrelation) would give a
    // wrong delay → a misaligned reference → `W` converges to a wrong path and
    // the apply pass subtracts that wrong echo estimate from the whole track
    // (the "added echo to everything" failure). Require the peak to exceed a
    // few× the mean |correlation|; if it doesn't, the delay is unreliable →
    // fall back to 0 (the pre-delay still keeps the impulse off the boundary,
    // and the filter adapts rather than chasing a spurious lag).
    let mean_abs = sum_abs / (end as f32 + 1e-9);
    if best_v <= 0.0 || best_v < 2.5 * mean_abs {
        eprintln!(
            "[offline_aec] delay peak {best_v:.6} not distinct from floor (mean |R| {mean_abs:.6}) → unreliable, falling back to delay 0"
        );
        return 0;
    }
    best_k
}

/// Partitioned-block frequency-domain NLMS adaptive filter (overlap-save).
/// Holds the filter `W` (per-partition spectra) + the rolling reference-spectra
/// buffer, and drives both the adaptation pass and the fixed-filter apply pass.
struct Pbfdaf {
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
    /// Adaptive filter: `PARTS` partitions, each `FFT/2+1` complex bins.
    w: Vec<Vec<Complex32>>,
    /// Rolling buffer of the last `PARTS` reference spectra (newest at the
    /// back). `x_buf[len-1-p]` is the reference spectrum from `p` blocks ago.
    x_buf: Vec<Vec<Complex32>>,
}

impl Pbfdaf {
    fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c: Arc<dyn RealToComplex<f32>> = planner.plan_fft_forward(FFT);
        let c2r: Arc<dyn ComplexToReal<f32>> = planner.plan_fft_inverse(FFT);
        let bins = FFT / 2 + 1;
        Self {
            r2c,
            c2r,
            w: vec![vec![Complex32::new(0.0, 0.0); bins]; PARTS],
            x_buf: Vec::with_capacity(PARTS),
        }
    }

    /// Process one block: returns `(echo_est[N], error[N])` where `error` is
    /// `mic_new − echo_est`. Adapts `W` when `adapt && !freeze`.
    ///
    /// - `force_adapt`: this block is inside a user-marked echo window (near ≈
    ///   0) → always adapt, never freeze (the error is a clean echo residual,
    ///   the best adaptation target — and it bootstraps `W` so the double-talk
    ///   guard below has a sane echo estimate to compare against).
    /// - `guard`: apply the double-talk freeze. True when the run has echo
    ///   windows (so `W` is bootstrapped and the error-vs-echo-estimate ratio is
    ///   meaningful). False for the unsupervised fallback (no windows) → adapt
    ///   everywhere and let two-pass iteration + the residual mask clean up.
    fn process_block(
        &mut self,
        mic_block: &[f32], // 2N samples (only [N..2N] are "new")
        ref_block: &[f32], // 2N samples
        adapt: bool,
        force_adapt: bool,
        guard: bool,
    ) -> (Vec<f32>, Vec<f32>) {
        debug_assert_eq!(mic_block.len(), FFT);
        debug_assert_eq!(ref_block.len(), FFT);

        // Forward FFT of the reference block; push into the rolling buffer.
        let mut ref_time = ref_block.to_vec();
        let mut x_spec = self.r2c.make_output_vec();
        self.r2c.process(&mut ref_time, &mut x_spec).ok();
        self.x_buf.push(x_spec.clone());
        if self.x_buf.len() > PARTS {
            self.x_buf.remove(0);
        }

        // Echo estimate (frequency domain) = Σ_p W_p · X_{k−p}.
        let mut y_spec = vec![Complex32::new(0.0, 0.0); self.r2c.make_output_vec().len()];
        for p in 0..self.x_buf.len() {
            let xp = &self.x_buf[self.x_buf.len() - 1 - p];
            let wp = &self.w[p];
            for i in 0..y_spec.len() {
                y_spec[i] += cmul(wp[i], xp[i]);
            }
        }
        // Inverse FFT → echo estimate in time; keep only the valid [N..2N] tail
        // (overlap-save: the first N samples are circular-convolution garbage).
        let mut y_time = self.c2r.make_output_vec();
        self.c2r.process(&mut y_spec, &mut y_time).ok();
        let scale = 1.0 / FFT as f32;
        let echo_est: Vec<f32> = y_time[N..FFT].iter().map(|v| v * scale).collect();

        // Error = near+residual − echo estimate, over the N new samples.
        let error: Vec<f32> = (0..N).map(|i| mic_block[N + i] - echo_est[i]).collect();

        // Double-talk guard: once `W` is bootstrapped (via echo windows), the
        // error ≈ near and the echo estimate ≈ echo. When the near dominates
        // (error power > DT_FACTOR · echo power), freeze adaptation so we don't
        // unlearn the echo path chasing the near voice. Before `W` converges the
        // echo estimate is ~0 so the guard would freeze everything — that's why
        // echo windows force adaptation first. With no windows (unsupervised)
        // the guard is off and we adapt everywhere.
        let e_p = msq(&error);
        let y_p = msq(&echo_est);
        let freeze = adapt && guard && !force_adapt && e_p > DT_FACTOR * y_p;
        if adapt && !freeze {
            // FFT of [0..N zeros, error] (overlap-save gradient: the error
            // lives in the new half so the cross-correlation gradient stays
            // linear, not circular).
            let mut e_pad = vec![0f32; FFT];
            e_pad[N..FFT].copy_from_slice(&error);
            let mut e_spec = self.r2c.make_output_vec();
            self.r2c.process(&mut e_pad, &mut e_spec).ok();
            let bins = e_spec.len();
            // Per-bin combined reference power across partitions (NLMS norm).
            let mut px = vec![0f32; bins];
            for xp in &self.x_buf {
                for i in 0..bins {
                    px[i] += xp[i].norm_sqr();
                }
            }
            // Regularize with a fraction of the mean bin power, not a tiny fixed
            // floor: a fixed 1e-6 floor leaves low-energy bins effectively
            // un-normalized, so the per-bin step μ/px explodes there and W blows
            // up to NaN once the constraint concentrates the energy. A relative
            // floor keeps the step bounded on every bin.
            let mean_px: f32 = px.iter().copied().sum::<f32>() / bins as f32;
            let reg = DELTA * mean_px.max(1e-12);
            for v in &mut px {
                *v += reg;
            }
            // realfft's inverse is unnormalized, so an IFFT→FFT round-trip scales
            // by FFT. Divide the IFFT output by FFT to keep the gradient (and W)
            // at their true scale through the constraint round-trips below.
            let inv_fft = 1.0 / FFT as f32;
            for p in 0..self.x_buf.len() {
                let xp = &self.x_buf[self.x_buf.len() - 1 - p];
                // Gradient spec = E · conj(X_p). Its IFFT's [0..N] half is the
                // valid linear cross-correlation (the right taps); [N..2N] is
                // circular-alias garbage that must be zeroed, or W converges to
                // spurious lags (the bug behind the inverted echo estimate).
                let mut g_spec: Vec<Complex32> = (0..bins)
                    .map(|i| cmul(e_spec[i], Complex32::new(xp[i].re, -xp[i].im)))
                    .collect();
                let mut g_time = self.c2r.make_output_vec();
                self.c2r.process(&mut g_spec, &mut g_time).ok();
                for v in g_time.iter_mut() {
                    *v *= inv_fft;
                }
                for v in &mut g_time[N..FFT] {
                    *v = 0.0;
                }
                let mut g_c = self.r2c.make_output_vec();
                self.r2c.process(&mut g_time, &mut g_c).ok();

                // NLMS step in the frequency domain (per-bin normalization),
                // with a per-bin magnitude clamp so one ill-conditioned bin
                // can't diverge the whole filter.
                let wp = &mut self.w[p];
                for i in 0..bins {
                    let mut step = (MU / px[i]) * g_c[i];
                    let sn = step.norm();
                    if sn > MAX_STEP {
                        step *= MAX_STEP / sn;
                    }
                    wp[i] += step;
                    let wn = wp[i].norm();
                    if wn > MAX_W {
                        wp[i] *= MAX_W / wn;
                    }
                }

                // Constrain W to a valid partition impulse: IFFT → zero the
                // [N..2N] half → FFT. Without this the circular garbage the
                // step leaks into W's [N..2N] accumulates over iterations and
                // corrupts the echo estimate — especially once adaptation is
                // frozen (the fixture's near-active region).
                let mut w_time = self.c2r.make_output_vec();
                self.c2r.process(wp, &mut w_time).ok();
                for v in w_time.iter_mut() {
                    *v *= inv_fft;
                }
                for v in &mut w_time[N..FFT] {
                    *v = 0.0;
                }
                self.r2c.process(&mut w_time, wp).ok();
            }
        }

        (echo_est, error)
    }
}

/// Build the per-sample "is this inside an echo window?" check as a sorted list
/// of `(start, end)` sample ranges (clamped to the mic length).
fn echo_ranges(mic_len: usize, windows: &[EchoWindow]) -> Vec<(usize, usize)> {
    let mut r: Vec<(usize, usize)> = windows
        .iter()
        .map(|w| {
            let s = (w.start_ms as usize * RATE as usize / 1000).min(mic_len);
            let e = (w.end_ms as usize * RATE as usize / 1000).min(mic_len);
            (s, e)
        })
        .filter(|(s, e)| e > s)
        .collect();
    r.sort_by_key(|&(s, _)| s);
    r
}

/// True if `pos` falls inside any echo range. `ptr` advances monotonically
/// (callers process blocks in time order).
fn in_echo(ranges: &[(usize, usize)], ptr: &mut usize, pos: usize) -> bool {
    while *ptr < ranges.len() && ranges[*ptr].1 <= pos {
        *ptr += 1;
    }
    *ptr < ranges.len() && pos >= ranges[*ptr].0 && pos < ranges[*ptr].1
}

/// Sort + merge overlapping/adjacent sample ranges into a disjoint list.
fn merge_ranges(mut r: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    r.sort_by_key(|&(s, _)| s);
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (s, e) in r {
        if e <= s {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        out.push((s, e));
    }
    out
}

/// Per-20 ms residual-echo suppressor: duck frames where the estimated echo
/// still dominates the residual, with smoothed gain (fast attack, slow release)
/// to avoid musical-noise chatter. `gain_prev` carries the smoothing state
/// across frames (and across blocks) — pass the same `&mut` through the apply
/// pass.
fn apply_residual_mask(error: &mut [f32], echo_est: &[f32], gain_prev: &mut f32) {
    let mut fi = 0;
    while fi < error.len() {
        let fe = (fi + RES_FRAME).min(error.len());
        let e_p = msq(&error[fi..fe]);
        let y_p = msq(&echo_est[fi..fe]);
        let mut g = e_p / (e_p + RES_BETA * y_p + DELTA);
        g = g.clamp(RES_GAIN_FLOOR, 1.0);
        // Smooth: attack fast (g down), release slow (g up).
        g = if g < *gain_prev {
            g
        } else {
            *gain_prev + (g - *gain_prev) * 0.3
        };
        *gain_prev = g;
        for v in &mut error[fi..fe] {
            *v *= g;
        }
        fi = fe;
    }
}

/// Crossfade length at each apply-range boundary (5 ms). Hard-switching between
/// the cleaned `error` and the original `mic` clicks (they differ by the echo
/// estimate); a short raised-cosine blend avoids the discontinuity.
const XFADE: usize = RATE as usize * 5 / 1000; // 80

/// Fade weight for `pos` within the (sorted, disjoint) apply ranges: 0 outside
/// any range, ramping 0→1 over `XFADE` at each entry and 1→0 at each exit, 1 in
/// the body. `ptr` advances monotonically (callers process samples in time
/// order). `out[pos] = w·error + (1−w)·mic[pos]`.
fn fade_weight(ranges: &[(usize, usize)], ptr: &mut usize, pos: usize) -> f32 {
    while *ptr < ranges.len() && ranges[*ptr].1 <= pos {
        *ptr += 1;
    }
    if *ptr >= ranges.len() || pos < ranges[*ptr].0 {
        return 0.0;
    }
    let (rs, re) = ranges[*ptr];
    // pos is inside [rs, re).
    let in_fade = (pos - rs).min(XFADE);
    let out_fade = (re - pos).min(XFADE);
    let w_in = in_fade as f32 / XFADE as f32;
    let w_out = out_fade as f32 / XFADE as f32;
    w_in.min(w_out).clamp(0.0, 1.0)
}

/// Fill a 2N overlap-save block so its new half `block[N..2N]` is `src[s..s+N]`
/// (the samples this block cleans) and its old half `block[0..N]` is the
/// preceding `src[s-N..s]` (zero-padded at the start of the track). This keeps
/// `out[s..s+N] = error` time-aligned with `mic[s..s+N]`; framing the block as
/// `src[s..s+2N]` instead would shift the output by one block (the new samples
/// would be `src[s+N..s+2N]` placed at `out[s..s+N]`), scrambling the cleaned
/// signal against the source.
fn fill_block(block: &mut [f32], src: &[f32], s: usize) {
    debug_assert_eq!(block.len(), FFT);
    let old_start = s.saturating_sub(N);
    let dest_off = N - s.min(N);
    let avail = src.len().saturating_sub(old_start);
    let len = avail.min(FFT - dest_off);
    if len > 0 {
        block[dest_off..dest_off + len].copy_from_slice(&src[old_start..old_start + len]);
    }
}

/// Core offline AEC on raw samples (no I/O). Returns the cleaned mic.
/// `windows` are the user-confirmed echo regions used for delay estimation and
/// forced adaptation (the learning signal). `apply` controls where the cleaned
/// signal is written (see [`ApplyScope`]); the filter still learns globally.
/// `progress` receives a 0..=1 fraction (coarse, per block).
fn clean_samples(
    mic: &[f32],
    system: &[f32],
    windows: &[EchoWindow],
    apply: &ApplyScope,
    mut progress: impl FnMut(f32),
) -> Vec<f32> {
    if mic.is_empty() {
        return Vec::new();
    }
    // Empty Ranges → nothing to clean: return the mic untouched (and skip the
    // filter work entirely). Callers usually handle this before calling, but
    // guard here too so the invariant holds at the algorithm boundary.
    if let ApplyScope::Ranges(ws) = apply {
        if ws.is_empty() {
            return mic.to_vec();
        }
    }

    let delay = estimate_delay(mic, system, windows);
    eprintln!(
        "[offline_aec] estimated delay: {delay} samples ({:.1} ms)",
        delay as f32 * 1000.0 / RATE as f32
    );

    // Reference alignment. The PB-FDAF filter has `PARTS * N` taps
    // (4096 = 256 ms) of echo-tail memory, so it can model the full acoustic
    // delay *internally*. We only pre-align (beyond the fixed `PRE_DELAY`) when
    // the delay would exceed the filter tail, centering the residual echo
    // mid-filter (shift by `delay − tail/2`). The fixed `PRE_DELAY` advance
    // (see its constant doc) keeps the echo impulse off the overlap-save block
    // boundary (tap 0) for *all* delays, fixing the sign-flipped convergence
    // path that doubled the echo on near-zero-delay loud-speaker audio.
    let tail = PARTS * N;
    let align = if delay >= tail { delay - tail / 2 } else { 0 };
    let src_start = align + PRE_DELAY;
    let mut ref_sig = vec![0f32; mic.len()];
    let copy_len = system.len().saturating_sub(src_start).min(mic.len());
    if copy_len > 0 {
        ref_sig[..copy_len].copy_from_slice(&system[src_start..src_start + copy_len]);
    }

    let ranges = echo_ranges(mic.len(), windows);
    let has_windows = !ranges.is_empty();
    let n_blocks = mic.len().div_ceil(N);
    let total_steps = (N_ADAPT_PASSES + 1) * n_blocks; // adapt passes + apply pass
    let mut step = 0usize;

    let mut filt = Pbfdaf::new();

    // Adaptation passes: replay the track `N_ADAPT_PASSES` times, adapting W.
    // `W` persists across passes; only the rolling reference buffer resets each
    // pass. A short echo window gets replayed → enough NLMS iterations to
    // converge the 4096-tap filter.
    for _pass in 0..N_ADAPT_PASSES {
        // Reset the rolling reference buffer at the start of each replay: the
        // previous pass left end-of-track spectra in it, which would corrupt
        // the echo estimate + gradient for the first PARTS blocks of the new
        // pass (spurious-lag adaptation). W persists; only the buffer resets.
        filt.x_buf.clear();
        let mut win_ptr = 0usize;
        for k in 0..n_blocks {
            let s = k * N;
            let mut mic_block = vec![0f32; FFT];
            let mut ref_block = vec![0f32; FFT];
            fill_block(&mut mic_block, mic, s);
            fill_block(&mut ref_block, &ref_sig[..], s);
            let force = in_echo(&ranges, &mut win_ptr, s + N / 2);
            let _ = filt.process_block(&mic_block, &ref_block, true, force, has_windows);
            step += 1;
            if step.is_multiple_of(64) {
                progress(step as f32 / total_steps as f32);
            }
        }
    }

    // Apply pass: run the converged W (no adaptation) → cleaned output. Reset
    // the reference buffer so the partitioned convolution starts clean.
    filt.x_buf.clear();
    let mut out: Vec<f32> = Vec::with_capacity(mic.len());
    let mut gain_prev = 1.0f32;
    let apply_ranges: Vec<(usize, usize)> = match apply {
        ApplyScope::WholeTrack => vec![(0, mic.len())],
        ApplyScope::Ranges(ws) => merge_ranges(echo_ranges(mic.len(), ws)),
    };
    let scoped = matches!(apply, ApplyScope::Ranges(_));
    let mut fade_ptr = 0usize;
    for k in 0..n_blocks {
        let s = k * N;
        let mut mic_block = vec![0f32; FFT];
        let mut ref_block = vec![0f32; FFT];
        fill_block(&mut mic_block, mic, s);
        fill_block(&mut ref_block, &ref_sig[..], s);
        // Every block runs through the filter so the partitioned-convolution
        // reference buffer stays aligned, even for fully out-of-range blocks
        // (whose output we discard). The mask runs on the full block so
        // `gain_prev` stays continuous; outside the apply region the masked
        // `error` is discarded in favor of the original `mic` below.
        let (echo_est, mut error) = filt.process_block(&mic_block, &ref_block, false, false, false);
        apply_residual_mask(&mut error, &echo_est, &mut gain_prev);

        if !scoped {
            // WholeTrack: write the cleaned error for the whole block.
            out.extend_from_slice(&error);
        } else {
            // Ranges: composite cleaned `error` (in-range) vs original `mic`
            // (out-of-range) per sample, with a crossfade at each boundary.
            for (i, &err) in error.iter().enumerate().take(N) {
                let pos = s + i;
                if pos >= mic.len() {
                    break;
                }
                let w = fade_weight(&apply_ranges, &mut fade_ptr, pos);
                if w >= 1.0 {
                    out.push(err);
                } else if w <= 0.0 {
                    out.push(mic[pos]);
                } else {
                    out.push(w * err + (1.0 - w) * mic[pos]);
                }
            }
        }
        step += 1;
        if step.is_multiple_of(64) {
            progress(step as f32 / total_steps as f32);
        }
    }
    // Truncate to exactly mic.len() (block padding may add a few extra samples).
    out.truncate(mic.len());
    progress(1.0);
    out
}

/// Run offline AEC on a `mic.wav` + `system.wav` pair and write the cleaned mic
/// to `out_path` (16 kHz mono i16). `windows` are the learning regions (marked
/// echo); `apply` controls where the cleaned signal is written (see
/// [`ApplyScope`]). `progress` receives a 0..=1 fraction. This is the entry
/// point the `clean_echo` / `clean_echo_segment` commands call (under
/// `spawn_blocking`).
pub fn clean(
    mic_path: &str,
    system_path: &str,
    windows: &[EchoWindow],
    apply: &ApplyScope,
    out_path: &str,
    progress: impl FnMut(f32),
) -> Result<(), String> {
    let (mic_spec, mic) = read_wav_mono_f32(mic_path)?;
    let (sys_spec, system) = read_wav_mono_f32(system_path)?;
    if mic_spec.sample_rate != RATE {
        return Err(format!(
            "mic.wav must be {RATE} Hz (got {}); the offline AEC expects the pipeline's 16 kHz output",
            mic_spec.sample_rate
        ));
    }
    if sys_spec.sample_rate != RATE {
        return Err(format!(
            "system.wav must be {RATE} Hz (got {})",
            sys_spec.sample_rate
        ));
    }
    let apply_desc = match apply {
        ApplyScope::WholeTrack => "whole track".to_string(),
        ApplyScope::Ranges(rs) => format!("{} region(s)", rs.len()),
    };
    eprintln!(
        "[offline_aec] mic: {} samples ({:.1}s), system: {} samples ({:.1}s), {} echo window(s), apply: {apply_desc}",
        mic.len(),
        mic.len() as f64 / RATE as f64,
        system.len(),
        system.len() as f64 / RATE as f64,
        windows.len(),
    );
    let cleaned = clean_samples(&mic, &system, windows, apply, progress);
    write_wav_mono_i16(out_path, &cleaned)?;
    eprintln!(
        "[offline_aec] wrote {} cleaned samples ({:.1}s) to {out_path}",
        cleaned.len(),
        cleaned.len() as f64 / RATE as f64,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproducible LCG noise (tests must be deterministic).
    fn lcg(seed: &mut u64) -> f32 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (((*seed >> 33) as f32) / (1u32 << 31) as f32) * 2.0 - 1.0
    }

    /// Build a synthetic echo fixture: a speech-like `system` signal, a distinct
    /// `near` voice (silent in the first second so it's a clean echo window),
    /// and `mic = echo_gain · system[n − delay] + near[n]`.
    fn fixture() -> (Vec<f32>, Vec<f32>, Vec<f32>, Vec<EchoWindow>) {
        let secs = 6.0;
        let len = (RATE as f32 * secs) as usize;
        let delay = 200usize; // 12.5 ms
        let echo_gain = 0.5f32;
        let mut seed_sys = 0x1234u64;
        let mut seed_near = 0x4321u64;

        let mut system = vec![0f32; len];
        let mut near = vec![0f32; len];
        for n in 0..len {
            let t = n as f32 / RATE as f32;
            // Speech-like: a couple of formants + broadband noise.
            system[n] = 0.35 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * 540.0 * t).sin()
                + 0.1 * lcg(&mut seed_sys);
            // Near voice: different spectrum, silent for the first 1 s (echo
            // window) so adaptation there is uncontaminated.
            if n < RATE as usize {
                near[n] = 0.0;
            } else {
                near[n] = 0.5 * (2.0 * std::f32::consts::PI * 820.0 * t).sin()
                    + 0.25 * lcg(&mut seed_near);
            }
        }
        let mut mic = vec![0f32; len];
        for n in 0..len {
            let e = if n >= delay {
                echo_gain * system[n - delay]
            } else {
                0.0
            };
            mic[n] = e + near[n];
        }
        let windows = vec![EchoWindow {
            start_ms: 0,
            end_ms: 1000,
        }];
        (mic, system, near, windows)
    }

    #[test]
    fn delay_estimator_finds_the_echo_lag() {
        let (mic, system, _near, windows) = fixture();
        let d = estimate_delay(&mic, &system, &windows);
        // The true delay is 200 samples; allow a tiny ±2 tolerance.
        assert!(
            (d as i32 - 200).abs() <= 2,
            "estimated delay {d}, expected ~200"
        );
    }

    #[test]
    fn pure_echo_is_cancelled() {
        // No near voice at all — the whole track is echo. The filter should
        // drive the cleaned output to near-silence everywhere. Isolates whether
        // PB-FDAF converges, independent of double-talk.
        let secs = 4.0;
        let len = (RATE as f32 * secs) as usize;
        let delay = 200usize;
        let echo_gain = 0.5f32;
        let mut seed = 0x9999u64;
        let mut system = vec![0f32; len];
        for (n, s) in system.iter_mut().enumerate() {
            let t = n as f32 / RATE as f32;
            *s = 0.35 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * 540.0 * t).sin()
                + 0.1 * lcg(&mut seed);
        }
        let mut mic = vec![0f32; len];
        for n in delay..len {
            mic[n] = echo_gain * system[n - delay];
        }
        // Mark the whole track as an echo window.
        let windows = vec![EchoWindow {
            start_ms: 0,
            end_ms: secs as u64 * 1000,
        }];
        let cleaned = clean_samples(&mic, &system, &windows, &ApplyScope::WholeTrack, |_| {});
        let mic_e = msq(&mic);
        let cleaned_e = msq(&cleaned);
        eprintln!(
            "[pure] mic_e={mic_e:.5} cleaned_e={cleaned_e:.5} ratio={:.4}",
            cleaned_e / (mic_e + 1e-9)
        );
        assert!(
            cleaned_e < 0.05 * mic_e,
            "pure echo not cancelled: ratio {:.4}",
            cleaned_e / (mic_e + 1e-9),
        );
    }

    #[test]
    fn cleans_synthetic_echo() {
        let (mic, system, near, windows) = fixture();
        let cleaned = clean_samples(&mic, &system, &windows, &ApplyScope::WholeTrack, |_| {});

        // 1) In the echo window (first 1 s, near ≡ 0): the cleaned output should
        //    be near-silent — the echo was removed. Compare against the raw mic
        //    energy there (which is pure echo).
        let win = RATE as usize; // first 1 s
        let mic_win_e = msq(&mic[..win]);
        let cleaned_win_e = msq(&cleaned[..win]);
        let ratio = cleaned_win_e / (mic_win_e + 1e-9);
        assert!(
            ratio < 0.06,
            "echo not suppressed in window: ratio {ratio:.4} (mic_win={mic_win_e:.5} cleaned_win={cleaned_win_e:.5})",
        );

        // 2) Where the near voice is active (after 1 s): the cleaned output
        //    should track the near voice, not the echo. The residual mask
        //    attenuates it some (a per-frame gain < 1), so compare shapes via
        //    relative error, not exact equality.
        let near_active = &near[RATE as usize..];
        let cleaned_active = &cleaned[RATE as usize..near.len()];
        let mut num = 0f32;
        let mut den = 0f32;
        for i in 0..near_active.len() {
            num += (cleaned_active[i] - near_active[i]).powi(2);
            den += near_active[i].powi(2);
        }
        let rel_err = (num / (den + 1e-9)).sqrt();
        assert!(
            rel_err < 0.5,
            "near voice not preserved: relative error {rel_err:.3}",
        );
    }

    #[test]
    fn scoped_clean_leaves_outside_bit_identical() {
        // The blast-radius contract: with `ApplyScope::Ranges`, only the listed
        // region(s) are cleaned; everything outside is the original mic
        // sample-for-sample (± the crossfade window at each boundary). This is
        // the fix for the "added echo to everything" bug — a mis-converged `W`
        // can only corrupt the requested region.
        let (mic, system, _near, windows) = fixture();
        // Apply only to the near-active region (1 s … 2 s). The echo window
        // (first 1 s) is still used for learning but is NOT in the apply scope,
        // so it must come back bit-identical.
        let apply = ApplyScope::Ranges(vec![EchoWindow {
            start_ms: 1000,
            end_ms: 2000,
        }]);
        let cleaned = clean_samples(&mic, &system, &windows, &apply, |_| {});

        // Outside the apply region the cleaned output must equal the original
        // mic exactly. The crossfade blends only *inside* the range, so
        // `fade_weight` is exactly 0 for pos < start or pos >= end — no margin
        // is needed on the outside.
        // Apply region is [1 s, 2 s] = [16000, 32000).
        // Echo window (0..1 s) is before the apply start → bit-identical.
        for n in (0..RATE as usize).step_by(97) {
            assert_eq!(
                cleaned[n], mic[n],
                "echo-window sample {n} changed outside apply scope"
            );
        }
        // Late region (from 2 s onward) is after the apply end → bit-identical.
        for n in (RATE as usize * 2..mic.len()).step_by(97) {
            assert_eq!(
                cleaned[n], mic[n],
                "late sample {n} changed outside apply scope"
            );
        }
        // The apply region itself is cleared of echo. The cleaned energy in the
        // body of [1 s, 2 s] (past the entry crossfade) should be well below the
        // raw mic energy there (echo + near); compare against the echo-only
        // reference energy (mic in the first 1 s, which is pure echo).
        let body_lo = RATE as usize + XFADE;
        let body_hi = RATE as usize * 2 - XFADE;
        let apply_e = msq(&cleaned[body_lo..body_hi]);
        let echo_ref_e = msq(&mic[..RATE as usize]);
        // The apply region contains the near voice too, so we can't expect near-
        // silence; assert the echo component was substantially reduced (cleaned
        // energy should not exceed a small multiple of the echo reference).
        assert!(
            apply_e < 4.0 * echo_ref_e + 1e-6,
            "echo not reduced in apply region: apply_e={apply_e:.5} echo_ref_e={echo_ref_e:.5}",
        );
    }

    #[test]
    fn scoped_clean_empty_ranges_returns_mic_unchanged() {
        let (mic, system, _near, windows) = fixture();
        let apply = ApplyScope::Ranges(vec![]);
        let cleaned = clean_samples(&mic, &system, &windows, &apply, |_| {});
        assert_eq!(cleaned, mic, "empty apply scope should return mic verbatim");
    }
}
