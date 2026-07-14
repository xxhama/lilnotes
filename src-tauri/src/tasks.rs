//! AI task extraction from a meeting's **summary**.
//!
//! The candidate tasks come from the summary's `## My action items` block
//! (produced by `summary::DEFAULT_TEMPLATE`). Only the note-taker's own
//! action items become task suggestions; `## Action items (others)` stays in
//! the summary as reference and is deliberately not parsed. The summary is
//! written to **read naturally** — plain action-item bullets with no mandated
//! `[priority]` tags or `due YYYY-MM-DD` clauses — so the deterministic parse
//! only extracts a clean **title** per bullet.
//!
//! **Priority and due date are suggested by the extraction LLM**, not parsed
//! from the summary. One best-effort LLM pass (the "enrichment" prompt) is
//! given the parsed action-item titles + the full transcript and returns one
//! element per item: a `priority` (`low`/`normal`/`high`), an optional
//! `dueDate` (`YYYY-MM-DD` — relative deadlines like "next week" are
//! converted to a concrete date from `{today}`; only unanchored terms like
//! "soon"/"ASAP" are omitted), a transcript anchor (`startMs`/`endMs`), and a
//! supporting `snippet`. The model gets a `{today}` calendar anchor so it can
//! tell future from past (the transcript only has `[mm:ss]` offsets). Due
//! dates the model returns that are in the past are dropped at enrichment-
//! parse time, so a freshly-extracted suggestion never surfaces as overdue
//! before the user acts on it.
//!
//! This is **best-effort, non-fatal**. The Markdown parse never fails (at
//! worst, zero items). Enrichment failure (sidecar off, bad JSON) is silent:
//! we still insert the parsed items as title-only suggestions (priority
//! defaults to `normal`, no due date, no transcript anchor). Extraction never
//! touches the transcript or summary; its worst case is "0 suggestions".
//!
//! Dedupe is exact-normalized by `(meeting_id, lowercase-trimmed title)` for
//! suggested/open/done tasks (enforced in `db::insert_suggestions`), so
//! re-running extraction never re-suggests something already accepted,
//! completed, or still pending. A **dismissed** suggestion is an exception:
//! re-extracting its title revives it as a suggestion (dismiss = "hide for
//! now," not "never again"), so only genuinely new or previously-dismissed
//! items appear.

use std::collections::HashMap;

use chrono::{Local, NaiveDate, TimeZone};
use serde_json::Value;

use crate::db::TaskSuggestion;

/// Prompt template for the extraction/enrichment LLM pass. Given the parsed
/// action-item titles + the full transcript, the model suggests a priority
/// and due date per item (looking at the items + the transcript), plus a
/// transcript anchor and a supporting quote. `{title}`, `{transcript}`,
/// `{items}` (a numbered list), and `{today}` are substituted at run time.
/// Asks for a JSON array only — no prose, no fences — but the parser
/// tolerates fences anyway. `dueDate` is a `YYYY-MM-DD` string (not epoch ms)
/// because LLMs are unreliable at epoch-ms arithmetic and the transcript has
/// no calendar anchor.
///
/// Guarantees the prompt enforces:
/// (1) return exactly one element per input index (1..N) — no skipping or
/// merging, so every split item ("write the PO and schedule the workshop by
/// next week" → two bullets) gets its own priority/due;
/// (2) convert relative deadlines ("next week", "end of week", "this Friday")
/// to a concrete `YYYY-MM-DD` from `{today}` instead of dropping them;
/// (3) always provide `startMs` (the transcript offset where the action was
/// discussed) so every extracted task can link to the transcript — only omit
/// it in the rare case the action is never mentioned.
pub const ENRICHMENT_TEMPLATE: &str = r#"You are an expert meeting assistant. Below is the transcript of a meeting ("{title}"), with [mm:ss] timestamps and speaker names, followed by a numbered list of the note-taker's action items extracted from the meeting's summary. Today is {today}.

