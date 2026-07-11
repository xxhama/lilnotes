# Implementation prompt — Milestone 9: Cross-meeting voiceprints & named personas

> Hand this whole file to Claude Code from the repo root. It is written to be
> executed against the current LilNotes codebase. Read the referenced files
> before writing any code; match the project's existing style, error handling
> (`Result<_, String>`), and single-mutex SQLite pattern.

## Context (read first)

LilNotes is a local-first Tauri v2 app (Rust core `lilnotes_lib` + React 19 / TS /
Tailwind / shadcn). Everything runs on-device — no cloud, no telemetry, **no
Python at runtime**. Meetings are captured as two 16 kHz mono WAVs (`mic.wav` =
the local user, always labeled "Me"; `system.wav` = remote participants).
`system.wav` is transcribed by whisper-rs and diarized by sherpa-onnx.

**The critical insight that makes this feature cheap:** diarization already runs
**3D-Speaker CAM++ speaker embeddings** (`src-tauri/src/models/mod.rs`,
`DIARIZE_EMBEDDING_*`, file `3dspeaker-campplus-sv-16k.onnx`, ~28 MB, already
downloaded on first diarization, largely language-agnostic). Those embeddings
_are_ voiceprints. Today they're used only for within-meeting clustering and
then thrown away. This milestone persists them, attaches them to named
**personas** that live across meetings, matches new meetings against known
personas, and **grows each persona's voiceprint gallery every time a human
confirms an identity** — so recognition improves over time.

### Files you will touch or extend

- `src-tauri/src/db/mod.rs` — schema/migrations (`migrate()`, currently
  `PRAGMA user_version = 1`), `Db` methods. Add migration to version 2.
- `src-tauri/src/diarize/mod.rs` — `DiarizeEngine`, `diarize_wav`, `Turn`
  (start_ms/end_ms/speaker). Currently returns only cluster labels, **not the
  embeddings**. You need the embeddings.
- `src-tauri/src/models/mod.rs` — `diarize_model_paths(app)` /
  `ensure_diarize_models(app, cancel)` return `(segmentation, embedding)`
  `PathBuf`s. Reuse the embedding path.
- `src-tauri/src/transcript/mod.rs` — `assign_speakers(&mut segments, &turns)`
  maps turns → segment speaker labels. Leaves raw `SPEAKER_xx` labels intact.
- `src-tauri/src/commands.rs` — IPC surface. `diarize_meeting` orchestrates
  diarize → `assign_speakers` → `db.ensure_speakers`. `rename_speaker` sets a
  per-meeting display name. Add new commands here.
- `src/views/MeetingDetail.tsx`, `src/components/TranscriptPane.tsx` — speaker
  chips + existing rename UX. `src/views/Settings.tsx`, `src/lib/ipc.ts`.

## Goal

1. After a meeting is diarized, compute one representative CAM++ embedding per
   diarized speaker (`SPEAKER_00`, `SPEAKER_01`, …) on the system channel.
2. Compare each against a persistent **persona** gallery (cosine similarity).
   - High confidence → auto-suggest the persona name on that speaker's chip
     (pre-filled, still user-dismissable — never silently rewrite).
   - Medium confidence → show it as a tentative suggestion to confirm/reject.
   - Low / no match → leave as `SPEAKER_xx` (unknown), offer "assign to persona".
3. When the user confirms a speaker is a given persona (existing or newly
   created), **enroll that meeting's embedding into the persona's gallery**.
   This is the adaptive-learning loop: the exact case where a known persona
   wasn't auto-matched but the human labels them must add a new voiceprint so
   future matches are more accurate.

Personas are a mapping/identity layer, exactly like the current rename feature —
**raw `SPEAKER_xx` labels are never rewritten in the DB.**

## Hard constraints

- Fully local. No network calls except the existing model download path. No
  Python. No new heavyweight services.
- **Stay in SQLite.** Do not add a vector database. Store embeddings as BLOBs
  and do brute-force cosine similarity in Rust — the dataset is tiny (hundreds
  of personas × tens of embeddings × ~192 floats = a few MB; a match is
  sub-millisecond). `sqlite-vec` is acceptable _only_ if it genuinely
  simplifies queries, but brute force is the expected default.
- Migration must be **additive and idempotent** (guard with `user_version`),
  and must not break existing databases.
- Enrollment is **only ever triggered by explicit user confirmation**, never
  automatically — auto-enrolling on an uncertain match poisons the gallery.

## Data model (migration to `user_version = 2`)

Add to `migrate()` a `version < 2` block:

