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
        if let Some(v) = get("voiceprint_gallery_cap") {
            if let Ok(n) = v.parse::<i32>() {
                s.voiceprint_gallery_cap = n;
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
            "voiceprint_gallery_cap",
            s.voiceprint_gallery_cap.to_string(),
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
                    Ok(crate::voiceprint::unpack_f32(&blob[..blob.len().min(dim as usize * 4)]))
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

    /// Cascade removes voiceprints; links' persona_id set to NULL by FK.
    pub fn delete_persona(&self, persona_id: i64) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM personas WHERE id = ?1", params![persona_id])
            .map(|_| ())
            .map_err(|e| e.to_string())
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

    /// Insert a voiceprint. If the persona now exceeds `cap`, drop the
    /// oldest rows beyond the cap (by `created_at`, then `id`).
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
        let now = chrono::Utc::now().timestamp_millis();
        let blob = crate::voiceprint::pack_f32(embedding);
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO voiceprints(persona_id, embedding, dim, source_meeting_id, source_label, speech_ms, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![persona_id, blob, dim, source_meeting_id, source_label, speech_ms as i64, now],
        )
        .map_err(|e| e.to_string())?;
        if cap > 0 {
            tx.execute(
                "DELETE FROM voiceprints WHERE persona_id = ?1 AND id NOT IN (
                    SELECT id FROM voiceprints WHERE persona_id = ?1
                    ORDER BY created_at DESC, id DESC LIMIT ?2
                 )",
                params![persona_id, cap],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute(
            "UPDATE personas SET updated_at = ?2 WHERE id = ?1",
            params![persona_id, now],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Upsert a `speaker_persona_links` row. Preserves an existing
    /// `confirmed = 1` (re-running identify never un-confirms).
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
                    confidence = excluded.confidence,
                    confirmed = MAX(speaker_persona_links.confirmed, 0)",
                params![meeting_id, raw_label, persona_id, confidence],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn set_link_confirmed(
        &self,
        meeting_id: i64,
        raw_label: &str,
        persona_id: i64,
    ) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE speaker_persona_links
                 SET persona_id = ?3, confirmed = 1
                 WHERE meeting_id = ?1 AND raw_label = ?2",
                params![meeting_id, raw_label, persona_id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn unlink_speaker(&self, meeting_id: i64, raw_label: &str) -> Result<(), String> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM speaker_persona_links WHERE meeting_id = ?1 AND raw_label = ?2",
                params![meeting_id, raw_label],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
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

/// Default database location: `<app data>/lilnotes.sqlite3`.
pub fn db_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("lilnotes.sqlite3")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tmp_db() -> Arc<Db> {
        let dir = std::env::temp_dir().join(format!(
            "lilnotes-m9-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sqlite3");
        // Fixed test key — bypasses the Keychain so tests stay hermetic.
        Arc::new(Db::open(&path, &[0x42u8; 32]).unwrap())
    }

    #[test]
    fn migrates_to_v2_with_tables() {
        let db = tmp_db();
        let conn = db.conn.lock().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2);
        for table in ["personas", "voiceprints", "speaker_persona_links"] {
            let n: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"),
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
        let meeting_id = db
            .insert_meeting("s1", "t", 0, 0, "m.wav", "s.wav")
            .unwrap();
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

        db.set_link_confirmed(meeting_id, "SPEAKER_00", pid).unwrap();
        let links = db.meeting_speaker_links(meeting_id).unwrap();
        assert!(links[0].confirmed);

        // Re-running upsert after confirm must NOT clear confirmed.
        db.upsert_link(meeting_id, "SPEAKER_00", Some(pid), Some(0.8))
            .unwrap();
        let links = db.meeting_speaker_links(meeting_id).unwrap();
        assert!(links[0].confirmed, "upsert must preserve confirmed=1");
        assert_eq!(links[0].confidence, Some(0.8), "upsert should update confidence");

        db.unlink_speaker(meeting_id, "SPEAKER_00").unwrap();
        assert!(db.meeting_speaker_links(meeting_id).unwrap().is_empty());

        db.delete_persona(pid).unwrap();
        assert!(db.list_personas().unwrap().is_empty());

        // Gallery cap pruning: with cap=1, a second insert drops the oldest.
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
            "cap=1 should keep only the newest voiceprint"
        );
    }
}
