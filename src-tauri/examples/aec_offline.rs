//! Offline AEC proof-of-concept.
//!
//! Reads a `mic` + `system` recording pair (16 kHz mono FLAC, or legacy WAV),
//! runs them through the same WebRTC AudioProcessing used by the live capture
//! path, and writes a cleaned `mic_cleaned.flac`. Use this to A/B-listen and
//! confirm the echo is gone and the near-end voice is preserved *before*
//! touching live capture.
//!
//! Usage:
//!   cargo run --example aec_offline -- <session>/mic.flac <session>/system.flac <out.flac>
//!
//! The session recordings live under the app data dir (see `tauri.conf.json` /
//! `app_data_dir`). Both files are captured at 16 kHz mono i16 by the pipeline,
//! so no resampling is needed here.

use std::env;
use std::path::Path;

use lilnotes_lib::audio::aec;
use lilnotes_lib::audio::codec::{read_mono_f32, MonoWriter};
use lilnotes_lib::AecAggressiveness;

const RATE: u32 = 16_000;

/// Reads a recording as mono f32 samples in [-1, 1] plus its sample rate.
fn read_mono(path: &str) -> (u32, Vec<f32>) {
    read_mono_f32(path).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    })
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: aec_offline <mic.flac> <system.flac> <out.flac>");
        std::process::exit(2);
    }
    let mic_path = &args[1];
    let sys_path = &args[2];
    let out_path = &args[3];

    let (mic_rate, mic) = read_mono(mic_path);
    let (sys_rate, system) = read_mono(sys_path);

    if mic_rate != RATE {
        eprintln!(
            "mic recording must be {RATE} Hz (got {mic_rate}); the offline tool expects the pipeline's 16 kHz output"
        );
        std::process::exit(1);
    }
    if mic_rate != sys_rate {
        eprintln!(
            "mic + system sample rates differ ({mic_rate} vs {sys_rate}); both must be {RATE}"
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

    // Write cleaned mic as 16 kHz mono i16 FLAC.
    let mut writer = MonoWriter::create(Path::new(out_path)).expect("create out flac");
    let pcm: Vec<i16> = out_samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect();
    writer.write_samples(&pcm).expect("write samples");
    writer.finalize().expect("finalize flac");
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