Return exactly one array element for EACH action item below — every index from 1 to N, in order. Do not skip, merge, or combine indices: if a commitment was split into separate items, each split item gets its own element. Each element MUST include "priority" and "startMs".

For each action item, decide:
- priority: "low" | "normal" | "high". Use "high" sparingly — only for genuinely urgent items; default to "normal"; use "low" for nice-to-haves.
- dueDate: a "YYYY-MM-DD" string (never before today) when a deadline was mentioned. Compute a concrete date from today ({today}) for relative deadlines — e.g. "next week"/"end of week"/"this week" → the upcoming Friday, "this Friday"/"next Monday" → that date. If a deadline applied to a commitment that was split into several items, give each split item that deadline unless the speaker tied it to a specific action. Omit dueDate only for unanchored terms ("soon", "ASAP", "end of quarter") or when no deadline was mentioned.
- startMs: the transcript timestamp (milliseconds from the start) where the action item was discussed, computed from the [mm:ss] offsets. ALWAYS provide startMs — the task links to the transcript at this offset, so pick the best moment (the clearest mention, or the start of the discussion if it spans a range). endMs: the end of that discussion span if it covers a range; otherwise omit. Only omit startMs if the action is genuinely never mentioned in the transcript (rare).
- snippet: a short supporting quote from the transcript. Omit if none.

Respond with ONLY a JSON array (no prose, no markdown fences).

Each element: { "index": <1-based>, "priority": "low"|"normal"|"high", "dueDate": "YYYY-MM-DD" (optional), "startMs": <ms> (optional), "endMs": <ms> (optional), "snippet": "short quoted line from the transcript" (optional) }

Action items:
{items}

Transcript:

{transcript}"#;

pub fn build_enrichment_prompt(
    title: &str,
    transcript: &str,
    items: &[ParsedActionItem],
) -> String {
    let list = items
        .iter()
        .enumerate()
        .map(|(i, it)| format!("{}. {}", i + 1, it.title))
        .collect::<Vec<_>>()
        .join("\n");
    ENRICHMENT_TEMPLATE
        .replace("{title}", title)
        .replace("{transcript}", transcript)
        .replace("{items}", &list)
        .replace("{today}", &Local::now().format("%Y-%m-%d").to_string())
}

/// A parsed action item from the summary: just the cleaned bullet title.
/// Priority and due date are NOT parsed from the summary (it reads
/// naturally); the extraction LLM suggests them (see `Enrichment`).
#[derive(Clone)]
pub struct ParsedActionItem {
    pub title: String,
}

/// LLM enrichment for one action item: the suggested priority and due date,
/// plus the transcript anchor (timestamps) and a supporting quote. All
/// optional except priority, which defaults to `normal` when absent.
#[derive(Clone)]
pub struct Enrichment {
    pub priority: Option<String>,
    pub due_at: Option<i64>,
    pub source_start_ms: Option<i64>,
    pub source_end_ms: Option<i64>,
    pub snippet: Option<String>,
}

/// Maximum lengths we'll store — guards against runaway model output.
const MAX_TITLE: usize = 200;
const MAX_SNIPPET: usize = 300;

/// Coerce a JSON value to a positive epoch ms timestamp (for start/end).
fn coerce_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().filter(|&ms| ms > 0),
        Value::String(s) => s.trim().parse::<i64>().ok().filter(|&ms| ms > 0),
        _ => None,
    }
}

/// Parse a `YYYY-MM-DD` string to epoch ms at *local* midnight (so the
/// frontend's local-midnight due-date formatting shows the right calendar
/// date). Falls back to treating the string as a raw ms integer.
fn parse_due_date(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(dt) = d.and_hms_opt(0, 0, 0) {
            if let Some(local) = Local.from_local_datetime(&dt).single() {
                return Some(local.timestamp_millis());
            }
        }
    }
    s.parse::<i64>().ok().filter(|&ms| ms > 0)
}

