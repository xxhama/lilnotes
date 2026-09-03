//! MCP tool handlers. Every tool is read-only and returns JSON both as a
//! text content block (for clients that only render `content`) and as
//! `structured_content`.
//!
//! Tool descriptions and parameter doc comments are the agent's only
//! documentation — keep them precise about units: `*_ms` fields are Unix
//! epoch milliseconds, except segment `start_ms`/`end_ms`, which are offsets
//! from the meeting start.

use std::sync::Arc;

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ErrorData, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{
    CustomerDetail, CustomerRollupRow, CustomerSearchResult, CustomerSummary, Db, LazyDb,
    MeetingFilter, MeetingIndexRow, Persona, SummaryRow,
};

use super::format::{self, Participant};

/// Max characters of transcript text returned by one `get_transcript` call.
const MAX_TRANSCRIPT_CHARS: usize = 300_000;
/// Max segments returned by one `get_transcript { format: "segments" }` call.
const MAX_TRANSCRIPT_SEGMENTS: usize = 5_000;
const DEFAULT_LIST_LIMIT: u32 = 50;
const MAX_LIST_LIMIT: u32 = 200;
const DEFAULT_SEARCH_LIMIT: u32 = 20;
const MAX_SEARCH_LIMIT: u32 = 100;

const DB_LOCKED_MSG: &str =
    "LilNotes database is not unlocked yet — finish onboarding in the LilNotes app first";

const INSTRUCTIONS: &str = "LilNotes is a local meeting recorder. These tools are read-only \
views over its database: meetings, transcripts (with speaker names and timestamps), saved \
LLM summaries, notes, customer accounts and personas (known people). Integer ids are stable. \
Fields ending in `_ms` are Unix epoch milliseconds, except transcript segment `start_ms`/`end_ms`, \
which are offsets from the meeting start. Typical flow: `list_meetings` or `search_meetings` to \
find ids, then `get_meeting` / `get_transcript` / `list_summaries` for detail. Audio files, app \
settings and voiceprints are never exposed.";

/// Why a tool could not produce a result.
enum Fail {
    /// The agent asked for something that does not exist / cannot be served;
    /// reported as a tool result with `is_error` so the model can adapt.
    User(String),
    /// Unexpected backend failure; reported as a JSON-RPC internal error.
    Internal(String),
}

fn not_found_or_internal(what: &str, id: i64, e: String) -> Fail {
    if e.contains("no rows") {
        Fail::User(format!("{what} {id} not found"))
    } else {
        Fail::Internal(e)
    }
}

fn ok_json<T: Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let json = serde_json::to_value(value)
        .map_err(|e| ErrorData::internal_error(format!("serialize result: {e}"), None))?;
    let text = serde_json::to_string_pretty(&json).unwrap_or_default();
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(json);
    Ok(result)
}

fn respond<T: Serialize>(r: Result<T, Fail>) -> Result<CallToolResult, ErrorData> {
    match r {
        Ok(v) => ok_json(&v),
        Err(Fail::User(msg)) => Ok(CallToolResult::error(vec![ContentBlock::text(msg)])),
        Err(Fail::Internal(msg)) => Err(ErrorData::internal_error(msg, None)),
    }
}

fn clamp_limit(limit: Option<u32>, default: u32, max: u32) -> i64 {
    i64::from(limit.unwrap_or(default).clamp(1, max))
}

