//! Transcript assembly: attach speaker labels to ASR segments.
//!
//! Mic segments are "Me" by construction. System segments get the diarized
//! speaker whose turn overlaps them the most; segments that overlap no turn
//! (e.g. speech the segmenter missed) fall back to the nearest turn by
//! midpoint distance, or stay unlabeled if there are no turns at all.
//!
//! Renaming (SPEAKER_00 → "Priya") is a per-meeting display mapping stored
//! with the meeting (SQLite, milestone 5). Raw labels are the key for
//! renames, persona links, and enrolled voiceprints, so they must be
//! **stable across re-diarization**: `remap_labels` matches a fresh
//! diarization run's clusters to the previous run's labels by time overlap
//! (the audio never changes) and reuses the old label for the same voice.
//! Only clusters that genuinely appear for the first time get a fresh label.

use std::collections::{HashMap, HashSet};

use crate::asr::Segment;
use crate::diarize::Turn;

/// Overlap in ms between [a0,a1) and [b0,b1).
fn overlap_ms(a0: u64, a1: u64, b0: u64, b1: u64) -> u64 {
    let start = a0.max(b0);
    let end = a1.min(b1);
    end.saturating_sub(start)
}

/// Prefix of diarizer-minted labels. Anything else in `Segment.speaker`
/// ("Me", a live-identified persona name) is not a diarization identity.
pub const RAW_LABEL_PREFIX: &str = "SPEAKER_";

/// A new cluster inherits an old label when their shared time covers at
/// least this fraction of the shorter of the two.
const REMAP_MIN_OVERLAP: f64 = 0.5;

/// Map each label in `new_turns` to the label it should carry so that a
/// voice keeps the label it had in `old_segments` (the meeting's previous
/// system-channel segments, labeled by the prior diarization run).
///
/// Greedy one-to-one assignment by descending shared time; a pair is
/// accepted when the overlap is >= `REMAP_MIN_OVERLAP` of the shorter side's
/// total speech. A merge (two old → one new) keeps the larger old label and
/// drops the other; a split (one old → two new) keeps the old label on the
/// larger half and mints a fresh label for the rest. An unmatched new
/// cluster keeps its minted label when the old run never used it; otherwise
/// it gets `SPEAKER_NN` with NN = 1 + the highest index in either run. Either
/// way a fresh label never equals an old one, so a vanished speaker's
/// rename/link/voiceprint (reconciled separately) can't re-attach to a
/// different voice.
///
/// Old segments whose label doesn't start with `SPEAKER_` are ignored. With
/// no old labels, the map is the identity.
pub fn remap_labels(old_segments: &[Segment], new_turns: &[Turn]) -> HashMap<String, String> {
    let old: Vec<&Segment> = old_segments
        .iter()
        .filter(|s| s.source == "system")
        .filter(|s| {
            s.speaker
                .as_deref()
                .is_some_and(|l| l.starts_with(RAW_LABEL_PREFIX))
        })
        .collect();

    let mut old_total: HashMap<&str, u64> = HashMap::new();
    for s in &old {
        *old_total.entry(s.speaker.as_deref().unwrap()).or_default() +=
            s.end_ms.saturating_sub(s.start_ms);
    }
    let mut new_total: HashMap<&str, u64> = HashMap::new();
    for t in new_turns {
        *new_total.entry(t.speaker.as_str()).or_default() += t.end_ms.saturating_sub(t.start_ms);
    }

    // Shared time per (old_label, new_label).
    let mut shared: HashMap<(&str, &str), u64> = HashMap::new();
    for s in &old {
        let ol = s.speaker.as_deref().unwrap();
        for t in new_turns {
            let ov = overlap_ms(s.start_ms, s.end_ms, t.start_ms, t.end_ms);
            if ov > 0 {
                *shared.entry((ol, t.speaker.as_str())).or_default() += ov;
            }
        }
    }

    // Greedy: biggest shared time first; deterministic tie-break on labels.
    let mut pairs: Vec<((&str, &str), u64)> = shared.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut map: HashMap<String, String> = HashMap::new();
    let mut used_old: HashSet<&str> = HashSet::new();
    for ((ol, nl), ov) in pairs {
        if used_old.contains(ol) || map.contains_key(nl) {
            continue;
        }
        let shorter = old_total[ol].min(new_total[nl]).max(1);
        if (ov as f64) / (shorter as f64) >= REMAP_MIN_OVERLAP {
            used_old.insert(ol);
            map.insert(nl.to_string(), ol.to_string());
        }
    }

    // Fresh labels for unmatched new clusters, never colliding with any
    // label from either run.
    let mut next = old_total
        .keys()
        .chain(new_total.keys())
        .filter_map(|l| label_index(l))
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);
    let mut new_labels: Vec<&str> = new_total.keys().copied().collect();
    new_labels.sort();
    for nl in new_labels {
        if map.contains_key(nl) {
            continue;
        }
        if !old_total.contains_key(nl) {
            // The minted label is unused by the old run: keep it as-is.
            map.insert(nl.to_string(), nl.to_string());
        } else {
            map.insert(nl.to_string(), format!("{RAW_LABEL_PREFIX}{next:02}"));
            next += 1;
        }
    }
    map
}

