//! Dual-source capture engine.
//!
//! Microphone and system audio are captured on independent threads and are
//! NEVER mixed: each channel is resampled to 16 kHz mono and written to its
//! own FLAC (`mic.flac`, `system.flac`; see `codec.rs`). Level meters and
//! elapsed time stream to the UI via the `capture:levels` Tauri event (~10 Hz).

pub mod aec;
pub mod codec;
pub mod mic;
pub mod migrate;
pub mod offline_aec;
pub mod pipeline;
pub mod playback;
pub mod resampler;
pub mod system_tap;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::Sender;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use pipeline::{ChannelMeters, LiveChunk};

/// Payload of the `capture:levels` event.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LevelsEvent {
    pub session_id: String,
    pub elapsed_ms: u64,
    pub mic_rms: f32,
    pub mic_peak: f32,
    pub system_rms: f32,
    pub system_peak: f32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StartedRecording {
    pub session_id: String,
    pub started_at_ms: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StoppedRecording {
    pub session_id: String,
    pub mic_wav: String,
    pub system_wav: String,
    pub duration_ms: u64,
    pub started_at_ms: u64,
}

struct ActiveSession {
    session_id: String,
    started_at: Instant,
    started_at_ms: u64,
    stop_flag: Arc<AtomicBool>,
    mic: Option<mic::MicThread>,
    system: Option<system_tap::SystemThread>,
    emitter: Option<std::thread::JoinHandle<()>>,
}

/// Managed by Tauri as shared state.
#[derive(Default)]
pub struct CaptureEngine {
    active: Mutex<Option<ActiveSession>>,
}

impl CaptureEngine {
    pub fn is_recording(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    pub fn status(&self) -> Option<(String, u64)> {
        self.active.lock().unwrap().as_ref().map(|s| {
            (
                s.session_id.clone(),
                s.started_at.elapsed().as_millis() as u64,
            )
        })
    }

    /// Start a new capture session. `dir` is the per-session directory the
    /// channel FLACs are written into. When `live_tx` is set, both pipelines feed
    /// 16 kHz chunks into it for near-live transcription; the senders drop
    /// when capture stops, which is the ASR worker's end-of-stream signal.
    /// When `aec_enabled` is true, a shared WebRTC APM is created: the system
    /// pipeline feeds it the render/reference signal and the mic pipeline
    /// runs its capture/forward path (echo cancellation + NS). The mic's
    /// Apple voice processing is disabled in that case so the APM sees the
    /// true echo.
    pub fn start(
        &self,
        app: AppHandle,
        dir: PathBuf,
        live_tx: Option<Sender<LiveChunk>>,
        aec_enabled: bool,
        aec_aggressiveness: crate::settings::AecAggressiveness,
    ) -> Result<StartedRecording, String> {
        let mut guard = self.active.lock().unwrap();
        if guard.is_some() {
            return Err("a recording is already in progress".into());
        }

        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create session dir {}: {e}", dir.display()))?;

        let session_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let stop_flag = Arc::new(AtomicBool::new(false));
        let mic_meters = Arc::new(ChannelMeters::default());
        let sys_meters = Arc::new(ChannelMeters::default());

        // Build the shared AEC pair when enabled. The AecProcessor (capture)
        // goes to the mic pipeline; the AecRenderFeeder (render) goes to the
        // system pipeline. Both share one inner Processor via Arc.
        let (aec_capture, aec_render) = if aec_enabled {
            eprintln!(
                "[aec] enabled — software AEC active (raw mic, WebRTC APM, {:?})",
                aec_aggressiveness
            );
            let (cap, ren) = aec::new_aec_pair(resampler::TARGET_RATE, aec_aggressiveness)
                .map_err(|e| format!("AEC init failed: {e}"))?;
            (Some(cap), Some(ren))
        } else {
            eprintln!("[aec] disabled — Apple voice processing active");
            (None, None)
        };

        let (mic_ready_tx, mic_ready_rx) = crossbeam_channel::bounded(1);
        let (sys_ready_tx, sys_ready_rx) = crossbeam_channel::bounded(1);

        let mic_thread = mic::spawn(
            dir.join(codec::MIC_FILE),
            stop_flag.clone(),
            mic_meters.clone(),
            live_tx.clone(),
            mic_ready_tx,
            !aec_enabled, // raw mic when AEC is on
            aec_capture,
        );
        let sys_thread = system_tap::spawn(
            dir.join(codec::SYSTEM_FILE),
            stop_flag.clone(),
            sys_meters.clone(),
            live_tx,
            sys_ready_tx,
            aec_render,
        );

        // Wait for both sources to come up (or fail) before declaring success.
        // The system tap may block on the TCC prompt the first time, so allow
        // a generous window.
        let wait = Duration::from_secs(120);
        let mic_ok = mic_ready_rx
            .recv_timeout(wait)
            .map_err(|_| "mic setup timed out")?;
        let sys_ok = sys_ready_rx
            .recv_timeout(wait)
            .map_err(|_| "system audio setup timed out")?;
        if let Err(e) = mic_ok.and(sys_ok) {
            // Abort: stop whichever side started, join threads, clean up.
            stop_flag.store(true, Ordering::Relaxed);
            let _ = mic_thread.handle.join();
            let _ = sys_thread.handle.join();
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }

        // Meter/elapsed emitter (~10 Hz).
        let emitter = {
            let stop = stop_flag.clone();
            let session = session_id.clone();
            let started = Instant::now();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let (mic_rms, mic_peak) = mic_meters.read_and_reset_peak();
                    let (system_rms, system_peak) = sys_meters.read_and_reset_peak();
                    let _ = app.emit_to(
                        "main",
                        "capture:levels",
                        LevelsEvent {
                            session_id: session.clone(),
                            elapsed_ms: started.elapsed().as_millis() as u64,
                            mic_rms,
                            mic_peak,
                            system_rms,
                            system_peak,
                        },
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        };

        *guard = Some(ActiveSession {
            session_id: session_id.clone(),
            started_at: Instant::now(),
            started_at_ms,
            stop_flag,
            mic: Some(mic_thread),
            system: Some(sys_thread),
            emitter: Some(emitter),
        });

        Ok(StartedRecording {
            session_id,
            started_at_ms,
        })
    }

    pub fn stop(&self) -> Result<StoppedRecording, String> {
        let mut session = self
            .active
            .lock()
            .unwrap()
            .take()
            .ok_or("no recording in progress")?;

        let duration_ms = session.started_at.elapsed().as_millis() as u64;
        session.stop_flag.store(true, Ordering::Relaxed);

        let mic_res = session
            .mic
            .take()
            .unwrap()
            .handle
            .join()
            .map_err(|_| "mic thread panicked".to_string())?;
        let sys_res = session
            .system
            .take()
            .unwrap()
            .handle
            .join()
            .map_err(|_| "system capture thread panicked".to_string())?;
        if let Some(h) = session.emitter.take() {
            let _ = h.join();
        }

        let mic_wav = mic_res?;
        let system_wav = sys_res?;

        Ok(StoppedRecording {
            session_id: session.session_id,
            mic_wav: mic_wav.to_string_lossy().into_owned(),
            system_wav: system_wav.to_string_lossy().into_owned(),
            duration_ms,
            started_at_ms: session.started_at_ms,
        })
    }
}
