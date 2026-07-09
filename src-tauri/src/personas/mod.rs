//! Persona identity layer: match a query embedding against persona
//! voiceprint galleries (max cosine), classify confidence into tiers, and
//! orchestrate enrollment when a human confirms an identity.

use serde::Serialize;

use crate::db::Db;
use crate::diarize::Turn;
use crate::settings::AppSettings;
use crate::voiceprint::{SpeakerEmbedding, VoiceprintEngine, cosine};

/// Confidence tier for a persona suggestion.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// >= auto threshold: strong suggestion, pre-filled.
    Auto,
    /// [suggest, auto): tentative, user must confirm.
    Suggest,
    /// < suggest: no match (caller should treat as unknown).
    Unknown,
}

/// One persona's match score against a query.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PersonaScore {
    pub persona_id: i64,
    pub display_name: String,
    pub score: f32,
    pub tier: Tier,
}

/// Per-`raw_label` match result returned by `identify_speakers`.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerMatch {
    pub raw_label: String,
    /// Ranked best-first; may be empty if no persona clears `suggest`.
    pub suggestions: Vec<PersonaScore>,
    /// The single best score (0.0 if none).
    pub best_score: f32,
    /// Whether a link already exists for this label (from a prior run).
    pub already_linked: bool,
    pub confirmed: bool,
}

/// A persona with its unpacked gallery embeddings — the input to
/// `rank_personas`. Populated by `Db::list_personas_with_voiceprints`.
pub struct PersonaWithEmbeddings {
    pub id: i64,
    pub display_name: String,
    pub embeddings: Vec<Vec<f32>>,
}

/// Classify a cosine score into a tier using the configured thresholds.
pub fn classify(score: f32, auto: f32, suggest: f32) -> Tier {
    if score >= auto {
        Tier::Auto
    } else if score >= suggest {
        Tier::Suggest
    } else {
        Tier::Unknown
    }
}

/// Rank personas against `query` by max cosine over their galleries.
/// Returns best-first. Personas with no voiceprints are skipped. Pure
/// function — takes the loaded galleries as a slice so it's testable
/// without a database.
pub fn rank_personas(personas: &[PersonaWithEmbeddings], query: &[f32]) -> Vec<PersonaScore> {
    let mut scored: Vec<PersonaScore> = personas
        .iter()
        .filter_map(|p| {
            let best = p
                .embeddings
                .iter()
                .map(|e| cosine(query, e))
                .fold(f32::NEG_INFINITY, f32::max);
            if best.is_finite() {
                Some(PersonaScore {
                    persona_id: p.id,
                    display_name: p.display_name.clone(),
                    score: best,
                    tier: Tier::Unknown, // caller stamps tiers after reading settings
                })
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

/// Run embedding + matching for a meeting's diarized speakers and persist
/// suggestions into `speaker_persona_links` (confirmed preserved). Returns
/// one `SpeakerMatch` per raw label that has a usable embedding.
pub fn identify_and_persist(
    db: &Db,
    voiceprint: &VoiceprintEngine,
    app: &tauri::AppHandle,
    meeting_id: i64,
    system_wav_path: &str,
    turns: &[Turn],
    settings: &AppSettings,
) -> Result<Vec<SpeakerMatch>, String> {
    let embeddings = voiceprint.embed_speakers(app, system_wav_path, turns)?;

    let mut out = Vec::with_capacity(embeddings.len());
    let personas = db.list_personas_with_voiceprints()?;
    for (raw_label, emb) in &embeddings {
        let mut scores = rank_personas(&personas, &emb.vec);
        for s in scores.iter_mut() {
            s.tier = classify(s.score, settings.persona_auto_threshold, settings.persona_suggest_threshold);
        }
        // Persist the top suggestion (if it clears `suggest`); else null link.
        let top = scores.first();
        let (pid, conf) = match top {
            Some(p) if p.tier != Tier::Unknown => (Some(p.persona_id), Some(p.score)),
            _ => (None, None),
        };
        db.upsert_link(meeting_id, raw_label, pid, conf)?;

        let existing = db
            .meeting_speaker_links(meeting_id)?
            .into_iter()
            .find(|l| l.raw_label == *raw_label);
        out.push(SpeakerMatch {
            raw_label: raw_label.clone(),
            best_score: top.map(|p| p.score).unwrap_or(0.0),
            suggestions: scores,
            already_linked: existing.is_some(),
            confirmed: existing.map(|l| l.confirmed).unwrap_or(false),
        });
    }
    Ok(out)
}

/// Enroll a speaker's embedding into a persona's gallery (called only on
/// human confirmation). Re-derives the embedding from `system_wav_path` for
/// the given label's turns. If the audio is gone, returns Ok(false) so the
/// caller can still mark the link confirmed without a voiceprint.
pub fn enroll(
    db: &Db,
    voiceprint: &VoiceprintEngine,
    app: &tauri::AppHandle,
    persona_id: i64,
    meeting_id: i64,
    raw_label: &str,
    system_wav_path: Option<&str>,
    turns: &[Turn],
    cap: i32,
) -> Result<bool, String> {
    let path = match system_wav_path {
        Some(p) => p,
        None => return Ok(false),
    };
    let embeddings = voiceprint.embed_speakers(app, path, turns)?;
    let emb: &SpeakerEmbedding = embeddings
        .get(raw_label)
        .ok_or_else(|| format!("no embedding for {raw_label} (need >= {} ms speech)", crate::voiceprint::MIN_SPEECH_MS))?;
    let dim = emb.vec.len() as i32;
    db.insert_voiceprint(
        persona_id,
        &emb.vec,
        dim,
        Some(meeting_id),
        Some(raw_label),
        emb.speech_ms,
        cap,
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona(id: i64, name: &str, embs: &[&[f32]]) -> PersonaWithEmbeddings {
        PersonaWithEmbeddings {
            id,
            display_name: name.into(),
            embeddings: embs.iter().map(|e| e.to_vec()).collect(),
        }
    }

    #[test]
    fn classify_auto() {
        assert_eq!(classify(0.7, 0.65, 0.45), Tier::Auto);
        assert_eq!(classify(0.65, 0.65, 0.45), Tier::Auto);
    }

    #[test]
    fn classify_suggest() {
        assert_eq!(classify(0.5, 0.65, 0.45), Tier::Suggest);
        assert_eq!(classify(0.45, 0.65, 0.45), Tier::Suggest);
    }

    #[test]
    fn classify_unknown() {
        assert_eq!(classify(0.44, 0.65, 0.45), Tier::Unknown);
        assert_eq!(classify(0.0, 0.65, 0.45), Tier::Unknown);
    }

    #[test]
    fn rank_best_first_max_over_gallery() {
        let q = vec![1.0, 0.0];
        let personas = vec![
            persona(1, "A", &[&[1.0, 0.0], &[0.0, 1.0]]), // max = 1.0
            persona(2, "B", &[&[0.707, 0.707]]),          // max ≈ 0.707
            persona(3, "C", &[]),                          // skipped
        ];
        let ranked = rank_personas(&personas, &q);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].persona_id, 1);
        assert!((ranked[0].score - 1.0).abs() < 1e-6);
        assert_eq!(ranked[1].persona_id, 2);
    }

    #[test]
    fn rank_empty_galleries_returns_empty() {
        let ranked = rank_personas(&[], &[1.0, 0.0]);
        assert!(ranked.is_empty());
    }
}