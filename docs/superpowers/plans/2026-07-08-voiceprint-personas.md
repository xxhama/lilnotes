# Voiceprint Personas Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist CAM++ speaker embeddings as per-persona "voiceprint galleries" across meetings, auto-suggest known personas on diarized speakers, and grow each gallery when the user confirms an identity — so recognition improves over time, fully on-device.

**Architecture:** sherpa-rs already computes CAM++ embeddings during diarization but discards them. We add a standalone `EmbeddingExtractor` (same CAM++ model) that re-derives one normalized embedding per diarized speaker from `system.wav`, a `personas`/`voiceprints`/`speaker_persona_links` SQLite layer (migration to `user_version = 2`), brute-force cosine matching in Rust (max-over-gallery), and an enrollment feedback loop triggered only by explicit user confirmation. Personas are an identity/mapping layer — raw `SPEAKER_xx` labels are never rewritten. Identity is additive to the existing diarize → assign → persist flow. **The entire SQLite database is encrypted at rest with SQLCipher** (the app is unreleased, so there's no plaintext-migration concern); the 256-bit key is generated on first run and stored in the macOS Keychain.

**Tech Stack:** Rust (`sherpa-rs = 0.6.8` → `sherpa_rs::speaker_id::{EmbeddingExtractor, ExtractorConfig}` + `compute_speaker_embedding`; `rusqlite` with `bundled-sqlcipher-vendored-openssl` for encrypted-at-rest SQLite; `security-framework` for Keychain key storage; `rand` OsRng for key generation), React 19 / TS / Tailwind / shadcn, Tauri v2 IPC.

**Confirmed decisions:**
- Pre-fill suggested name on the chip; require one-click confirm (never silent auto-apply).
- Scoring: max cosine over the persona's gallery (not centroid).
- Personas management: new dedicated view in the sidebar.
- `identify_speakers` runs automatically right after `diarize_meeting` finishes (additive; doesn't alter the `DiarizedTranscript` return shape — results surface via a `speakers:identified` event and via `speaker_links` added to `MeetingDetail`).
- **Full-DB encryption via SQLCipher** (not per-table BLOB encryption). The whole `lilnotes.sqlite3` — meetings, transcripts, summaries, personas, and voiceprints — is encrypted. Key lives in the macOS Keychain.

**Default thresholds / caps (exposed in Settings, tune later):** `auto = 0.65`, `suggest = 0.45`, `gallery_cap = 50` voiceprints per persona (drop oldest on overflow).

---

## File Structure

**Backend (Rust) — new/modified:**
- `src-tauri/Cargo.toml` (modify) — `rusqlite` → `bundled-sqlcipher-vendored-openssl`; add `security-framework = "3.7"` and `rand = "0.8"`.
- `src-tauri/src/keystore/mod.rs` (create) — `db_key() -> Result<Vec<u8>, String>`: get-or-create a 32-byte key in the macOS Keychain (service `co.elastic.lilnote`, account `db-key`) via `security_framework::passwords`; first run generates it with `rand::rngs::OsRng`.
- `src-tauri/src/db/mod.rs` (modify) — `Db::open(path, key: &[u8])` sets `PRAGMA key = "x'<hex>'"` before any other pragma, then existing pragmas + `migrate()`; migration to `user_version = 2`; `Persona`, `SpeakerLink` row types; CRUD methods for the three new tables; extend `MeetingDetail` with `speaker_links`.
- `src-tauri/src/voiceprint/mod.rs` (create) — `VoiceprintEngine` (wraps `EmbeddingExtractor`, loaded once with the CAM++ model from `models::diarize_model_paths(app).1`); `embed_speakers(...)`; `pack_f32`/`unpack_f32`; `l2_normalize`; `cosine`; `SpeakerEmbedding` struct.
- `src-tauri/src/personas/mod.rs` (create) — `PersonaScore`, `SpeakerMatch`, `Tier` types; `rank_personas(db, query) -> Vec<PersonaScore>` (max-over-gallery); `classify(score, auto, suggest) -> Tier`; `identify_and_persist(db, voiceprint, meeting_id, system_wav, turns, settings) -> Result<Vec<SpeakerMatch>>`; `enroll(db, persona_id, embedding, meeting_id, raw_label, speech_ms)` with gallery-cap pruning.
- `src-tauri/src/settings.rs` (modify) — `persona_auto_threshold: f32`, `persona_suggest_threshold: f32`, `voiceprint_gallery_cap: i32`.
- `src-tauri/src/commands.rs` (modify) — new IPC commands; `diarize_meeting` runs `identify_and_persist` and emits `speakers:identified`.
- `src-tauri/src/lib.rs` (modify) — `mod keystore; mod voiceprint; mod personas;`, manage `VoiceprintEngine`, fetch the DB key from the Keychain and pass it to `Db::open`, register new commands.

**Frontend — new/modified:**
- `src/lib/ipc.ts` (modify) — new types + typed wrappers + `onSpeakersIdentified`.
- `src/components/SpeakerPersonaPicker.tsx` (create) — confirm / choose / create / dismiss popover.
- `src/components/TranscriptPane.tsx` (modify) — persona-aware chip: shows suggested name + confidence + "suggested" affordance; opens the picker.
- `src/views/MeetingDetail.tsx` (modify) — pass `speakerLinks` to `TranscriptPane`; wire confirm/unlink; reload on `speakers:identified`.
- `src/views/Personas.tsx` (create) — list/rename/delete personas + voiceprint counts; delete-all-voiceprints.
- `src/views/Settings.tsx` (modify) — thresholds + gallery cap + delete-all-voiceprints + storage note.
- `src/App.tsx` (modify) — `personas` route + nav item.

**Docs:**
- `README.md` (modify) — milestone 9 row + verification + privacy note about voiceprints.

---

## Task 0: Enable SQLCipher full-DB encryption + Keychain key management (prerequisite)

The entire SQLite database is encrypted at rest. The 256-bit key is generated on first run and stored in the macOS Keychain; `Db::open` takes the key as a parameter so the `db` module stays Keychain-agnostic and tests stay hermetic. Do this before any migration task — the encrypted connection must exist before tables are created.

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/keystore/mod.rs`
- Modify: `src-tauri/src/db/mod.rs:54-68` (`Db::open`)
- Modify: `src-tauri/src/lib.rs:48-51` (setup: fetch key, pass to `Db::open`; add `mod keystore;`)

- [ ] **Step 1: Switch rusqlite to SQLCipher + add key deps**

In `src-tauri/Cargo.toml`, replace the rusqlite line and add two deps:

```toml
# --- persistence (M5) + encryption at rest (M9) ---
rusqlite = { version = "0.40", features = ["bundled-sqlcipher-vendored-openssl"] }
security-framework = "3.7"   # macOS Keychain for the DB key
rand = "0.8"                 # OsRng for key generation
```

`bundled-sqlcipher-vendored-openssl` bundles SQLCipher + a vendored OpenSSL so the build is self-contained (no system-OpenSSL dependency, reproducible for the eventual signed/notarized .app). On macOS the crypto provider is OpenSSL via `openssl-sys` (vendored). First build compiles OpenSSL — expect a few extra minutes once, then cached.

- [ ] **Step 2: Create the keystore module**

Create `src-tauri/src/keystore/mod.rs`:

```rust
//! macOS Keychain storage for the SQLCipher database key.
//!
//! On first run a 32-byte random key is generated with `OsRng` and stored as
//! a generic password in the user's login keychain (service
//! `co.elastic.lilnote`, account `db-key`). Subsequent launches retrieve it.
//! If the key is deleted from the Keychain, the encrypted DB becomes
//! unreadable — which is the correct privacy behavior (effectively a factory
//! reset of all meeting/voiceprint data).

use security_framework::passwords::{get_generic_password, set_generic_password};

const SERVICE: &str = "co.elastic.lilnote";
const ACCOUNT: &str = "db-key";

/// Get the 32-byte DB key from the Keychain, generating and storing it on
/// first run.
pub fn db_key() -> Result<Vec<u8>, String> {
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(key) if key.len() == 32 => Ok(key),
        Ok(_) => Err("stored DB key is not 32 bytes — delete it and relaunch".into()),
        Err(_) => {
            // Not found (or inaccessible) → generate + store.
            let mut key = [0u8; 32];
            use rand::RngCore;
            rand::rngs::OsRng.fill_bytes(&mut key);
            set_generic_password(SERVICE, ACCOUNT, &key)
                .map_err(|e| format!("cannot store DB key in Keychain: {e}"))?;
            Ok(key.to_vec())
        }
    }
}
```

- [ ] **Step 3: Change `Db::open` to take and apply the key**

In `src-tauri/src/db/mod.rs`, change the signature and set `PRAGMA key` as the **first** pragma (before `journal_mode` and `foreign_keys`):

```rust
    pub fn open(path: &Path, key: &[u8]) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(path).map_err(|e| format!("cannot open database: {e}"))?;

        // SQLCipher: raw 256-bit key as hex blob literal (bypasses KDF — we
        // already hold a cryptographically random key).
        let key_hex: String = key.iter().map(|b| format!("{:02x}", b)).collect();
        conn.pragma_update(None, "key", format!("x'{key_hex}'"))
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
```

- [ ] **Step 4: Wire the key in `lib.rs` setup + register the module**

In `src-tauri/src/lib.rs`:
- Add `mod keystore;` to the module list (before `mod db;`).
- In `.setup(|app| { ... })`, fetch the key and pass it to `Db::open`:

```rust
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let key = keystore::db_key()
                .map_err(|e| std::io::Error::other(format!("cannot unlock database: {e}")))?;
            let db = Arc::new(
                Db::open(&db::db_path(&data_dir), &key)
                    .map_err(std::io::Error::other)?,
            );
            settings::migrate_json_settings(&data_dir, &db);
            app.manage(db);
            Ok(())
        })
