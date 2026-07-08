//! Microphone capture thread.
//!
//! Primary path: AVAudioEngine input node with **voice processing** enabled —
//! Apple's echo cancellation + noise suppression (the FaceTime stack). This
//! subtracts whatever is playing on the speakers from the mic signal and
//! suppresses steady background noise, which keeps remote voices and room
//! noise out of `mic.wav` when the user isn't wearing headphones.
//!
//! Fallback path: plain cpal capture (raw mic), used only if voice
//! processing fails to initialize on this device.
//!
//! Both engines are `!Send`-ish (stream/engine must live on one thread), so
//! one dedicated thread owns the capture for its whole lifetime.

use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use block2::RcBlock;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, Receiver, Sender};
use objc2_avf_audio::{
    AVAudioEngine, AVAudioPCMBuffer, AVAudioTime,
    AVAudioVoiceProcessingOtherAudioDuckingConfiguration,
    AVAudioVoiceProcessingOtherAudioDuckingLevel,
};

use super::pipeline::{downmix_interleaved, ChannelMeters, ChannelPipeline, LiveChunk, Source};

pub struct MicThread {
    pub handle: JoinHandle<Result<PathBuf, String>>,
}

/// Spawn the mic capture thread. `ready_tx` receives Ok(()) once audio is
/// flowing or Err(reason) if setup failed on all paths.
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
    match run_voice_processed(&wav_path, &stop, &meters, &live_tx, &ready_tx) {
        Ok(path) => Ok(path),
        Err(MicError::Runtime(e)) => Err(e),
        Err(MicError::Setup(e)) => {
            eprintln!("voice-processed mic capture unavailable ({e}); falling back to raw mic");
            run_cpal(wav_path, stop, meters, live_tx, ready_tx)
        }
    }
}

enum MicError {
    /// Failed before audio flowed — falling back to cpal is safe.
    Setup(String),
    /// Failed after `ready` was signalled — do not fall back.
    Runtime(String),
}

// ---------------------------------------------------------------------------
// Primary: AVAudioEngine with voice processing (AEC + noise suppression)
// ---------------------------------------------------------------------------

