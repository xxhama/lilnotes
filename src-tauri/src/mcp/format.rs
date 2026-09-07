//! Pure text/shape helpers for the MCP tools: speaker-name resolution,
//! transcript rendering, participant rollups, and size capping. No I/O, so
//! everything here is unit-tested directly.

use std::collections::HashMap;

use serde::Serialize;

use crate::asr::Segment;
use crate::db::{MeetingDetail, SpeakerLink};

/// One distinct speaker in a meeting, with the name an agent should use.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    /// Raw diarization label ("Me" for the mic channel, "SPEAKER_00", …).
    pub raw_label: String,
    /// Resolved name: persona name > per-meeting rename > raw label.
    pub display_name: String,
    pub persona_id: Option<i64>,
    pub persona_name: Option<String>,
    /// True when the user confirmed the persona link for this meeting.
    pub confirmed: bool,
    pub segment_count: i64,
}

/// The raw label for a segment: the stored speaker, else "Me" for the mic
/// channel and "Speaker" for a not-yet-diarized system segment.
pub fn raw_label(seg: &Segment) -> String {
    seg.speaker.clone().unwrap_or_else(|| {
        if seg.source == "mic" {
            "Me".into()
        } else {
            "Speaker".into()
        }
    })
}

/// Display name for a raw label: linked persona name (suggested or
/// confirmed) > per-meeting rename > the raw label itself.
///
/// This deliberately differs from `summary::transcript_text`, which only
/// applies renames: agents want the identity the UI shows, and the UI shows
/// the persona name whenever a link exists.
pub fn resolve_label(
    raw: &str,
    renames: &HashMap<String, String>,
    links: &HashMap<String, SpeakerLink>,
) -> String {
    if let Some(name) = links.get(raw).and_then(|l| l.persona_name.clone()) {
        return name;
    }
    if let Some(name) = renames.get(raw) {
        return name.clone();
    }
    raw.to_string()
}

pub fn resolve_speaker_name(
    seg: &Segment,
    renames: &HashMap<String, String>,
    links: &HashMap<String, SpeakerLink>,
) -> String {
    resolve_label(&raw_label(seg), renames, links)
}

/// `mm:ss`, zero-padded, minutes unbounded (`75:03`).
pub fn mmss(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// `[mm:ss] Name: text\n` per segment, names resolved via `resolve_label`.
pub fn render_transcript(
    segments: &[Segment],
    renames: &HashMap<String, String>,
    links: &HashMap<String, SpeakerLink>,
) -> String {
    let mut out = String::new();
    for s in segments {
        out.push_str(&format!(
            "[{}] {}: {}\n",
            mmss(s.start_ms),
            resolve_speaker_name(s, renames, links),
            s.text
        ));
    }
    out
}

/// Distinct speakers of a meeting in first-appearance order.
pub fn participants(detail: &MeetingDetail) -> Vec<Participant> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, i64> = HashMap::new();
    for s in &detail.segments {
        let raw = raw_label(s);
        if !counts.contains_key(&raw) {
            order.push(raw.clone());
        }
        *counts.entry(raw).or_insert(0) += 1;
    }
    order
        .into_iter()
        .map(|raw| {
            let link = detail.speaker_links.get(&raw);
            Participant {
                display_name: resolve_label(&raw, &detail.renames, &detail.speaker_links),
                persona_id: link.and_then(|l| l.persona_id),
                persona_name: link.and_then(|l| l.persona_name.clone()),
                confirmed: link.map(|l| l.confirmed).unwrap_or(false),
                segment_count: counts.get(&raw).copied().unwrap_or(0),
                raw_label: raw,
            }
        })
        .collect()
}

