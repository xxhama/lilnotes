//! Speaker diarization on the system channel (sherpa-onnx).
//!
//! Pipeline: pyannote segmentation-3.0 detects speech regions and speaker
//! changes; CAM++ embeddings + clustering group them into speakers. Runs
//! offline over the finished `system.wav` — the mic channel is "Me" by
//! construction and is never diarized.
//!
//! Speaker count is unknown in a meeting, so clustering uses a similarity
//! threshold (`num_clusters: -1`) rather than a fixed k.

use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use sherpa_rs::diarize::{Diarize, DiarizeConfig};
use tauri::{AppHandle, Emitter};

use crate::models;

/// A diarized speaker turn on the system channel.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Raw label: "SPEAKER_00", "SPEAKER_01", …
    pub speaker: String,
}

/// Payload of the `diarize:progress` event.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DiarizeProgress {
    processed: i32,
    total: i32,
}

#[derive(Default)]
pub struct DiarizeEngine {
    inner: Mutex<Option<Diarize>>,
}

impl DiarizeEngine {
    /// Download models if needed and initialize sherpa-onnx (kept loaded).
    fn ensure_loaded(&self, app: &AppHandle) -> Result<(), String> {
        let mut guard = self.inner.lock().unwrap();
        if guard.is_some() {
            return Ok(());
        }
        let cancel = AtomicBool::new(false); // small models; not cancellable
        let (seg, emb) = models::ensure_diarize_models(app, &cancel)?;
        let config = DiarizeConfig {
            // Threshold-based clustering: speaker count is unknown.
            num_clusters: Some(-1),
            threshold: Some(0.5),
            // Ignore micro-blips; bridge sub-second gaps within a turn.
            min_duration_on: Some(0.3),
            min_duration_off: Some(0.5),
            provider: None,
            debug: false,
        };
        let diarize = Diarize::new(seg, emb, config)
            .map_err(|e| format!("failed to initialize diarization: {e}"))?;
        *guard = Some(diarize);
        Ok(())
    }

    /// Diarize a 16 kHz mono WAV; emits `diarize:progress` events.
    pub fn diarize_wav(&self, app: &AppHandle, wav_path: &str) -> Result<Vec<Turn>, String> {
        self.ensure_loaded(app)?;

        let mut reader = hound::WavReader::open(wav_path)
            .map_err(|e| format!("cannot open {wav_path}: {e}"))?;
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("cannot read {wav_path}: {e}"))?;
        if samples.len() < 16_000 {
            return Ok(Vec::new()); // < 1 s of audio: nothing to diarize
        }

        let progress_app = app.clone();
        let last_emit = Mutex::new(Instant::now() - Duration::from_secs(1));
        let callback = Box::new(move |processed: i32, total: i32| -> i32 {
            let mut last = last_emit.lock().unwrap();
            if last.elapsed() > Duration::from_millis(200) {
                let _ = progress_app.emit("diarize:progress", DiarizeProgress { processed, total });
                *last = Instant::now();
            }
            0 // continue
        });

        let mut guard = self.inner.lock().unwrap();
        let diarize = guard.as_mut().ok_or("diarization not initialized")?;
        let segments = diarize
            .compute(samples, Some(callback))
            .map_err(|e| format!("diarization failed: {e}"))?;

        Ok(segments
            .into_iter()
            .map(|s| Turn {
                start_ms: (s.start.max(0.0) * 1000.0) as u64,
                end_ms: (s.end.max(0.0) * 1000.0) as u64,
                speaker: format!("SPEAKER_{:02}", s.speaker),
            })
            .collect())
    }
}