fn run_voice_processed(
    wav_path: &PathBuf,
    stop: &Arc<AtomicBool>,
    meters: &Arc<ChannelMeters>,
    live_tx: &Option<Sender<LiveChunk>>,
    ready_tx: &Sender<Result<(), String>>,
) -> Result<PathBuf, MicError> {
    let setup = (|| -> Result<_, String> {
        // SAFETY: engine + nodes are created and used on this thread only.
        unsafe {
            let engine = AVAudioEngine::new();
            let input = engine.inputNode();

            // Must happen while the engine is stopped. Enabling VP on the
            // input node automatically enables it on the output node.
            input
                .setVoiceProcessingEnabled_error(true)
                .map_err(|e| format!("voice processing not available: {e}"))?;

            // Don't let voice processing duck "other audio" — that other
            // audio is exactly what the system tap is recording.
            input.setVoiceProcessingOtherAudioDuckingConfiguration(
                AVAudioVoiceProcessingOtherAudioDuckingConfiguration {
                    enableAdvancedDucking: objc2::runtime::Bool::NO,
                    duckingLevel: AVAudioVoiceProcessingOtherAudioDuckingLevel::Min,
                },
            );

            let format = input.outputFormatForBus(0);
            let sample_rate = format.sampleRate() as u32;
            let channels = format.channelCount() as usize;
            let interleaved = format.isInterleaved();
            if sample_rate == 0 || channels == 0 {
                return Err("voice-processed input has an empty format".into());
            }

            let (chunk_tx, chunk_rx) = bounded::<Vec<f32>>(64);

            // Tap callback: copy out samples, downmix to mono, hand off.
            // CAUTION: runs on an audio thread; keep it allocation-light.
            let tap = RcBlock::new(
                move |buf: NonNull<AVAudioPCMBuffer>, _when: NonNull<AVAudioTime>| {
                    let buf = buf.as_ref();
                    let frames = buf.frameLength() as usize;
                    if frames == 0 {
                        return;
                    }
                    let data = buf.floatChannelData();
                    if data.is_null() {
                        return;
                    }
                    // `floatChannelData` yields NonNull channel pointers.
                    let mono: Vec<f32> = if interleaved {
                        let stride = (buf.stride() as usize).max(1);
                        let ptr = (*data).as_ptr();
                        let slice = std::slice::from_raw_parts(ptr, frames * stride);
                        downmix_interleaved(slice, stride)
                    } else {
                        let chans = std::slice::from_raw_parts(data, channels);
                        let mut acc = vec![0.0f32; frames];
                        for &cptr in chans {
                            let ch = std::slice::from_raw_parts(cptr.as_ptr(), frames);
                            for (a, &s) in acc.iter_mut().zip(ch.iter()) {
                                *a += s;
                            }
                        }
                        if channels > 1 {
                            for a in acc.iter_mut() {
                                *a /= channels as f32;
                            }
                        }
                        acc
                    };
                    let _ = chunk_tx.try_send(mono);
                },
            );

            input.installTapOnBus_bufferSize_format_block(
                0,
                4096,
                Some(&format),
                &*tap as *const _ as *mut _,
            );

            engine.prepare();
            if let Err(e) = engine.startAndReturnError() {
                input.removeTapOnBus(0);
                return Err(format!("audio engine failed to start: {e}"));
            }

            Ok((engine, input, sample_rate, chunk_rx, tap))
        }
    })();

    let (engine, input, sample_rate, chunk_rx, _tap) = setup.map_err(MicError::Setup)?;

    let mut pipeline = match ChannelPipeline::new(
        wav_path,
        sample_rate,
        Source::Mic,
        meters.clone(),
        live_tx.clone(),
    ) {
        Ok(p) => p,
        Err(e) => {
            unsafe {
                input.removeTapOnBus(0);
                engine.stop();
            }
            return Err(MicError::Setup(e));
        }
    };

    let _ = ready_tx.send(Ok(()));

    let result = pump(&chunk_rx, stop, &mut pipeline);

    // SAFETY: same thread that created them.
    unsafe {
        input.removeTapOnBus(0);
        engine.stop();
    }
    result.map_err(MicError::Runtime)?;
    while let Ok(chunk) = chunk_rx.try_recv() {
        pipeline.push(&chunk).map_err(MicError::Runtime)?;
    }
    pipeline.finalize().map_err(MicError::Runtime)
}

// ---------------------------------------------------------------------------
// Fallback: raw cpal capture (no echo cancellation)
// ---------------------------------------------------------------------------

fn run_cpal(
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

    let mut pipeline =
        match ChannelPipeline::new(&wav_path, sample_rate, Source::Mic, meters, live_tx) {
            Ok(p) => p,
            Err(e) => {
                let _ = ready_tx.send(Err(e.clone()));
                return Err(e);
            }
        };

    let (chunk_tx, chunk_rx) = bounded::<Vec<f32>>(64);
    // Type of `e` (cpal's stream error) is inferred from the trait bound.
    let err_fn = move |e| {
        eprintln!("mic stream error: {e}");
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

    let pump_result = {
        let mut push = |chunk: Vec<f32>| -> Result<(), String> {
            let mono = downmix_interleaved(&chunk, channels);
            pipeline.push(&mono)
        };
        let mut res: Result<(), String> = Ok(());
        while !stop.load(Ordering::Relaxed) {
            match chunk_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(chunk) => {
                    if let Err(e) = push(chunk) {
                        res = Err(e);
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
        res
    };

    drop(stream); // stop callbacks before draining
    pump_result?;
    while let Ok(chunk) = chunk_rx.try_recv() {
        let mono = downmix_interleaved(&chunk, channels);
        pipeline.push(&mono)?;
    }

    pipeline.finalize()
}

// ---------------------------------------------------------------------------
// Shared pump loop
// ---------------------------------------------------------------------------

fn pump(
    rx: &Receiver<Vec<f32>>,
    stop: &Arc<AtomicBool>,
    pipeline: &mut ChannelPipeline,
) -> Result<(), String> {
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => pipeline.push(&chunk)?,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}
