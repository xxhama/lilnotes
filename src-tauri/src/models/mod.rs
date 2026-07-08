//! ML model registry + downloader (Whisper ggml models in M3; the
//! diarization ONNX models reuse the same downloader in M4).
//!
//! Models are cached under `<app data>/models/`. Downloads stream to a
//! `.part` file with progress events, support cancellation, and are renamed
//! into place only when complete.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// A downloadable Whisper model (ggml format, from the whisper.cpp HF repo).
pub struct WhisperModel {
    pub id: &'static str,
    pub label: &'static str,
    pub filename: &'static str,
    pub url: &'static str,
    /// Approximate download size in bytes (display only; live size comes
    /// from the HTTP response).
    pub approx_bytes: u64,
    pub note: &'static str,
}

pub const WHISPER_MODELS: &[WhisperModel] = &[
    WhisperModel {
        id: "large-v3-turbo",
        label: "Large v3 Turbo",
        filename: "ggml-large-v3-turbo.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        approx_bytes: 1_620_000_000,
        note: "Recommended: near large-v3 accuracy, much faster — best for live transcription.",
    },
    WhisperModel {
        id: "large-v3",
        label: "Large v3",
        filename: "ggml-large-v3.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        approx_bytes: 3_100_000_000,
        note: "Highest accuracy; slower — may lag in live mode.",
    },
    WhisperModel {
        id: "medium",
        label: "Medium",
        filename: "ggml-medium.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        approx_bytes: 1_530_000_000,
        note: "Lighter; noticeably weaker on accents and crosstalk.",
    },
    WhisperModel {
        id: "small",
        label: "Small",
        filename: "ggml-small.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        approx_bytes: 488_000_000,
        note: "Fast and compact; fine for quick notes in quiet audio.",
    },
];

pub fn whisper_model(id: &str) -> Option<&'static WhisperModel> {
    WHISPER_MODELS.iter().find(|m| m.id == id)
}

pub fn models_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?
        .join("models");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub fn whisper_model_path(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    let model = whisper_model(id).ok_or_else(|| format!("unknown model: {id}"))?;
    Ok(models_dir(app)?.join(model.filename))
}

// ---------------------------------------------------------------------------
// Download manager
// ---------------------------------------------------------------------------

/// Payload of the `model:progress` event.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub id: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct DownloadManager {
    cancel_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl DownloadManager {
    pub fn begin(&self, id: &str) -> Result<Arc<AtomicBool>, String> {
        let mut flags = self.cancel_flags.lock().unwrap();
        if flags.contains_key(id) {
            return Err(format!("{id} is already downloading"));
        }
        let flag = Arc::new(AtomicBool::new(false));
        flags.insert(id.to_string(), flag.clone());
        Ok(flag)
    }

    pub fn finish(&self, id: &str) {
        self.cancel_flags.lock().unwrap().remove(id);
    }

    pub fn cancel(&self, id: &str) -> bool {
        match self.cancel_flags.lock().unwrap().get(id) {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }
}

/// Blocking streamed download with progress events and cancellation.
/// Emits `model:progress` events keyed by `id`.
pub fn download_with_progress(
    app: &AppHandle,
    id: &str,
    url: &str,
    dest: &PathBuf,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let emit = |downloaded: u64, total: Option<u64>, done: bool, error: Option<String>| {
        let _ = app.emit(
            "model:progress",
            DownloadProgress {
                id: id.to_string(),
                downloaded,
                total,
                done,
                error,
            },
        );
    };

    let result = (|| -> Result<(), String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(None) // large files; rely on read timeouts from the OS
            .build()
            .map_err(|e| e.to_string())?;
        let mut resp = client
            .get(url)
            .send()
            .map_err(|e| format!("download failed to start: {e}"))?
            .error_for_status()
            .map_err(|e| format!("download failed: {e}"))?;
        let total = resp.content_length();

        let part = dest.with_extension("part");
        let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 1024 * 1024];
        let mut downloaded: u64 = 0;
        let mut last_emit = Instant::now();

        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err("cancelled".into());
            }
            let n = resp.read(&mut buf).map_err(|e| format!("read error: {e}"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            downloaded += n as u64;
            if last_emit.elapsed() > Duration::from_millis(250) {
                emit(downloaded, total, false, None);
                last_emit = Instant::now();
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);

        if let Some(t) = total {
            if downloaded < t {
                let _ = std::fs::remove_file(&part);
                return Err(format!("download incomplete ({downloaded}/{t} bytes)"));
            }
        }
        std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
        emit(downloaded, total, true, None);
        Ok(())
    })();

    if let Err(e) = &result {
        emit(0, None, true, Some(e.clone()));
    }
    result
}