```

- [ ] **Step 5: Update the existing test helper to pass a fixed test key**

Every `Db::open` caller in tests must now pass a key. The test helper in `src-tauri/src/db/mod.rs` (added in Task 1) uses `Db::open(&path)` — change it to `Db::open(&path, &[0x42u8; 32])`. (If Task 1's `tmp_db` hasn't been written yet, write it with the key from the start — see Task 1 Step 2.) Also fix any other test caller of `Db::open` in the crate (there are none currently outside `db::tests`).

- [ ] **Step 6: Build (first build compiles SQLCipher + OpenSSL — be patient)**

Run: `cd src-tauri && cargo build`
Expected: compiles with no errors. A plain `sqlite3` against the created DB now fails with "file is not a database" (encrypted).

- [ ] **Step 7: Run existing tests to confirm the encrypted DB works**

Run: `cd src-tauri && cargo test`
Expected: existing `transcript::tests` pass (pure logic, no DB) and any DB-using tests pass with the fixed test key. (Keychain is not touched by tests — the test key bypasses `keystore`.)

- [ ] **Step 8: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/keystore/mod.rs src-tauri/src/db/mod.rs src-tauri/src/lib.rs
git commit -m "Milestone 9: SQLCipher full-DB encryption at rest + macOS Keychain key management"
```

---

## Task 1: SQLite migration to user_version = 2

**Files:**
- Modify: `src-tauri/src/db/mod.rs:70-125` (the `migrate()` method)

- [ ] **Step 1: Add the v2 migration block after the v1 block**

In `Db::migrate()`, after the `if version < 1 { ... }` block (before the final `Ok(())`), add:

```rust
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
```

- [ ] **Step 2: Add a round-trip migration test**

Add at the bottom of `src-tauri/src/db/mod.rs` (after `db_path`):

```rust
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
}
```

- [ ] **Step 3: Run the test**

Run: `cd src-tauri && cargo test db::tests::migrates_to_v2_with_tables -- --nocapture`
Expected: PASS, `user_version = 2` and all three tables present.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/db/mod.rs
git commit -m "Milestone 9: SQLite migration to v2 — personas, voiceprints, speaker_persona_links"
```

---

## Task 2: voiceprint module — embedding extraction + vector math

**Files:**
- Create: `src-tauri/src/voiceprint/mod.rs`
- Modify: `src-tauri/src/lib.rs:15-24` (add `mod voiceprint;`) and `:42-46` (manage state)

- [ ] **Step 1: Write failing unit tests for vec math**

Create `src-tauri/src/voiceprint/mod.rs` with just the tests + stubs:

```rust
//! Standalone CAM++ speaker-embedding extraction for voiceprint enrollment
//! and matching. Reuses the diarization embedding model
//! (`models::diarize_model_paths(app).1`); embeddings are L2-normalized so
//! cosine similarity is a plain dot product.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::AppHandle;

use crate::diarize::Turn;
use crate::models;

/// One representative embedding for a diarized speaker in a meeting.
#[derive(Serialize, Clone, Debug)]
pub struct SpeakerEmbedding {
    /// L2-normalized CAM++ vector.
    pub vec: Vec<f32>,
    /// Total speech duration used to compute it (ms).
    pub speech_ms: u64,
}

/// Wraps a sherpa-rs `EmbeddingExtractor`, loaded once with the CAM++ model.
#[derive(Default)]
pub struct VoiceprintEngine {
    inner: Mutex<Option<sherpa_rs::speaker_id::EmbeddingExtractor>>,
}

