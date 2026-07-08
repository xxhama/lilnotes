//! Microphone capture thread (cpal / Core Audio).
//!
//! The cpal `Stream` is `!Send`, so one dedicated thread owns the stream for
//! its whole lifetime: create -> pump samples through the pipeline -> drop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, Sender};

use super::pipeline::{downmix_interleaved, ChannelMeters, ChannelPipeline, LiveChunk, Source};

pub struct MicThread {
    pub handle: JoinHandle<Result<PathBuf, String>>,
}

/// Spawn the mic capture thread. `ready_tx` receives Ok(()) once audio is
/// flowing (stream built) or Err(reason) if setup failed.
pub fn spawn(
    wav_path: PathBuf,
    stop: Arc<AtomicBool>,
    meters: Arc<ChannelMeters>,
    live_tx: Option<Sender<LiveChunk>>,
    ready_tx: Sender<Result<(), String>>,
) -> MicThread {
    let handle = std::thread::Builder::new()
        .name("mic-capture".into())
        .spawn(move || run(wav_path, stop, meters, live_tx, ready_tx))
        .expect("failed to spawn mic thread");
    MicThread { handle }
}

fn run(
    wav_path: PathBuf,
    stop: Arc<AtomicBool>,
    meters: Arc<ChannelMeters>,
    live_tx: Option<Sender<LiveChunk>>,
    ready_tx: Sender<Result<(), String>>,
) -> Result<PathBuf, String> {
    let setup = (|| -> Result<_, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device (is a microphone connected?)".to_string())?;
        let supported = device
            .default_input_config()
            .map_err(|e| format!("failed to read mic config: {e}"))?;
        Ok((device, supported))
    })();

    let (device, supported) = match setup {
        Ok(x) => x,
        Err(e) => {
            let _ = ready_tx.send(Err(e.clone()));
            return Err(e);
        }
    };

    let sample_rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.config();

    let mut pipeline = match ChannelPipeline::new(&wav_path, sample_rate, Source::Mic, meters, live_tx)
    {
        Ok(p) => p,
        Err(e) => {
            let _ = ready_tx.send(Err(e.clone()));
            return Err(e);
        }
    };

    // Audio callback -> bounded channel -> this thread. The callback stays
    // light (convert + send); resampling/writing happens here.
    let (chunk_tx, chunk_rx) = bounded::<Vec<f32>>(64);
    let err_flag: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let err_flag_cb = err_flag.clone();
    // Type of `e` (cpal's stream error) is inferred from the trait bound.
    let err_fn = move |e| {
        eprintln!("mic stream error: {e}");
        err_flag_cb.store(true, Ordering::Relaxed);
    };

    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let tx = chunk_tx.clone();
            device.build_input_stream(
                config.clone(),
                move |data: &[f32], _| {
                    let _ = tx.try_send(data.to_vec());
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let tx = chunk_tx.clone();
            device.build_input_stream(
                config.clone(),
                move |data: &[i16], _| {
                    let v: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                    let _ = tx.try_send(v);
                },
                err_fn,
                None,
            )
        }
        other => {
            let e = format!("unsupported mic sample format: {other:?}");
            let _ = ready_tx.send(Err(e.clone()));
            return Err(e);
        }
    };

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            // On macOS this is also where a TCC denial surfaces.
            let e = format!("failed to open mic stream (check Microphone permission): {e}");
            let _ = ready_tx.send(Err(e.clone()));
            return Err(e);
        }
    };
    if let Err(e) = stream.play() {
        let e = format!("failed to start mic stream: {e}");
        let _ = ready_tx.send(Err(e.clone()));
        return Err(e);
    }
    let _ = ready_tx.send(Ok(()));

    // Pump until stop is requested, then drain what's left.
    while !stop.load(Ordering::Relaxed) {
        match chunk_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                let mono = downmix_interleaved(&chunk, channels);
                pipeline.push(&mono)?;
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(stream); // stop callbacks before draining
    while let Ok(chunk) = chunk_rx.try_recv() {
        let mono = downmix_interleaved(&chunk, channels);
        pipeline.push(&mono)?;
    }

    pipeline.finalize()
}
