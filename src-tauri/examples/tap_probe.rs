//! Headless system-audio capture probe (dev diagnostics).
//!
//! Drives the exact production capture path (`audio::system_tap`) without the
//! UI: plays a system sound through the default output while tapping system
//! audio for a few seconds, then reports live meter readings and the RMS of
//! the captured FLAC. Use it to check whether the current launch context
//! (bare binary, app bundle, LaunchServices) actually receives system audio
//! from TCC, independent of the app window.
//!
//! Usage:
//!   cargo run --example tap_probe
//!
//! Exit codes: 0 = captured real audio, 2 = tap setup failed,
//! 3 = tap ran but captured only silence (TCC silently denying).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lilnotes_lib::audio::codec;
use lilnotes_lib::audio::pipeline::{ChannelMeters, LiveChunk};
use lilnotes_lib::audio::system_tap;

fn main() {
    let wav_path = std::env::temp_dir().join("lilnotes_tap_probe.flac");
    let _ = std::fs::remove_file(&wav_path);

    // Keep the default output busy so the tap has something to hear.
    let player = std::thread::spawn(|| {
        for _ in 0..3 {
            let _ = std::process::Command::new("afplay")
                .arg("/System/Library/Sounds/Submarine.aiff")
                .status();
        }
    });

    let stop = Arc::new(AtomicBool::new(false));
    let meters = Arc::new(ChannelMeters::default());
    let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
    let live_tx: Option<crossbeam_channel::Sender<LiveChunk>> = None;

    let thread = system_tap::spawn(
        wav_path.clone(),
        stop.clone(),
        meters.clone(),
        live_tx,
        ready_tx,
        None,
    );

    match ready_rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(())) => println!("tap: up"),
        Ok(Err(e)) => {
            println!("tap: SETUP FAILED: {e}");
            std::process::exit(2);
        }
        Err(_) => {
            println!("tap: setup timed out");
            std::process::exit(2);
        }
    }

    for i in 1..=5 {
        std::thread::sleep(Duration::from_secs(1));
        let (rms, peak) = meters.read_and_reset_peak();
        println!("t={i}s meter rms={rms:.5} peak={peak:.5}");
    }

    stop.store(true, Ordering::Relaxed);
    let path = thread
        .handle
        .join()
        .expect("capture thread panicked")
        .expect("capture thread errored");
    let _ = player.join();

    let (_, samples) =
        codec::read_mono_f32(&path.to_string_lossy()).expect("open captured recording");
    let n = samples.len();
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = if n > 0 {
        (sum_sq / n as f64).sqrt()
    } else {
        0.0
    };
    println!("flac: {} samples, rms={rms:.6} ({})", n, path.display());

    if rms < 1e-5 {
        println!("verdict: SILENCE — tap ran but delivered zeros");
        std::process::exit(3);
    }
    println!("verdict: OK — real system audio captured");
}
