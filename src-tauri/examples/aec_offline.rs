//! Offline AEC proof-of-concept.
//!
//! Reads a `mic.wav` + `system.wav` pair (both 16 kHz mono), runs them through
//! the same WebRTC AudioProcessing used by the live capture path, and writes a
//! cleaned `mic_cleaned.wav`. Use this to A/B-listen and confirm the echo is
//! gone and the near-end voice is preserved *before* touching live capture.
//!
//! Usage:
//!   cargo run --example aec_offline -- <session>/mic.wav <session>/system.wav <out.wav>
//!
//! The session WAVs live under the app data dir (see `tauri.conf.json` /
//! `app_data_dir`). Both files are captured at 16 kHz mono i16 by the pipeline,
//! so no resampling is needed here.

use std::env;

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

use lilnotes_lib::audio::aec;
use lilnotes_lib::AecAggressiveness;

const RATE: u32 = 16_000;

/// Reads a WAV as mono f32 samples in [-1, 1]. Multi-channel files are
/// downmixed to the first channel (sufficient for the offline AEC test — the
/// live capture path is already mono).
fn read_wav_mono_f32(path: &str) -> (WavSpec, Vec<f32>) {
    let mut reader = WavReader::open(path).unwrap_or_else(|e| {
        eprintln!("could not open {path}: {e}");
        std::process::exit(1);
    });
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;

    let raw: Vec<f32> = match spec.sample_format {
        SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.expect("i16 decode") as f32 / 32768.0)
            .collect(),
        SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.expect("f32 decode"))
            .collect(),
    };

    let mono = if ch == 1 {
        raw
    } else {
        raw.into_iter().step_by(ch).collect()
    };
    (spec, mono)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: aec_offline <mic.wav> <system.wav> <out.wav>");
        std::process::exit(2);
    }
    let mic_path = &args[1];
    let sys_path = &args[2];
    let out_path = &args[3];

    let (mic_spec, mic) = read_wav_mono_f32(mic_path);
    let (_sys_spec, system) = read_wav_mono_f32(sys_path);

    if mic_spec.sample_rate != RATE {
        eprintln!(
            "mic.wav must be {RATE} Hz (got {}); the offline tool expects the pipeline's 16 kHz output",
            mic_spec.sample_rate
        );
        std::process::exit(1);
    }
    if mic_spec.sample_rate != _sys_spec.sample_rate {
        eprintln!(
            "mic + system sample rates differ ({} vs {}); both must be {RATE}",
            mic_spec.sample_rate, _sys_spec.sample_rate
        );
        std::process::exit(1);
    }

    eprintln!(
        "mic: {} samples ({:.1}s), system: {} samples ({:.1}s)",
        mic.len(),
        mic.len() as f64 / RATE as f64,
        system.len(),
        system.len() as f64 / RATE as f64,
    );

    let (mut aec, mut render) =
        aec::new_aec_pair(RATE, AecAggressiveness::Strong).expect("APM init");

    let frame = aec::FRAME_SAMPLES;
    // Feed render (system = reference) and capture (mic = forward) in
    // lockstep, one 10 ms frame at a time. APM's internal delay estimator
    // finds the acoustic delay, so exact sample alignment between the two
    // isn't required — both share t=0 = recording start.
    let n = mic.len().max(system.len());
    let mut out_samples: Vec<f32> = Vec::with_capacity(mic.len());
    let mut mi = 0usize;
    let mut si = 0usize;
    while mi < n {
        // One render frame (system); zero-pad if system is shorter than mic.
        let mut rframe = vec![0.0f32; frame];
        let rlen = frame.min(system.len().saturating_sub(si));
        if rlen > 0 {
            rframe[..rlen].copy_from_slice(&system[si..si + rlen]);
            si += rlen;
        }
        render.feed_render(&rframe).expect("render frame");

        // One capture frame (mic); zero-pad if mic is shorter.
        let mut cframe = vec![0.0f32; frame];
        let clen = frame.min(mic.len().saturating_sub(mi));
        if clen > 0 {
            cframe[..clen].copy_from_slice(&mic[mi..mi + clen]);
            mi += clen;
        }
        let cleaned = aec.process_capture(&cframe).expect("capture frame");
        out_samples.extend_from_slice(&cleaned);
    }

    // Recover any buffered tail (shouldn't be any since we pad per-frame, but
    // be safe).
    let tail = aec.flush().expect("flush");
    out_samples.extend_from_slice(&tail);
    render.flush().expect("render flush");

    let stats = aec.stats();
    eprintln!("AEC stats:");
    eprintln!(
        "  echo_return_loss:             {} dB",
        fmt_opt(stats.echo_return_loss)
    );
    eprintln!(
        "  echo_return_loss_enhancement: {} dB",
        fmt_opt(stats.echo_return_loss_enhancement),
    );
    eprintln!(
        "  residual_echo_likelihood:     {}",
        fmt_opt(stats.residual_echo_likelihood),
    );
    eprintln!(
        "  delay_ms:                     {}",
        fmt_opt(stats.delay_ms)
    );

    // Write cleaned mic as 16 kHz mono i16 WAV.
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(out_path, spec).expect("create out wav");
    for &s in &out_samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        writer.write_sample(v).expect("write sample");
    }
    writer.finalize().expect("finalize wav");
    eprintln!(
        "wrote {} samples ({:.1}s) to {out_path}",
        out_samples.len(),
        out_samples.len() as f64 / RATE as f64,
    );
}

fn fmt_opt<T: std::fmt::Display>(v: Option<T>) -> String {
    match v {
        Some(x) => format!("{x:.2}"),
        None => "n/a".to_string(),
    }
}