/// Local-midnight epoch ms for today — the threshold for "due date is in the
/// past". A due date exactly at today's local midnight (due "today") is kept;
/// anything strictly before is dropped, so a freshly-extracted suggestion
/// never surfaces as overdue before the user has acted on it.
fn start_of_today_ms() -> i64 {
    let today = Local::now().date_naive();
    let dt = today.and_hms_opt(0, 0, 0).unwrap();
    Local
        .from_local_datetime(&dt)
        .single()
        .unwrap()
        .timestamp_millis()
}

fn clamp(s: String, max_chars: usize) -> String {
    let trimmed = s.trim().to_string();
    if trimmed.chars().count() <= max_chars {
        return trimmed;
    }
    trimmed.chars().take(max_chars).collect()
}

/// Normalize a priority string to the canonical enum, defaulting to `normal`.
fn normalize_priority(p: &str) -> String {
    match p.trim().to_ascii_lowercase().as_str() {
        "low" => "low".to_string(),
        "high" => "high".to_string(),
        _ => "normal".to_string(),
    }
}

/// Strip a single surrounding markdown code fence (```json ... ``` or ``` ... ```
/// ) and any leading/trailing prose, then return the first `[...]` substring.
/// Returns `None` if no array is found.
fn extract_json_array(raw: &str) -> Option<&str> {
    let mut s = raw.trim();
    // Strip a leading ```...``` fence.
    if s.starts_with("```") {
        if let Some(nl) = s.find('\n') {
            s = s[nl + 1..].trim_start();
        }
        if let Some(end) = s.rfind("```") {
            s = s[..end].trim();
        }
    }
    let start = s.find('[')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'[' {
            depth += 1;
        } else if c == b']' {
            depth -= 1;
            if depth == 0 {
                return Some(&s[start..=i]);
            }
        }
        i += 1;
    }
    None
}

/// Strip markdown emphasis (`**`, `__`, `*`) from a string.
fn strip_emphasis(s: &str) -> String {
    s.replace("**", "").replace("__", "").replace('*', "")
}

/// Strip a `- `/`* ` bullet marker (after leading whitespace). Returns the
/// bullet text, or `None` if the line isn't a bullet.
fn strip_bullet_marker(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if let Some(rest) = line.strip_prefix("- ") {
        Some(rest)
    } else if let Some(rest) = line.strip_prefix("* ") {
        Some(rest)
    } else {
        None
    }
}

/// Parse a bullet's text into a `ParsedActionItem`: strip checkbox markers,
/// strip emphasis, collapse whitespace, trim stray separators, clamp. The
/// title is the bullet text as-is — the summary reads naturally, so there are
/// no `[priority]` tags or `due` clauses to extract (priority and due date
/// are suggested by the extraction LLM in `parse_enrichment`). Returns `None`
/// if nothing meaningful remains.
fn parse_bullet(raw: &str) -> Option<ParsedActionItem> {
    let mut s = raw.trim().to_string();

    // Strip a leading checkbox marker ("[ ] " / "[x] ").
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("[ ] ") || lower.starts_with("[x] ") {
        s = s[4..].to_string();
    }

    // Strip emphasis (owner `**Name**` -> `Name`, kept in the title) and
    // normalize whitespace. The bullet is the title as-is.
    s = strip_emphasis(&s);
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s = s
        .trim_matches(|c: char| c == '—' || c == '-' || c.is_whitespace())
        .to_string();

    let title = clamp(s, MAX_TITLE);
    if title.is_empty() {
        return None;
    }
    Some(ParsedActionItem { title })
}

