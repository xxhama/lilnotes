//! Standalone CAM++ speaker-embedding extraction for voiceprint enrollment
//! and matching. Reuses the diarization embedding model
//! (`models::diarize_model_paths(app).1`); embeddings are L2-normalized so
//! cosine similarity is a plain dot product.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::AppHandle;

use crate::diarize::Turn;
use crate::models;

/// One representative embedding for a diarized speaker in a meeting.
#[derive(Serialize, Clone, Debug)]
pub struct SpeakerEmbedding {
    /// L2-normalized CAM++ vector.
    pub vec: Vec<f32>,
    /// Total speech duration used to compute it (ms).
    pub speech_ms: u64,
}

/// Wraps a sherpa-rs `EmbeddingExtractor`, loaded once with the CAM++ model.
#[derive(Default)]
pub struct VoiceprintEngine {
    inner: Mutex<Option<sherpa_rs::speaker_id::EmbeddingExtractor>>,
}

/// Pack f32 slice into little-endian bytes for SQLite BLOB storage.
pub fn pack_f32(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Unpack little-endian bytes back into f32.
pub fn unpack_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// L2-normalize in place; returns the original Vec for convenience.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity for L2-normalized vectors == dot product. Returns 0.0
/// for mismatched lengths (shouldn't happen with one model).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Minimum speech (ms) to produce a reliable voiceprint.
pub const MIN_SPEECH_MS: u64 = 3000;

impl VoiceprintEngine {
    fn ensure_loaded(&self, app: &AppHandle) -> Result<(), String> {
        let mut guard = self.inner.lock().unwrap();
        if guard.is_some() {
            return Ok(());
        }
        let (_, emb) = models::diarize_model_paths(app)?;
        if !emb.exists() {
            // Download with a non-cancellable flag (small file, likely cached).
            let cancel = std::sync::atomic::AtomicBool::new(false);
            models::ensure_diarize_models(app, &cancel)?;
        }
        let config = sherpa_rs::speaker_id::ExtractorConfig {
            model: emb.to_string_lossy().to_string(),
            ..Default::default()
        };
        let extractor = sherpa_rs::speaker_id::EmbeddingExtractor::new(config)
            .map_err(|e| format!("failed to initialize voiceprint extractor: {e}"))?;
        *guard = Some(extractor);
        Ok(())
    }

    /// Compute one normalized embedding per diarized speaker on the system
    /// channel. Speakers with < `MIN_SPEECH_MS` of speech are omitted (their
    /// voiceprints are unreliable). `turns` must be the diarized turns for
    /// `system_wav_path`.
    pub fn embed_speakers(
        &self,
        app: &AppHandle,
        system_wav_path: &str,
        turns: &[Turn],
    ) -> Result<HashMap<String, SpeakerEmbedding>, String> {
        self.ensure_loaded(app)?;

        let mut reader = hound::WavReader::open(system_wav_path)
            .map_err(|e| format!("cannot open {system_wav_path}: {e}"))?;
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("cannot read {system_wav_path}: {e}"))?;
        let sample_rate = reader.spec().sample_rate as usize;

        // Group turns by speaker and concatenate their samples.
        let mut by_speaker: HashMap<&str, (Vec<f32>, u64)> = HashMap::new();
        for t in turns {
            let (buf, ms) = by_speaker.entry(t.speaker.as_str()).or_default();
            let start = ((t.start_ms as usize) * sample_rate / 1000).min(samples.len());
            let end = ((t.end_ms as usize) * sample_rate / 1000).min(samples.len());
            if end > start {
                buf.extend_from_slice(&samples[start..end]);
            }
            *ms += t.end_ms - t.start_ms;
        }

        let mut guard = self.inner.lock().unwrap();
        let extractor = guard.as_mut().ok_or("voiceprint extractor not initialized")?;

        let mut out = HashMap::new();
        for (speaker, (buf, speech_ms)) in by_speaker {
            if speech_ms < MIN_SPEECH_MS || buf.is_empty() {
                continue; // skip unreliable short utterances
            }
            let mut emb = extractor
                .compute_speaker_embedding(buf, sample_rate as u32)
                .map_err(|e| format!("embedding failed for {speaker}: {e}"))?;
            l2_normalize(&mut emb);
            out.insert(
                speaker.to_string(),
                SpeakerEmbedding {
                    vec: emb,
                    speech_ms,
                },
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        let v = vec![0.1, -0.2, 0.3, 1.5, -1234.5];
        let packed = pack_f32(&v);
        assert_eq!(packed.len(), v.len() * 4);
        let back = unpack_f32(&packed);
        for (a, b) in v.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![0.6, 0.8, 0.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_length_mismatch_is_zero() {
        assert_eq!(cosine(&[1.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn l2_normalize_unit_length() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_is_noop() {
        let mut v = vec![0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0]);
    }
}