/// Pack f32 slice into little-endian bytes for SQLite BLOB storage.
pub fn pack_f32(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Unpack little-endian bytes back into f32.
pub fn unpack_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// L2-normalize in place; returns the original Vec for convenience.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity for L2-normalized vectors == dot product. Returns 0.0
/// for mismatched lengths (shouldn't happen with one model).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Minimum speech (ms) to produce a reliable voiceprint.
pub const MIN_SPEECH_MS: u64 = 3000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        let v = vec![0.1, -0.2, 0.3, 1.5, -1234.5];
        let packed = pack_f32(&v);
        assert_eq!(packed.len(), v.len() * 4);
        let back = unpack_f32(&packed);
        for (a, b) in v.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![0.6, 0.8, 0.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_length_mismatch_is_zero() {
        assert_eq!(cosine(&[1.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn l2_normalize_unit_length() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_is_noop() {
        let mut v = vec![0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0]);
    }
}
```

- [ ] **Step 2: Run tests to confirm vec math passes**

Run: `cd src-tauri && cargo test voiceprint::tests -- --nocapture`
Expected: all 6 tests PASS.

- [ ] **Step 3: Implement `VoiceprintEngine::embed_speakers`**

Append to `src-tauri/src/voiceprint/mod.rs` (above the `#[cfg(test)]` block):

```rust
impl VoiceprintEngine {
    fn ensure_loaded(&self, app: &AppHandle) -> Result<(), String> {
        let mut guard = self.inner.lock().unwrap();
        if guard.is_some() {
            return Ok(());
        }
        let (_, emb) = models::diarize_model_paths(app)?;
        if !emb.exists() {
            // Download with a non-cancellable flag (small file, likely cached).
            let cancel = std::sync::atomic::AtomicBool::new(false);
            models::ensure_diarize_models(app, &cancel)?;
        }
        let config = sherpa_rs::speaker_id::ExtractorConfig {
            model: emb.to_string_lossy().to_string(),
            ..Default::default()
        };
        let extractor = sherpa_rs::speaker_id::EmbeddingExtractor::new(config)
            .map_err(|e| format!("failed to initialize voiceprint extractor: {e}"))?;
        *guard = Some(extractor);
        Ok(())
    }

    /// Compute one normalized embedding per diarized speaker on the system
    /// channel. Speakers with < `MIN_SPEECH_MS` of speech are omitted (their
    /// voiceprints are unreliable). `turns` must be the diarized turns for
    /// `system_wav_path`.
    pub fn embed_speakers(
        &self,
        app: &AppHandle,
        system_wav_path: &str,
        turns: &[Turn],
    ) -> Result<HashMap<String, SpeakerEmbedding>, String> {
        self.ensure_loaded(app)?;

        let mut reader = hound::WavReader::open(system_wav_path)
            .map_err(|e| format!("cannot open {system_wav_path}: {e}"))?;
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("cannot read {system_wav_path}: {e}"))?;
        let sample_rate = reader.sampling_rate() as usize;

        // Group turns by speaker and concatenate their samples.
        let mut by_speaker: HashMap<&str, (Vec<f32>, u64)> = HashMap::new();
        for t in turns {
            let (buf, ms) = by_speaker.entry(t.speaker.as_str()).or_default();
            let start = ((t.start_ms as usize) * sample_rate / 1000).min(samples.len());
            let end = ((t.end_ms as usize) * sample_rate / 1000).min(samples.len());
            if end > start {
                buf.extend_from_slice(&samples[start..end]);
            }
            *ms += t.end_ms - t.start_ms;
        }

        let mut guard = self.inner.lock().unwrap();
        let extractor = guard.as_mut().ok_or("voiceprint extractor not initialized")?;

        let mut out = HashMap::new();
        for (speaker, (buf, speech_ms)) in by_speaker {
            if speech_ms < MIN_SPEECH_MS || buf.is_empty() {
                continue; // skip unreliable short utterances
            }
            let mut emb = extractor
                .compute_speaker_embedding(buf, sample_rate as i32)
                .map_err(|e| format!("embedding failed for {speaker}: {e}"))?;
            l2_normalize(&mut emb);
            out.insert(
                speaker.to_string(),
                SpeakerEmbedding {
                    vec: emb,
                    speech_ms,
                },
            );
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Register the module + managed state**

In `src-tauri/src/lib.rs`:
- Add `mod voiceprint;` to the module list (after `mod transcript;`).
- Add `use voiceprint::VoiceprintEngine;` to the `use` block.
- Add `.manage(Arc::new(VoiceprintEngine::default()))` after the `DiarizeEngine` manage line.

- [ ] **Step 5: Build to confirm it compiles**

Run: `cd src-tauri && cargo build`
Expected: compiles with no errors. (Warnings about unused functions are fine — they're used in later tasks.)

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/voiceprint/mod.rs src-tauri/src/lib.rs
git commit -m "Milestone 9: voiceprint module — CAM++ embedding extraction + vec math"
```

---

## Task 3: personas module — types, matching, enrollment

**Files:**
- Create: `src-tauri/src/personas/mod.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod personas;`)

- [ ] **Step 1: Write the failing classification + ranking tests**

Create `src-tauri/src/personas/mod.rs`:

```rust
//! Persona identity layer: match a query embedding against persona
//! voiceprint galleries (max cosine), classify confidence into tiers, and
//! orchestrate enrollment when a human confirms an identity.

use serde::Serialize;

use crate::voiceprint::cosine;

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
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored
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
            persona(3, "C", &[]),                          // skipped
        ];
        let ranked = rank_personas(&personas, &q);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].persona_id, 1);
        assert!((ranked[0].score - 1.0).abs() < 1e-6);
        assert_eq!(ranked[1].persona_id, 2);
    }

    #[test]
    fn rank_empty_galleries_returns_empty() {
        let ranked = rank_personas(&[], &[1.0, 0.0]);
        assert!(ranked.is_empty());
    }
}
```

- [ ] **Step 2: Register the module**

In `src-tauri/src/lib.rs`, add `mod personas;` to the module list (after `mod transcript;`).

- [ ] **Step 3: Build + run tests**

Run: `cd src-tauri && cargo test personas::tests -- --nocapture`
Expected: 5 tests PASS; full crate compiles. (`rank_personas` is a pure function — no `Db` dependency, no stub needed.)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/personas/mod.rs src-tauri/src/lib.rs
git commit -m "Milestone 9: personas module — Tier classification + gallery ranking"
```

---

## Task 4: DB CRUD for personas / voiceprints / links

**Files:**
- Modify: `src-tauri/src/db/mod.rs` (replace the `#[cfg(test)]` stub from Task 3 with real methods + add the rest)

- [ ] **Step 1: Add row types**

Add near the other row types in `src-tauri/src/db/mod.rs` (after `SummaryRow`):

```rust
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
```

- [ ] **Step 2: Add the persona/voiceprint/link CRUD methods**

Add a new `impl Db` block (or extend the existing one) with all of these methods:

```rust
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
        let conn = self.conn.lock().unwrap();
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
```

- [ ] **Step 3: Add a CRUD round-trip test**

Append to the `tests` module in `src-tauri/src/db/mod.rs`:

```rust
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

    db.unlink_speaker(meeting_id, "SPEAKER_00").unwrap();
    assert!(db.meeting_speaker_links(meeting_id).unwrap().is_empty());

    db.delete_persona(pid).unwrap();
    assert!(db.list_personas().unwrap().is_empty());
}
```

- [ ] **Step 4: Run tests**

Run: `cd src-tauri && cargo test db::tests -- --nocapture`
Expected: both `migrates_to_v2_with_tables` and `persona_voiceprint_link_roundtrip` PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/db/mod.rs
git commit -m "Milestone 9: DB CRUD for personas, voiceprints, speaker_persona_links"
```

---

## Task 5: Settings — thresholds + gallery cap

**Files:**
- Modify: `src-tauri/src/settings.rs`
- Modify: `src-tauri/src/db/mod.rs:131-195` (`get_settings` / `set_settings`)

- [ ] **Step 1: Extend `AppSettings`**

In `src-tauri/src/settings.rs`, add three fields to the struct and `Default`:

```rust
    /// Cosine score at/above which a persona is a strong (pre-filled) suggestion.
    pub persona_auto_threshold: f32,
    /// Cosine score at/above which a persona is a tentative suggestion.
    pub persona_suggest_threshold: f32,
    /// Max voiceprints kept per persona (oldest pruned on enroll). 0 = unlimited.
    pub voiceprint_gallery_cap: i32,
```

In `Default`:

```rust
            persona_auto_threshold: 0.65,
            persona_suggest_threshold: 0.45,
            voiceprint_gallery_cap: 50,
```

- [ ] **Step 2: Wire them through `get_settings` / `set_settings`**

In `src-tauri/src/db/mod.rs` `get_settings`, after the `summary_template` block:

```rust
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
```

In `set_settings`, after the `summary_template` put:

```rust
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
```

- [ ] **Step 3: Build + run all tests**

Run: `cd src-tauri && cargo build && cargo test`
Expected: compiles; all tests pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/settings.rs src-tauri/src/db/mod.rs
git commit -m "Milestone 9: persona match thresholds + gallery cap in settings"
```

---

## Task 6: personas — identify_and_persist + enroll orchestration

**Files:**
- Modify: `src-tauri/src/personas/mod.rs` (add `identify_and_persist` + `enroll`)

- [ ] **Step 1: Add `identify_and_persist` and `enroll`**

Append to `src-tauri/src/personas/mod.rs` (above the `#[cfg(test)]` block), replacing the placeholder `Tier::Unknown` assignment in `rank_personas` results with proper classification done here instead (so `rank_personas` returns raw scores and the caller classifies). Concretely, change `rank_personas`'s `tier: Tier::Unknown` line to `tier: Tier::Unknown` is fine because `identify_and_persist` re-stamps tiers after reading settings. Add:

```rust
use crate::diarize::Turn;
use crate::voiceprint::VoiceprintEngine;

/// Run embedding + matching for a meeting's diarized speakers and persist
/// suggestions into `speaker_persona_links` (confirmed preserved). Returns
/// one `SpeakerMatch` per raw label that has a usable embedding.
pub fn identify_and_persist(
    db: &Db,
    voiceprint: &VoiceprintEngine,
    meeting_id: i64,
    system_wav_path: &str,
    turns: &[Turn],
    settings: &AppSettings,
) -> Result<Vec<SpeakerMatch>, String> {
    let embeddings = voiceprint.embed_speakers(&tauri::AppHandle::default(), system_wav_path, turns)?;
    // NOTE: `embed_speakers` needs a real AppHandle for model-path lookup.
    // Callers must pass it through; see the signature note below.

    let mut out = Vec::with_capacity(embeddings.len());
    let personas = db.list_personas_with_voiceprints()?;
    for (raw_label, emb) in &embeddings {
        let mut scores = rank_personas(&personas, &emb.vec);
        for s in scores.iter_mut() {
            s.tier = classify(s.score, settings.persona_auto_threshold, settings.persona_suggest_threshold);
        }
        // Persist the top suggestion (if it clears `suggest`); else null link.
        let top = scores.first();
        let (pid, conf) = match top {
            Some(p) if p.tier != Tier::Unknown => (Some(p.persona_id), Some(p.score)),
            _ => (None, None),
        };
        db.upsert_link(meeting_id, raw_label, pid, conf)?;

        let existing = db
            .meeting_speaker_links(meeting_id)?
            .into_iter()
            .find(|l| l.raw_label == *raw_label);
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

/// Enroll a speaker's embedding into a persona's gallery (called only on
/// human confirmation). Re-derives the embedding from `system_wav_path` for
/// the given label's turns. If the audio is gone, returns Ok(false) so the
/// caller can still mark the link confirmed without a voiceprint.
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
    let embeddings = voiceprint.embed_speakers(app, path, turns)?;
    let emb: &SpeakerEmbedding = embeddings
        .get(raw_label)
        .ok_or_else(|| format!("no embedding for {raw_label} (need >= {MIN_SPEECH_MS_CONST} ms speech)"))?;
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

const MIN_SPEECH_MS_CONST: u64 = crate::voiceprint::MIN_SPEECH_MS;
```

**Signature fix:** `embed_speakers` takes `&AppHandle`, but `identify_and_persist` as written calls `tauri::AppHandle::default()` which is wrong. Update the signature to take `app: &AppHandle` and pass it through:

```rust
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
    // ... rest unchanged, but `rank_personas` + `classify` loop uses `settings`.
```

Remove the `use crate::diarize::Turn;` duplicate if present; keep a single `use` at the top.

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo build`
Expected: compiles. (No new tests here — the logic is orchestration over DB + voiceprint, both already tested; end-to-end covered by manual verification in Task 13.)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/personas/mod.rs
git commit -m "Milestone 9: identify_and_persist + enroll orchestration"
```

---

## Task 7: IPC commands + wire identify into diarize_meeting

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/db/mod.rs` (`MeetingDetail` gets `speaker_links`)
- Modify: `src-tauri/src/lib.rs` (register commands)

- [ ] **Step 1: Extend `MeetingDetail` with `speaker_links`**

In `src-tauri/src/db/mod.rs`, add a field to `MeetingDetail`:

```rust
    /// raw_label -> persona link (suggestion/confirmed) per meeting.
    pub speaker_links: std::collections::HashMap<String, crate::db::SpeakerLink>,
```

In `Db::get_meeting`, after loading `renames`, load links and include them:

```rust
        let links = self.meeting_speaker_links(id)?;
        let speaker_links = links
            .into_iter()
            .map(|l| (l.raw_label.clone(), l))
            .collect();

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
            speaker_links,
        })
```

- [ ] **Step 2: Add the new IPC commands**

In `src-tauri/src/commands.rs`, add `use crate::personas;` and `use crate::voiceprint::VoiceprintEngine;` to the imports, then add these commands (after `rename_speaker`):

```rust
// ---------------------------------------------------------------------------
// Personas + voiceprints (milestone 9)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonaInfo {
    pub id: i64,
    pub display_name: String,
    pub notes: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub voiceprint_count: i64,
}

