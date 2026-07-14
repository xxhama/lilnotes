//! Summarization via a locally running Ollama instance.
//!
//! Everything talks to `http://localhost:11434` — the app's only permitted
//! network peer at meeting time. Ollama runs as a separate native process
//! (installed by the user) so it keeps its own Metal acceleration.
//!
//! - health check: GET /api/version
//! - installed models: GET /api/tags
//! - pull with progress: POST /api/pull (streaming NDJSON, per-digest
//!   progress aggregated; cancellable — Ollama resumes cancelled pulls)
//! - summarize: POST /api/chat (streaming; tokens relayed as events)

pub mod ollama;
pub mod sidecar;

use chrono::Local;
use serde::Serialize;

use crate::asr::Segment;

/// Which LLM backend to use for summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryBackend {
    Native,
    Ollama,
}

impl SummaryBackend {
    pub fn from_str(s: &str) -> Self {
        match s {
            "ollama" => SummaryBackend::Ollama,
            _ => SummaryBackend::Native, // default
        }
    }
}

/// Built-in speaker-aware summary template. Users can override it in
/// Settings; `{transcript}`, `{title}`, and `{today}` are substituted at run
/// time. The default template no longer uses `{today}` (priority and due dates
/// are decided by the extraction flow, not the summary), but it's kept for
/// custom templates that want a calendar anchor.
pub const DEFAULT_TEMPLATE: &str = r#"You are an expert meeting-notes assistant. Below is the transcript of a meeting ("{title}"), with timestamps and speaker names.

Write a concise summary in Markdown with exactly these sections:

## TL;DR
2-4 sentences capturing what the meeting was about and its outcome.

## Key decisions
Bullet list of decisions that were actually made (not merely discussed). If none, write "None".

## My action items
Concrete things the note-taker ("Me") will DO — not decisions or topics. Only include an item here if "Me" clearly agreed to do it; if ownership is unclear or it belongs to someone else, put it under "Action items (others)". Keep each item on a single bullet line, phrased naturally. One action per bullet — if a single commitment covers two distinct actions (often joined by "and", "then", or a comma), split them into separate bullets rather than one compound bullet. If none, write "None".

## Action items (others)
Concrete things others will DO — not decisions or topics — grouped by owner. Keep each item on a single bullet line, phrased naturally, one action per bullet (split "and"/comma compounds into separate bullets), and end with ` — **Owner**` (use the speaker names from the transcript). If none, write "None".

## Open questions
Bullet list of unresolved questions or topics deferred for later. If none, write "None".

Rules: base everything strictly on the transcript — never invent facts, names, or dates. Keep the whole summary under 400 words. Refer to "Me" as the note-taker.

Transcript:

{transcript}"#;

/// Template for the customer-level "recent topics" rollup. `{customer}` is
/// the customer name; `{summaries}` is replaced with the recent meetings'
/// summaries (title, date, content). Bases strictly on those summaries.
pub const DEFAULT_CUSTOMER_ROLLUP_TEMPLATE: &str = r#"You are reviewing a series of meetings with a customer ("{customer}"). Below are the AI summaries of the most recent meetings, each preceded by its title and date.

Synthesize a concise "Customer at a glance" overview in Markdown with exactly these sections:

## Recent themes
3-5 bullets of recurring topics or themes across these meetings.

## Latest developments
2-4 bullets on what is most recent or currently in progress.

## Watch items
2-4 bullets of open questions or things to follow up on.

Rules: base everything strictly on the provided summaries — never invent facts, names, or dates. Refer to "Me" as the note-taker. Keep the whole overview under 350 words.

Meeting summaries:

{summaries}"#;

/// A curated, meeting-summarization-friendly seed list (sizes are
/// approximate; live sizes come from /api/tags). Ordered by preference.
pub struct CuratedModel {
    pub tag: &'static str,
    pub tier: &'static str,
    pub approx_download: &'static str,
    pub note: &'static str,
}

pub const CURATED_MODELS: &[CuratedModel] = &[
    CuratedModel {
        tag: "gemma4:26b",
        tier: "default",
        approx_download: "18 GB",
        note: "Best balance: MoE runs near small-model speed with big-model quality. Top pick for meeting summaries on this Mac.",
    },
    CuratedModel {
        tag: "qwen3.5:27b",
        tier: "balance",
        approx_download: "17 GB",
        note: "Strong all-rounder with excellent instruction-following; the main alternative to gemma4:26b.",
    },
    CuratedModel {
        tag: "gemma4:31b",
        tier: "quality",
        approx_download: "20 GB",
        note: "Highest-quality Gemma that fits comfortably; dense, so slower than 26b.",
    },
    CuratedModel {
        tag: "qwen3.5:35b",
        tier: "quality+",
        approx_download: "24 GB",
        note: "Highest-quality local option here; leave RAM headroom.",
    },
    CuratedModel {
        tag: "gemma4:12b",
        tier: "fast",
        approx_download: "7.6 GB",
        note: "Fast, light, still very capable — good default if speed matters.",
    },
    CuratedModel {
        tag: "qwen3.5:9b",
        tier: "fast",
        approx_download: "6.6 GB",
        note: "Fast Qwen option (the qwen3.5:latest default tag).",
    },
    CuratedModel {
        tag: "gemma4:e4b",
        tier: "light",
        approx_download: "9.6 GB",
        note: "Smallest Gemma worth using for summaries; for lighter machines.",
    },
    CuratedModel {
        tag: "qwen3.5:4b",
        tier: "minimal",
        approx_download: "3.4 GB",
        note: "Smallest sensible fallback; noticeably weaker — only if RAM is tight.",
    },
];