// ---------------------------------------------------------------------------
// Parameter + result shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListMeetingsParams {
    /// Case-insensitive substring matched against the meeting title and
    /// transcript text.
    pub query: Option<String>,
    /// Only meetings assigned to this customer id.
    pub customer_id: Option<i64>,
    /// Only meetings that started at or after this Unix epoch ms.
    pub from_ms: Option<i64>,
    /// Only meetings that started at or before this Unix epoch ms.
    pub to_ms: Option<i64>,
    /// Page size (default 50, max 200).
    pub limit: Option<u32>,
    /// Number of meetings to skip (for paging); default 0.
    pub offset: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListMeetingsResult {
    pub meetings: Vec<MeetingIndexRow>,
    /// Total matches before paging.
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct MeetingIdParams {
    /// Meeting id from `list_meetings` / `search_meetings`.
    pub meeting_id: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingResult {
    pub id: i64,
    pub title: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub customer_id: Option<i64>,
    /// Whisper model that produced the transcript, if recorded.
    pub asr_model: Option<String>,
    /// The user's free-form notes (Markdown), if any.
    pub notes: Option<String>,
    pub notes_updated_at_ms: Option<i64>,
    pub participants: Vec<Participant>,
    /// Most recent saved summary (Markdown), if any.
    pub latest_summary: Option<SummaryRow>,
    pub summary_count: i64,
    pub segment_count: i64,
    pub speaker_count: i64,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptFormat {
    /// One line per segment: `[m:ss] Speaker: text`.
    #[default]
    Text,
    /// Structured segments with ids, speakers and ms offsets.
    Segments,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetTranscriptParams {
    /// Meeting id from `list_meetings` / `search_meetings`.
    pub meeting_id: i64,
    /// `text` (default) or `segments`.
    pub format: Option<TranscriptFormat>,
    /// Only segments starting at or after this offset from meeting start (ms).
    pub from_ms: Option<u64>,
    /// Only segments starting at or before this offset from meeting start (ms).
    pub to_ms: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub id: i64,
    /// Resolved speaker name (persona > rename > raw label).
    pub speaker: String,
    /// Raw diarization label ("Me", "SPEAKER_00", …).
    pub raw_label: String,
    /// "mic" (the user's microphone) or "system" (other participants).
    pub source: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "format")]
pub enum TranscriptResult {
    #[serde(rename = "text")]
    Text {
        meeting_id: i64,
        text: String,
        segment_count: usize,
        /// True when the output was cut at the size cap; narrow with
        /// `from_ms`/`to_ms` to get the rest.
        truncated: bool,
    },
    #[serde(rename = "segments")]
    Segments {
        meeting_id: i64,
        segments: Vec<TranscriptSegment>,
        truncated: bool,
    },
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchMeetingsParams {
    /// Case-insensitive substring to look for (required, non-empty).
    pub query: String,
    /// Only meetings assigned to this customer id.
    pub customer_id: Option<i64>,
    /// Max meetings returned (default 20, max 100).
    pub limit: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMeetingsResult {
    /// Newest first. Each hit names the field that matched (`title`,
    /// `notes`, `transcript`, `summary`, `persona`) with a short snippet.
    pub results: Vec<CustomerSearchResult>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummariesResult {
    /// Newest first. `content` is Markdown.
    pub summaries: Vec<SummaryRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomersResult {
    pub customers: Vec<CustomerSummary>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CustomerIdParams {
    /// Customer id from `list_customers`.
    pub customer_id: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollupsResult {
    /// Newest first. Cross-meeting customer summaries (Markdown).
    pub rollups: Vec<CustomerRollupRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonasResult {
    pub personas: Vec<Persona>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// One handler instance per MCP session (rmcp calls the factory in
/// `build_router` on each `initialize`). `#[tool_handler]` reaches the
/// generated `Self::tool_router()` directly, so no router field is needed.
#[derive(Clone)]
pub struct LilNotesMcp {
    db: Arc<LazyDb>,
}

impl LilNotesMcp {
    pub fn new(db: Arc<LazyDb>) -> Self {
        Self { db }
    }

    /// Run a blocking DB closure off the async runtime. The DB may still be
    /// locked (onboarding not finished) — that is a user-facing tool error,
    /// not a crash.
    async fn with_db<T, F>(&self, f: F) -> Result<T, Fail>
    where
        T: Send + 'static,
        F: FnOnce(&Db) -> Result<T, Fail> + Send + 'static,
    {
        let db = self.db.clone();
        tauri::async_runtime::spawn_blocking(move || match db.get() {
            Some(d) => f(d),
            None => Err(Fail::User(DB_LOCKED_MSG.into())),
        })
        .await
        .map_err(|e| Fail::Internal(format!("db task failed: {e}")))?
    }
}

#[tool_router]
impl LilNotesMcp {
    #[tool(
        description = "List recorded meetings, newest first, with optional substring search over title + transcript, customer filter, date range and paging. Returns ids for get_meeting / get_transcript.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_meetings(
        &self,
        Parameters(p): Parameters<ListMeetingsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let filter = MeetingFilter {
            query: p.query,
            customer_id: p.customer_id,
            from_ms: p.from_ms,
            to_ms: p.to_ms,
            limit: clamp_limit(p.limit, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT),
            offset: i64::from(p.offset.unwrap_or(0)),
        };
        let (limit, offset) = (filter.limit, filter.offset);
        respond(
            self.with_db(move |db| {
                let (meetings, total) =
                    db.list_meetings_filtered(&filter).map_err(Fail::Internal)?;
                Ok(ListMeetingsResult {
                    meetings,
                    total,
                    limit,
                    offset,
                })
            })
            .await,
        )
    }

    #[tool(
        description = "Get one meeting: metadata, notes, participants with resolved names, and the latest saved summary. Use get_transcript for the full transcript.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn get_meeting(
        &self,
        Parameters(p): Parameters<MeetingIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = p.meeting_id;
        respond(
            self.with_db(move |db| {
                let m = db
                    .get_meeting(id)
                    .map_err(|e| not_found_or_internal("meeting", id, e))?;
                let summaries = db.list_summaries(id).map_err(Fail::Internal)?;
                Ok(MeetingResult {
                    id: m.id,
                    title: m.title.clone(),
                    started_at_ms: m.started_at_ms,
                    ended_at_ms: m.ended_at_ms,
                    duration_ms: m.ended_at_ms.map(|e| e - m.started_at_ms),
                    customer_id: m.customer_id,
                    asr_model: m.asr_model.clone(),
                    notes: m.notes.clone(),
                    notes_updated_at_ms: m.notes_updated_at_ms,
                    participants: format::participants(&m),
                    latest_summary: summaries.first().cloned(),
                    summary_count: summaries.len() as i64,
                    segment_count: m.segments.len() as i64,
                    speaker_count: m.speaker_count,
                })
            })
            .await,
        )
    }

    #[tool(
        description = "Get a meeting's transcript with speaker names resolved (persona > rename > raw label) and timestamps. format=text gives `[m:ss] Speaker: text` lines; format=segments gives structured rows. Large transcripts are capped — page with from_ms/to_ms (offsets from meeting start) when `truncated` is true.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn get_transcript(
        &self,
        Parameters(p): Parameters<GetTranscriptParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = p.meeting_id;
        let fmt = p.format.unwrap_or_default();
        let (from, to) = (p.from_ms.unwrap_or(0), p.to_ms.unwrap_or(u64::MAX));
        if from > to {
            return Err(ErrorData::invalid_params("from_ms must be <= to_ms", None));
        }
        respond(
            self.with_db(move |db| {
                let m = db
                    .get_meeting(id)
                    .map_err(|e| not_found_or_internal("meeting", id, e))?;
                let segments: Vec<_> = m
                    .segments
                    .iter()
                    .filter(|s| s.start_ms >= from && s.start_ms <= to)
                    .cloned()
                    .collect();
                Ok(match fmt {
                    TranscriptFormat::Text => {
                        let full =
                            format::render_transcript(&segments, &m.renames, &m.speaker_links);
                        let (text, truncated) = format::truncate_chars(&full, MAX_TRANSCRIPT_CHARS);
                        TranscriptResult::Text {
                            meeting_id: id,
                            text,
                            segment_count: segments.len(),
                            truncated,
                        }
                    }
                    TranscriptFormat::Segments => {
                        let truncated = segments.len() > MAX_TRANSCRIPT_SEGMENTS;
                        let rows = segments
                            .iter()
                            .take(MAX_TRANSCRIPT_SEGMENTS)
                            .map(|s| TranscriptSegment {
                                id: s.id,
                                speaker: format::resolve_speaker_name(
                                    s,
                                    &m.renames,
                                    &m.speaker_links,
                                ),
                                raw_label: format::raw_label(s),
                                source: s.source.clone(),
                                start_ms: s.start_ms,
                                end_ms: s.end_ms,
                                text: s.text.clone(),
                            })
                            .collect();
                        TranscriptResult::Segments {
                            meeting_id: id,
                            segments: rows,
                            truncated,
                        }
                    }
                })
            })
            .await,
        )
    }

    #[tool(
        description = "Full-text (substring) search across all meetings: titles, notes, transcript text, saved summaries and participant persona names. Returns matching meetings newest first with a snippet per matched field.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn search_meetings(
        &self,
        Parameters(p): Parameters<SearchMeetingsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let query = p.query.trim().to_string();
        if query.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = clamp_limit(p.limit, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT);
        let customer_id = p.customer_id;
        respond(
            self.with_db(move |db| {
                let results = db
                    .search_meetings(customer_id, &query, limit)
                    .map_err(Fail::Internal)?;
                Ok(SearchMeetingsResult { results })
            })
            .await,
        )
    }

    #[tool(
        description = "List all saved LLM summaries of a meeting, newest first (Markdown content, model name, creation time).",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_summaries(
        &self,
        Parameters(p): Parameters<MeetingIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = p.meeting_id;
        respond(
            self.with_db(move |db| {
                // Distinguish "no meeting" from "no summaries yet".
                db.get_meeting(id)
                    .map_err(|e| not_found_or_internal("meeting", id, e))?;
                let summaries = db.list_summaries(id).map_err(Fail::Internal)?;
                Ok(SummariesResult { summaries })
            })
            .await,
        )
    }

    #[tool(
        description = "List customer accounts (companies/projects meetings are filed under) with meeting counts and last-meeting time.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_customers(&self) -> Result<CallToolResult, ErrorData> {
        respond(
            self.with_db(|db| {
                let customers = db.list_customers().map_err(Fail::Internal)?;
                Ok(CustomersResult { customers })
            })
            .await,
        )
    }

    #[tool(
        description = "Get one customer account: notes, meeting list, the personas seen across its meetings, and the latest cross-meeting rollup summary.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn get_customer(
        &self,
        Parameters(p): Parameters<CustomerIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = p.customer_id;
        respond(
            self.with_db(move |db| -> Result<CustomerDetail, Fail> {
                db.get_customer(id)
                    .map_err(|e| not_found_or_internal("customer", id, e))
            })
            .await,
        )
    }

    #[tool(
        description = "List a customer's saved rollup summaries (cross-meeting Markdown digests), newest first.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_customer_summaries(
        &self,
        Parameters(p): Parameters<CustomerIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = p.customer_id;
        respond(
            self.with_db(move |db| {
                db.get_customer(id)
                    .map_err(|e| not_found_or_internal("customer", id, e))?;
                let rollups = db.list_customer_summaries(id).map_err(Fail::Internal)?;
                Ok(RollupsResult { rollups })
            })
            .await,
        )
    }

    #[tool(
        description = "List personas: named people LilNotes recognizes across meetings (id, display name, notes, timestamps). Voice data is never included.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_personas(&self) -> Result<CallToolResult, ErrorData> {
        respond(
            self.with_db(|db| {
                let personas = db.list_personas().map_err(Fail::Internal)?;
                Ok(PersonasResult { personas })
            })
            .await,
        )
    }
}

#[tool_handler]
impl ServerHandler for LilNotesMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("lilnotes", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