#[tauri::command]
pub fn list_personas(db: State<'_, Arc<Db>>) -> Result<Vec<PersonaInfo>, String> {
    db.list_personas()
        .map(|v| v.into_iter().map(|p| PersonaInfo {
            id: p.id,
            display_name: p.display_name,
            notes: p.notes,
            created_at_ms: p.created_at_ms,
            updated_at_ms: p.updated_at_ms,
            voiceprint_count: p.voiceprint_count,
        }).collect())
}

#[tauri::command]
pub fn create_persona(db: State<'_, Arc<Db>>, display_name: String) -> Result<i64, String> {
    db.create_persona(&display_name)
}

#[tauri::command]
pub fn rename_persona(
    db: State<'_, Arc<Db>>,
    persona_id: i64,
    display_name: String,
) -> Result<(), String> {
    db.rename_persona(persona_id, &display_name)
}

#[tauri::command]
pub fn delete_persona(db: State<'_, Arc<Db>>, persona_id: i64) -> Result<(), String> {
    db.delete_persona(persona_id)
}

#[tauri::command]
pub fn delete_all_voiceprints(db: State<'_, Arc<Db>>) -> Result<(), String> {
    db.delete_all_voiceprints()
}

/// Re-run embedding + matching for a meeting's diarized speakers; persists
/// suggestions (confirmed preserved). Useful to re-match after the gallery
/// grew. Fails gracefully if the audio has been deleted.
#[tauri::command]
pub async fn identify_speakers(
    app: AppHandle,
    db: State<'_, Arc<Db>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    meeting_id: i64,
) -> Result<Vec<personas::SpeakerMatch>, String> {
    let db = db.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let diarizer = diarizer.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (_, system_wav) = db.meeting_wavs(meeting_id)?;
        let system_wav = system_wav.ok_or("this meeting's audio files have been deleted")?;
        let turns = diarizer.diarize_wav(&app, &system_wav, None)?;
        let settings = db.get_settings();
        let matches = personas::identify_and_persist(
            &db, &voiceprint, &app, meeting_id, &system_wav, &turns, &settings,
        )?;
        let _ = app.emit_to("main", "speakers:identified", matches.clone());
        Ok(matches)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Confirm that `raw_label` in a meeting is `persona_id`. Marks the link
/// confirmed AND enrolls that speaker's embedding (if audio is available).
/// Also applies the persona name via the per-meeting display mapping so the
/// transcript shows the name.
#[tauri::command]
pub async fn confirm_speaker_persona(
    app: AppHandle,
    db: State<'_, Arc<Db>>,
    voiceprint: State<'_, Arc<VoiceprintEngine>>,
    diarizer: State<'_, Arc<DiarizeEngine>>,
    meeting_id: i64,
    raw_label: String,
    persona_id: i64,
) -> Result<(), String> {
    let db2 = db.inner().clone();
    let voiceprint = voiceprint.inner().clone();
    let diarizer = diarizer.inner().clone();
    let raw = raw_label.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // 1. Mark the link confirmed.
        db2.set_link_confirmed(meeting_id, &raw, persona_id)?;
        // 2. Apply the persona name to the per-meeting display mapping.
        let name = db2
            .list_personas()?
            .into_iter()
            .find(|p| p.id == persona_id)
            .map(|p| p.display_name)
            .ok_or("persona not found")?;
        db2.rename_speaker(meeting_id, &raw, Some(&name))?;
        // 3. Enroll the embedding (best-effort if audio still present).
        let (mic, system) = db2.meeting_wavs(meeting_id)?;
        if let Some(system_wav) = system {
            let turns = diarizer.diarize_wav(&app, &system_wav, None)?;
            let cap = db2.get_settings().voiceprint_gallery_cap;
            let _ = personas::enroll(
                &db2, &voiceprint, &app, persona_id, meeting_id, &raw,
                Some(&system_wav), &turns, cap,
            );
        }
        let _ = mic; // mic unused for enrollment
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove the persona link for a raw label and clear any display rename
/// applied by a prior confirm (revert to the raw `SPEAKER_xx` label).
#[tauri::command]
pub async fn unlink_speaker_persona(
    db: State<'_, Arc<Db>>,
    meeting_id: i64,
    raw_label: String,
) -> Result<(), String> {
    db.unlink_speaker(meeting_id, &raw_label)?;
    db.rename_speaker(meeting_id, &raw_label, None)?;
    Ok(())
}
```

- [ ] **Step 3: Run identification inside `diarize_meeting`**

In `src-tauri/src/commands.rs`, change `diarize_meeting` to also take `voiceprint: State<'_, Arc<VoiceprintEngine>>` and, after `db.ensure_speakers(...)` and **before** the audio-deletion block, run identification:

```rust
        // Milestone 9: identity layer (additive). Runs after speakers are
        // persisted; never blocks the diarize result on failure.
        let settings = db.get_settings();
        if let Err(e) = {
            let matches = personas::identify_and_persist(
                &db, &voiceprint, &app, meeting_id, &system_wav, &turns, &settings,
            );
            match matches {
                Ok(m) => { let _ = app.emit_to("main", "speakers:identified", m); Ok(()) }
                Err(e) => Err(e),
            }
        } {
            eprintln!("identify_speakers failed (non-fatal): {e}");
        }
```

Add `voiceprint: State<'_, Arc<VoiceprintEngine>>` to the `diarize_meeting` signature (after `diarizer`), and `let voiceprint = voiceprint.inner().clone();` in the clone block at the top of the function.

- [ ] **Step 4: Register the new commands**

In `src-tauri/src/lib.rs`, add to the `invoke_handler!` list (after `commands::rename_speaker,`):

```rust
            commands::list_personas,
            commands::create_persona,
            commands::rename_persona,
            commands::delete_persona,
            commands::delete_all_voiceprints,
            commands::identify_speakers,
            commands::confirm_speaker_persona,
            commands::unlink_speaker_persona,
```

- [ ] **Step 5: Build**

Run: `cd src-tauri && cargo build`
Expected: compiles. Run `cargo test` — all existing + new tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/db/mod.rs src-tauri/src/lib.rs
git commit -m "Milestone 9: persona/voiceprint IPC commands + identify runs after diarize"
```

---

## Task 8: Frontend — ipc.ts types + wrappers

**Files:**
- Modify: `src/lib/ipc.ts`

- [ ] **Step 1: Add types**

Append to `src/lib/ipc.ts` (after the `MeetingDetail` interface, update it):

```ts
export interface SpeakerLink {
  rawLabel: string;
  personaId: number | null;
  personaName: string | null;
  confidence: number | null;
  confirmed: boolean;
}
```

Add `speakerLinks: Record<string, SpeakerLink>;` to the `MeetingDetail` interface.

Add the persona/match types and wrappers:

```ts
// ---------------------------------------------------------------------------
// Personas + voiceprints (M9)
// ---------------------------------------------------------------------------

export interface Persona {
  id: number;
  displayName: string;
  notes: string | null;
  createdAtMs: number;
  updatedAtMs: number;
  voiceprintCount: number;
}

export type Tier = "auto" | "suggest" | "unknown";

export interface PersonaScore {
  personaId: number;
  displayName: string;
  score: number;
  tier: Tier;
}

export interface SpeakerMatch {
  rawLabel: string;
  suggestions: PersonaScore[];
  bestScore: number;
  alreadyLinked: boolean;
  confirmed: boolean;
}

export function listPersonas(): Promise<Persona[]> {
  return invoke<Persona[]>("list_personas");
}

export function createPersona(displayName: string): Promise<number> {
  return invoke<number>("create_persona", { displayName });
}

export function renamePersona(personaId: number, displayName: string): Promise<void> {
  return invoke("rename_persona", { personaId, displayName });
}

export function deletePersona(personaId: number): Promise<void> {
  return invoke("delete_persona", { personaId });
}

export function deleteAllVoiceprints(): Promise<void> {
  return invoke("delete_all_voiceprints");
}

/** Re-run embedding + matching; emits speakers:identified. */
export function identifySpeakers(meetingId: number): Promise<SpeakerMatch[]> {
  return invoke<SpeakerMatch[]>("identify_speakers", { meetingId });
}

export function confirmSpeakerPersona(
  meetingId: number,
  rawLabel: string,
  personaId: number,
): Promise<void> {
  return invoke("confirm_speaker_persona", { meetingId, rawLabel, personaId });
}

export function unlinkSpeakerPersona(
  meetingId: number,
  rawLabel: string,
): Promise<void> {
  return invoke("unlink_speaker_persona", { meetingId, rawLabel });
}

export function onSpeakersIdentified(
  cb: (e: SpeakerMatch[]) => void,
): Promise<UnlistenFn> {
  return listen<SpeakerMatch[]>("speakers:identified", (ev) => cb(ev.payload));
}
```

Also extend `AppSettings` with `personaAutoThreshold: number;`, `personaSuggestThreshold: number;`, `voiceprintGalleryCap: number;`.

- [ ] **Step 2: Type-check**

Run: `npm run build` (runs `tsc && vite build`)
Expected: compiles with no type errors.

- [ ] **Step 3: Commit**

```bash
git add src/lib/ipc.ts
git commit -m "Milestone 9: frontend IPC types + wrappers for personas/voiceprints"
```

---

## Task 9: Frontend — SpeakerPersonaPicker + persona-aware chip

**Files:**
- Create: `src/components/SpeakerPersonaPicker.tsx`
- Modify: `src/components/TranscriptPane.tsx`
- Modify: `src/views/MeetingDetail.tsx`

- [ ] **Step 1: Create the picker component**

Create `src/components/SpeakerPersonaPicker.tsx`:

```tsx
import { useEffect, useRef, useState } from "react";
import { Check, Plus, X } from "lucide-react";

import type { Persona, PersonaScore } from "@/lib/ipc";

interface Props {
  rawLabel: string;
  current: PersonaScore | null;
  /** All personas for the "choose different" list. */
  personas: Persona[];
  onConfirm: (personaId: number) => void;
  onCreatePersona: (name: string) => Promise<number>;
  onDismiss: () => void;
}

/**
 * Picker for confirming / choosing / creating / dismissing a persona for a
 * diarized speaker. Free-text create doubles as the legacy rename path.
 */
export default function SpeakerPersonaPicker({
  rawLabel,
  current,
  personas,
  onConfirm,
  onCreatePersona,
  onDismiss,
}: Props) {
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (creating) inputRef.current?.focus();
  }, [creating]);

  const pct = (s: number) => `${Math.round(Math.max(0, Math.min(1, s)) * 100)}%`;

  const commitCreate = async () => {
    const name = draft.trim();
    if (!name) return;
    const id = await onCreatePersona(name);
    onConfirm(id);
  };

  return (
    <div
      className="absolute z-50 w-64 rounded-lg border bg-popover p-2 shadow-md"
      // crude click-outside dismissal
      onClick={(e) => e.stopPropagation()}
    >
      <div className="mb-1 flex items-center justify-between">
        <span className="text-[11px] font-medium text-muted-foreground">
          Assign {rawLabel}
        </span>
        <button
          className="text-muted-foreground hover:text-foreground"
          onClick={onDismiss}
          aria-label="Dismiss"
        >
          <X className="size-3.5" />
        </button>
      </div>

      {current && (
        <button
          onClick={() => onConfirm(current.personaId)}
          className="mb-1 flex w-full items-center justify-between rounded-md bg-primary/10 px-2 py-1.5 text-xs hover:bg-primary/15"
        >
          <span className="flex items-center gap-1.5 font-medium">
            <Check className="size-3.5" /> {current.displayName}
          </span>
          <span className="text-[10px] text-muted-foreground">{pct(current.score)}</span>
        </button>
      )}

      {!creating && (
        <>
          <div className="max-h-40 overflow-y-auto">
            {personas
              .filter((p) => p.id !== current?.personaId)
              .map((p) => (
                <button
                  key={p.id}
                  onClick={() => onConfirm(p.id)}
                  className="flex w-full items-center justify-between rounded-md px-2 py-1.5 text-xs hover:bg-accent"
                >
                  <span>{p.displayName}</span>
                  <span className="text-[10px] text-muted-foreground">
                    {p.voiceprintCount} prints
                  </span>
                </button>
              ))}
          </div>
          <button
            onClick={() => setCreating(true)}
            className="mt-1 flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-xs hover:bg-accent"
          >
            <Plus className="size-3.5" /> Create new persona…
          </button>
        </>
      )}

      {creating && (
        <div className="space-y-1">
          <input
            ref={inputRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commitCreate}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitCreate();
              if (e.key === "Escape") setCreating(false);
            }}
            placeholder="Persona name"
            className="w-full rounded-md border bg-background px-2 py-1 text-xs outline-none focus:border-ring"
          />
          <p className="text-[10px] text-muted-foreground">
            Enter to create + confirm, Esc to cancel.
          </p>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Make the chip persona-aware**

In `src/components/TranscriptPane.tsx`, extend `Props` with `speakerLinks?: Record<string, SpeakerLink>` and `personas?: Persona[]`, plus callbacks `onConfirmPersona?: (raw: string, personaId: number) => void` and `onUnlinkPersona?: (raw: string) => void` and `onCreatePersona?: (name: string) => Promise<number>`. Import the new types and `SpeakerPersonaPicker`.

Update `SpeakerChip` to:
- Accept the new props.
- If `speakerLinks[raw]` exists and is **not confirmed**, show the suggested persona name (from the link) with a subtle dashed ring + confidence %; clicking opens the `SpeakerPersonaPicker`.
- If confirmed, show the name plainly; clicking opens the picker (to reassign/unlink).
- If no link, keep current behavior but clicking opens the picker instead of the free-text input (so personas are the primary path; "create new persona" covers free-text rename).

Concretely, replace the `SpeakerChip` component body's editing/render block: remove the old `editing`/`draft` free-text input and render the picker popover instead. Keep the `chipColor` styling. A minimal implementation:

```tsx
import { useEffect, useRef, useState } from "react";
import SpeakerPersonaPicker from "@/components/SpeakerPersonaPicker";
import type { Persona, SpeakerLink } from "@/lib/ipc";
// ... existing imports ...

interface Props {
  segments: TranscriptSegment[];
  follow?: boolean;
  renames?: Record<string, string>;
  speakerLinks?: Record<string, SpeakerLink>;
  personas?: Persona[];
  onRenameSpeaker?: (raw: string, name: string) => void;
  onConfirmPersona?: (raw: string, personaId: number) => void;
  onUnlinkPersona?: (raw: string) => void;
  onCreatePersona?: (name: string) => Promise<number>;
  className?: string;
}
```

In `SpeakerChip`: compute `const link = speakerLinks?.[raw];` and `const suggestedName = link?.personaName ?? renames[raw] ?? raw;`. Render:

- A relative-positioned wrapper `<span className="relative">`.
- The chip button showing `suggestedName`; if `link && !link.confirmed`, add a dashed ring (`ring-1 ring-dashed ring-amber-500/50`) and append a small `{Math.round((link.confidence ?? 0) * 100)}%` superscript.
- A `pickerOpen` state that toggles `<SpeakerPersonaPicker .../>` absolutely positioned below the chip.
- The picker's `onConfirm` → call `onConfirmPersona(raw, id)` and close; `onDismiss` → call `onUnlinkPersona?.(raw)` only if a link exists, else just close; `onCreatePersona` → `onCreatePersona`.

Keep the "Me" and unlabeled branches unchanged.

- [ ] **Step 3: Wire it in `MeetingDetail.tsx`**

In `src/views/MeetingDetail.tsx`:
- Import `confirmSpeakerPersona`, `unlinkSpeakerPersona`, `createPersona`, `listPersonas`, `onSpeakersIdentified`, and the new types.
- State: `const [personas, setPersonas] = useState<Persona[]>([]);` loaded on mount and reloaded after confirm/create.
- Subscribe to `onSpeakersIdentified` → `reload()` (the meeting detail now carries updated `speakerLinks`).
- Pass `speakerLinks={meeting.speakerLinks}`, `personas`, and the three callbacks to `TranscriptPane`.
- `onConfirmPersona`: `await confirmSpeakerPersona(id, raw, personaId); reload(); setPersonas(await listPersonas());`
- `onUnlinkPersona`: `await unlinkSpeakerPersona(id, raw); reload();`
- `onCreatePersona`: `const pid = await createPersona(name); setPersonas(await listPersonas()); return pid;`
- Keep the existing `onRename` for the "Me"/no-link path (still used by the chip's fallback).

- [ ] **Step 4: Build**

Run: `npm run build`
Expected: compiles with no type errors.

- [ ] **Step 5: Commit**

```bash
git add src/components/SpeakerPersonaPicker.tsx src/components/TranscriptPane.tsx src/views/MeetingDetail.tsx
git commit -m "Milestone 9: persona-aware speaker chips with confirm/create/dismiss picker"
```

---

## Task 10: Frontend — Personas view + nav

**Files:**
- Create: `src/views/Personas.tsx`
- Modify: `src/App.tsx`

- [ ] **Step 1: Create the Personas view**

Create `src/views/Personas.tsx`:

```tsx
import { useCallback, useEffect, useState } from "react";
import { Trash2, Users } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  createPersona,
  deleteAllVoiceprints,
  deletePersona,
  listPersonas,
  renamePersona,
  type Persona,
} from "@/lib/ipc";

export default function PersonasView() {
  const [personas, setPersonas] = useState<Persona[]>([]);
  const [draft, setDraft] = useState("");
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(() => {
    listPersonas().then(setPersonas).catch((e) => setError(String(e)));
  }, []);

  useEffect(reload, [reload]);

  const add = useCallback(async () => {
    const name = draft.trim();
    if (!name) return;
    try {
      await createPersona(name);
      setDraft("");
      reload();
    } catch (e) {
      setError(String(e));
    }
  }, [draft, reload]);

  const commitRename = useCallback(
    async (id: number) => {
      const name = editDraft.trim();
      setEditingId(null);
      if (!name) return;
      try {
        await renamePersona(id, name);
        reload();
      } catch (e) {
        setError(String(e));
      }
    },
    [editDraft, reload],
  );

  const remove = useCallback(
    async (id: number) => {
      if (!confirm("Delete this persona and all its stored voiceprints?")) return;
      try {
        await deletePersona(id);
        reload();
      } catch (e) {
        setError(String(e));
      }
    },
    [reload],
  );

  const removeAll = useCallback(async () => {
    if (
      !confirm(
        "Delete ALL stored voiceprints for every persona? Personas stay, but recognition will reset.",
      )
    )
      return;
    try {
      await deleteAllVoiceprints();
      reload();
    } catch (e) {
      setError(String(e));
    }
  }, [reload]);

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-8 pt-12">
      <div>
        <h1 className="flex items-center gap-2 text-lg font-semibold tracking-tight">
          <Users className="size-5" /> Personas
        </h1>
        <p className="text-sm text-muted-foreground">
          Named speakers LilNotes learns across meetings. Each persona holds a
          gallery of voiceprints that grows when you confirm an identity.
        </p>
      </div>

      <div className="flex gap-2">
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && add()}
          placeholder="New persona name…"
          className="h-9 flex-1 rounded-md border bg-background px-2 text-sm outline-none focus:border-ring"
        />
        <Button size="sm" onClick={add} disabled={!draft.trim()}>
          Add
        </Button>
      </div>

      {error && <p className="text-sm text-destructive">{error}</p>}

      <div className="divide-y rounded-xl border bg-card">
        {personas.length === 0 && (
          <div className="p-4 text-sm text-muted-foreground">
            No personas yet. Confirm a speaker in any meeting to create one.
          </div>
        )}
        {personas.map((p) => (
          <div key={p.id} className="flex items-center justify-between gap-3 p-3">
            <div className="min-w-0 flex-1">
              {editingId === p.id ? (
                <input
                  autoFocus
                  value={editDraft}
                  onChange={(e) => setEditDraft(e.target.value)}
                  onBlur={() => commitRename(p.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") commitRename(p.id);
                    if (e.key === "Escape") setEditingId(null);
                  }}
                  className="w-full rounded-md border bg-background px-2 py-0.5 text-sm outline-none focus:border-ring"
                />
              ) : (
                <button
                  onClick={() => {
                    setEditingId(p.id);
                    setEditDraft(p.displayName);
                  }}
                  className="block w-full truncate text-left text-sm font-medium hover:opacity-80"
                >
                  {p.displayName}
                </button>
              )}
              <p className="text-xs text-muted-foreground">
                {p.voiceprintCount} voiceprint{p.voiceprintCount === 1 ? "" : "s"}
              </p>
            </div>
            <Button
              size="icon"
              variant="ghost"
              className="size-7 text-muted-foreground hover:text-destructive"
              onClick={() => remove(p.id)}
              aria-label={`Delete ${p.displayName}`}
            >
              <Trash2 className="size-3.5" />
            </Button>
          </div>
        ))}
      </div>

      {personas.length > 0 && (
        <Button variant="outline" size="sm" onClick={removeAll}>
          <Trash2 className="size-3.5" /> Delete all voiceprints
        </Button>
      )}

      <p className="text-xs text-muted-foreground">
        Voiceprints are stored locally in{" "}
        <code className="rounded bg-secondary px-1">
          ~/Library/Application Support/co.elastic.lilnote/lilnotes.sqlite3
        </code>
        . They never leave your Mac.
      </p>
    </div>
  );
}
```

- [ ] **Step 2: Add the route + nav item**

In `src/App.tsx`:
- Import `PersonasView` and the `Users` icon (already used in MeetingDetail; import from `lucide-react`).
- Add `| { name: "personas" }` to the `Route` union.
- Add to `NAV`: `{ route: { name: "personas" } as Route, label: "Personas", icon: Users }`.
- Render: `{route.name === "personas" && <PersonasView />}`.

- [ ] **Step 3: Build**

Run: `npm run build`
Expected: compiles with no type errors.

- [ ] **Step 4: Commit**

```bash
git add src/views/Personas.tsx src/App.tsx
git commit -m "Milestone 9: Personas management view + sidebar nav"
```

---

## Task 11: Settings — thresholds, gallery cap, privacy controls

**Files:**
- Modify: `src/views/Settings.tsx`

- [ ] **Step 1: Add a "Personas & voiceprints" section**

In `src/views/Settings.tsx`, after the "Summaries" section, add:

```tsx
      {/* ------------------------------------------------------------- */}
      {/* Personas & voiceprints                                          */}
      {/* ------------------------------------------------------------- */}
      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">
          Personas &amp; voiceprints
        </h2>

        <div className="divide-y rounded-xl border bg-card">
          <div className="space-y-2 p-4">
            <div className="text-sm font-medium">Match thresholds</div>
            <p className="text-xs text-muted-foreground">
              Cosine similarity cutoffs for suggesting known personas on
              diarized speakers. Higher = fewer false matches.
            </p>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Auto-suggest (pre-fill)</span>
              <input
                type="number"
                step="0.01"
                min="0"
                max="1"
                value={settings?.personaAutoThreshold ?? 0.65}
                onChange={(e) =>
                  settings &&
                  saveSettings({
                    ...settings,
                    personaAutoThreshold: parseFloat(e.target.value) || 0,
                  })
                }
                className="h-7 w-20 rounded-md border bg-background px-2 text-right outline-none focus:border-ring"
              />
            </label>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Tentative suggestion</span>
              <input
                type="number"
                step="0.01"
                min="0"
                max="1"
                value={settings?.personaSuggestThreshold ?? 0.45}
                onChange={(e) =>
                  settings &&
                  saveSettings({
                    ...settings,
                    personaSuggestThreshold: parseFloat(e.target.value) || 0,
                  })
                }
                className="h-7 w-20 rounded-md border bg-background px-2 text-right outline-none focus:border-ring"
              />
            </label>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Voiceprints per persona (cap)</span>
              <input
                type="number"
                step="1"
                min="0"
                value={settings?.voiceprintGalleryCap ?? 50}
                onChange={(e) =>
                  settings &&
                  saveSettings({
                    ...settings,
                    voiceprintGalleryCap: parseInt(e.target.value, 10) || 0,
                  })
                }
                className="h-7 w-20 rounded-md border bg-background px-2 text-right outline-none focus:border-ring"
              />
            </label>
          </div>

          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="text-sm font-medium">Delete all voiceprints</div>
              <p className="text-xs text-muted-foreground">
                Clears every persona's stored voiceprints. Personas stay, but
                recognition starts over. Manage individual personas in the
                Personas view.
              </p>
            </div>
            <Button
              size="sm"
              variant="outline"
              onClick={async () => {
                if (!confirm("Delete ALL stored voiceprints?")) return;
                await deleteAllVoiceprints();
              }}
            >
              Clear
            </Button>
          </div>

          <div className="p-4 text-xs text-muted-foreground">
            Voiceprints are biometric data stored only in the local SQLite
            database at{" "}
            <code className="rounded bg-secondary px-1">
              ~/Library/Application Support/co.elastic.lilnote/lilnotes.sqlite3
            </code>
            . They never leave your Mac.
          </div>
        </div>
      </section>
```

Add `deleteAllVoiceprints` to the imports from `@/lib/ipc`.

- [ ] **Step 2: Build**

Run: `npm run build`
Expected: compiles with no type errors.

- [ ] **Step 3: Commit**

```bash
git add src/views/Settings.tsx
git commit -m "Milestone 9: persona thresholds, gallery cap, voiceprint privacy controls in Settings"
```

---

## Task 12: README — milestone 9 row, verification, privacy note

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the milestone status table**

In `README.md`, change the milestone-9 row (currently absent) — add a new row after row 8:

```
| 9 | Cross-meeting voiceprints & named personas | ✅ done |
```

(If row 9 isn't present, append it to the table.)

- [ ] **Step 2: Add the privacy framing note**

In the "Architecture" section, after the line about mic/system labeling, add:

```markdown
**Voiceprints & personas.** CAM++ speaker embeddings computed during
diarization are persisted (L2-normalized f32 vectors) as per-persona
"voiceprint galleries" in the same SQLite database. Matching is brute-force
cosine similarity on-device; a persona's gallery grows only when you confirm
an identity, so unconfirmed suggestions never corrupt it. Voiceprints are
biometric data at rest and never leave your Mac. Manage or delete them in
**Personas** (sidebar) or **Settings → Personas & voiceprints**.

**Encryption at rest.** The entire SQLite database (meetings, transcripts,
summaries, personas, and voiceprints) is encrypted with SQLCipher. The
256-bit key is generated on first run and stored in the macOS Keychain
(service `co.elastic.lilnote`); it is protected by your login keychain
(FileVault + user password). If the key is deleted, the database becomes
unreadable — effectively a factory reset. Nothing in the database or the
key ever leaves your Mac.
```

- [ ] **Step 3: Add the "Verifying milestone 9" section**

Append after the "Verifying milestone 6" section:

```markdown
### Verifying milestone 9

1. Record/transcribe/diarize a meeting with a distinct remote speaker; click
   that speaker's chip → **Create new persona** "Priya" → confirm. Verify via
   the Personas view (shows "Priya — 1 voiceprint"). The DB is now encrypted
   with SQLCipher, so plain `sqlite3` reports "file is not a database" — that
   itself confirms encryption is active. To inspect rows, install
   `sqlcipher` (`brew install sqlcipher`), fetch the key from the Keychain
   (`security find-generic-password -s co.elastic.lilnote -a db-key -w`),
   hex-encode it, and open with `PRAGMA key = "x'<64 hex chars>'"`.
2. Record a **second** meeting with the same person. After diarization the
   chip should pre-fill "Priya" with a confidence % (dashed ring = suggested).
   Confirm it → `SELECT COUNT(*) FROM voiceprints;` increments (gallery grew).
3. Negative: a brand-new voice stays `SPEAKER_xx` (no false auto-match).
4. Adaptive case: in a meeting where Priya is *not* auto-matched (below
   threshold), manually assign her via the picker; confirm a new voiceprint
   row is enrolled, then re-run a similar later recording and check the
   confidence is higher / now clears the threshold.
5. `delete_persona("Priya")` (Personas view) removes her voiceprints and
   nulls links; the transcript falls back to the raw `SPEAKER_xx` label.
6. Encryption: on first launch a Keychain entry is created (service
   `co.elastic.lilnote`, account `db-key` — verify with `security find-generic-password -s co.elastic.lilnote -a db-key`). Quit, relaunch — the DB
   unlocks and all data reloads. `sqlite3` on the DB file reports "file is
   not a database" (encrypted). Deleting the Keychain item and relaunching
   makes the DB unreadable (intended factory-reset behavior). `cd src-tauri
   && cargo test` passes (pack/unpack round-trip, cosine, threshold
   classification, CRUD — all with the fixed test key, no Keychain access).
```

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "Milestone 9: README — milestone row, verification, voiceprint privacy note"
```

---

## Task 13: Final verification

- [ ] **Step 1: Run the full Rust test suite**

Run: `cd src-tauri && cargo test`
Expected: all tests pass — existing transcript tests + new `db::tests`, `voiceprint::tests`, `personas::tests`.

- [ ] **Step 2: Build the whole app**

Run: `npm run build` then `cd src-tauri && cargo build`
Expected: both succeed with no errors or warnings about the new code.

- [ ] **Step 3: Manual smoke test (dev)**

Run: `npm run tauri dev`
- First launch: macOS may show a Keychain authorization prompt to store the DB key — allow it (this is the one-time encryption-setup prompt; the app is self-contained and doesn't use the key for anything else).
- Open Personas view — empty state shows.
- Record a short meeting with a remote speaker; transcribe + diarize; after "Identifying speakers…" a chip shows `SPEAKER_00` with no suggestion (gallery empty).
- Click the chip → create persona "Priya" → confirm. Reopen Personas: Priya with 1 voiceprint.
- Record a second meeting with the same speaker; diarize; chip pre-fills "Priya" + confidence; confirm; Personas shows 2 voiceprints.
- Delete Priya in Personas; the meeting chip reverts to `SPEAKER_00`.
- Settings → Personas & voiceprints: adjust thresholds; "Delete all voiceprints" clears the gallery.

- [ ] **Step 4: Final commit (if any drift)**

If the smoke test surfaced fixes, commit them. Otherwise no-op.

---

## Self-Review Notes

- **Spec coverage:** full-DB SQLCipher encryption + Keychain key (Task 0) ✓; migration (Task 1) ✓; embeddings per speaker + 3 s gate (Task 2) ✓; cosine/max matching + two thresholds + classify (Task 3) ✓; enroll + gallery cap/pruning (Tasks 4, 6) ✓; IPC commands incl. list/create/rename/delete/identify/confirm/unlink + delete-all-voiceprints (Tasks 7) ✓; identify wired after diarize (Task 7) ✓; register in invoke_handler (Task 7) ✓; `speaker_persona_links` shown in detail view (Tasks 7–9) ✓; frontend ipc.ts wrappers (Task 8) ✓; persona-aware chip + confirm/choose/create/dismiss picker (Task 9) ✓; Personas management surface (Task 10) ✓; thresholds in Settings (Task 11) ✓; README privacy note + verification + encryption note (Task 12) ✓; edge cases (< 3 s skip, over-split both-confirmable, no re-enroll on confirmed, idempotent re-identify via `upsert_link` preserving `confirmed`) covered in Tasks 2/4/6/7 ✓. The spec's "Encryption-at-rest of the DB is a reasonable stretch goal" is upgraded to a hard requirement per user direction (app is unreleased → no migration concern).
- **Placeholders:** none — every step has concrete code or commands.
- **Type consistency:** `SpeakerLink` (Rust `confirmed: bool` ↔ TS `confirmed: boolean`), `PersonaScore`/`SpeakerMatch`/`Tier` shapes match across `personas/mod.rs`, `commands.rs`, and `ipc.ts`. `MeetingDetail.speaker_links` ↔ `speakerLinks` (camelCase via `rename_all`). `PersonaWithEmbeddings` used only Rust-side by `rank_personas`. `Db::open(path, key)` signature is consistent across `lib.rs` setup and the test helper.
- **Encryption notes:** `Db::open` takes the key as a parameter (Keychain-agnostic) so tests pass a fixed `[0x42u8; 32]` key and never touch the Keychain. `bundled-sqlcipher-vendored-openssl` is chosen over plain `bundled-sqlcipher` to keep the signed/notarized build self-contained (no system-OpenSSL ambiguity). First build is slower (compiles OpenSSL); subsequent builds are cached. The `PRAGMA key = "x'<hex>'"` raw-key form bypasses SQLCipher's PBKDF2 since we already hold a CSPRNG-generated 256-bit key.
- **Known limitations surfaced in plan:** `identify_speakers` standalone command re-diarizes to get turns (matches `confirm_speaker_persona`); if audio is deleted, both degrade gracefully (identify errors; confirm marks the link confirmed without enrolling). If the Keychain key is deleted, the DB is unrecoverable (intended privacy behavior — surface a clear error in `lib.rs` setup).
```