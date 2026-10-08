//! Persona identity layer: match a query embedding against persona
//! voiceprint galleries (max cosine), classify confidence into tiers, and
//! orchestrate enrollment when a human confirms an identity.

use serde::Serialize;
use std::collections::HashMap;

use crate::db::Db;
use crate::diarize::Turn;
use crate::settings::AppSettings;
use crate::voiceprint::{cosine, SpeakerEmbedding, VoiceprintEngine};

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
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

/// Run embedding + matching for a meeting's diarized speakers and persist
/// suggestions into `speaker_persona_links`. Confirmed links are frozen —
/// `Db::upsert_link` never touches them — but a fresh `SpeakerMatch` is still
/// returned for every raw label that has a usable embedding. Callers are
/// responsible for `Db::reconcile_speakers` (dropping rows for labels that no
/// longer exist) before invoking this.
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
    // Keep each speaker's embedding with the meeting so a later confirm can
    // enroll it instantly (no audio pass) — and even after the audio is gone.
    for (raw_label, emb) in &embeddings {
        db.set_speaker_embedding(meeting_id, raw_label, &emb.vec, emb.speech_ms)?;
    }

    let personas = db.list_personas_with_voiceprints()?;
    // Snapshot links BEFORE upserting so `already_linked` reflects prior
    // identify runs (not the row we're about to write). Also collapses the
    // per-label `meeting_speaker_links` query into one upfront call.
    let prior_links: HashMap<String, crate::db::SpeakerLink> = db
        .meeting_speaker_links(meeting_id)?
        .into_iter()
        .map(|l| (l.raw_label.clone(), l))
        .collect();

    let mut out = Vec::with_capacity(embeddings.len());
    for (raw_label, emb) in &embeddings {
        let mut scores = rank_personas(&personas, &emb.vec);
        for s in scores.iter_mut() {
            s.tier = classify(
                s.score,
                settings.persona_auto_threshold,
                settings.persona_suggest_threshold,
            );
        }
        // Persist the top suggestion (if it clears `suggest`); else null link.
        let top = scores.first();
        let (pid, conf) = match top {
            Some(p) if p.tier != Tier::Unknown => (Some(p.persona_id), Some(p.score)),
            _ => (None, None),
        };
        let existing = prior_links.get(raw_label);
        db.upsert_link(meeting_id, raw_label, pid, conf)?;

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

/// What the speaker picker needs to rank personas by relevance instead of
/// alphabetically. Returned by `speaker_persona_candidates`.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerCandidates {
    /// raw label -> personas at/above the suggest threshold, best first.
    pub voice_matches: HashMap<String, Vec<PersonaScore>>,
    /// Name of the customer this meeting is assigned to, if any.
    pub customer_name: Option<String>,
    /// Personas confirmed in that customer's meetings (empty when unassigned).
    pub customer_roster: Vec<crate::db::CustomerRosterEntry>,
}

/// Rank every persona against each label's embedding, keeping only those
/// that clear the `suggest` threshold. Pure, so it's testable without a DB.
fn voice_matches(
    labels: &[(String, Vec<f32>)],
    personas: &[PersonaWithEmbeddings],
    auto: f32,
    suggest: f32,
) -> HashMap<String, Vec<PersonaScore>> {
    labels
        .iter()
        .map(|(raw_label, emb)| {
            let scores = rank_personas(personas, emb)
                .into_iter()
                .filter_map(|mut s| {
                    s.tier = classify(s.score, auto, suggest);
                    (s.tier != Tier::Unknown).then_some(s)
                })
                .collect();
            (raw_label.clone(), scores)
        })
        .collect()
}

/// Score a meeting's speakers against every persona from the embeddings
/// stored at identify time (no audio pass), plus the assigned customer's
/// roster. Cheap enough to re-run whenever galleries or the customer change.
pub fn speaker_candidates(
    db: &Db,
    meeting_id: i64,
    settings: &AppSettings,
) -> Result<SpeakerCandidates, String> {
    let labels = db.meeting_speaker_embeddings(meeting_id)?;
    let voice_matches = if labels.is_empty() {
        HashMap::new()
    } else {
        voice_matches(
            &labels,
            &db.list_personas_with_voiceprints()?,
            settings.persona_auto_threshold,
            settings.persona_suggest_threshold,
        )
    };
    let (customer_name, customer_roster) = match db.meeting_customer(meeting_id)? {
        Some((id, name)) => (Some(name), db.customer_roster(id)?),
        None => (None, Vec::new()),
    };
    Ok(SpeakerCandidates {
        voice_matches,
        customer_name,
        customer_roster,
    })
}

/// Fallback enrollment from audio, for a speaker with no embedding stored on
/// its `speakers` row (identified before embeddings were persisted). The
/// normal path is `Db::confirm_link`, which enrolls the stored embedding
/// synchronously. Re-derives the embedding from `system_wav_path` for the
/// given label's turns only. If the audio is gone, returns Ok(false) so the
/// caller can still leave the link confirmed without a voiceprint.
#[allow(clippy::too_many_arguments)]
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
    // Only this speaker's turns: embedding every label would multiply the
    // (already slow) CAM++ pass by the speaker count for nothing.
    let own: Vec<Turn> = turns
        .iter()
        .filter(|t| t.speaker == raw_label)
        .cloned()
        .collect();
    let embeddings = voiceprint.embed_speakers(app, path, &own)?;
    let emb: &SpeakerEmbedding = embeddings.get(raw_label).ok_or_else(|| {
        format!(
            "no embedding for {raw_label} (need >= {} ms speech)",
            crate::voiceprint::MIN_SPEECH_MS
        )
    })?;
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
            persona(3, "C", &[]),                         // skipped
        ];
        let ranked = rank_personas(&personas, &q);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].persona_id, 1);
        assert!((ranked[0].score - 1.0).abs() < 1e-6);
        assert_eq!(ranked[1].persona_id, 2);
    }

    #[test]
    fn voice_matches_drop_unknown_and_rank_best_first() {
        let personas = vec![
            persona(1, "A", &[&[0.6, 0.8]]), // 0.6 vs S1: suggest
            persona(2, "B", &[&[1.0, 0.0]]), // 1.0 vs S1: auto
            persona(3, "C", &[&[0.0, 1.0]]), // 0.0 vs S1: unknown
        ];
        let labels = vec![
            ("S1".to_string(), vec![1.0, 0.0]),
            ("S2".to_string(), vec![-1.0, 0.0]), // matches nobody
        ];
        let m = voice_matches(&labels, &personas, 0.65, 0.45);
        let s1: Vec<i64> = m["S1"].iter().map(|s| s.persona_id).collect();
        assert_eq!(s1, vec![2, 1]);
        assert_eq!(m["S1"][0].tier, Tier::Auto);
        assert_eq!(m["S1"][1].tier, Tier::Suggest);
        assert!(m["S2"].is_empty());
    }

    #[test]
    fn rank_empty_galleries_returns_empty() {
        let ranked = rank_personas(&[], &[1.0, 0.0]);
        assert!(ranked.is_empty());
    }
}
