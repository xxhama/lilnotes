//! Transcript assembly: attach speaker labels to ASR segments.
//!
//! Mic segments are "Me" by construction. System segments get the diarized
//! speaker whose turn overlaps them the most; segments that overlap no turn
//! (e.g. speech the segmenter missed) fall back to the nearest turn by
//! midpoint distance, or stay unlabeled if there are no turns at all.
//!
//! Renaming (SPEAKER_00 → "Priya") is a per-meeting display mapping stored
//! with the meeting (SQLite, milestone 5); raw labels are never rewritten.

use crate::asr::Segment;
use crate::diarize::Turn;

/// Overlap in ms between [a0,a1) and [b0,b1).
fn overlap_ms(a0: u64, a1: u64, b0: u64, b1: u64) -> u64 {
    let start = a0.max(b0);
    let end = a1.min(b1);
    end.saturating_sub(start)
}

/// Assign speakers in place. Returns the number of distinct system speakers.
pub fn assign_speakers(segments: &mut [Segment], turns: &[Turn]) -> usize {
    let mut speakers = std::collections::BTreeSet::new();

    for seg in segments.iter_mut() {
        if seg.source == "mic" {
            seg.speaker = Some("Me".into());
            continue;
        }

        // Best overlap wins.
        let mut best: Option<(&Turn, u64)> = None;
        for turn in turns {
            let ov = overlap_ms(seg.start_ms, seg.end_ms, turn.start_ms, turn.end_ms);
            if ov > 0 && best.map(|(_, b)| ov > b).unwrap_or(true) {
                best = Some((turn, ov));
            }
        }

        // Fall back to the nearest turn (by midpoint distance).
        let chosen = best.map(|(t, _)| t).or_else(|| {
            let mid = (seg.start_ms + seg.end_ms) / 2;
            turns.iter().min_by_key(|t| {
                let tmid = (t.start_ms + t.end_ms) / 2;
                tmid.abs_diff(mid)
            })
        });

        if let Some(turn) = chosen {
            speakers.insert(turn.speaker.clone());
            seg.speaker = Some(turn.speaker.clone());
        }
    }
    speakers.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(source: &str, start_ms: u64, end_ms: u64) -> Segment {
        Segment {
            source: source.into(),
            start_ms,
            end_ms,
            text: "x".into(),
            speaker: None,
            ..Default::default()
        }
    }

    fn turn(start_ms: u64, end_ms: u64, speaker: &str) -> Turn {
        Turn {
            start_ms,
            end_ms,
            speaker: speaker.into(),
        }
    }

    #[test]
    fn mic_is_always_me() {
        let mut segs = vec![seg("mic", 0, 1000)];
        assign_speakers(&mut segs, &[]);
        assert_eq!(segs[0].speaker.as_deref(), Some("Me"));
    }

    #[test]
    fn best_overlap_wins() {
        let mut segs = vec![seg("system", 1000, 4000)];
        let turns = vec![turn(0, 1500, "SPEAKER_00"), turn(1500, 5000, "SPEAKER_01")];
        assign_speakers(&mut segs, &turns);
        assert_eq!(segs[0].speaker.as_deref(), Some("SPEAKER_01"));
    }

    #[test]
    fn no_overlap_falls_back_to_nearest() {
        let mut segs = vec![seg("system", 10_000, 11_000)];
        let turns = vec![
            turn(0, 2000, "SPEAKER_00"),
            turn(12_000, 15_000, "SPEAKER_01"),
        ];
        assign_speakers(&mut segs, &turns);
        assert_eq!(segs[0].speaker.as_deref(), Some("SPEAKER_01"));
    }

    #[test]
    fn no_turns_leaves_unlabeled() {
        let mut segs = vec![seg("system", 0, 1000)];
        let n = assign_speakers(&mut segs, &[]);
        assert_eq!(n, 0);
        assert_eq!(segs[0].speaker, None);
    }

    #[test]
    fn counts_distinct_speakers() {
        let mut segs = vec![seg("system", 0, 1000), seg("system", 2000, 3000)];
        let turns = vec![turn(0, 1000, "SPEAKER_00"), turn(2000, 3000, "SPEAKER_01")];
        assert_eq!(assign_speakers(&mut segs, &turns), 2);
    }
}