/// Cap a string at `max` chars (not bytes — never splits a code point).
/// Returns the (possibly shortened) string and whether it was cut.
pub fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((idx, _)) => (s[..idx].to_string(), true),
        None => (s.to_string(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(source: &str, speaker: Option<&str>, start_ms: u64, text: &str) -> Segment {
        Segment {
            id: 0,
            source: source.into(),
            start_ms,
            end_ms: start_ms + 1000,
            text: text.into(),
            speaker: speaker.map(Into::into),
            kind: "speech".into(),
            deleted: false,
        }
    }

    fn link(raw: &str, persona: Option<&str>, confirmed: bool) -> (String, SpeakerLink) {
        (
            raw.to_string(),
            SpeakerLink {
                raw_label: raw.into(),
                persona_id: persona.map(|_| 7),
                persona_name: persona.map(Into::into),
                confidence: Some(0.9),
                confirmed,
            },
        )
    }

    #[test]
    fn name_precedence_persona_over_rename_over_raw() {
        let renames: HashMap<String, String> =
            [("SPEAKER_00".to_string(), "Bob".to_string())].into();
        let links: HashMap<String, SpeakerLink> = [link("SPEAKER_00", Some("Alice"), false)].into();
        let s = seg("system", Some("SPEAKER_00"), 0, "hi");
        assert_eq!(resolve_speaker_name(&s, &renames, &links), "Alice");
        // Link without persona name → rename wins.
        let links2: HashMap<String, SpeakerLink> = [link("SPEAKER_00", None, false)].into();
        assert_eq!(resolve_speaker_name(&s, &renames, &links2), "Bob");
        // Nothing → raw.
        assert_eq!(
            resolve_speaker_name(&s, &HashMap::new(), &HashMap::new()),
            "SPEAKER_00"
        );
    }

    #[test]
    fn fallbacks_for_unlabeled_segments() {
        let mic = seg("mic", None, 0, "a");
        let sys = seg("system", None, 0, "b");
        assert_eq!(raw_label(&mic), "Me");
        assert_eq!(raw_label(&sys), "Speaker");
    }

    #[test]
    fn mmss_formats() {
        assert_eq!(mmss(0), "0:00");
        assert_eq!(mmss(59_999), "0:59");
        assert_eq!(mmss(61_000), "1:01");
        assert_eq!(mmss(3_600_000), "60:00");
    }

    #[test]
    fn renders_transcript_lines() {
        let links: HashMap<String, SpeakerLink> = [link("SPEAKER_00", Some("Alice"), true)].into();
        let segs = vec![
            seg("mic", Some("Me"), 0, "Hello"),
            seg("system", Some("SPEAKER_00"), 65_000, "Hi there"),
        ];
        let text = render_transcript(&segs, &HashMap::new(), &links);
        assert_eq!(text, "[0:00] Me: Hello\n[1:05] Alice: Hi there\n");
    }

    #[test]
    fn participants_are_distinct_in_order_with_counts() {
        let detail = MeetingDetail {
            id: 1,
            session_id: "s".into(),
            title: "t".into(),
            started_at_ms: 0,
            ended_at_ms: Some(1),
            mic_wav: None,
            system_wav: None,
            mic_cleaned_wav: None,
            notes: None,
            notes_updated_at_ms: None,
            segments: vec![
                seg("system", Some("SPEAKER_01"), 0, "x"),
                seg("mic", Some("Me"), 1, "y"),
                seg("system", Some("SPEAKER_01"), 2, "z"),
            ],
            renames: [("SPEAKER_01".to_string(), "Carol".to_string())].into(),
            speaker_links: HashMap::new(),
            speaker_count: 2,
            customer_id: None,
            hidden_segment_count: 0,
            asr_model: None,
        };
        let p = participants(&detail);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].raw_label, "SPEAKER_01");
        assert_eq!(p[0].display_name, "Carol");
        assert_eq!(p[0].segment_count, 2);
        assert!(!p[0].confirmed);
        assert_eq!(p[1].raw_label, "Me");
        assert_eq!(p[1].segment_count, 1);
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let (s, cut) = truncate_chars("héllo wörld", 5);
        assert_eq!(s, "héllo");
        assert!(cut);
        let (s, cut) = truncate_chars("abc", 10);
        assert_eq!(s, "abc");
        assert!(!cut);
    }
}