/// Parse only the `## My action items` block (and a plain `## Action items`
/// heading, for older/custom templates) from a summary. `## Action items
/// (others)` is deliberately NOT parsed — what others committed to stays in
/// the summary as reference and does not become task suggestions.
/// Deterministic and non-fatal: returns whatever bullets it finds.
pub fn parse_action_items(summary_md: &str) -> Vec<ParsedActionItem> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in summary_md.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim().to_ascii_lowercase();
            in_block = heading == "my action items" || heading == "action items";
            continue;
        }
        if !in_block {
            continue;
        }
        let bullet = match strip_bullet_marker(trimmed) {
            Some(b) => b,
            None => continue, // owner sub-heading lines like "**Alice:**"
        };
        if bullet.trim().eq_ignore_ascii_case("none") {
            continue;
        }
        if let Some(item) = parse_bullet(bullet) {
            out.push(item);
        }
    }
    out
}

/// Parse the model's enrichment response into a map keyed by 1-based index.
/// Each element may carry a suggested `priority` (normalized to
/// `low`/`normal`/`high`) and `dueDate` (a `YYYY-MM-DD` string converted to
/// local-midnight epoch ms; a past date is dropped), plus the transcript
/// anchor + snippet. Defensive: any parse failure yields an empty map
/// (callers then get title-only suggestions with default `normal` priority).
/// Never panics.
pub fn parse_enrichment(raw: &str) -> HashMap<usize, Enrichment> {
    let mut map = HashMap::new();
    let Some(array) = extract_json_array(raw) else {
        return map;
    };
    let items: Vec<Value> = match serde_json::from_str(array) {
        Ok(v) => v,
        Err(_) => return map,
    };
    for item in items {
        let Value::Object(obj) = &item else { continue };
        let idx = match obj.get("index").and_then(|v| v.as_u64()) {
            Some(i) => i as usize,
            None => continue,
        };
        let priority = obj
            .get("priority")
            .and_then(|v| v.as_str())
            .map(normalize_priority);
        // `dueDate` is a YYYY-MM-DD string (the prompt asks for that, not epoch
        // ms — LLMs are bad at ms arithmetic). Lenient on the key name. A date
        // in the past is dropped so a fresh suggestion is never overdue before
        // the user acts on it.
        let due_at = ["dueDate", "due_date", "due"]
            .iter()
            .find_map(|k| obj.get(*k))
            .and_then(|v| v.as_str())
            .and_then(parse_due_date)
            .filter(|&ms| ms >= start_of_today_ms());
        let source_start_ms = obj.get("startMs").and_then(coerce_ms);
        let mut source_end_ms = obj.get("endMs").and_then(coerce_ms);
        if let (Some(s), Some(e)) = (source_start_ms, source_end_ms) {
            if e < s {
                source_end_ms = None;
            }
        }
        let snippet = obj
            .get("snippet")
            .and_then(|v| v.as_str())
            .map(|s| clamp(s.to_string(), MAX_SNIPPET))
            .filter(|s| !s.is_empty());
        map.insert(
            idx,
            Enrichment {
                priority,
                due_at,
                source_start_ms,
                source_end_ms,
                snippet,
            },
        );
    }
    map
}