/// Numeric index of a `SPEAKER_NN` label, if it has one.
fn label_index(label: &str) -> Option<u32> {
    label.strip_prefix(RAW_LABEL_PREFIX)?.parse().ok()
}

/// Rewrite each turn's label through `map` in place (labels absent from the
/// map are left untouched).
pub fn relabel_turns(turns: &mut [Turn], map: &HashMap<String, String>) {
    for t in turns.iter_mut() {
        if let Some(l) = map.get(&t.speaker) {
            t.speaker = l.clone();
        }
    }
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

    // ---- remap_labels ----

    fn lseg(start_ms: u64, end_ms: u64, speaker: &str) -> Segment {
        let mut s = seg("system", start_ms, end_ms);
        s.speaker = Some(speaker.into());
        s
    }

    fn m(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn remap_identity_when_clusters_unchanged() {
        let old = vec![
            lseg(0, 10_000, "SPEAKER_00"),
            lseg(10_000, 20_000, "SPEAKER_01"),
        ];
        let new = vec![
            turn(0, 10_000, "SPEAKER_00"),
            turn(10_000, 20_000, "SPEAKER_01"),
        ];
        assert_eq!(
            remap_labels(&old, &new),
            m(&[("SPEAKER_00", "SPEAKER_00"), ("SPEAKER_01", "SPEAKER_01")])
        );
    }

    #[test]
    fn remap_swaps_permuted_labels() {
        let old = vec![
            lseg(0, 10_000, "SPEAKER_00"),
            lseg(10_000, 20_000, "SPEAKER_01"),
        ];
        let new = vec![
            turn(0, 10_000, "SPEAKER_01"),
            turn(10_000, 20_000, "SPEAKER_00"),
        ];
        assert_eq!(
            remap_labels(&old, &new),
            m(&[("SPEAKER_01", "SPEAKER_00"), ("SPEAKER_00", "SPEAKER_01")])
        );
    }

    #[test]
    fn remap_tolerates_boundary_shifts() {
        // ASR segments vs. diarizer turns rarely line up exactly.
        let old = vec![
            lseg(200, 9_800, "SPEAKER_00"),
            lseg(10_300, 19_500, "SPEAKER_01"),
        ];
        let new = vec![
            turn(0, 10_000, "SPEAKER_01"),
            turn(10_000, 20_000, "SPEAKER_00"),
        ];
        assert_eq!(
            remap_labels(&old, &new),
            m(&[("SPEAKER_01", "SPEAKER_00"), ("SPEAKER_00", "SPEAKER_01")])
        );
    }

    #[test]
    fn remap_split_keeps_old_label_on_larger_half_and_mints_fresh() {
        // One old voice, now split in two. The 0..7s half wins SPEAKER_00;
        // the other half must NOT reuse SPEAKER_01 (an old label) — it
        // gets a fresh index past both runs.
        let old = vec![
            lseg(0, 10_000, "SPEAKER_00"),
            lseg(10_000, 20_000, "SPEAKER_01"),
        ];
        let new = vec![
            turn(0, 7_000, "SPEAKER_00"),
            turn(7_000, 10_000, "SPEAKER_01"),
            turn(10_000, 20_000, "SPEAKER_02"),
        ];
        assert_eq!(
            remap_labels(&old, &new),
            m(&[
                ("SPEAKER_00", "SPEAKER_00"),
                ("SPEAKER_02", "SPEAKER_01"),
                ("SPEAKER_01", "SPEAKER_03"),
            ])
        );
    }

    #[test]
    fn remap_merge_keeps_larger_old_label() {
        let old = vec![
            lseg(0, 10_000, "SPEAKER_00"),
            lseg(10_000, 14_000, "SPEAKER_01"),
            lseg(14_000, 20_000, "SPEAKER_02"),
        ];
        // 00 and 01 merged into one cluster; 02 unchanged but relabeled.
        let new = vec![
            turn(0, 14_000, "SPEAKER_00"),
            turn(14_000, 20_000, "SPEAKER_01"),
        ];
        assert_eq!(
            remap_labels(&old, &new),
            m(&[("SPEAKER_00", "SPEAKER_00"), ("SPEAKER_01", "SPEAKER_02")])
        );
    }

    #[test]
    fn remap_unmatched_new_cluster_keeps_unused_minted_label() {
        let old = vec![lseg(0, 10_000, "SPEAKER_00")];
        let new = vec![
            turn(0, 10_000, "SPEAKER_00"),
            turn(10_000, 20_000, "SPEAKER_01"),
        ];
        assert_eq!(
            remap_labels(&old, &new),
            m(&[("SPEAKER_00", "SPEAKER_00"), ("SPEAKER_01", "SPEAKER_01")])
        );
    }

    #[test]
    fn remap_is_identity_without_old_labels() {
        // First diarization after a live recording: system segments carry a
        // persona display name or nothing — neither is an old identity.
        let mut named = seg("system", 0, 10_000);
        named.speaker = Some("Priya".into());
        let old = vec![named, seg("system", 10_000, 20_000)];
        let new = vec![
            turn(0, 10_000, "SPEAKER_00"),
            turn(10_000, 20_000, "SPEAKER_01"),
        ];
        assert_eq!(
            remap_labels(&old, &new),
            m(&[("SPEAKER_00", "SPEAKER_00"), ("SPEAKER_01", "SPEAKER_01")])
        );
    }

    #[test]
    fn remap_below_threshold_is_not_a_match() {
        // Old voice spoke 0..10s; the new cluster covers 8..20s — only 2s
        // shared (20% of the shorter side). Not the same voice: the old
        // label vanishes and the new cluster can't reuse SPEAKER_00.
        let old = vec![lseg(0, 10_000, "SPEAKER_00")];
        let new = vec![turn(8_000, 20_000, "SPEAKER_00")];
        assert_eq!(remap_labels(&old, &new), m(&[("SPEAKER_00", "SPEAKER_01")]));
    }

    #[test]
    fn remap_one_to_one_prefers_largest_overlap() {
        let old = vec![
            lseg(0, 2_000, "SPEAKER_00"),
            lseg(2_000, 10_000, "SPEAKER_01"),
        ];
        let new = vec![turn(0, 10_000, "SPEAKER_00")];
        // 01 claims the merged cluster (8s shared); 00 vanishes.
        assert_eq!(remap_labels(&old, &new), m(&[("SPEAKER_00", "SPEAKER_01")]));
    }

    #[test]
    fn relabel_turns_applies_map() {
        let mut turns = vec![turn(0, 1, "SPEAKER_00"), turn(1, 2, "SPEAKER_01")];
        relabel_turns(&mut turns, &m(&[("SPEAKER_00", "SPEAKER_05")]));
        assert_eq!(turns[0].speaker, "SPEAKER_05");
        assert_eq!(turns[1].speaker, "SPEAKER_01");
    }
}