/// Auto-pick a summary model from what's installed, preferring the curated
/// order (an installed `-mlx` variant of a curated tag wins over the plain
/// tag — MLX builds are faster on Apple Silicon).
pub fn pick_default_model(installed: &[String]) -> Option<String> {
    for c in CURATED_MODELS {
        let mlx = format!("{}-mlx", c.tag);
        if installed.iter().any(|m| m == &mlx) {
            return Some(mlx);
        }
        if installed.iter().any(|m| m == c.tag) {
            return Some(c.tag.to_string());
        }
    }
    // Any non-embedding model beats nothing.
    installed
        .iter()
        .find(|m| !m.contains("embed") && !m.contains("bge-"))
        .cloned()
}

/// Render the transcript as "[mm:ss] Speaker: text" lines with display
/// names applied.
pub fn transcript_text(
    segments: &[Segment],
    renames: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = String::new();
    for s in segments {
        let raw = s.speaker.clone().unwrap_or_else(|| {
            if s.source == "mic" {
                "Me".into()
            } else {
                "Speaker".into()
            }
        });
        let name = renames.get(&raw).cloned().unwrap_or(raw);
        let secs = s.start_ms / 1000;
        out.push_str(&format!(
            "[{}:{:02}] {}: {}\n",
            secs / 60,
            secs % 60,
            name,
            s.text
        ));
    }
    out
}

/// Fill the template placeholders.
pub fn build_prompt(template: &str, title: &str, transcript: &str) -> String {
    template
        .replace("{title}", title)
        .replace("{transcript}", transcript)
        .replace("{today}", &Local::now().format("%Y-%m-%d").to_string())
}

/// Prompt for generating a short meeting title from the just-generated
/// summary. `{summary}` is substituted with the summary content. Kept short
/// so the second LLM call (after the summary streams in) is fast.
pub const DEFAULT_TITLE_TEMPLATE: &str = r#"You are naming a meeting. Based on the summary below, write a concise, descriptive title in 3 to 8 words. Do not include surrounding quotes, trailing punctuation, or generic prefixes like "Meeting about". Output only the title on a single line.

Summary:

{summary}"#;

/// Fill the title-template `{summary}` placeholder.
pub fn build_title_prompt(summary: &str) -> String {
    DEFAULT_TITLE_TEMPLATE.replace("{summary}", summary)
}

/// Payload of the `summary:token` event.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SummaryToken {
    pub meeting_id: i64,
    pub token: String,
    pub is_thinking: bool,
}

/// Payload of the `customer-summary:token` event (customer-level rollup).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CustomerSummaryToken {
    pub customer_id: i64,
    pub token: String,
    pub is_thinking: bool,
}

/// Render the per-meeting summaries block fed to the customer rollup prompt.
/// Each entry: `### {title} ({date})` followed by the summary content.
pub fn format_rollup_summaries(entries: &[(String, String, String)]) -> String {
    let mut out = String::new();
    for (title, date, content) in entries {
        out.push_str(&format!("### {title} ({date})\n{content}\n\n"));
    }
    out
}

/// Fill the customer rollup template placeholders.
pub fn build_customer_rollup_prompt(template: &str, customer: &str, summaries: &str) -> String {
    template
        .replace("{customer}", customer)
        .replace("{summaries}", summaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_mlx_over_plain() {
        let installed = vec!["gemma4:26b".to_string(), "gemma4:26b-mlx".to_string()];
        assert_eq!(
            pick_default_model(&installed).as_deref(),
            Some("gemma4:26b-mlx")
        );
    }

    #[test]
    fn respects_curated_order() {
        let installed = vec!["qwen3.5:9b".to_string(), "qwen3.5:27b".to_string()];
        assert_eq!(
            pick_default_model(&installed).as_deref(),
            Some("qwen3.5:27b")
        );
    }

    #[test]
    fn falls_back_to_any_non_embedding() {
        let installed = vec![
            "nomic-embed-text:latest".to_string(),
            "llama3:8b".to_string(),
        ];
        assert_eq!(pick_default_model(&installed).as_deref(), Some("llama3:8b"));
    }

    #[test]
    fn template_substitution() {
        let p = build_prompt("T={title} X={transcript}", "Standup", "hello");
        assert_eq!(p, "T=Standup X=hello");
    }

    #[test]
    fn today_substitution() {
        // A template with {today} gets a YYYY-MM-DD-shaped date.
        let p = build_prompt("{today}", "Standup", "hello");
        assert!(
            chrono::NaiveDate::parse_from_str(&p, "%Y-%m-%d").is_ok(),
            "expected a YYYY-MM-DD date, got {p}"
        );
        // A template without {today} is unchanged (no-op replace).
        let p2 = build_prompt("T={title} X={transcript}", "Standup", "hello");
        assert_eq!(p2, "T=Standup X=hello");
    }
}
