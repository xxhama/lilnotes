//! SQLite persistence (rusqlite, bundled).
//!
//! One connection behind a mutex — queries are short and the app is a
//! single user; contention is not a concern. Migrations run at open via
//! `PRAGMA user_version`.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::asr::Segment;
use crate::settings::AppSettings;

pub struct Db {
    conn: Mutex<Connection>,
}

/// Lazily-initialized `Db` wrapper. For new users the DB (and its
/// Keychain key) is not opened until `init()` is called from the
/// onboarding wizard. For returning users — whose DB file already
/// exists — `init()` is called eagerly in `setup()` so the keychain
/// access is silent (the key already exists and access was previously
/// granted). `Deref<Target = Db>` lets command bodies stay unchanged.
pub struct LazyDb {
    data_dir: PathBuf,
    inner: OnceLock<Db>,
}

impl LazyDb {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            inner: OnceLock::new(),
        }
    }

    /// Open the DB (retrieving the keychain key). Idempotent — safe to
    /// call multiple times; only the first call opens the connection.
    pub fn init(&self) -> Result<(), String> {
        if self.inner.get().is_some() {
            return Ok(());
        }
        let key = crate::keystore::db_key()?;
        let db = Db::open(&db_path(&self.data_dir), &key)?;
        crate::settings::migrate_json_settings(&self.data_dir, &db);
        // `set` returns Err(val) if already set — first writer wins, the
        // loser's DB is simply discarded. No race in practice: `init()` is
        // called from `setup()` (returning users) or `init_db` (new users),
        // never both.
        let _ = self.inner.set(db);
        Ok(())
    }

    /// Returns `Some(&Db)` if already initialized, `None` otherwise.
    pub fn get(&self) -> Option<&Db> {
        self.inner.get()
    }

    /// Wrap an already-open `Db` (tests only — bypasses the Keychain).
    #[cfg(test)]
    pub fn for_tests(db: Db) -> Self {
        let inner = OnceLock::new();
        let _ = inner.set(db);
        Self {
            data_dir: PathBuf::new(),
            inner,
        }
    }
}

impl Deref for LazyDb {
    type Target = Db;
    fn deref(&self) -> &Self::Target {
        self.inner
            .get()
            .expect("DB not initialized — call init_db() before any DB command")
    }
}

// ---------------------------------------------------------------------------
// Row types (serialized to the UI)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSummary {
    pub id: i64,
    pub title: String,
    pub started_at_ms: i64,
    pub duration_ms: Option<i64>,
    pub segment_count: i64,
    pub speaker_count: i64,
    pub preview: Option<String>,
    pub has_audio: bool,
}

/// Filter/pagination for `list_meetings_filtered` (MCP `list_meetings`).
#[derive(Default, Clone, Debug)]
pub struct MeetingFilter {
    /// Case-insensitive substring over title + transcript text.
    pub query: Option<String>,
    pub customer_id: Option<i64>,
    /// Inclusive bounds on `started_at` (epoch ms).
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub limit: i64,
    pub offset: i64,
}

/// One row of `list_meetings_filtered`: `MeetingSummary` minus the audio
/// flag (never exposed to agents) plus customer/summary/notes flags.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MeetingIndexRow {
    pub id: i64,
    pub title: String,
    pub started_at_ms: i64,
    pub duration_ms: Option<i64>,
    pub segment_count: i64,
    pub speaker_count: i64,
    pub customer_id: Option<i64>,
    pub has_summary: bool,
    pub has_notes: bool,
    pub preview: Option<String>,
}

/// The three on-disk audio files a meeting can own (each `None` once deleted).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioPaths {
    pub mic: Option<String>,
    pub system: Option<String>,
    pub cleaned: Option<String>,
}

impl AudioPaths {
    /// The paths that are set, in (mic, system, cleaned) order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        [&self.mic, &self.system, &self.cleaned]
            .into_iter()
            .filter_map(|p| p.as_deref())
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MeetingDetail {
    pub id: i64,
    pub session_id: String,
    pub title: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub mic_wav: Option<String>,
    pub system_wav: Option<String>,
    /// Path to an offline echo-cleaned mic recording, if `clean_echo` has been
    /// run. The UI / AudioPlayer / re-transcribe prefer this over `mic_wav`.
    ///
    /// The `*_wav` names are historical: since 0.3 the files are FLAC
    /// (`audio::codec`), and recordings from before that are converted on
    /// startup. The column/field names stay so no migration is needed; the
    /// extension is the format signal.
    pub mic_cleaned_wav: Option<String>,
    pub notes: Option<String>,
    /// Wall-clock epoch ms of the last `update_notes` write; null until notes
    /// have ever been saved. Surfaced to the UI for the "Saved at" tooltip.
    pub notes_updated_at_ms: Option<i64>,
    pub segments: Vec<Segment>,
    /// raw_label -> display_name (only rows the user renamed).
    pub renames: std::collections::HashMap<String, String>,
    /// raw_label -> persona link (suggestion/confirmed) per meeting.
    pub speaker_links: std::collections::HashMap<String, crate::db::SpeakerLink>,
    pub speaker_count: i64,
    /// Customer (account) this meeting belongs to; null = unassigned.
    pub customer_id: Option<i64>,
    /// Number of segments hidden from the default transcript (echo-marked or
    /// soft-deleted). Drives the "Show N hidden" toggle in the UI.
    pub hidden_segment_count: i64,
    /// Whisper model id that produced this meeting's transcript (e.g.
    /// "large-v3-turbo"). Recorded at transcription time; null for meetings
    /// transcribed before this column existed. The Meeting Detail UI shows it
    /// as "Transcribed with: <label>" and defaults the re-transcribe dropdown
    /// to it. Echo-clean re-transcribes keep this model instead of jumping to
    /// the current global setting.
    pub asr_model: Option<String>,
}

/// A saved echo/delete mark on a segment, keyed by `(source, start_ms)` so it
/// can be re-applied after a re-transcribe replaces the segment rows (new ids,
/// possibly slightly shifted timestamps). Short-lived: snapshoted before a
/// `replace_segments` and dropped right after `reapply_marks`.
#[derive(Clone)]
pub struct SegmentMark {
    pub source: String,
    pub start_ms: u64,
    pub kind: String,
    pub deleted: bool,
}

impl Db {
    pub fn open(path: &Path, key: &[u8]) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(path).map_err(|e| format!("cannot open database: {e}"))?;