```sql
CREATE TABLE personas(
    id           INTEGER PRIMARY KEY,
    display_name TEXT NOT NULL UNIQUE,
    notes        TEXT,
    created_at   INTEGER NOT NULL,   -- unix epoch ms
    updated_at   INTEGER NOT NULL
);

-- One row per enrolled voiceprint. A persona accumulates many over time
-- (a "gallery"), which is what makes matching robust to mic/room/health.
CREATE TABLE voiceprints(
    id           INTEGER PRIMARY KEY,
    persona_id   INTEGER NOT NULL REFERENCES personas(id) ON DELETE CASCADE,
    embedding    BLOB NOT NULL,      -- packed little-endian f32[dim]
    dim          INTEGER NOT NULL,   -- store the length; do NOT hardcode it
    source_meeting_id INTEGER REFERENCES meetings(id) ON DELETE SET NULL,
    source_label TEXT,               -- e.g. 'SPEAKER_01' this came from
    speech_ms    INTEGER NOT NULL,   -- total speech used to compute it (quality)
    created_at   INTEGER NOT NULL
);
CREATE INDEX idx_voiceprints_persona ON voiceprints(persona_id);

-- Optional but recommended: record which persona a meeting's raw label was
-- linked to, so the detail view can show identities without re-matching.
CREATE TABLE speaker_persona_links(
    meeting_id  INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    raw_label   TEXT NOT NULL,
    persona_id  INTEGER REFERENCES personas(id) ON DELETE SET NULL,
    confidence  REAL,                -- cosine sim at link time (null if manual)
    confirmed   INTEGER NOT NULL DEFAULT 0,  -- 1 = human-confirmed
    PRIMARY KEY(meeting_id, raw_label)
);

PRAGMA user_version = 2;
```

Store embeddings **L2-normalized** so cosine similarity is a plain dot product.
Helpers: `fn pack_f32(v: &[f32]) -> Vec<u8>` / `fn unpack_f32(b: &[u8]) -> Vec<f32>`
(little-endian). Read `dim` back from the row; never assume a fixed length.

## Backend work (Rust)

### 1. Extract embeddings per diarized speaker

sherpa-rs exposes a standalone speaker-embedding extractor separate from
`Diarize` (confirm the exact type against the `sherpa-rs = "0.6"` API — it is in
the speaker-id / embedding-manager area, e.g. `sherpa_rs::embedding_manager` /
`EmbeddingExtractor`). Load it **once** with the already-present CAM++ model from
`models::diarize_model_paths(app).1` (download via `ensure_diarize_models` if
missing — same pattern `DiarizeEngine::ensure_loaded` uses).

For each diarized speaker in a meeting:

- Gather that speaker's `Turn`s (from `diarize_wav`), read the corresponding
  `system.wav` samples, and compute an embedding. Prefer concatenating the
  speaker's clean turns up to a few seconds; **skip enrollment/matching for
  speakers with less than ~3 s of speech** (`speech_ms` gate) — short segments
  give unreliable voiceprints.
- Produce one representative normalized embedding per speaker per meeting.

Suggested shape: extend `DiarizeEngine` (or a new `voiceprint` module) with
`fn embed_speakers(&self, app, wav_path, turns: &[Turn]) -> Result<HashMap<String, SpeakerEmbedding>, String>`
where `SpeakerEmbedding { vec: Vec<f32>, speech_ms: u64 }`. Keep `Turn` as-is;
don't break the existing diarize path or its tests.

### 2. Matching

New module `src-tauri/src/personas/mod.rs` (or add to `db`):

- `fn cosine(a: &[f32], b: &[f32]) -> f32` (dot product on normalized vecs).
- For a query embedding, score against every persona. A persona's score =
  **max** cosine over its voiceprints (max is more forgiving than centroid for a
  multi-condition gallery; optionally also compute the mean and use max as
  primary). Return ranked `(persona_id, display_name, score)`.
- Classify with two thresholds (see calibration): `>= auto` ⇒ strong suggestion,
  `[suggest, auto)` ⇒ tentative, `< suggest` ⇒ unknown.

### 3. Enrollment (the feedback loop)

`fn enroll(persona_id, embedding, source_meeting_id, source_label, speech_ms)`
inserts a `voiceprints` row and bumps `personas.updated_at`. Called when the
user confirms an identity — including the key case where a persona existed but
the speaker wasn't auto-matched: confirming links + enrolls, expanding the
gallery. Add a light cap/pruning policy (e.g. keep newest N per persona, or drop
near-duplicate embeddings) so galleries don't grow unbounded.

### 4. IPC commands (`src-tauri/src/commands.rs`)

Add (mirror existing command style, `#[tauri::command]`, `State` for `Db` /
engines):

- `list_personas() -> Vec<Persona>` (with voiceprint counts).
- `create_persona(display_name) -> Persona`.
- `rename_persona(persona_id, display_name)`, `delete_persona(persona_id)`
  (cascade removes voiceprints + nulls links).
- `identify_speakers(meeting_id) -> Vec<SpeakerMatch>` — runs embedding +
  matching for the meeting's diarized speakers, returns per-`raw_label` ranked
  persona suggestions with confidence + a suggested tier. Persist top results
  into `speaker_persona_links` (confirmed = 0).