/// Merge parsed action-item titles with their LLM enrichment (by 1-based
/// index). The title comes from the parse; priority, due date, and the
/// transcript anchor come from enrichment. Missing enrichment → a title-only
/// suggestion with default `normal` priority, no due date, no transcript
/// anchor.
pub fn merge(
    items: &[ParsedActionItem],
    enrichment: &HashMap<usize, Enrichment>,
) -> Vec<TaskSuggestion> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let en = enrichment.get(&(i + 1));
            TaskSuggestion {
                title: item.title.clone(),
                description: None,
                priority: en
                    .and_then(|e| e.priority.clone())
                    .unwrap_or_else(|| "normal".to_string()),
                due_at: en.and_then(|e| e.due_at),
                source_start_ms: en.and_then(|e| e.source_start_ms),
                source_end_ms: en.and_then(|e| e.source_end_ms),
                snippet: en.and_then(|e| e.snippet.clone()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_with(action: &str) -> String {
        format!(
            "## TL;DR\nA quick sync.\n\n## Key decisions\n- Ship Friday\n\n{action}\n\n## Open questions\n- Who owns the spec?\n"
        )
    }

    #[test]
    fn parses_only_my_action_items() {
        let md = summary_with(
            "## My action items\n- Send the quote to Acme by July 18\n- Book the follow-up call\n\n## Action items (others)\n- Share the draft spec — **Priya**\n- Confirm legal review — **Sam**",
        );
        let items = parse_action_items(&md);
        // Only the "My action items" bullets become tasks; "(others)" stays
        // in the summary as reference and is NOT extracted. The natural bullet
        // text is the title as-is (no [tag]/due clauses to strip).
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Send the quote to Acme by July 18");
        assert_eq!(items[1].title, "Book the follow-up call");
        assert!(!items
            .iter()
            .any(|i| i.title.contains("Priya") || i.title.contains("Sam")));
    }

    #[test]
    fn stops_at_open_questions() {
        let md = summary_with("## My action items\n- Do the thing\n");
        let items = parse_action_items(&md);
        assert_eq!(items.len(), 1);
        // The "Who owns the spec?" bullet under Open questions is NOT picked up.
        assert!(!items.iter().any(|i| i.title.contains("spec")));
    }

    #[test]
    fn none_block_yields_nothing() {
        let md = summary_with("## My action items\nNone\n\n## Action items (others)\nNone\n");
        assert!(parse_action_items(&md).is_empty());
    }

    #[test]
    fn handles_star_bullets_and_case_insensitive_headings() {
        let md = "## MY ACTION ITEMS\n* Review the contract\n## action items (others)\n* Ping legal — **Robin**";
        let items = parse_action_items(&md);
        // Only the "MY ACTION ITEMS" bullet; "(others)" is not extracted.
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Review the contract");
    }

    #[test]
    fn legacy_single_action_items_heading_still_parses() {
        let md = summary_with("## Action items\n- Follow up with Sam\n");
        let items = parse_action_items(&md);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Follow up with Sam");
    }

    #[test]
    fn skips_owner_subheading_lines() {
        let md = "## My action items\n**Alice:**\n- Send the deck — **Alice**\n";
        let items = parse_action_items(&md);
        // The "**Alice:**" line isn't a bullet (`* ` prefix), so it's skipped;
        // the real bullet still parses with the owner kept in the title.
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Send the deck — Alice");
    }

    #[test]
    fn checkbox_bullets_are_stripped() {
        let md = "## My action items\n- [x] Done-ish thing\n";
        let items = parse_action_items(&md);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Done-ish thing");
    }

    #[test]
    fn enrichment_index_keyed_and_defensive() {
        let raw = r#"[
          {"index":1,"priority":"high","startMs":120000,"snippet":"I'll send it Friday"},
          {"index":3,"priority":"normal","startMs":540000,"endMs":560000}
        ]"#;
        let map = parse_enrichment(raw);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&1).unwrap().priority.as_deref(), Some("high"));
        assert_eq!(map.get(&1).unwrap().source_start_ms, Some(120000));
        assert_eq!(
            map.get(&1).unwrap().snippet.as_deref(),
            Some("I'll send it Friday")
        );
        assert_eq!(map.get(&3).unwrap().priority.as_deref(), Some("normal"));
        assert_eq!(map.get(&3).unwrap().source_end_ms, Some(560000));
    }

    #[test]
    fn enrichment_malformed_yields_empty() {
        assert!(parse_enrichment("not json").is_empty());
        assert!(parse_enrichment("[{oops}]").is_empty());
        assert!(parse_enrichment("[]").is_empty());
    }

    #[test]
    fn enrichment_ignores_end_before_start() {
        let raw = r#"[{"index":1,"priority":"normal","startMs":1000,"endMs":500}]"#;
        let map = parse_enrichment(raw);
        assert_eq!(map.get(&1).unwrap().source_start_ms, Some(1000));
        assert!(map.get(&1).unwrap().source_end_ms.is_none());
    }

    #[test]
    fn enrichment_keeps_future_due_date() {
        let raw = r#"[{"index":1,"priority":"normal","dueDate":"2099-09-01"}]"#;
        let map = parse_enrichment(raw);
        assert!(map.get(&1).unwrap().due_at.is_some());
    }

    #[test]
    fn enrichment_drops_past_due_date() {
        let raw = r#"[{"index":1,"priority":"normal","dueDate":"2000-01-01"}]"#;
        let map = parse_enrichment(raw);
        // A past due date is dropped so a fresh suggestion is never overdue.
        assert!(map.get(&1).unwrap().due_at.is_none());
    }

    #[test]
    fn enrichment_priority_is_normalized() {
        let raw = r#"[{"index":1,"priority":"HIGH"},{"index":2,"priority":"weird"}]"#;
        let map = parse_enrichment(raw);
        assert_eq!(map.get(&1).unwrap().priority.as_deref(), Some("high"));
        // An unrecognized priority falls back to "normal".
        assert_eq!(map.get(&2).unwrap().priority.as_deref(), Some("normal"));
    }

    #[test]
    fn enrichment_carries_priority_and_due_across_split_items() {
        // The model split "write the PO and schedule the workshop by next week"
        // into two action items and returns one complete element per index —
        // each with its own priority and dueDate (a concrete future date, since
        // the prompt now asks it to convert relative deadlines). Both indices
        // must keep their priority and due date; neither falls back to defaults.
        let raw = r#"[
          {"index":1,"priority":"normal","dueDate":"2099-07-18","startMs":120000},
          {"index":2,"priority":"high","dueDate":"2099-07-18","startMs":540000}
        ]"#;
        let map = parse_enrichment(raw);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&1).unwrap().priority.as_deref(), Some("normal"));
        assert!(map.get(&1).unwrap().due_at.is_some());
        assert_eq!(map.get(&2).unwrap().priority.as_deref(), Some("high"));
        assert!(map.get(&2).unwrap().due_at.is_some());
    }

    #[test]
    fn merge_without_enrichment_defaults_to_normal_no_due() {
        let items = vec![
            ParsedActionItem {
                title: "Send the quote".into(),
            },
            ParsedActionItem {
                title: "Book the call".into(),
            },
        ];
        let merged = merge(&items, &HashMap::new());
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].title, "Send the quote");
        // No enrichment → default priority "normal", no due date, no anchor.
        assert_eq!(merged[0].priority, "normal");
        assert!(merged[0].due_at.is_none());
        assert!(merged[0].source_start_ms.is_none());
        assert!(merged[0].snippet.is_none());
    }

    #[test]
    fn merge_applies_priority_due_timestamps_and_snippet() {
        let items = vec![ParsedActionItem {
            title: "Send the quote".into(),
        }];
        let mut en = HashMap::new();
        en.insert(
            1,
            Enrichment {
                priority: Some("high".into()),
                due_at: Some(1_900_000_000_000),
                source_start_ms: Some(120000),
                source_end_ms: Some(130000),
                snippet: Some("I'll send it Friday".into()),
            },
        );
        let merged = merge(&items, &en);
        assert_eq!(merged[0].priority, "high");
        assert_eq!(merged[0].due_at, Some(1_900_000_000_000));
        assert_eq!(merged[0].source_start_ms, Some(120000));
        assert_eq!(merged[0].source_end_ms, Some(130000));
        assert_eq!(merged[0].snippet.as_deref(), Some("I'll send it Friday"));
    }

    #[test]
    fn merge_defaults_priority_when_enrichment_omits_it() {
        let items = vec![ParsedActionItem {
            title: "Do the thing".into(),
        }];
        let mut en = HashMap::new();
        en.insert(
            1,
            Enrichment {
                priority: None,
                due_at: None,
                source_start_ms: Some(5000),
                source_end_ms: None,
                snippet: None,
            },
        );
        let merged = merge(&items, &en);
        // Enrichment present but priority absent → default "normal".
        assert_eq!(merged[0].priority, "normal");
        assert_eq!(merged[0].source_start_ms, Some(5000));
    }
}