        // SQLCipher: raw 256-bit key as hex blob literal (bypasses KDF — we
        // already hold a cryptographically random key). Must be executed as
        // literal SQL, not a bound parameter, so SQLCipher parses x'...' as a
        // blob literal rather than a passphrase.
        let key_hex: String = key.iter().map(|b| format!("{:02x}", b)).collect();
        conn.execute_batch(&format!("PRAGMA key = \"x'{key_hex}'\";"))
            .map_err(|e| format!("invalid database key: {e}"))?;

        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| e.to_string())?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if version < 1 {
            conn.execute_batch(
                r#"
                BEGIN;
                CREATE TABLE meetings(
                    id          INTEGER PRIMARY KEY,
                    session_id  TEXT NOT NULL UNIQUE,
                    title       TEXT NOT NULL,
                    started_at  INTEGER NOT NULL,          -- unix epoch ms
                    ended_at    INTEGER,
                    mic_wav     TEXT,
                    system_wav  TEXT,
                    notes       TEXT
                );
                CREATE TABLE segments(
                    id          INTEGER PRIMARY KEY,
                    meeting_id  INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
                    source      TEXT NOT NULL,             -- 'mic' | 'system'
                    speaker     TEXT,                      -- 'Me' | 'SPEAKER_xx' | NULL
                    start_ms    INTEGER NOT NULL,
                    end_ms      INTEGER NOT NULL,
                    text        TEXT NOT NULL
                );
                CREATE INDEX idx_segments_meeting ON segments(meeting_id, start_ms);
                CREATE TABLE speakers(
                    id           INTEGER PRIMARY KEY,
                    meeting_id   INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
                    raw_label    TEXT NOT NULL,
                    display_name TEXT,
                    UNIQUE(meeting_id, raw_label)
                );
                CREATE TABLE summaries(
                    id          INTEGER PRIMARY KEY,
                    meeting_id  INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
                    model       TEXT NOT NULL,
                    template    TEXT NOT NULL,
                    content     TEXT NOT NULL,
                    created_at  INTEGER NOT NULL
                );
                CREATE TABLE settings(
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                PRAGMA user_version = 1;
                COMMIT;
                "#,
            )
            .map_err(|e| format!("migration failed: {e}"))?;
        }
        if version < 2 {
            conn.execute_batch(
                r#"
                BEGIN;
                CREATE TABLE personas(
                    id           INTEGER PRIMARY KEY,
                    display_name TEXT NOT NULL UNIQUE,
                    notes        TEXT,
                    created_at   INTEGER NOT NULL,
                    updated_at   INTEGER NOT NULL
                );
                CREATE TABLE voiceprints(
                    id                INTEGER PRIMARY KEY,
                    persona_id        INTEGER NOT NULL REFERENCES personas(id) ON DELETE CASCADE,
                    embedding         BLOB NOT NULL,
                    dim               INTEGER NOT NULL,
                    source_meeting_id INTEGER REFERENCES meetings(id) ON DELETE SET NULL,
                    source_label      TEXT,
                    speech_ms         INTEGER NOT NULL,
                    created_at        INTEGER NOT NULL
                );
                CREATE INDEX idx_voiceprints_persona ON voiceprints(persona_id);
                CREATE TABLE speaker_persona_links(
                    meeting_id  INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
                    raw_label   TEXT NOT NULL,
                    persona_id  INTEGER REFERENCES personas(id) ON DELETE SET NULL,
                    confidence  REAL,
                    confirmed   INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY(meeting_id, raw_label)
                );
                PRAGMA user_version = 2;
                COMMIT;
                "#,
            )
            .map_err(|e| format!("migration to v2 failed: {e}"))?;
        }
        if version < 3 {
            conn.execute_batch(
                r#"
                BEGIN;
                ALTER TABLE meetings ADD COLUMN notes_updated_at INTEGER;
                PRAGMA user_version = 3;
                COMMIT;
                "#,
            )
            .map_err(|e| format!("migration to v3 failed: {e}"))?;
        }
        if version < 4 {
            conn.execute_batch(
                r#"
                BEGIN;
                CREATE TABLE customers(
                    id          INTEGER PRIMARY KEY,
                    name        TEXT NOT NULL,
                    logo        TEXT,
                    notes       TEXT,
                    created_at  INTEGER NOT NULL,
                    updated_at  INTEGER NOT NULL
                );
                ALTER TABLE meetings ADD COLUMN customer_id INTEGER REFERENCES customers(id) ON DELETE SET NULL;
                CREATE INDEX idx_meetings_customer ON meetings(customer_id);
                CREATE TABLE customer_summaries(
                    id              INTEGER PRIMARY KEY,
                    customer_id     INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
                    model           TEXT NOT NULL,
                    content         TEXT NOT NULL,
                    from_meeting_ids TEXT NOT NULL,
                    created_at      INTEGER NOT NULL
                );
                CREATE INDEX idx_customer_summaries ON customer_summaries(customer_id, created_at DESC);
                PRAGMA user_version = 4;
                COMMIT;
                "#,
            )
            .map_err(|e| format!("migration to v4 failed: {e}"))?;
        }
        if version < 5 {
            // Echo handling: per-segment kind ('speech' | 'echo') + soft-delete
            // flag, and a cleaned-mic path on the meeting (written by the
            // offline echo re-processing command). Defaults make existing rows
            // appear as normal speech / not-deleted / no cleaned mic.
            conn.execute_batch(
                r#"
                BEGIN;
                ALTER TABLE segments ADD COLUMN kind TEXT NOT NULL DEFAULT 'speech';
                ALTER TABLE segments ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;
                ALTER TABLE meetings ADD COLUMN mic_cleaned_wav TEXT;
                PRAGMA user_version = 5;
                COMMIT;
                "#,
            )
            .map_err(|e| format!("migration to v5 failed: {e}"))?;
        }
        if version < 6 {
            // Per-meeting ASR model tracking. Records which whisper model
            // produced each meeting's transcript, so the UI can show it and
            // re-transcribe with the same (or a different) model without
            // touching the global live setting. Nullable: meetings transcribed
            // before this migration have NULL and the UI treats that as
            // "unknown" (no "Transcribed with" label; dropdown defaults to the
            // global active model).
            conn.execute_batch(
                r#"
                BEGIN;
                ALTER TABLE meetings ADD COLUMN asr_model TEXT;
                PRAGMA user_version = 6;
                COMMIT;
                "#,
            )
            .map_err(|e| format!("migration to v6 failed: {e}"))?;
        }
        if version < 7 {
            // Tasks: to-dos tied to a customer, created manually or AI-extracted
            // from a transcript. `customer_id` is nullable to hold customer-less
            // AI suggestions until they're accepted (a customer is assigned at
            // accept time); manual creation always sets it. `ON DELETE CASCADE`
            // on customer_id is a deliberate product choice — deleting a customer
            // takes its tasks with it (unlike meetings, which are orphaned).
            // `source_meeting_id` is `ON DELETE SET NULL` so an accepted task
            // survives its meeting being deleted (it just loses the link);
            // pending suggestions are dropped app-level in `delete_meeting`.
            // The transcript-segment reference is a *timestamp anchor*
            // (`source_start_ms`), NOT a `segments.id` FK — segment ids are
            // unstable across re-transcribe (see `replace_segments`); resolve to
            // a segment at display time with the same ±250ms tolerance
            // `reapply_marks` uses.
            conn.execute_batch(
                r#"
                BEGIN;
                CREATE TABLE tasks(
                    id                INTEGER PRIMARY KEY,
                    customer_id       INTEGER REFERENCES customers(id) ON DELETE CASCADE,
                    source_meeting_id INTEGER REFERENCES meetings(id)  ON DELETE SET NULL,
                    source_start_ms   INTEGER,
                    source_end_ms     INTEGER,
                    snippet           TEXT,
                    title             TEXT NOT NULL,
                    description       TEXT,
                    status            TEXT NOT NULL DEFAULT 'open',
                    priority          TEXT NOT NULL DEFAULT 'normal',
                    due_at            INTEGER,
                    origin            TEXT NOT NULL DEFAULT 'manual',
                    created_at        INTEGER NOT NULL,
                    completed_at      INTEGER
                );
                CREATE INDEX idx_tasks_customer ON tasks(customer_id, status);
                CREATE INDEX idx_tasks_meeting  ON tasks(source_meeting_id, status);
                CREATE INDEX idx_tasks_due      ON tasks(due_at, status);
                PRAGMA user_version = 7;
                COMMIT;
                "#,
            )
            .map_err(|e| format!("migration to v7 failed: {e}"))?;
        }
        if version < 8 {
            // The cluster count the meeting was last diarized with (NULL =
            // automatic). Re-transcribe re-diarizes the unchanged system
            // audio; feeding sherpa the same k makes that run reproduce the
            // original clustering (it's deterministic), so `SPEAKER_xx`
            // labels — the key for renames, persona links and voiceprints —
            // stay put. See `transcript::remap_labels` for the safety net.
            conn.execute_batch(
                r#"
                BEGIN;
                ALTER TABLE meetings ADD COLUMN diarize_num_speakers INTEGER;
                PRAGMA user_version = 8;
                COMMIT;
                "#,
            )
            .map_err(|e| format!("migration to v8 failed: {e}"))?;
        }
        if version < 9 {
            // Per-meeting speaker embedding (CAM++, L2-normalized, packed
            // f32 LE like `voiceprints.embedding`), captured when identify
            // runs after diarization. Lets a persona confirm enroll the
            // voiceprint instantly and atomically instead of re-reading the
            // audio — and even after the audio has been deleted. Rows go
            // with their label in `reconcile_speakers`, so an embedding
            // never outlives the speaker it describes.
            conn.execute_batch(
                r#"
                BEGIN;
                ALTER TABLE speakers ADD COLUMN embedding BLOB;
                ALTER TABLE speakers ADD COLUMN embedding_dim INTEGER;
                ALTER TABLE speakers ADD COLUMN speech_ms INTEGER;
                PRAGMA user_version = 9;
                COMMIT;
                "#,
            )
            .map_err(|e| format!("migration to v9 failed: {e}"))?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Settings (key/value; AppSettings serialized field-per-key)
    // -----------------------------------------------------------------------

    pub fn get_settings(&self) -> AppSettings {
        let conn = self.conn.lock().unwrap();
        let get = |key: &str| -> Option<String> {
            conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
        };
        let mut s = AppSettings::default();
        if let Some(v) = get("asr_model") {
            s.asr_model = v;
        }
        if let Some(v) = get("live_transcription") {
            s.live_transcription = v == "true";
        }
        if let Some(v) = get("storage_dir") {
            if !v.is_empty() {
                s.storage_dir = Some(v);
            }
        }
        if let Some(v) = get("delete_audio_after_transcription") {
            s.delete_audio_after_transcription = v == "true";
        }
        if let Some(v) = get("summary_model") {
            if !v.is_empty() {
                s.summary_model = Some(v);
            }
        }
        if let Some(v) = get("summary_template") {
            if !v.is_empty() {
                s.summary_template = Some(v);
            }
        }
        if let Some(v) = get("persona_auto_threshold") {
            if let Ok(f) = v.parse::<f32>() {
                s.persona_auto_threshold = f;
            }
        }
        if let Some(v) = get("persona_suggest_threshold") {
            if let Ok(f) = v.parse::<f32>() {
                s.persona_suggest_threshold = f;
            }
        }
        if let Some(v) = get("persona_live_threshold") {
            if let Ok(f) = v.parse::<f32>() {
                s.persona_live_threshold = f;
            }
        }
        if let Some(v) = get("voiceprint_gallery_cap") {
            if let Ok(n) = v.parse::<i32>() {
                s.voiceprint_gallery_cap = n;
            }
        }
        if let Some(v) = get("aec_enabled") {
            s.aec_enabled = v == "true";
        }
        if let Some(v) = get("aec_aggressiveness") {
            s.aec_aggressiveness = crate::settings::AecAggressiveness::parse(&v);
        }
        if let Some(v) = get("onboarding_complete") {
            s.onboarding_complete = v == "true";
        }
        if let Some(v) = get("summary_backend") {
            s.summary_backend = v;
        }
        if let Some(v) = get("mcp_enabled") {
            s.mcp_enabled = v == "true";
        }
        if let Some(v) = get("mcp_port") {
            if let Ok(p) = v.parse::<u16>() {
                s.mcp_port = p;
            }
        }
        if let Some(v) = get("mcp_token") {
            if !v.is_empty() {
                s.mcp_token = Some(v);
            }
        }
        s
    }

    pub fn set_settings(&self, s: &AppSettings) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let put = |key: &str, value: String| -> Result<(), String> {
            conn.execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        };
        put("asr_model", s.asr_model.clone())?;
        put("live_transcription", s.live_transcription.to_string())?;
        put("storage_dir", s.storage_dir.clone().unwrap_or_default())?;
        put(
            "delete_audio_after_transcription",
            s.delete_audio_after_transcription.to_string(),
        )?;
        put("summary_model", s.summary_model.clone().unwrap_or_default())?;
        put(
            "summary_template",
            s.summary_template.clone().unwrap_or_default(),
        )?;
        put(
            "persona_auto_threshold",
            s.persona_auto_threshold.to_string(),
        )?;
        put(
            "persona_suggest_threshold",
            s.persona_suggest_threshold.to_string(),
        )?;
        put(
            "persona_live_threshold",
            s.persona_live_threshold.to_string(),
        )?;
        put(
            "voiceprint_gallery_cap",
            s.voiceprint_gallery_cap.to_string(),
        )?;
        put("aec_enabled", s.aec_enabled.to_string())?;
        put(
            "aec_aggressiveness",
            s.aec_aggressiveness.as_str().to_string(),
        )?;
        put("onboarding_complete", s.onboarding_complete.to_string())?;
        put("summary_backend", s.summary_backend.clone())?;
        put("mcp_enabled", s.mcp_enabled.to_string())?;
        put("mcp_port", s.mcp_port.to_string())?;
        put("mcp_token", s.mcp_token.clone().unwrap_or_default())?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Meetings
    // -----------------------------------------------------------------------

    /// Insert a meeting row the moment recording starts, so notes taken
    /// during the live recording have a row to attach to. `ended_at` and the
    /// WAV paths are NULL until `finalize_meeting` is called on stop.
    pub fn insert_meeting_started(
        &self,
        session_id: &str,
        title: &str,
        started_at_ms: i64,
    ) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO meetings(session_id, title, started_at, ended_at, mic_wav, system_wav)
             VALUES(?1, ?2, ?3, NULL, NULL, NULL)",
            params![session_id, title, started_at_ms],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    /// Fill in `ended_at` and the recording paths on a row created by
    /// `insert_meeting_started`. Called when recording stops.
    pub fn finalize_meeting(
        &self,
        id: i64,
        ended_at_ms: i64,
        mic_wav: &str,
        system_wav: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE meetings SET ended_at = ?2, mic_wav = ?3, system_wav = ?4 WHERE id = ?1",
            params![id, ended_at_ms, mic_wav, system_wav],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Look up a meeting id by session_id (session_id is UNIQUE). Used by
    /// `stop_recording` to find the row created at start.
    pub fn meeting_id_by_session(&self, session_id: &str) -> Result<Option<i64>, String> {
        let conn = self.conn.lock().unwrap();
        let id = conn
            .query_row(
                "SELECT id FROM meetings WHERE session_id = ?1",
                params![session_id],
                |r| r.get::<_, i64>(0),
            )
            .ok();
        Ok(id)
    }

    /// Checkpoint the WAL file (TRUNCATE mode). Best-effort; called on
    /// shutdown to reclaim WAL space. Errors are logged by the caller.
    pub fn wal_checkpoint(&self) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_meetings(&self, search: Option<&str>) -> Result<Vec<MeetingSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let like = search
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("%{}%", s.trim()));
        let sql = r#"
            SELECT m.id, m.title, m.started_at, m.ended_at, m.mic_wav,
                   (SELECT COUNT(*) FROM segments s WHERE s.meeting_id = m.id),
                   (SELECT COUNT(DISTINCT
                       CASE
                         WHEN s.speaker = 'Me' THEN 'me'
                         ELSE COALESCE(
                           (SELECT 'p:' || l.persona_id
                            FROM speaker_persona_links l
                            WHERE l.meeting_id = m.id
                              AND l.raw_label = s.speaker
                              AND l.persona_id IS NOT NULL
                              AND l.confirmed = 1
                            LIMIT 1),
                           s.speaker)
                       END)
                    FROM segments s
                    WHERE s.meeting_id = m.id AND s.speaker IS NOT NULL),
                   (SELECT s.text FROM segments s WHERE s.meeting_id = m.id
                     ORDER BY s.start_ms LIMIT 1)
            FROM meetings m
            WHERE m.ended_at IS NOT NULL
              AND (?1 IS NULL
               OR m.title LIKE ?1
               OR EXISTS(SELECT 1 FROM segments s
                          WHERE s.meeting_id = m.id AND s.text LIKE ?1))
            ORDER BY m.started_at DESC
        "#;
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![like], |r| {
                let ended: Option<i64> = r.get(3)?;
                let started: i64 = r.get(2)?;
                let mic: Option<String> = r.get(4)?;
                Ok(MeetingSummary {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    started_at_ms: started,
                    duration_ms: ended.map(|e| e - started),
                    segment_count: r.get(5)?,
                    speaker_count: r.get(6)?,
                    preview: r.get(7)?,
                    has_audio: mic.is_some(),
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    /// Filtered + paginated variant of `list_meetings` for the MCP server.
    /// Returns `(rows, total)` where `total` is the match count before
    /// `LIMIT/OFFSET`. `%`/`_` in the query are escaped so they match
    /// literally (the UI's `list_meetings` keeps SQLite's wildcard semantics).
    pub fn list_meetings_filtered(
        &self,
        f: &MeetingFilter,
    ) -> Result<(Vec<MeetingIndexRow>, i64), String> {
        let conn = self.conn.lock().unwrap();
        let like = f
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| format!("%{}%", escape_like(q)));
        const WHERE: &str = r#"
            WHERE m.ended_at IS NOT NULL
              AND (?1 IS NULL
               OR m.title LIKE ?1 ESCAPE '\'
               OR EXISTS(SELECT 1 FROM segments s
                          WHERE s.meeting_id = m.id AND s.text LIKE ?1 ESCAPE '\'))
              AND (?2 IS NULL OR m.customer_id = ?2)
              AND (?3 IS NULL OR m.started_at >= ?3)
              AND (?4 IS NULL OR m.started_at <= ?4)
        "#;
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM meetings m {WHERE}"),
                params![like, f.customer_id, f.from_ms, f.to_ms],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let sql = format!(
            r#"
            SELECT m.id, m.title, m.started_at, m.ended_at, m.customer_id,
                   (SELECT COUNT(*) FROM segments s WHERE s.meeting_id = m.id),
                   (SELECT COUNT(DISTINCT
                       CASE
                         WHEN s.speaker = 'Me' THEN 'me'
                         ELSE COALESCE(
                           (SELECT 'p:' || l.persona_id
                            FROM speaker_persona_links l
                            WHERE l.meeting_id = m.id
                              AND l.raw_label = s.speaker
                              AND l.persona_id IS NOT NULL
                              AND l.confirmed = 1
                            LIMIT 1),
                           s.speaker)
                       END)
                    FROM segments s
                    WHERE s.meeting_id = m.id AND s.speaker IS NOT NULL),
                   EXISTS(SELECT 1 FROM summaries su WHERE su.meeting_id = m.id),
                   (m.notes IS NOT NULL AND m.notes != ''),
                   (SELECT s.text FROM segments s WHERE s.meeting_id = m.id
                     ORDER BY s.start_ms LIMIT 1)
            FROM meetings m
            {WHERE}
            ORDER BY m.started_at DESC
            LIMIT ?5 OFFSET ?6
        "#
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![
                    like,
                    f.customer_id,
                    f.from_ms,
                    f.to_ms,
                    f.limit.max(0),
                    f.offset.max(0)
                ],
                |r| {
                    let started: i64 = r.get(2)?;
                    let ended: Option<i64> = r.get(3)?;
                    Ok(MeetingIndexRow {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        started_at_ms: started,
                        duration_ms: ended.map(|e| e - started),
                        customer_id: r.get(4)?,
                        segment_count: r.get(5)?,
                        speaker_count: r.get(6)?,
                        has_summary: r.get(7)?,
                        has_notes: r.get(8)?,
                        preview: r.get(9)?,
                    })
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok((rows, total))
    }

    pub fn get_meeting(&self, id: i64) -> Result<MeetingDetail, String> {
        let conn = self.conn.lock().unwrap();
        let (session_id, title, started_at_ms, ended_at_ms, mic_wav, system_wav, mic_cleaned_wav, notes, notes_updated_at_ms, customer_id, asr_model) = conn
            .query_row(
                "SELECT session_id, title, started_at, ended_at, mic_wav, system_wav, mic_cleaned_wav, notes, notes_updated_at, customer_id, asr_model
                 FROM meetings WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, Option<i64>>(8)?,
                        r.get::<_, Option<i64>>(9)?,
                        r.get::<_, Option<String>>(10)?,
                    ))
                },
            )
            .map_err(|e| format!("meeting {id} not found: {e}"))?;

        let segments: Vec<Segment> = {
            let mut stmt = conn
                .prepare(
                    // Hide echo-marked + soft-deleted segments from the default
                    // transcript + summary (user chose "hide entirely"). The
                    // "show hidden" UI path uses `meeting_segments_all`.
                    "SELECT id, source, speaker, start_ms, end_ms, text, kind, deleted
                     FROM segments
                     WHERE meeting_id = ?1 AND deleted = 0 AND kind = 'speech'
                     ORDER BY start_ms",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![id], |r| {
                    Ok(Segment {
                        id: r.get(0)?,
                        source: r.get(1)?,
                        speaker: r.get(2)?,
                        start_ms: r.get::<_, i64>(3)? as u64,
                        end_ms: r.get::<_, i64>(4)? as u64,
                        text: r.get(5)?,
                        kind: r.get(6)?,
                        deleted: r.get::<_, i64>(7)? == 1,
                    })
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };

        let hidden_segment_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM segments
                 WHERE meeting_id = ?1 AND (deleted = 1 OR kind = 'echo')",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;

        let renames: std::collections::HashMap<String, String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT raw_label, display_name FROM speakers
                     WHERE meeting_id = ?1 AND display_name IS NOT NULL",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<std::collections::HashMap<_, _>, _>>()
                .map_err(|e| e.to_string())?
        };

        // Inlined links query (NOT self.meeting_speaker_links — that would
        // re-lock self.conn and deadlock under std::sync::Mutex).
        let speaker_links: std::collections::HashMap<String, SpeakerLink> = {
            let mut stmt = conn
                .prepare(
                    "SELECT l.raw_label, l.persona_id, p.display_name, l.confidence, l.confirmed
                     FROM speaker_persona_links l
                     LEFT JOIN personas p ON p.id = l.persona_id
                     WHERE l.meeting_id = ?1
                     ORDER BY l.raw_label",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        SpeakerLink {
                            raw_label: r.get(0)?,
                            persona_id: r.get(1)?,
                            persona_name: r.get(2)?,
                            confidence: r.get(3)?,
                            confirmed: r.get::<_, i64>(4)? == 1,
                        },
                    ))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<std::collections::HashMap<_, _>, _>>()
                .map_err(|e| e.to_string())?
        };

        // Drop the lock before calling count_speaker_identities to avoid deadlock.
        drop(conn);

        let speaker_count = self.count_speaker_identities(id)?;

        Ok(MeetingDetail {
            id,
            session_id,
            title,
            started_at_ms,
            ended_at_ms,
            mic_wav,
            system_wav,
            mic_cleaned_wav,
            notes,
            notes_updated_at_ms,
            segments,
            renames,
            speaker_links,
            speaker_count,
            customer_id,
            hidden_segment_count,
            asr_model,
        })
    }

    /// Count distinct speaker identities for a meeting.
    ///
    /// Rules:
    /// - "Me" always counts as exactly one identity.
    /// - Raw labels linked to the same confirmed persona collapse to one identity.
    /// - Unlinked or merely-suggested raw labels each count separately.
    /// - NULL speakers are ignored.
    pub fn count_speaker_identities(&self, meeting_id: i64) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                r#"
                SELECT COUNT(DISTINCT
                    CASE
                      WHEN s.speaker = 'Me' THEN 'me'
                      ELSE COALESCE(
                        (SELECT 'p:' || l.persona_id
                         FROM speaker_persona_links l
                         WHERE l.meeting_id = s.meeting_id
                           AND l.raw_label = s.speaker
                           AND l.persona_id IS NOT NULL
                           AND l.confirmed = 1
                         LIMIT 1),
                        s.speaker)
                    END)
                FROM segments s
                WHERE s.meeting_id = ?1 AND s.speaker IS NOT NULL
                "#,
                params![meeting_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(count)
    }

    pub fn update_title(&self, id: i64, title: &str) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE meetings SET title = ?2 WHERE id = ?1",
                params![id, title],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn update_notes(&self, id: i64, notes: &str) -> Result<(), String> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE meetings SET notes = ?2, notes_updated_at = ?3 WHERE id = ?1",
                params![id, notes, now_ms],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Delete a meeting row. Returns its audio paths so the caller can remove
    /// the files from disk.
    pub fn delete_meeting(&self, id: i64) -> Result<AudioPaths, String> {
        let conn = self.conn.lock().unwrap();
        let paths = conn
            .query_row(
                "SELECT mic_wav, system_wav, mic_cleaned_wav FROM meetings WHERE id = ?1",
                params![id],
                |r| {
                    Ok(AudioPaths {
                        mic: r.get(0)?,
                        system: r.get(1)?,
                        cleaned: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        conn.execute("DELETE FROM meetings WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(paths)
    }

    /// Clear all audio paths, including the echo-cleaned mic (used by
    /// "delete audio after transcription").
    pub fn clear_audio_paths(&self, id: i64) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE meetings SET mic_wav = NULL, system_wav = NULL, mic_cleaned_wav = NULL
                 WHERE id = ?1",
                params![id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Point a meeting at (re)encoded audio files. Used by the WAV → FLAC
    /// migration; `None` clears a column.
    pub fn set_audio_paths(&self, id: i64, paths: &AudioPaths) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE meetings SET mic_wav = ?2, system_wav = ?3, mic_cleaned_wav = ?4
                 WHERE id = ?1",
                params![id, paths.mic, paths.system, paths.cleaned],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Finished meetings that still reference a legacy `.wav` recording in
    /// any audio column, oldest first. Empty once the migration has run.
    pub fn meetings_with_wav_audio(&self) -> Result<Vec<(i64, AudioPaths)>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, mic_wav, system_wav, mic_cleaned_wav FROM meetings
                 WHERE ended_at IS NOT NULL
                   AND (mic_wav LIKE '%.wav' OR system_wav LIKE '%.wav'
                        OR mic_cleaned_wav LIKE '%.wav')
                 ORDER BY started_at ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    AudioPaths {
                        mic: r.get(1)?,
                        system: r.get(2)?,
                        cleaned: r.get(3)?,
                    },
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Record the path of an offline echo-cleaned mic recording produced by
    /// `clean_echo`. The original mic file is preserved so the action is
    /// revertible via `clear_mic_cleaned_wav`.
    pub fn set_mic_cleaned_wav(&self, id: i64, path: &str) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE meetings SET mic_cleaned_wav = ?2 WHERE id = ?1",
                params![id, path],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Record which whisper model produced this meeting's transcript. Called
    /// after every successful transcription (first-pass, re-transcribe, and
    /// echo-clean re-transcribe) so the UI can show "Transcribed with" and
    /// default the re-transcribe dropdown to it.
    pub fn set_meeting_asr_model(&self, id: i64, model_id: &str) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE meetings SET asr_model = ?2 WHERE id = ?1",
                params![id, model_id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Record the remote-speaker count the meeting was diarized with
    /// (`None` = automatic estimation).
    pub fn set_diarize_num_speakers(&self, id: i64, n: Option<i32>) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE meetings SET diarize_num_speakers = ?2 WHERE id = ?1",
                params![id, n],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The remote-speaker count from the last diarization, if one was
    /// declared (NULL/None = automatic, including meetings diarized before
    /// this column existed).
    pub fn diarize_num_speakers(&self, id: i64) -> Result<Option<i32>, String> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT diarize_num_speakers FROM meetings WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| format!("meeting {id} not found: {e}"))
    }

    /// Drop the echo-cleaned mic pointer, reverting to the original mic file
    /// for playback and re-transcription. Returns the path that was stored
    /// (if any) so the caller can remove the file from disk.
    pub fn clear_mic_cleaned_wav(&self, id: i64) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap();
        let previous: Option<String> = conn
            .query_row(
                "SELECT mic_cleaned_wav FROM meetings WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        conn.execute(
            "UPDATE meetings SET mic_cleaned_wav = NULL WHERE id = ?1",
            params![id],
        )
        .map_err(|e| e.to_string())?;
        Ok(previous)
    }

    // -----------------------------------------------------------------------
    // Segments + speakers
    // -----------------------------------------------------------------------

    /// Replace a meeting's segments (idempotent for re-transcription).
    pub fn replace_segments(&self, meeting_id: i64, segments: &[Segment]) -> Result<(), String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM segments WHERE meeting_id = ?1",
            params![meeting_id],
        )
        .map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO segments(meeting_id, source, speaker, start_ms, end_ms, text, kind, deleted)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(|e| e.to_string())?;
            for s in segments {
                stmt.execute(params![
                    meeting_id,
                    s.source,
                    s.speaker,
                    s.start_ms as i64,
                    s.end_ms as i64,
                    s.text,
                    s.kind,
                    s.deleted as i64
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn rename_speaker(
        &self,
        meeting_id: i64,
        raw_label: &str,
        display_name: Option<&str>,
    ) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO speakers(meeting_id, raw_label, display_name) VALUES(?1, ?2, ?3)
                 ON CONFLICT(meeting_id, raw_label) DO UPDATE SET display_name = excluded.display_name",
                params![meeting_id, raw_label, display_name],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// (mic, system) recording paths for a meeting, if still present.
    pub fn meeting_wavs(&self, id: i64) -> Result<(Option<String>, Option<String>), String> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT mic_wav, system_wav FROM meetings WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| format!("meeting {id} not found: {e}"))
    }

    pub fn meeting_segments(&self, id: i64) -> Result<Vec<Segment>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                // Default view: hide echo-marked + soft-deleted segments.
                "SELECT id, source, speaker, start_ms, end_ms, text, kind, deleted
                 FROM segments
                 WHERE meeting_id = ?1 AND deleted = 0 AND kind = 'speech'
                 ORDER BY start_ms",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![id], |r| {
                Ok(Segment {
                    id: r.get(0)?,
                    source: r.get(1)?,
                    speaker: r.get(2)?,
                    start_ms: r.get::<_, i64>(3)? as u64,
                    end_ms: r.get::<_, i64>(4)? as u64,
                    text: r.get(5)?,
                    kind: r.get(6)?,
                    deleted: r.get::<_, i64>(7)? == 1,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    /// Every segment for a meeting, including echo-marked + soft-deleted ones,
    /// with `kind`/`deleted` populated. Used by the "show hidden" UI path and
    /// by `clean_echo` to collect echo windows.
    pub fn meeting_segments_all(&self, id: i64) -> Result<Vec<Segment>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, source, speaker, start_ms, end_ms, text, kind, deleted
                 FROM segments WHERE meeting_id = ?1 ORDER BY start_ms",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![id], |r| {
                Ok(Segment {
                    id: r.get(0)?,
                    source: r.get(1)?,
                    speaker: r.get(2)?,
                    start_ms: r.get::<_, i64>(3)? as u64,
                    end_ms: r.get::<_, i64>(4)? as u64,
                    text: r.get(5)?,
                    kind: r.get(6)?,
                    deleted: r.get::<_, i64>(7)? == 1,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    /// Mark a single segment as echo (`kind = 'echo'`) or back to speech.
    pub fn set_segment_kind(&self, id: i64, kind: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE segments SET kind = ?2 WHERE id = ?1",
            params![id, kind],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Soft-delete / restore a single segment.
    pub fn set_segment_deleted(&self, id: i64, deleted: bool) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE segments SET deleted = ?2 WHERE id = ?1",
            params![id, deleted as i64],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Snapshot all non-default marks (echo or deleted) before a
    /// `replace_segments` so they can be re-applied afterward. Returns marks
    /// keyed by the old segments' (source, start_ms).
    pub fn snapshot_marks(&self, meeting_id: i64) -> Result<Vec<SegmentMark>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT source, start_ms, kind, deleted FROM segments
                 WHERE meeting_id = ?1 AND (kind = 'echo' OR deleted = 1)",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![meeting_id], |r| {
                Ok(SegmentMark {
                    source: r.get(0)?,
                    start_ms: r.get::<_, i64>(1)? as u64,
                    kind: r.get(2)?,
                    deleted: r.get::<_, i64>(3)? == 1,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    /// Re-apply snapshoted marks to the current segment rows, matching by
    /// `(source, start_ms)` within ±250 ms (re-transcribe can shift segment
    /// bounds a little). Marks that no longer match any row are dropped — a
    /// re-transcribe genuinely changes the segmentation.
    pub fn reapply_marks(&self, meeting_id: i64, marks: &[SegmentMark]) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        for m in marks {
            conn.execute(
                "UPDATE segments SET kind = ?3, deleted = ?4
                 WHERE meeting_id = ?1 AND source = ?2
                   AND start_ms BETWEEN ?5 AND ?6",
                params![
                    meeting_id,
                    m.source,
                    m.kind,
                    m.deleted as i64,
                    m.start_ms.saturating_sub(250) as i64,
                    (m.start_ms + 250) as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRow {
    pub id: i64,
    pub model: String,
    pub content: String,
    pub created_at_ms: i64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Persona {
    pub id: i64,
    pub display_name: String,
    pub notes: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub voiceprint_count: i64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerLink {
    pub raw_label: String,
    pub persona_id: Option<i64>,
    pub persona_name: Option<String>,
    pub confidence: Option<f32>,
    pub confirmed: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CustomerSummary {
    pub id: i64,
    pub name: String,
    pub logo: Option<String>,
    pub meeting_count: i64,
    pub last_meeting_at_ms: Option<i64>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CustomerRosterEntry {
    pub persona_id: i64,
    pub display_name: String,
    pub meeting_count: i64,
    pub last_seen_ms: Option<i64>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CustomerRollupRow {
    pub id: i64,
    pub model: String,
    pub content: String,
    pub created_at_ms: i64,
    pub meeting_count: i64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CustomerDetail {
    pub id: i64,
    pub name: String,
    pub logo: Option<String>,
    pub notes: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub meeting_count: i64,
    pub last_meeting_at_ms: Option<i64>,
    pub first_meeting_at_ms: Option<i64>,
    pub total_duration_ms: Option<i64>,
    pub persona_roster: Vec<CustomerRosterEntry>,
    pub meetings: Vec<MeetingSummary>,
    /// Count of this customer's meetings that have at least one saved summary
    /// (drives whether a customer rollup can be generated).
    pub meetings_with_summary_count: i64,
    pub latest_rollup: Option<CustomerRollupRow>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CustomerSearchHit {
    pub field: String,
    pub snippet: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CustomerSearchResult {
    pub meeting_id: i64,
    pub title: String,
    pub started_at_ms: i64,
    pub hits: Vec<CustomerSearchHit>,
}

impl Db {
    // -----------------------------------------------------------------------
    // Summaries (milestone 6)
    // -----------------------------------------------------------------------

    pub fn insert_summary(
        &self,
        meeting_id: i64,
        model: &str,
        template: &str,
        content: &str,
    ) -> Result<i64, String> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO summaries(meeting_id, model, template, content, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![meeting_id, model, template, content, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    /// Summaries for a meeting, newest first.
    pub fn list_summaries(&self, meeting_id: i64) -> Result<Vec<SummaryRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, model, content, created_at FROM summaries
                 WHERE meeting_id = ?1 ORDER BY created_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![meeting_id], |r| {
                Ok(SummaryRow {
                    id: r.get(0)?,
                    model: r.get(1)?,
                    content: r.get(2)?,
                    created_at_ms: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
}

impl Db {
    // -------------------------------------------------------------------
    // Personas + voiceprints (milestone 9)
    // -------------------------------------------------------------------

    pub fn list_personas(&self) -> Result<Vec<Persona>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT p.id, p.display_name, p.notes, p.created_at, p.updated_at,
                        (SELECT COUNT(*) FROM voiceprints v WHERE v.persona_id = p.id)
                 FROM personas p ORDER BY p.display_name COLLATE NOCASE",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Persona {
                    id: r.get(0)?,
                    display_name: r.get(1)?,
                    notes: r.get(2)?,
                    created_at_ms: r.get(3)?,
                    updated_at_ms: r.get(4)?,
                    voiceprint_count: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    /// Used by `personas::rank_personas` — loads embeddings unpacked.
    pub fn list_personas_with_voiceprints(
        &self,
    ) -> Result<Vec<crate::personas::PersonaWithEmbeddings>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, display_name FROM personas ORDER BY id")
            .map_err(|e| e.to_string())?;
        let personas: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);

        let mut out = Vec::with_capacity(personas.len());
        for (id, display_name) in personas {
            let mut stmt2 = conn
                .prepare("SELECT embedding, dim FROM voiceprints WHERE persona_id = ?1")
                .map_err(|e| e.to_string())?;
            let embeddings: Vec<Vec<f32>> = stmt2
                .query_map(params![id], |r| {
                    let blob: Vec<u8> = r.get(0)?;
                    let dim: i64 = r.get(1)?;
                    Ok(crate::voiceprint::unpack_f32(
                        &blob[..blob.len().min(dim as usize * 4)],
                    ))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            out.push(crate::personas::PersonaWithEmbeddings {
                id,
                display_name,
                embeddings,
            });
        }
        Ok(out)
    }

    pub fn create_persona(&self, display_name: &str) -> Result<i64, String> {
        let name = display_name.trim();
        if name.is_empty() {
            return Err("persona name cannot be empty".into());
        }
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO personas(display_name, created_at, updated_at) VALUES(?1, ?2, ?2)",
            params![name, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    pub fn rename_persona(&self, persona_id: i64, display_name: &str) -> Result<(), String> {
        let name = display_name.trim();
        if name.is_empty() {
            return Err("persona name cannot be empty".into());
        }
        let now = chrono::Utc::now().timestamp_millis();
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE personas SET display_name = ?2, updated_at = ?3 WHERE id = ?1",
                params![persona_id, name, now],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Delete a persona: cascade-removes its voiceprints and SET NULLs the
    /// `persona_id` on `speaker_persona_links` (FK). Also clears the
    /// per-meeting `speakers.display_name` for every label that was linked
    /// to this persona, so the transcript falls back to the raw
    /// `SPEAKER_xx` label instead of retaining the deleted persona's name.
    pub fn delete_persona(&self, persona_id: i64) -> Result<(), String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE speakers SET display_name = NULL
             WHERE rowid IN (
                 SELECT s.rowid FROM speakers s
                 JOIN speaker_persona_links l
                   ON s.meeting_id = l.meeting_id AND s.raw_label = l.raw_label
                 WHERE l.persona_id = ?1
             )",
            params![persona_id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM personas WHERE id = ?1", params![persona_id])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Hard-delete every voiceprint row (personas kept).
    pub fn delete_all_voiceprints(&self) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM voiceprints", [])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Insert a voiceprint. If the persona now exceeds `cap`, drop the most
    /// redundant print (see `prune_redundant`) — keeping the gallery diverse
    /// and quality-weighted rather than FIFO.
    ///
    /// A print is keyed by where it came from: when both `source_meeting_id`
    /// and `source_label` are given, any existing print with that source —
    /// under *any* persona — is replaced in the same transaction. One
    /// speaker in one meeting therefore backs at most one persona, even if a
    /// late background enrollment lands after the user re-assigned the
    /// speaker.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_voiceprint(
        &self,
        persona_id: i64,
        embedding: &[f32],
        dim: i32,
        source_meeting_id: Option<i64>,
        source_label: Option<&str>,
        speech_ms: u64,
        cap: i32,
    ) -> Result<(), String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        insert_voiceprint_tx(
            &tx,
            persona_id,
            embedding,
            dim,
            source_meeting_id,
            source_label,
            speech_ms,
            cap,
        )?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Store a speaker's per-meeting embedding (see migration v9). Upserts
    /// so it works whether or not the row exists yet; the rename is left
    /// alone.
    pub fn set_speaker_embedding(
        &self,
        meeting_id: i64,
        raw_label: &str,
        embedding: &[f32],
        speech_ms: u64,
    ) -> Result<(), String> {
        let blob = crate::voiceprint::pack_f32(embedding);
        let dim = embedding.len() as i64;
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO speakers(meeting_id, raw_label, embedding, embedding_dim, speech_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(meeting_id, raw_label) DO UPDATE SET
                    embedding = excluded.embedding,
                    embedding_dim = excluded.embedding_dim,
                    speech_ms = excluded.speech_ms",
                params![meeting_id, raw_label, blob, dim, speech_ms as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Upsert an automatic suggestion into `speaker_persona_links`. A row the
    /// user has confirmed is frozen: neither `persona_id` nor `confidence`
    /// changes (the chip only shows confidence for unconfirmed links, so a
    /// stale value there is never visible). Re-running identify can
    /// therefore never flip or clear a human decision.
    pub fn upsert_link(
        &self,
        meeting_id: i64,
        raw_label: &str,
        persona_id: Option<i64>,
        confidence: Option<f32>,
    ) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO speaker_persona_links(meeting_id, raw_label, persona_id, confidence, confirmed)
                 VALUES(?1, ?2, ?3, ?4, 0)
                 ON CONFLICT(meeting_id, raw_label) DO UPDATE SET
                    persona_id = excluded.persona_id,
                    confidence = excluded.confidence
                 WHERE speaker_persona_links.confirmed = 0",
                params![meeting_id, raw_label, persona_id, confidence],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The user says `raw_label` in this meeting is `persona_id`. In one
    /// transaction: drop any voiceprint previously enrolled from this
    /// (meeting, label) — whichever persona it went to, so a wrong first
    /// pick can't keep matching that voice — write the link as confirmed
    /// (inserting it if identify never ran), apply `display_name` as the
    /// per-meeting rename so the chip updates immediately, and, when the
    /// speaker row carries an embedding (stored at identify time), enroll it
    /// into `persona_id` right here so both personas' counts move together.
    /// Returns whether a voiceprint was enrolled; when `false` the caller
    /// may fall back to enrolling from audio.
    pub fn confirm_link(
        &self,
        meeting_id: i64,
        raw_label: &str,
        persona_id: i64,
        display_name: &str,
        cap: i32,
    ) -> Result<bool, String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        delete_source_voiceprints(&tx, meeting_id, raw_label)?;
        tx.execute(
            "INSERT INTO speaker_persona_links(meeting_id, raw_label, persona_id, confidence, confirmed)
             VALUES(?1, ?2, ?3, NULL, 1)
             ON CONFLICT(meeting_id, raw_label) DO UPDATE SET
                persona_id = excluded.persona_id,
                confidence = NULL,
                confirmed = 1",
            params![meeting_id, raw_label, persona_id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO speakers(meeting_id, raw_label, display_name) VALUES(?1, ?2, ?3)
             ON CONFLICT(meeting_id, raw_label) DO UPDATE SET display_name = excluded.display_name",
            params![meeting_id, raw_label, display_name],
        )
        .map_err(|e| e.to_string())?;

        let stored: Option<(Vec<u8>, i64, i64)> = tx
            .query_row(
                "SELECT embedding, embedding_dim, speech_ms FROM speakers
                 WHERE meeting_id = ?1 AND raw_label = ?2
                   AND embedding IS NOT NULL AND embedding_dim IS NOT NULL",
                params![meeting_id, raw_label],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let enrolled = match stored {
            Some((blob, dim, speech_ms)) if dim > 0 => {
                let emb = crate::voiceprint::unpack_f32(&blob[..blob.len().min(dim as usize * 4)]);
                insert_voiceprint_tx(
                    &tx,
                    persona_id,
                    &emb,
                    dim as i32,
                    Some(meeting_id),
                    Some(raw_label),
                    speech_ms.max(0) as u64,
                    cap,
                )?;
                true
            }
            _ => false,
        };
        tx.commit().map_err(|e| e.to_string())?;
        Ok(enrolled)
    }

    /// Atomically remove a persona link, the voiceprint enrolled from that
    /// speaker, AND the per-meeting display rename for the label (revert to
    /// the raw `SPEAKER_xx` chip). One transaction, so a partial failure
    /// can't leave the link gone but the persona's name or voice behind.
    pub fn unlink_and_clear_rename(&self, meeting_id: i64, raw_label: &str) -> Result<(), String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        delete_source_voiceprints(&tx, meeting_id, raw_label)?;
        tx.execute(
            "DELETE FROM speaker_persona_links WHERE meeting_id = ?1 AND raw_label = ?2",
            params![meeting_id, raw_label],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO speakers(meeting_id, raw_label, display_name) VALUES(?1, ?2, NULL)
             ON CONFLICT(meeting_id, raw_label) DO UPDATE SET display_name = NULL",
            params![meeting_id, raw_label],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Bring every per-label table in line with the labels that actually
    /// exist in the meeting after a (re-)diarization. Labels not in `keep`
    /// have vanished (a re-run merged clusters, or they were never real
    /// diarization labels): their persona link, display rename, and any
    /// voiceprint enrolled from them are removed; every label in `keep`
    /// gets a `speakers` row. One transaction. Voiceprints from other
    /// meetings are untouched.
    pub fn reconcile_speakers(&self, meeting_id: i64, keep: &[String]) -> Result<(), String> {
        let keep: std::collections::HashSet<&str> = keep.iter().map(String::as_str).collect();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;

        let mut stale: Vec<String> = Vec::new();
        {
            let mut stmt = tx
                .prepare(
                    "SELECT raw_label FROM speakers WHERE meeting_id = ?1
                     UNION
                     SELECT raw_label FROM speaker_persona_links WHERE meeting_id = ?1
                     UNION
                     SELECT source_label FROM voiceprints
                      WHERE source_meeting_id = ?1 AND source_label IS NOT NULL",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![meeting_id], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            for label in rows {
                let label = label.map_err(|e| e.to_string())?;
                if !keep.contains(label.as_str()) {
                    stale.push(label);
                }
            }
        }
        for label in &stale {
            let dropped = delete_source_voiceprints(&tx, meeting_id, label)?;
            if dropped > 0 {
                eprintln!(
                    "reconcile_speakers: meeting {meeting_id} label {label} vanished; \
                     dropped {dropped} voiceprint(s) enrolled from it"
                );
            }
            tx.execute(
                "DELETE FROM speaker_persona_links WHERE meeting_id = ?1 AND raw_label = ?2",
                params![meeting_id, label],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM speakers WHERE meeting_id = ?1 AND raw_label = ?2",
                params![meeting_id, label],
            )
            .map_err(|e| e.to_string())?;
        }
        for label in keep {
            tx.execute(
                "INSERT OR IGNORE INTO speakers(meeting_id, raw_label) VALUES(?1, ?2)",
                params![meeting_id, label],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// All links for a meeting, joined with persona display_name.
    pub fn meeting_speaker_links(&self, meeting_id: i64) -> Result<Vec<SpeakerLink>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT l.raw_label, l.persona_id, p.display_name, l.confidence, l.confirmed
                 FROM speaker_persona_links l
                 LEFT JOIN personas p ON p.id = l.persona_id
                 WHERE l.meeting_id = ?1
                 ORDER BY l.raw_label",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![meeting_id], |r| {
                Ok(SpeakerLink {
                    raw_label: r.get(0)?,
                    persona_id: r.get(1)?,
                    persona_name: r.get(2)?,
                    confidence: r.get(3)?,
                    confirmed: r.get::<_, i64>(4)? == 1,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
}

impl Db {
    // -------------------------------------------------------------------
    // Customers (accounts) + customer-level rollups
    // -------------------------------------------------------------------

    pub fn list_customers(&self) -> Result<Vec<CustomerSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.name, c.logo,
                        (SELECT COUNT(*) FROM meetings m
                          WHERE m.customer_id = c.id AND m.ended_at IS NOT NULL),
                        (SELECT MAX(m.started_at) FROM meetings m
                          WHERE m.customer_id = c.id AND m.ended_at IS NOT NULL)
                 FROM customers c
                 ORDER BY c.name COLLATE NOCASE",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CustomerSummary {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    logo: r.get(2)?,
                    meeting_count: r.get(3)?,
                    last_meeting_at_ms: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    pub fn create_customer(&self, name: &str, notes: Option<&str>) -> Result<i64, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("customer name cannot be empty".into());
        }
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO customers(name, notes, created_at, updated_at) VALUES(?1, ?2, ?3, ?3)",
            params![name, notes, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    pub fn rename_customer(&self, id: i64, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("customer name cannot be empty".into());
        }
        let now = chrono::Utc::now().timestamp_millis();
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE customers SET name = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, name, now],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn update_customer_notes(&self, id: i64, notes: Option<&str>) -> Result<(), String> {
        let now = chrono::Utc::now().timestamp_millis();
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE customers SET notes = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, notes, now],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Completed meetings for a customer, reverse-chronological. Mirrors the
    /// `list_meetings` row shape but filtered by `customer_id`.
    fn customer_meetings(&self, customer_id: i64) -> Result<Vec<MeetingSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let sql = r#"
            SELECT m.id, m.title, m.started_at, m.ended_at, m.mic_wav,
                   (SELECT COUNT(*) FROM segments s WHERE s.meeting_id = m.id),
                   (SELECT COUNT(DISTINCT
                       CASE
                         WHEN s.speaker = 'Me' THEN 'me'
                         ELSE COALESCE(
                           (SELECT 'p:' || l.persona_id
                            FROM speaker_persona_links l
                            WHERE l.meeting_id = m.id
                              AND l.raw_label = s.speaker
                              AND l.persona_id IS NOT NULL
                              AND l.confirmed = 1
                            LIMIT 1),
                           s.speaker)
                       END)
                    FROM segments s
                    WHERE s.meeting_id = m.id AND s.speaker IS NOT NULL),
                   (SELECT s.text FROM segments s WHERE s.meeting_id = m.id
                     ORDER BY s.start_ms LIMIT 1)
            FROM meetings m
            WHERE m.customer_id = ?1 AND m.ended_at IS NOT NULL
            ORDER BY m.started_at DESC
        "#;
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![customer_id], |r| {
                let ended: Option<i64> = r.get(3)?;
                let started: i64 = r.get(2)?;
                let mic: Option<String> = r.get(4)?;
                Ok(MeetingSummary {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    started_at_ms: started,
                    duration_ms: ended.map(|e| e - started),
                    segment_count: r.get(5)?,
                    speaker_count: r.get(6)?,
                    preview: r.get(7)?,
                    has_audio: mic.is_some(),
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    /// Distinct personas with confirmed links in this customer's meetings,
    /// with per-persona meeting count and last-seen time.
    fn customer_roster(&self, customer_id: i64) -> Result<Vec<CustomerRosterEntry>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT p.id, p.display_name, COUNT(DISTINCT m.id), MAX(m.started_at)
                 FROM meetings m
                 JOIN speaker_persona_links l ON l.meeting_id = m.id
                 JOIN personas p ON p.id = l.persona_id
                 WHERE m.customer_id = ?1
                   AND l.confirmed = 1
                   AND l.persona_id IS NOT NULL
                 GROUP BY p.id, p.display_name
                 ORDER BY COUNT(DISTINCT m.id) DESC, MAX(m.started_at) DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![customer_id], |r| {
                Ok(CustomerRosterEntry {
                    persona_id: r.get(0)?,
                    display_name: r.get(1)?,
                    meeting_count: r.get(2)?,
                    last_seen_ms: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    pub fn get_customer(&self, id: i64) -> Result<CustomerDetail, String> {
        let conn = self.conn.lock().unwrap();
        let (name, logo, notes, created_at, updated_at) = conn
            .query_row(
                "SELECT name, logo, notes, created_at, updated_at FROM customers WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .map_err(|e| format!("customer {id} not found: {e}"))?;
        let (meeting_count, first_meeting_at_ms, last_meeting_at_ms, total_duration_ms) = conn
            .query_row(
                "SELECT COUNT(*), MIN(started_at), MAX(started_at),
                        COALESCE(SUM(ended_at - started_at), 0)
                 FROM meetings
                 WHERE customer_id = ?1 AND ended_at IS NOT NULL",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .map_err(|e| e.to_string())?;
        // total_duration is returned as 0 (not NULL) by COALESCE when no meetings.
        let total_duration_ms = if total_duration_ms == Some(0) && meeting_count == 0 {
            None
        } else {
            total_duration_ms
        };
        drop(conn);

        let persona_roster = self.customer_roster(id)?;
        let meetings = self.customer_meetings(id)?;
        let meetings_with_summary_count = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM meetings m
                 WHERE m.customer_id = ?1 AND m.ended_at IS NOT NULL
                   AND EXISTS(SELECT 1 FROM summaries su WHERE su.meeting_id = m.id)",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| e.to_string())?
        };
        let latest_rollup = self.list_customer_summaries(id)?.into_iter().next();

        Ok(CustomerDetail {
            id,
            name,
            logo,
            notes,
            created_at_ms: created_at,
            updated_at_ms: updated_at,
            meeting_count,
            last_meeting_at_ms,
            first_meeting_at_ms,
            total_duration_ms,
            persona_roster,
            meetings,
            meetings_with_summary_count,
            latest_rollup,
        })
    }

    /// Relies on `ON DELETE SET NULL` for meetings (they become unassigned).
    /// Personas are never owned by a customer and are untouched.
    pub fn delete_customer(&self, id: i64) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM customers WHERE id = ?1", params![id])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Reassign a meeting to a customer (or unassign when `customer_id` is None).
    pub fn set_meeting_customer(
        &self,
        meeting_id: i64,
        customer_id: Option<i64>,
    ) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE meetings SET customer_id = ?2 WHERE id = ?1",
                params![meeting_id, customer_id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Merge `source_id` into `target_id`: reassign all of source's meetings
    /// to target, then delete source. Personas are global and need no changes
    /// — target's roster naturally reflects the union after the meetings move.
    pub fn merge_customers(&self, source_id: i64, target_id: i64) -> Result<(), String> {
        if source_id == target_id {
            return Err("cannot merge a customer into itself".into());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE meetings SET customer_id = ?2 WHERE customer_id = ?1",
            params![source_id, target_id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM customers WHERE id = ?1", params![source_id])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Scoped search across a customer's meetings: title, notes, transcript
    /// text, summary content, and persona display names.
    pub fn search_customer_meetings(
        &self,
        customer_id: i64,
        query: &str,
    ) -> Result<Vec<CustomerSearchResult>, String> {
        self.search_meetings(Some(customer_id), query, i64::MAX)
    }

    /// Search meetings (optionally scoped to one customer) across title,
    /// notes, transcript text, summary content, and persona display names,
    /// newest first, with a short per-field snippet for each hit. Backs both
    /// the customer search UI and the MCP `search_meetings` tool.
    pub fn search_meetings(
        &self,
        customer_id: Option<i64>,
        query: &str,
        limit: i64,
    ) -> Result<Vec<CustomerSearchResult>, String> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let like = format!("%{}%", q);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.title, m.started_at, m.notes FROM meetings m
                 WHERE (?1 IS NULL OR m.customer_id = ?1)
                   AND (m.title LIKE ?2
                        OR (m.notes IS NOT NULL AND m.notes LIKE ?2)
                        OR EXISTS(SELECT 1 FROM segments s
                                   WHERE s.meeting_id = m.id AND s.text LIKE ?2)
                        OR EXISTS(SELECT 1 FROM summaries su
                                   WHERE su.meeting_id = m.id AND su.content LIKE ?2)
                        OR EXISTS(SELECT 1 FROM speaker_persona_links l
                                   JOIN personas p ON p.id = l.persona_id
                                   WHERE l.meeting_id = m.id
                                     AND l.persona_id IS NOT NULL
                                     AND p.display_name LIKE ?2))
                 ORDER BY m.started_at DESC
                 LIMIT ?3",
            )
            .map_err(|e| e.to_string())?;
        let matched: Vec<(i64, String, i64, Option<String>)> = stmt
            .query_map(params![customer_id, like, limit.max(0)], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);

        let mut out = Vec::with_capacity(matched.len());
        for (mid, title, started, notes) in matched {
            let mut hits = Vec::new();
            if title.to_lowercase().contains(&q.to_lowercase()) {
                hits.push(CustomerSearchHit {
                    field: "title".into(),
                    snippet: snippet_around(&title, q, 40),
                });
            }
            if let Some(n) = notes.as_ref() {
                if n.to_lowercase().contains(&q.to_lowercase()) {
                    hits.push(CustomerSearchHit {
                        field: "notes".into(),
                        snippet: snippet_around(n, q, 60),
                    });
                }
            }
            // First matching transcript segment.
            if let Ok(text) = conn.query_row(
                "SELECT text FROM segments WHERE meeting_id = ?1 AND text LIKE ?2
                     ORDER BY start_ms LIMIT 1",
                params![mid, like],
                |r| r.get::<_, String>(0),
            ) {
                hits.push(CustomerSearchHit {
                    field: "transcript".into(),
                    snippet: snippet_around(&text, q, 80),
                });
            }
            // Latest matching summary.
            if let Ok(text) = conn.query_row(
                "SELECT content FROM summaries WHERE meeting_id = ?1 AND content LIKE ?2
                     ORDER BY created_at DESC LIMIT 1",
                params![mid, like],
                |r| r.get::<_, String>(0),
            ) {
                hits.push(CustomerSearchHit {
                    field: "summary".into(),
                    snippet: snippet_around(&text, q, 80),
                });
            }
            // Matching persona name.
            if let Ok(name) = conn.query_row(
                "SELECT p.display_name FROM speaker_persona_links l
                     JOIN personas p ON p.id = l.persona_id
                     WHERE l.meeting_id = ?1 AND l.persona_id IS NOT NULL
                       AND p.display_name LIKE ?2 LIMIT 1",
                params![mid, like],
                |r| r.get::<_, String>(0),
            ) {
                hits.push(CustomerSearchHit {
                    field: "persona".into(),
                    snippet: name,
                });
            }
            out.push(CustomerSearchResult {
                meeting_id: mid,
                title,
                started_at_ms: started,
                hits,
            });
        }
        Ok(out)
    }

    pub fn insert_customer_summary(
        &self,
        customer_id: i64,
        model: &str,
        content: &str,
        from_meeting_ids: &[i64],
    ) -> Result<i64, String> {
        let now = chrono::Utc::now().timestamp_millis();
        let ids_json = serde_json::to_string(from_meeting_ids).unwrap_or_else(|_| "[]".into());
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO customer_summaries(customer_id, model, content, from_meeting_ids, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![customer_id, model, content, ids_json, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    /// Customer rollups, newest first.
    pub fn list_customer_summaries(
        &self,
        customer_id: i64,
    ) -> Result<Vec<CustomerRollupRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, model, content, from_meeting_ids, created_at
                 FROM customer_summaries WHERE customer_id = ?1
                 ORDER BY created_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![customer_id], |r| {
                let ids_json: String = r.get(3)?;
                let meeting_count = serde_json::from_str::<Vec<i64>>(&ids_json)
                    .map(|v| v.len() as i64)
                    .unwrap_or(0);
                Ok(CustomerRollupRow {
                    id: r.get(0)?,
                    model: r.get(1)?,
                    content: r.get(2)?,
                    created_at_ms: r.get(4)?,
                    meeting_count,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    /// The last `limit` meetings (reverse-chronological) for a customer that
    /// have at least one saved summary, with their latest summary content.
    /// Used to build the customer rollup prompt.
    pub fn customer_meetings_with_latest_summary(
        &self,
        customer_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, String, i64, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.title, m.started_at,
                        (SELECT su.content FROM summaries su
                          WHERE su.meeting_id = m.id
                          ORDER BY su.created_at DESC LIMIT 1) AS content
                 FROM meetings m
                 WHERE m.customer_id = ?1 AND m.ended_at IS NOT NULL
                   AND EXISTS(SELECT 1 FROM summaries su WHERE su.meeting_id = m.id)
                 ORDER BY m.started_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![customer_id, limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
}

/// First byte range `[start, end)` in `haystack` whose lowercased chars equal
/// `needle_lower` (already lowercased). Returns `None` if no match. The range
/// is in `haystack`'s bytes (not the lowercased copy), so it is safe to slice.
fn ci_find_range(haystack: &str, needle_lower: &str) -> Option<(usize, usize)> {
    let nl: Vec<char> = needle_lower.chars().collect();
    // Lowercased haystack, each char tagged with its original byte offset in
    // `haystack` (one source char can lowercased-expand to several, e.g. 'İ').
    let hl: Vec<(usize, char)> = haystack
        .char_indices()
        .flat_map(|(i, c)| c.to_lowercase().map(move |lc| (i, lc)).collect::<Vec<_>>())
        .collect();
    let n = nl.len();
    if n == 0 || n > hl.len() {
        return None;
    }
    for start in 0..=(hl.len() - n) {
        if (0..n).all(|k| hl[start + k].1 == nl[k]) {
            let byte_start = hl[start].0;
            let byte_end = if start + n < hl.len() {
                hl[start + n].0
            } else {
                haystack.len()
            };
            return Some((byte_start, byte_end));
        }
    }
    None
}

/// Extract a short snippet around the first case-insensitive occurrence of
/// `needle` in `haystack`, with `pad` bytes of context on each side. All slice
/// boundaries are clamped to UTF-8 char boundaries, so this never panics on
/// non-ASCII content.
fn snippet_around(haystack: &str, needle: &str, pad: usize) -> String {
    let nl = needle.to_lowercase();
    let (start, end) = match ci_find_range(haystack, &nl) {
        Some((ms, me)) => {
            let s = haystack.floor_char_boundary(ms.saturating_sub(pad));
            let e = haystack.ceil_char_boundary((me + pad).min(haystack.len()));
            (s, e)
        }
        None => (0, haystack.floor_char_boundary(haystack.len().min(80))),
    };
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.push_str(&haystack[start..end]);
    if end < haystack.len() {
        s.push('…');
    }
    s
}

/// Escape `\`, `%` and `_` so a user string matches literally inside a
/// `LIKE ... ESCAPE '\'` pattern.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Default database location: `<app data>/lilnotes.sqlite3`.
pub fn db_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("lilnotes.sqlite3")
}

/// Redundancy-based gallery pruning: when a persona's voiceprint count
/// exceeds `cap`, drop the lower-quality member of the most-redundant pair
/// (highest cosine similarity). This keeps the gallery diverse — exactly
/// what max-over-gallery matching rewards — and quality-weighted.
///
/// Of the most-redundant pair, the dropped row is the one with **lower
/// `speech_ms`** (less speech → less reliable embedding); tiebreak: older
/// `created_at` (favor freshness when quality is equal); then lower `id`.
/// So a high-quality old print survives over a marginal new duplicate, and
/// a high-quality new duplicate replaces an old marginal one.
///
/// Body of `Db::insert_voiceprint`, runnable inside a caller's transaction
/// (`confirm_link` enrolls in the same transaction that drops the old print
/// and confirms the link). Replaces any print with the same source, inserts,
/// prunes to `cap`, and bumps the persona's `updated_at`.
#[allow(clippy::too_many_arguments)]
fn insert_voiceprint_tx(
    tx: &rusqlite::Transaction<'_>,
    persona_id: i64,
    embedding: &[f32],
    dim: i32,
    source_meeting_id: Option<i64>,
    source_label: Option<&str>,
    speech_ms: u64,
    cap: i32,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();
    let blob = crate::voiceprint::pack_f32(embedding);
    if let (Some(mid), Some(label)) = (source_meeting_id, source_label) {
        delete_source_voiceprints(tx, mid, label)?;
    }
    tx.execute(
        "INSERT INTO voiceprints(persona_id, embedding, dim, source_meeting_id, source_label, speech_ms, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![persona_id, blob, dim, source_meeting_id, source_label, speech_ms as i64, now],
    )
    .map_err(|e| e.to_string())?;
    if cap > 0 {
        prune_redundant(tx, persona_id, cap)?;
    }
    tx.execute(
        "UPDATE personas SET updated_at = ?2 WHERE id = ?1",
        params![persona_id, now],
    )
    .map_err(|e| e.to_string())
    .map(|_| ())
}

/// Delete every voiceprint enrolled from `(meeting_id, label)`, whichever
/// persona holds it. Returns the number of rows removed. Runs inside the
/// caller's transaction.
fn delete_source_voiceprints(
    tx: &rusqlite::Transaction<'_>,
    meeting_id: i64,
    label: &str,
) -> Result<usize, String> {
    tx.execute(
        "DELETE FROM voiceprints WHERE source_meeting_id = ?1 AND source_label = ?2",
        params![meeting_id, label],
    )
    .map_err(|e| e.to_string())
}

/// Runs inside the caller's transaction (atomic with the enroll insert).
/// Inserts are one-at-a-time, so the gallery is at most 1 over cap → one
/// deletion per call.
fn prune_redundant(
    tx: &rusqlite::Transaction<'_>,
    persona_id: i64,
    cap: i32,
) -> Result<(), String> {
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM voiceprints WHERE persona_id = ?1",
            params![persona_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if count <= cap as i64 {
        return Ok(());
    }

    // Load all voiceprints for this persona: (id, embedding, speech_ms,
    // created_at). Embeddings are unpacked from their BLOB (clipped to
    // dim*4 bytes, as in list_personas_with_voiceprints).
    let mut stmt = tx
        .prepare(
            "SELECT id, embedding, dim, speech_ms, created_at
             FROM voiceprints WHERE persona_id = ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(i64, Vec<f32>, i64, i64)> = stmt
        .query_map(params![persona_id], |r| {
            let id: i64 = r.get(0)?;
            let blob: Vec<u8> = r.get(1)?;
            let dim: i64 = r.get(2)?;
            let speech_ms: i64 = r.get(3)?;
            let created_at: i64 = r.get(4)?;
            let emb = crate::voiceprint::unpack_f32(&blob[..blob.len().min(dim as usize * 4)]);
            Ok((id, emb, speech_ms, created_at))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt); // release the prepared statement before the DELETE.

    if rows.len() < 2 {
        return Ok(());
    }

    // Find the most-redundant unordered pair by cosine similarity. O(N²)
    // over at most cap+1 (~151) prints — sub-millisecond, runs only on
    // enroll (user-triggered, rare).
    let mut best_sim = f32::NEG_INFINITY;
    let mut best: (usize, usize) = (0, 1);
    for i in 0..rows.len() {
        for j in (i + 1)..rows.len() {
            let sim = crate::voiceprint::cosine(&rows[i].1, &rows[j].1);
            if sim > best_sim {
                best_sim = sim;
                best = (i, j);
            }
        }
    }

    // Of the most-redundant pair, pick the lower-quality row to delete:
    // lower speech_ms, then older created_at, then lower id.
    let (ai, bi) = best;
    let drop_idx = if rows[ai].2 != rows[bi].2 {
        if rows[ai].2 < rows[bi].2 {
            ai
        } else {
            bi
        }
    } else if rows[ai].3 != rows[bi].3 {
        if rows[ai].3 < rows[bi].3 {
            ai
        } else {
            bi
        }
    } else if rows[ai].0 < rows[bi].0 {
        ai
    } else {
        bi
    };
    let drop_id = rows[drop_idx].0;
    tx.execute("DELETE FROM voiceprints WHERE id = ?1", params![drop_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn tmp_db() -> Arc<Db> {
        let dir = std::env::temp_dir().join(format!(
            "lilnotes-m9-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            TEST_ID.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sqlite3");
        // Fixed test key — bypasses the Keychain so tests stay hermetic.
        Arc::new(Db::open(&path, &[0x42u8; 32]).unwrap())
    }

    #[test]
    fn migrates_to_latest_with_tables() {
        let db = tmp_db();
        let conn = db.conn.lock().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 9);
        let has_k: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('meetings') WHERE name = 'diarize_num_speakers'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_k, 1, "v8 adds meetings.diarize_num_speakers");
        let has_emb: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('speakers')
                 WHERE name IN ('embedding', 'embedding_dim', 'speech_ms')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            has_emb, 3,
            "v9 adds speakers.embedding/embedding_dim/speech_ms"
        );
        for table in ["personas", "voiceprints", "speaker_persona_links"] {
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing table {table}");
        }
    }

    #[test]
    fn persona_voiceprint_link_roundtrip() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s1", "t", 0).unwrap();
        let pid = db.create_persona("Priya").unwrap();
        let emb = vec![0.1, 0.2, 0.3];
        db.insert_voiceprint(pid, &emb, 3, Some(meeting_id), Some("SPEAKER_00"), 5000, 50)
            .unwrap();

        let personas = db.list_personas().unwrap();
        assert_eq!(personas.len(), 1);
        assert_eq!(personas[0].display_name, "Priya");
        assert_eq!(personas[0].voiceprint_count, 1);

        let with_emb = db.list_personas_with_voiceprints().unwrap();
        assert_eq!(with_emb[0].embeddings.len(), 1);
        assert_eq!(with_emb[0].embeddings[0].len(), 3);

        db.upsert_link(meeting_id, "SPEAKER_00", Some(pid), Some(0.9))
            .unwrap();
        let links = db.meeting_speaker_links(meeting_id).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].persona_name.as_deref(), Some("Priya"));
        assert!(!links[0].confirmed);

        db.confirm_link(meeting_id, "SPEAKER_00", pid, "Priya", 50)
            .unwrap();
        let links = db.meeting_speaker_links(meeting_id).unwrap();
        assert!(links[0].confirmed);
        assert_eq!(
            links[0].confidence, None,
            "confirm clears the suggestion score"
        );

        // Re-running identify after confirm must leave the row frozen —
        // neither a new score, a different persona, nor a NULL suggestion
        // may touch a human decision.
        let other = db.create_persona("Other").unwrap();
        db.upsert_link(meeting_id, "SPEAKER_00", Some(other), Some(0.8))
            .unwrap();
        db.upsert_link(meeting_id, "SPEAKER_00", None, None)
            .unwrap();
        let links = db.meeting_speaker_links(meeting_id).unwrap();
        assert!(links[0].confirmed, "upsert must preserve confirmed=1");
        assert_eq!(
            links[0].persona_id,
            Some(pid),
            "upsert must not flip a confirmed persona"
        );
        assert_eq!(
            links[0].confidence, None,
            "upsert must not touch a confirmed row"
        );
        db.delete_persona(other).unwrap();

        db.unlink_and_clear_rename(meeting_id, "SPEAKER_00")
            .unwrap();
        assert!(db.meeting_speaker_links(meeting_id).unwrap().is_empty());

        db.delete_persona(pid).unwrap();
        assert!(db.list_personas().unwrap().is_empty());

        // Gallery cap pruning: with cap=1, the second insert drops the
        // lower-quality member of the (only, hence most-redundant) pair.
        // The first print has speech_ms=1000, the second 2000 → the first
        // is dropped and the higher-quality second print survives.
        let pid2 = db.create_persona("CapTest").unwrap();
        db.insert_voiceprint(pid2, &[0.1, 0.2, 0.3], 3, None, None, 1000, 1)
            .unwrap();
        db.insert_voiceprint(pid2, &[0.4, 0.5, 0.6], 3, None, None, 2000, 1)
            .unwrap();
        let with_emb = db.list_personas_with_voiceprints().unwrap();
        let cap_persona = with_emb.iter().find(|p| p.id == pid2).unwrap();
        assert_eq!(
            cap_persona.embeddings.len(),
            1,
            "cap=1 should keep only the higher-quality voiceprint"
        );
        assert_eq!(
            cap_persona.embeddings[0],
            vec![0.4, 0.5, 0.6],
            "lower-speech_ms print is the one dropped"
        );
    }

    #[test]
    fn voiceprint_pruning_keeps_higher_quality() {
        // Two near-duplicate embeddings, cap=1. The older print has MORE
        // speech (higher quality); the newer has the minimum. Smart pruning
        // must keep the high-quality old print, proving quality beats age
        // (unlike the old FIFO policy, which would have dropped the old one).
        let db = tmp_db();
        let pid = db.create_persona("Q").unwrap();
        // Near-identical direction so they form the most-redundant pair.
        db.insert_voiceprint(pid, &[1.0, 0.0, 0.0], 3, None, None, 30_000, 1)
            .unwrap(); // old, high quality
        db.insert_voiceprint(pid, &[1.0, 0.0, 0.0], 3, None, None, 3_100, 1)
            .unwrap(); // new, low quality
        let with_emb = db.list_personas_with_voiceprints().unwrap();
        assert_eq!(with_emb[0].embeddings.len(), 1, "only one print survives");
        // The low-quality new print is the redundant duplicate that gets
        // dropped; the high-quality old one stays.
        assert_eq!(
            with_emb[0].embeddings[0],
            vec![1.0, 0.0, 0.0],
            "high-quality old print retained over low-quality new duplicate"
        );
    }

    #[test]
    fn voiceprint_pruning_drops_most_redundant() {
        // Three prints, cap=2. Two are near-identical (a redundant pair);
        // the third is unique. Pruning must drop the lower-quality member of
        // the redundant pair and keep the unique print regardless of age.
        let db = tmp_db();
        let pid = db.create_persona("D").unwrap();
        // Two near-duplicates (different quality), then one unique direction.
        db.insert_voiceprint(pid, &[1.0, 0.0, 0.0], 3, None, None, 5_000, 2)
            .unwrap(); // redundant, low quality
        db.insert_voiceprint(pid, &[1.0, 0.0, 0.0], 3, None, None, 20_000, 2)
            .unwrap(); // redundant, high quality
        db.insert_voiceprint(pid, &[0.0, 1.0, 0.0], 3, None, None, 3_100, 2)
            .unwrap(); // unique (orthogonal), lowest quality
        let with_emb = db.list_personas_with_voiceprints().unwrap();
        assert_eq!(with_emb[0].embeddings.len(), 2, "cap=2 keeps two prints");
        // The redundant low-quality print is dropped; the high-quality
        // redundant one AND the unique low-quality one both survive.
        let embs = &with_emb[0].embeddings;
        assert!(
            embs.contains(&vec![1.0, 0.0, 0.0]),
            "high-quality redundant print survives"
        );
        assert!(
            embs.contains(&vec![0.0, 1.0, 0.0]),
            "unique print survives even though it's lowest quality"
        );
        assert_eq!(embs.len(), 2);
    }

    #[test]
    fn delete_persona_clears_display_renames() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s1", "t", 0).unwrap();
        let pid = db.create_persona("Priya").unwrap();
        // Simulate a prior confirm: link + display rename applied.
        db.upsert_link(meeting_id, "SPEAKER_00", Some(pid), Some(0.9))
            .unwrap();
        db.confirm_link(meeting_id, "SPEAKER_00", pid, "Priya", 50)
            .unwrap();
        // Sanity: the rename is present.
        let detail = db.get_meeting(meeting_id).unwrap();
        assert_eq!(
            detail.renames.get("SPEAKER_00").map(String::as_str),
            Some("Priya")
        );

        db.delete_persona(pid).unwrap();
        // After delete: link's persona_id is NULL, rename cleared.
        let detail = db.get_meeting(meeting_id).unwrap();
        assert!(
            !detail.renames.contains_key("SPEAKER_00"),
            "delete_persona should clear the display rename"
        );
        let link = detail.speaker_links.get("SPEAKER_00").unwrap();
        assert!(
            link.persona_id.is_none(),
            "link persona_id should be NULL after persona delete"
        );
    }

    #[test]
    fn unlink_and_clear_rename_is_atomic() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s2", "t", 0).unwrap();
        let pid = db.create_persona("Jordan").unwrap();
        db.upsert_link(meeting_id, "SPEAKER_01", Some(pid), Some(0.8))
            .unwrap();
        db.rename_speaker(meeting_id, "SPEAKER_01", Some("Jordan"))
            .unwrap();

        db.unlink_and_clear_rename(meeting_id, "SPEAKER_01")
            .unwrap();
        let detail = db.get_meeting(meeting_id).unwrap();
        assert!(
            !detail.speaker_links.contains_key("SPEAKER_01"),
            "link should be gone"
        );
        assert!(
            !detail.renames.contains_key("SPEAKER_01"),
            "display rename should be cleared"
        );
    }

    #[test]
    fn reconcile_speakers_purges_vanished_labels() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s3", "t", 0).unwrap();
        let other_meeting = db.insert_meeting_started("s3b", "t", 0).unwrap();
        let priya = db.create_persona("Priya").unwrap();
        let sam = db.create_persona("Sam").unwrap();
        // Prior run: SPEAKER_00 confirmed Priya, SPEAKER_01 confirmed Sam
        // (rename + voiceprint each), plus a Sam print from another meeting.
        db.confirm_link(meeting_id, "SPEAKER_00", priya, "Priya", 50)
            .unwrap();
        db.insert_voiceprint(
            priya,
            &[1.0, 0.0],
            2,
            Some(meeting_id),
            Some("SPEAKER_00"),
            5000,
            50,
        )
        .unwrap();
        db.confirm_link(meeting_id, "SPEAKER_01", sam, "Sam", 50)
            .unwrap();
        db.insert_voiceprint(
            sam,
            &[0.0, 1.0],
            2,
            Some(meeting_id),
            Some("SPEAKER_01"),
            5000,
            50,
        )
        .unwrap();
        db.insert_voiceprint(
            sam,
            &[0.5, 0.5],
            2,
            Some(other_meeting),
            Some("SPEAKER_01"),
            5000,
            50,
        )
        .unwrap();
        assert_eq!(db.meeting_speaker_links(meeting_id).unwrap().len(), 2);

        // Re-diarization: SPEAKER_01 merged away; SPEAKER_02 is new.
        db.reconcile_speakers(meeting_id, &["SPEAKER_00".into(), "SPEAKER_02".into()])
            .unwrap();

        let links = db.meeting_speaker_links(meeting_id).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].raw_label, "SPEAKER_00");
        assert!(links[0].confirmed, "surviving confirmed link is untouched");

        let detail = db.get_meeting(meeting_id).unwrap();
        assert_eq!(
            detail.renames.get("SPEAKER_00").map(String::as_str),
            Some("Priya")
        );
        assert!(
            !detail.renames.contains_key("SPEAKER_01"),
            "vanished rename dropped"
        );

        let counts: std::collections::HashMap<i64, i64> = db
            .list_personas()
            .unwrap()
            .into_iter()
            .map(|p| (p.id, p.voiceprint_count))
            .collect();
        assert_eq!(counts[&priya], 1, "surviving label keeps its print");
        assert_eq!(
            counts[&sam], 1,
            "vanished label's print dropped; other meeting's kept"
        );

        let conn = db.conn.lock().unwrap();
        let rows: Vec<String> = conn
            .prepare("SELECT raw_label FROM speakers WHERE meeting_id = ?1 ORDER BY raw_label")
            .unwrap()
            .query_map(params![meeting_id], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec!["SPEAKER_00".to_string(), "SPEAKER_02".to_string()]
        );
    }

    #[test]
    fn confirm_link_replaces_source_voiceprint_and_inserts_missing_row() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s4", "t", 0).unwrap();
        let a = db.create_persona("A").unwrap();
        let b = db.create_persona("B").unwrap();

        // No identify run → no link row yet. Confirm must still create it.
        db.confirm_link(meeting_id, "SPEAKER_00", a, "A", 50)
            .unwrap();
        db.insert_voiceprint(
            a,
            &[1.0, 0.0],
            2,
            Some(meeting_id),
            Some("SPEAKER_00"),
            5000,
            50,
        )
        .unwrap();
        let links = db.meeting_speaker_links(meeting_id).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!((links[0].persona_id, links[0].confirmed), (Some(a), true));

        // Change of mind: A → B. A must lose the print from this speaker.
        db.confirm_link(meeting_id, "SPEAKER_00", b, "B", 50)
            .unwrap();
        let links = db.meeting_speaker_links(meeting_id).unwrap();
        assert_eq!(
            links[0].persona_id,
            Some(b),
            "re-confirm overrides a confirmed row"
        );
        let detail = db.get_meeting(meeting_id).unwrap();
        assert_eq!(
            detail.renames.get("SPEAKER_00").map(String::as_str),
            Some("B")
        );
        let counts: std::collections::HashMap<i64, i64> = db
            .list_personas()
            .unwrap()
            .into_iter()
            .map(|p| (p.id, p.voiceprint_count))
            .collect();
        assert_eq!(counts[&a], 0, "A no longer holds this speaker's voice");
        assert_eq!(
            counts[&b], 0,
            "enrollment into B happens later, by the caller"
        );
    }

    #[test]
    fn insert_voiceprint_replaces_same_source_across_personas() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s5", "t", 0).unwrap();
        let a = db.create_persona("A").unwrap();
        let b = db.create_persona("B").unwrap();
        db.insert_voiceprint(
            a,
            &[1.0, 0.0],
            2,
            Some(meeting_id),
            Some("SPEAKER_00"),
            5000,
            50,
        )
        .unwrap();
        // A late enrollment into B for the same speaker evicts A's print.
        db.insert_voiceprint(
            b,
            &[1.0, 0.0],
            2,
            Some(meeting_id),
            Some("SPEAKER_00"),
            6000,
            50,
        )
        .unwrap();
        // Re-enrolling the same source into B is idempotent, not additive.
        db.insert_voiceprint(
            b,
            &[1.0, 0.0],
            2,
            Some(meeting_id),
            Some("SPEAKER_00"),
            6000,
            50,
        )
        .unwrap();
        // Source-less prints are never treated as duplicates.
        db.insert_voiceprint(a, &[0.0, 1.0], 2, None, None, 6000, 50)
            .unwrap();
        db.insert_voiceprint(a, &[0.0, 1.0], 2, None, None, 6000, 50)
            .unwrap();
        let counts: std::collections::HashMap<i64, i64> = db
            .list_personas()
            .unwrap()
            .into_iter()
            .map(|p| (p.id, p.voiceprint_count))
            .collect();
        assert_eq!(counts[&a], 2);
        assert_eq!(counts[&b], 1);
    }

    #[test]
    fn unlink_deletes_source_voiceprint() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s6", "t", 0).unwrap();
        let a = db.create_persona("A").unwrap();
        db.confirm_link(meeting_id, "SPEAKER_00", a, "A", 50)
            .unwrap();
        db.insert_voiceprint(
            a,
            &[1.0, 0.0],
            2,
            Some(meeting_id),
            Some("SPEAKER_00"),
            5000,
            50,
        )
        .unwrap();
        db.insert_voiceprint(a, &[0.0, 1.0], 2, None, None, 5000, 50)
            .unwrap();
        db.unlink_and_clear_rename(meeting_id, "SPEAKER_00")
            .unwrap();
        assert!(db.meeting_speaker_links(meeting_id).unwrap().is_empty());
        assert_eq!(
            db.list_personas().unwrap()[0].voiceprint_count,
            1,
            "only the source print goes"
        );
        assert!(db.get_meeting(meeting_id).unwrap().renames.is_empty());
    }

    #[test]
    fn confirm_link_enrolls_stored_embedding_atomically() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s8", "t", 0).unwrap();
        let a = db.create_persona("A").unwrap();
        let b = db.create_persona("B").unwrap();
        db.set_speaker_embedding(meeting_id, "SPEAKER_00", &[0.6, 0.8], 7000)
            .unwrap();

        assert!(db
            .confirm_link(meeting_id, "SPEAKER_00", a, "A", 50)
            .unwrap());
        let counts = |db: &Db| -> std::collections::HashMap<i64, i64> {
            db.list_personas()
                .unwrap()
                .into_iter()
                .map(|p| (p.id, p.voiceprint_count))
                .collect()
        };
        let c = counts(&db);
        assert_eq!((c[&a], c[&b]), (1, 0));
        let with_emb = db.list_personas_with_voiceprints().unwrap();
        let ga = with_emb.iter().find(|p| p.id == a).unwrap();
        assert_eq!(
            ga.embeddings[0],
            vec![0.6, 0.8],
            "enrolled the stored embedding"
        );
        {
            let conn = db.conn.lock().unwrap();
            let (src_m, src_l, ms): (i64, String, i64) = conn
                .query_row(
                    "SELECT source_meeting_id, source_label, speech_ms FROM voiceprints WHERE persona_id = ?1",
                    params![a],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                (src_m, src_l.as_str(), ms),
                (meeting_id, "SPEAKER_00", 7000)
            );
        }

        // Change of mind A → B: one transaction moves the print.
        assert!(db
            .confirm_link(meeting_id, "SPEAKER_00", b, "B", 50)
            .unwrap());
        let c = counts(&db);
        assert_eq!((c[&a], c[&b]), (0, 1));

        // Re-confirming B is idempotent.
        assert!(db
            .confirm_link(meeting_id, "SPEAKER_00", b, "B", 50)
            .unwrap());
        let c = counts(&db);
        assert_eq!((c[&a], c[&b]), (0, 1));
    }

    #[test]
    fn confirm_link_without_stored_embedding_enrolls_nothing() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s9", "t", 0).unwrap();
        let a = db.create_persona("A").unwrap();
        assert!(!db
            .confirm_link(meeting_id, "SPEAKER_00", a, "A", 50)
            .unwrap());
        assert_eq!(db.list_personas().unwrap()[0].voiceprint_count, 0);
        let links = db.meeting_speaker_links(meeting_id).unwrap();
        assert_eq!((links[0].persona_id, links[0].confirmed), (Some(a), true));
    }

    #[test]
    fn diarize_num_speakers_roundtrip() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s7", "t", 0).unwrap();
        assert_eq!(db.diarize_num_speakers(meeting_id).unwrap(), None);
        db.set_diarize_num_speakers(meeting_id, Some(3)).unwrap();
        assert_eq!(db.diarize_num_speakers(meeting_id).unwrap(), Some(3));
        db.set_diarize_num_speakers(meeting_id, None).unwrap();
        assert_eq!(db.diarize_num_speakers(meeting_id).unwrap(), None);
    }

    #[test]
    fn speaker_identity_count_collapses_confirmed_personas() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s4", "t", 0).unwrap();
        db.finalize_meeting(meeting_id, 0, "m.wav", "s.wav")
            .unwrap();

        // 3 remote raw labels + local user = 4 identities.
        db.replace_segments(
            meeting_id,
            &[
                Segment {
                    source: "mic".into(),
                    speaker: Some("Me".into()),
                    start_ms: 0,
                    end_ms: 1000,
                    text: "hello".into(),
                    ..Default::default()
                },
                Segment {
                    source: "system".into(),
                    speaker: Some("SPEAKER_00".into()),
                    start_ms: 1000,
                    end_ms: 2000,
                    text: "a".into(),
                    ..Default::default()
                },
                Segment {
                    source: "system".into(),
                    speaker: Some("SPEAKER_01".into()),
                    start_ms: 2000,
                    end_ms: 3000,
                    text: "b".into(),
                    ..Default::default()
                },
                Segment {
                    source: "system".into(),
                    speaker: Some("SPEAKER_02".into()),
                    start_ms: 3000,
                    end_ms: 4000,
                    text: "c".into(),
                    ..Default::default()
                },
            ],
        )
        .unwrap();

        assert_eq!(db.count_speaker_identities(meeting_id).unwrap(), 4);
        let summary = db.list_meetings(None).unwrap().pop().unwrap();
        assert_eq!(summary.speaker_count, 4);
        let detail = db.get_meeting(meeting_id).unwrap();
        assert_eq!(detail.speaker_count, 4);

        // Confirm two remote labels are the same persona -> count drops to 3.
        let pid = db.create_persona("Priya").unwrap();
        db.upsert_link(meeting_id, "SPEAKER_00", Some(pid), Some(0.9))
            .unwrap();
        db.confirm_link(meeting_id, "SPEAKER_00", pid, "Priya", 50)
            .unwrap();
        db.upsert_link(meeting_id, "SPEAKER_01", Some(pid), Some(0.85))
            .unwrap();
        db.confirm_link(meeting_id, "SPEAKER_01", pid, "Priya", 50)
            .unwrap();

        assert_eq!(db.count_speaker_identities(meeting_id).unwrap(), 3);
        assert_eq!(db.list_meetings(None).unwrap()[0].speaker_count, 3);
        assert_eq!(db.get_meeting(meeting_id).unwrap().speaker_count, 3);

        // Unlink one -> count returns to 4.
        db.unlink_and_clear_rename(meeting_id, "SPEAKER_01")
            .unwrap();
        assert_eq!(db.count_speaker_identities(meeting_id).unwrap(), 4);
        assert_eq!(db.list_meetings(None).unwrap()[0].speaker_count, 4);
    }

    #[test]
    fn speaker_identity_count_ignores_unconfirmed_suggestions() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("s5", "t", 0).unwrap();

        db.replace_segments(
            meeting_id,
            &[
                Segment {
                    source: "system".into(),
                    speaker: Some("SPEAKER_00".into()),
                    start_ms: 0,
                    end_ms: 1000,
                    text: "a".into(),
                    ..Default::default()
                },
                Segment {
                    source: "system".into(),
                    speaker: Some("SPEAKER_01".into()),
                    start_ms: 1000,
                    end_ms: 2000,
                    text: "b".into(),
                    ..Default::default()
                },
            ],
        )
        .unwrap();

        // Suggestion (confirmed = 0) should NOT merge the two labels.
        let pid = db.create_persona("Priya").unwrap();
        db.upsert_link(meeting_id, "SPEAKER_00", Some(pid), Some(0.9))
            .unwrap();

        assert_eq!(db.count_speaker_identities(meeting_id).unwrap(), 2);
    }

    #[test]
    fn speaker_identity_count_edge_cases() {
        let db = tmp_db();
        let empty_id = db.insert_meeting_started("empty", "t", 0).unwrap();
        assert_eq!(db.count_speaker_identities(empty_id).unwrap(), 0);

        let me_id = db.insert_meeting_started("me", "t", 0).unwrap();
        db.replace_segments(
            me_id,
            &[
                Segment {
                    source: "mic".into(),
                    speaker: Some("Me".into()),
                    start_ms: 0,
                    end_ms: 1000,
                    text: "hello".into(),
                    ..Default::default()
                },
                Segment {
                    source: "mic".into(),
                    speaker: Some("Me".into()),
                    start_ms: 1000,
                    end_ms: 2000,
                    text: "world".into(),
                    ..Default::default()
                },
            ],
        )
        .unwrap();
        assert_eq!(db.count_speaker_identities(me_id).unwrap(), 1);

        let null_id = db.insert_meeting_started("null", "t", 0).unwrap();
        db.replace_segments(
            null_id,
            &[Segment {
                source: "system".into(),
                speaker: None,
                start_ms: 0,
                end_ms: 1000,
                text: "no speaker".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        assert_eq!(db.count_speaker_identities(null_id).unwrap(), 0);
    }

    #[test]
    fn mark_preservation_survives_replace_segments() {
        let db = tmp_db();
        let meeting_id = db.insert_meeting_started("marks", "t", 0).unwrap();
        db.finalize_meeting(meeting_id, 0, "m.wav", "s.wav")
            .unwrap();

        // Two mic segments + one system. Mark the first mic as echo and
        // soft-delete the second; the system segment stays default.
        db.replace_segments(
            meeting_id,
            &[
                Segment {
                    source: "mic".into(),
                    speaker: Some("Me".into()),
                    start_ms: 0,
                    end_ms: 1000,
                    text: "echo-of-remote".into(),
                    ..Default::default()
                },
                Segment {
                    source: "mic".into(),
                    speaker: Some("Me".into()),
                    start_ms: 1000,
                    end_ms: 2000,
                    text: "stutter".into(),
                    ..Default::default()
                },
                Segment {
                    source: "system".into(),
                    speaker: Some("SPEAKER_00".into()),
                    start_ms: 2000,
                    end_ms: 3000,
                    text: "hi".into(),
                    ..Default::default()
                },
            ],
        )
        .unwrap();
        let all = db.meeting_segments_all(meeting_id).unwrap();
        let echo_id = all.iter().find(|s| s.start_ms == 0).unwrap().id;
        let del_id = all.iter().find(|s| s.start_ms == 1000).unwrap().id;
        db.set_segment_kind(echo_id, "echo").unwrap();
        db.set_segment_deleted(del_id, true).unwrap();

        // Default view hides both; hidden count = 2.
        assert_eq!(db.meeting_segments(meeting_id).unwrap().len(), 1);
        let detail = db.get_meeting(meeting_id).unwrap();
        assert_eq!(detail.hidden_segment_count, 2);

        // Simulate a re-transcribe: snapshot marks, replace with fresh rows
        // (timestamps shifted by a few ms — within the ±250ms tolerance), then
        // re-apply. The first mic shifted +10ms should still match the echo mark.
        let marks = db.snapshot_marks(meeting_id).unwrap();
        assert_eq!(marks.len(), 2);
        db.replace_segments(
            meeting_id,
            &[
                Segment {
                    source: "mic".into(),
                    speaker: Some("Me".into()),
                    start_ms: 10,
                    end_ms: 1010,
                    text: "echo-of-remote v2".into(),
                    ..Default::default()
                },
                Segment {
                    source: "mic".into(),
                    speaker: Some("Me".into()),
                    start_ms: 1010,
                    end_ms: 2010,
                    text: "stutter v2".into(),
                    ..Default::default()
                },
                Segment {
                    source: "system".into(),
                    speaker: Some("SPEAKER_00".into()),
                    start_ms: 2000,
                    end_ms: 3000,
                    text: "hi".into(),
                    ..Default::default()
                },
            ],
        )
        .unwrap();
        db.reapply_marks(meeting_id, &marks).unwrap();

        // Both marks survived on the fresh rows.
        let all = db.meeting_segments_all(meeting_id).unwrap();
        let echo = all
            .iter()
            .find(|s| s.source == "mic" && s.start_ms == 10)
            .unwrap();
        assert_eq!(echo.kind, "echo");
        assert!(!echo.deleted);
        let del = all
            .iter()
            .find(|s| s.source == "mic" && s.start_ms == 1010)
            .unwrap();
        assert!(del.deleted);
        assert_eq!(del.kind, "speech");
        // Default view still hides both; the system segment shows.
        assert_eq!(db.meeting_segments(meeting_id).unwrap().len(), 1);
    }

    #[test]
    fn snippet_around_handles_non_ascii_without_panic() {
        // Multi-byte chars around/inside the match — the old code panicked here
        // because pad (bytes) landed mid-character. "meeting" is preceded by an
        // em-dash (3 bytes); "emoji" is preceded by a 4-byte 🎉.
        let s = "café résumé — meeting notes with an em-dash and 🎉 emoji";
        assert!(snippet_around(s, "meeting", 10)
            .to_lowercase()
            .contains("meeting"));
        assert!(snippet_around(s, "em-dash", 20).contains("em-dash"));
        assert!(snippet_around(s, "emoji", 5).contains("emoji"));
        // Guaranteed match-branch repro of the old panic: 5 emojis (4 bytes
        // each) push the match to byte 20; pad=6 makes start=14, which is
        // mid-character inside the 3rd emoji. Old code panicked slicing here.
        let stacked = "🎉🎉🎉🎉🎉hello";
        assert!(snippet_around(stacked, "hello", 6).contains("hello"));
        // No match: fallback path must not panic when byte 80 is mid-char.
        let long: String = "xé".repeat(60); // every other char is 2 bytes
        let _ = snippet_around(&long, "zzz", 40);
    }

    #[test]
    fn clear_mic_cleaned_wav_returns_previous_path_once() {
        let db = tmp_db();
        let id = db.insert_meeting_started("s-clean", "t", 0).unwrap();
        db.finalize_meeting(id, 1, "/r/mic.flac", "/r/system.flac")
            .unwrap();
        assert_eq!(db.clear_mic_cleaned_wav(id).unwrap(), None);
        db.set_mic_cleaned_wav(id, "/r/mic_cleaned.flac").unwrap();
        assert_eq!(
            db.clear_mic_cleaned_wav(id).unwrap().as_deref(),
            Some("/r/mic_cleaned.flac")
        );
        assert_eq!(db.clear_mic_cleaned_wav(id).unwrap(), None);
        assert_eq!(db.get_meeting(id).unwrap().mic_cleaned_wav, None);
    }

    #[test]
    fn delete_meeting_returns_all_three_audio_paths() {
        let db = tmp_db();
        let id = db.insert_meeting_started("s-del", "t", 0).unwrap();
        db.finalize_meeting(id, 1, "/r/mic.flac", "/r/system.flac")
            .unwrap();
        db.set_mic_cleaned_wav(id, "/r/mic_cleaned.flac").unwrap();
        let paths = db.delete_meeting(id).unwrap();
        assert_eq!(
            paths,
            AudioPaths {
                mic: Some("/r/mic.flac".into()),
                system: Some("/r/system.flac".into()),
                cleaned: Some("/r/mic_cleaned.flac".into()),
            }
        );
        assert_eq!(paths.iter().count(), 3);
        assert!(db.get_meeting(id).is_err());
        // Unknown ids yield no paths rather than an error.
        assert_eq!(db.delete_meeting(id).unwrap(), AudioPaths::default());
    }

    #[test]
    fn clear_audio_paths_also_drops_cleaned_mic() {
        let db = tmp_db();
        let id = db.insert_meeting_started("s-clear", "t", 0).unwrap();
        db.finalize_meeting(id, 1, "/r/mic.flac", "/r/system.flac")
            .unwrap();
        db.set_mic_cleaned_wav(id, "/r/mic_cleaned.flac").unwrap();
        db.clear_audio_paths(id).unwrap();
        let m = db.get_meeting(id).unwrap();
        assert_eq!(
            (m.mic_wav, m.system_wav, m.mic_cleaned_wav),
            (None, None, None)
        );
    }

    #[test]
    fn meetings_with_wav_audio_selects_legacy_rows_only() {
        let db = tmp_db();
        // Legacy: all three columns .wav.
        let legacy = db.insert_meeting_started("s-legacy", "t", 10).unwrap();
        db.finalize_meeting(legacy, 11, "/a/mic.wav", "/a/system.wav")
            .unwrap();
        db.set_mic_cleaned_wav(legacy, "/a/mic_cleaned.wav")
            .unwrap();
        // Half-migrated: only the cleaned copy is still WAV.
        let half = db.insert_meeting_started("s-half", "t", 20).unwrap();
        db.finalize_meeting(half, 21, "/b/mic.flac", "/b/system.flac")
            .unwrap();
        db.set_mic_cleaned_wav(half, "/b/mic_cleaned.wav").unwrap();
        // Already FLAC.
        let done = db.insert_meeting_started("s-done", "t", 30).unwrap();
        db.finalize_meeting(done, 31, "/c/mic.flac", "/c/system.flac")
            .unwrap();
        // Still recording (no ended_at): never touched.
        let live = db.insert_meeting_started("s-live", "t", 5).unwrap();
        let _ = live;

        let rows = db.meetings_with_wav_audio().unwrap();
        let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![legacy, half], "oldest first, legacy rows only");

        db.set_audio_paths(
            legacy,
            &AudioPaths {
                mic: Some("/a/mic.flac".into()),
                system: Some("/a/system.flac".into()),
                cleaned: None,
            },
        )
        .unwrap();
        let m = db.get_meeting(legacy).unwrap();
        assert_eq!(m.mic_wav.as_deref(), Some("/a/mic.flac"));
        assert_eq!(m.mic_cleaned_wav, None);
        let ids: Vec<i64> = db
            .meetings_with_wav_audio()
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(ids, vec![half]);
    }
}

#[cfg(test)]
mod mcp_query_tests {
    use super::*;
    use crate::asr::Segment;

    fn tmp() -> Db {
        let dir = std::env::temp_dir().join(format!(
            "lilnotes-mcpq-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("t.sqlite3"), &[7u8; 32]).unwrap()
    }

    fn seg(source: &str, speaker: &str, start_ms: u64, text: &str) -> Segment {
        Segment {
            id: 0,
            source: source.into(),
            start_ms,
            end_ms: start_ms + 1000,
            text: text.into(),
            speaker: Some(speaker.into()),
            kind: "speech".into(),
            deleted: false,
        }
    }

    /// Two finished meetings (one per customer) + one unfinished.
    fn seed(db: &Db) -> (i64, i64, i64, i64) {
        let c1 = db.create_customer("Acme", None).unwrap();
        let c2 = db.create_customer("Globex", None).unwrap();
        let m1 = db
            .insert_meeting_started("a", "Acme kickoff", 1_000)
            .unwrap();
        db.finalize_meeting(m1, 61_000, "m.wav", "s.wav").unwrap();
        db.replace_segments(m1, &[seg("mic", "Me", 0, "budget 100% done")])
            .unwrap();
        db.set_meeting_customer(m1, Some(c1)).unwrap();
        db.insert_summary(m1, "m", "t", "Budget approved").unwrap();
        let m2 = db
            .insert_meeting_started("b", "Globex sync", 2_000)
            .unwrap();
        db.finalize_meeting(m2, 62_000, "m.wav", "s.wav").unwrap();
        db.replace_segments(m2, &[seg("system", "SPEAKER_00", 0, "hello world")])
            .unwrap();
        db.set_meeting_customer(m2, Some(c2)).unwrap();
        db.update_notes(m2, "note about budget").unwrap();
        // Unfinished (no ended_at) — must never be listed.
        db.insert_meeting_started("c", "live", 3_000).unwrap();
        (c1, c2, m1, m2)
    }

    #[test]
    fn filtered_list_honors_filters_and_paging() {
        let db = tmp();
        let (c1, _c2, m1, m2) = seed(&db);

        let all = MeetingFilter {
            limit: 50,
            ..Default::default()
        };
        let (rows, total) = db.list_meetings_filtered(&all).unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![m2, m1]);
        assert!(rows[1].has_summary && !rows[1].has_notes);
        assert!(!rows[0].has_summary && rows[0].has_notes);
        assert_eq!(rows[1].customer_id, Some(c1));

        let (rows, total) = db
            .list_meetings_filtered(&MeetingFilter {
                customer_id: Some(c1),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!((rows.len(), total), (1, 1));
        assert_eq!(rows[0].id, m1);

        let (rows, total) = db
            .list_meetings_filtered(&MeetingFilter {
                from_ms: Some(1_500),
                to_ms: Some(2_500),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!((rows.len(), total), (1, 1));
        assert_eq!(rows[0].id, m2);

        let (rows, total) = db
            .list_meetings_filtered(&MeetingFilter {
                limit: 1,
                offset: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, m1);

        // Query over title and transcript; `%` is literal.
        let (rows, _) = db
            .list_meetings_filtered(&MeetingFilter {
                query: Some("world".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, m2);
        let (rows, _) = db
            .list_meetings_filtered(&MeetingFilter {
                query: Some("100%".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        let (rows, _) = db
            .list_meetings_filtered(&MeetingFilter {
                query: Some("100%x".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn global_search_spans_customers_and_fields() {
        let db = tmp();
        let (c1, _c2, m1, m2) = seed(&db);

        let r = db.search_meetings(None, "budget", 10).unwrap();
        assert_eq!(
            r.iter().map(|x| x.meeting_id).collect::<Vec<_>>(),
            vec![m2, m1]
        );
        let fields = |i: usize| {
            r[i].hits
                .iter()
                .map(|h| h.field.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(fields(0), vec!["notes"]);
        assert_eq!(fields(1), vec!["transcript", "summary"]);

        // Scoped variant equals the customer search.
        let scoped = db.search_meetings(Some(c1), "budget", 10).unwrap();
        let legacy = db.search_customer_meetings(c1, "budget").unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].meeting_id, legacy[0].meeting_id);

        assert_eq!(db.search_meetings(None, "budget", 1).unwrap().len(), 1);
        assert!(db.search_meetings(None, "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
        assert_eq!(escape_like("plain"), "plain");
    }
}