- `confirm_speaker_persona(meeting_id, raw_label, persona_id)` — sets the link
  `confirmed = 1` **and enrolls** that speaker's embedding into the persona.
  Also apply the persona name via the existing per-meeting display mapping so
  the transcript shows the name (reuse/extend `rename_speaker`).
- `unlink_speaker_persona(meeting_id, raw_label)`.

Wire `identify_speakers` to run right after `diarize_meeting` finishes (or as an
explicit follow-up step, matching how diarization is a discrete post-transcribe
step today). Do **not** block or alter the existing diarize/transcribe result
shape; add identity as an additive layer. Register every new command in the
`invoke_handler` (check `src-tauri/src/lib.rs`).

## Frontend work (React)

- `src/lib/ipc.ts`: typed wrappers for the new commands.
- Speaker chips (`TranscriptPane.tsx` / `MeetingDetail.tsx`): when a chip has a
  suggested persona, show the name with a subtle "suggested" affordance +
  confidence; clicking opens a picker to **confirm**, **choose a different
  persona**, **create new persona**, or **dismiss** (keep `SPEAKER_xx`).
  Confirming calls `confirm_speaker_persona`. This replaces/extends the current
  free-text rename with a persona-aware picker (free-text creating a persona is
  fine).
- A **Personas** management surface (new view or a Settings section): list
  personas, voiceprint counts, rename, merge (optional/stretch), and delete —
  with a clear note that deleting removes stored voiceprints.

## Thresholds & calibration

CAM++ cosine similarity for same-speaker pairs is typically high but
condition-dependent. **Do not ship magic numbers as fact — calibrate.** Start
with `auto ≈ 0.65`, `suggest ≈ 0.45` as _placeholders_, expose both in Settings
(persisted via the existing `settings` table), and tune against real recordings.
Note the existing diarization `AUTO_THRESHOLD = 0.7` is a _clustering distance_,
a different quantity — don't conflate them. Favor false-reject over false-accept
(a wrong auto-label is worse than an unknown speaker).

## Privacy / security

Voiceprints are biometric data at rest. Keep them local (already the default).
`delete_persona` must hard-delete its voiceprints. Consider: a "delete all
voiceprints" control, and documenting where they're stored (the existing SQLite
DB at `<app data>/lilnotes.sqlite3`). Add a short note to the README's privacy
framing. Encryption-at-rest of the DB is a reasonable stretch goal, not required
for this milestone.

## Edge cases to handle

- Speakers with < ~3 s speech: no enroll, no confident match.
- Same persona appearing as two `SPEAKER_xx` in one meeting (diarization
  over-split): confirming both to the same persona is valid; just enroll both.
- Two different people matched to one persona: only human-confirmed links enroll,
  so an unconfirmed suggestion never corrupts the gallery.
- Re-running identify on an already-linked meeting must not duplicate links or
  re-enroll confirmed prints.
- Migration on an existing v1 DB must be clean; test upgrade path.
- The mic/"Me" channel is the local user by construction — out of scope to
  diarize, but optionally the local user could be a persona too (stretch).

## Verification (milestone-9 style, add to README)

1. Record/transcribe/diarize a meeting with a distinct remote speaker; confirm
   that speaker to a new persona "Priya". Check `personas` + `voiceprints` rows
   exist (`sqlite3 <app data>/lilnotes.sqlite3 'SELECT * FROM personas;'`).
2. Record a **second** meeting with the same person. Run identify: Priya should
   be suggested on the right chip with a visible confidence. Confirm it and
   verify a **second** voiceprint row was added (gallery grew).
3. Negative: a brand-new voice stays `SPEAKER_xx` (no false auto-match).
4. Adaptive case (the important one): a meeting where Priya is _not_ auto-matched
   (below threshold). Manually assign her; confirm a new voiceprint is enrolled,
   then re-run identify on a later similar recording and confirm the match
   confidence is higher / now clears threshold.
5. `delete_persona("Priya")` removes her voiceprints and nulls links; transcripts
   fall back to raw labels.
6. Upgrade path: open an existing pre-migration DB, confirm it migrates to
   v2 without data loss. `cd src-tauri && cargo test` passes (add unit tests for
   pack/unpack round-trip, cosine, and threshold classification).

## Out of scope (do not build)

Cloud sync, real-time/streaming identification during capture, voice cloning/TTS,
speaker anti-spoofing, and any non-SQLite datastore.

## Open decisions to confirm with Johnny before finalizing

- Auto-apply high-confidence names vs. always require one-click confirm
  (default in this spec: pre-fill/suggest, require confirm).
- Persona representation for scoring: max-over-gallery (default) vs. centroid.
- Gallery cap / pruning policy and default thresholds.
