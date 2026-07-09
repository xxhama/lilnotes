//! SQLite persistence (rusqlite, bundled).
//!
//! One connection behind a mutex — queries are short and the app is a
//! single user; contention is not a concern. Migrations run at open via
//! `PRAGMA user_version`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::asr::Segment;
use crate::settings::AppSettings;

pub struct Db {
    conn: Mutex<Connection>,
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
    pub notes: Option<String>,
    pub segments: Vec<Segment>,
    /// raw_label -> display_name (only rows the user renamed).
    pub renames: std::collections::HashMap<String, String>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(path).map_err(|e| format!("cannot open database: {e}"))?;
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
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Meetings
    // -----------------------------------------------------------------------

    pub fn insert_meeting(
        &self,
        session_id: &str,
        title: &str,
        started_at_ms: i64,
        ended_at_ms: i64,
        mic_wav: &str,
        system_wav: &str,
    ) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO meetings(session_id, title, started_at, ended_at, mic_wav, system_wav)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, title, started_at_ms, ended_at_ms, mic_wav, system_wav],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_meetings(&self, search: Option<&str>) -> Result<Vec<MeetingSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let like = search
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("%{}%", s.trim()));
        let sql = r#"
            SELECT m.id, m.title, m.started_at, m.ended_at, m.mic_wav,
                   (SELECT COUNT(*) FROM segments s WHERE s.meeting_id = m.id),
                   (SELECT COUNT(DISTINCT s.speaker) FROM segments s
                     WHERE s.meeting_id = m.id AND s.speaker IS NOT NULL),
                   (SELECT s.text FROM segments s WHERE s.meeting_id = m.id
                     ORDER BY s.start_ms LIMIT 1)
            FROM meetings m
            WHERE ?1 IS NULL
               OR m.title LIKE ?1
               OR EXISTS(SELECT 1 FROM segments s
                          WHERE s.meeting_id = m.id AND s.text LIKE ?1)
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

    pub fn get_meeting(&self, id: i64) -> Result<MeetingDetail, String> {
        let conn = self.conn.lock().unwrap();
        let (session_id, title, started_at_ms, ended_at_ms, mic_wav, system_wav, notes) = conn
            .query_row(
                "SELECT session_id, title, started_at, ended_at, mic_wav, system_wav, notes
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
                    ))
                },
            )
            .map_err(|e| format!("meeting {id} not found: {e}"))?;

        let mut stmt = conn
            .prepare(
                "SELECT source, speaker, start_ms, end_ms, text FROM segments
                 WHERE meeting_id = ?1 ORDER BY start_ms",
            )
            .map_err(|e| e.to_string())?;
        let segments = stmt
            .query_map(params![id], |r| {
                Ok(Segment {
                    source: r.get(0)?,
                    speaker: r.get(1)?,
                    start_ms: r.get::<_, i64>(2)? as u64,
                    end_ms: r.get::<_, i64>(3)? as u64,
                    text: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let mut stmt = conn
            .prepare(
                "SELECT raw_label, display_name FROM speakers
                 WHERE meeting_id = ?1 AND display_name IS NOT NULL",
            )
            .map_err(|e| e.to_string())?;
        let renames = stmt
            .query_map(params![id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<std::collections::HashMap<_, _>, _>>()
            .map_err(|e| e.to_string())?;

        Ok(MeetingDetail {
            id,
            session_id,
            title,
            started_at_ms,
            ended_at_ms,
            mic_wav,
            system_wav,
            notes,
            segments,
            renames,
        })
    }

    pub fn update_title(&self, id: i64, title: &str) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute("UPDATE meetings SET title = ?2 WHERE id = ?1", params![id, title])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn delete_meeting(&self, id: i64) -> Result<(Option<String>, Option<String>), String> {
        let conn = self.conn.lock().unwrap();
        let wavs = conn
            .query_row(
                "SELECT mic_wav, system_wav FROM meetings WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or((None, None));
        conn.execute("DELETE FROM meetings WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(wavs)
    }

    /// Clear the audio paths (used by "delete audio after transcription").
    pub fn clear_audio_paths(&self, id: i64) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE meetings SET mic_wav = NULL, system_wav = NULL WHERE id = ?1",
                params![id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    // -----------------------------------------------------------------------
    // Segments + speakers
    // -----------------------------------------------------------------------

    /// Replace a meeting's segments (idempotent for re-transcription).
    pub fn replace_segments(&self, meeting_id: i64, segments: &[Segment]) -> Result<(), String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM segments WHERE meeting_id = ?1", params![meeting_id])
            .map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO segments(meeting_id, source, speaker, start_ms, end_ms, text)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(|e| e.to_string())?;
            for s in segments {
                stmt.execute(params![
                    meeting_id,
                    s.source,
                    s.speaker,
                    s.start_ms as i64,
                    s.end_ms as i64,
                    s.text
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Ensure a speakers row exists for every distinct raw label.
    pub fn ensure_speakers(&self, meeting_id: i64, raw_labels: &[String]) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        for raw in raw_labels {
            conn.execute(
                "INSERT OR IGNORE INTO speakers(meeting_id, raw_label) VALUES(?1, ?2)",
                params![meeting_id, raw],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
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

    /// (mic_wav, system_wav) for a meeting, if still present.
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
                "SELECT source, speaker, start_ms, end_ms, text FROM segments
                 WHERE meeting_id = ?1 ORDER BY start_ms",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![id], |r| {
                Ok(Segment {
                    source: r.get(0)?,
                    speaker: r.get(1)?,
                    start_ms: r.get::<_, i64>(2)? as u64,
                    end_ms: r.get::<_, i64>(3)? as u64,
                    text: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
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

/// Default database location: `<app data>/lilnotes.sqlite3`.
pub fn db_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("lilnotes.sqlite3")
}
