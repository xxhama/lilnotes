# LilNotes

Local-first meeting notes for Apple Silicon Macs. Records microphone and
system audio as **two separate channels**, transcribes and diarizes entirely
on-device, and summarizes with a locally running [Ollama](https://ollama.com)
model. No cloud, no telemetry, no Python at runtime.

- **Shell:** Tauri v2 (Rust core, React + TypeScript + Tailwind + shadcn/ui front-end)
- **Capture:** Core Audio process tap (system) + mic, 16 kHz mono WAV per channel
- **ASR:** whisper.cpp via `whisper-rs` (Metal), default model `large-v3-turbo`
- **Diarization:** sherpa-onnx (pyannote segmentation-3.0 + CAM++ embeddings)
- **Summaries:** Ollama HTTP API at `http://localhost:11434`, streaming
- **Storage:** SQLite; WAVs + models in Application Support

**Target:** Apple Silicon, macOS 26 (Tahoe)+. Bundle ID: `com.lilnotes`.

## Development

Prerequisites: Rust (stable, via rustup), Node 20+, Xcode Command Line
Tools, CMake for the whisper.cpp build (`brew install cmake`), and
meson + ninja for the bundled WebRTC AEC build
(`brew install meson ninja`).

```sh
npm install
npm run tauri:dev     # dev app window (builds sidecar first)
npm run tauri:build   # release .app/.dmg (builds sidecar + fixes dylibs)
```

## Testing

```sh
npm test                          # Rust unit tests
npm run typecheck                 # TypeScript typecheck
npm run lint                      # ESLint
cd src-tauri && cargo clippy      # Rust lints
cd src-tauri && cargo fmt --check # Rust format check
```

## Milestone status

| #   | Milestone                                                          | Status  |
| --- | ------------------------------------------------------------------ | ------- |
| 1   | Scaffold: app shell, IPC round-trip                                | ✅ done |
| 2   | Dual-source capture → two 16 kHz WAVs, level meters, permissions   | ✅ done |
| 3   | Transcription (whisper-rs Metal), model download, near-live chunks | ✅ done |
| 4   | Diarization + merged speaker-labeled transcript                    | ✅ done |
| 5   | SQLite persistence, history/detail UI, speaker rename              | ✅ done |
| 6   | Ollama summaries + in-app model manager (pull w/ progress)         | ✅ done |
| 7   | Export: Markdown, PDF, clipboard                                   | ⬜      |
| 8   | Signing, notarization, .dmg, first-run flow                        | ⬜      |
| 9   | Cross-meeting voiceprints & named personas                         | ✅ done |

### Verifying milestone 1

`npm install && npm run tauri dev` — the window should open with the LilNotes
sidebar; on the Meetings screen, click **Test backend connection**: it should
show the backend version, round-trip latency, and echoed message.

### Verifying milestone 2

1. Go to **Record**, hit the record button. Approve the Microphone prompt,
   then the System Audio Recording prompt (first run only).
2. Talk, and play something (music, a video) so both meters move.
3. Stop. The card shows the two WAV paths under
   `~/Library/Application Support/com.lilnotes/recordings/<session>/`.
4. Inspect: `afinfo mic.wav system.wav` (both 16 kHz mono 16-bit) and play
   them — mic.wav has only your voice, system.wav only the playback.

Rust unit tests (resampler + limiter): `cd src-tauri && cargo test`.

**Troubleshooting capture**

- _No system-audio prompt appears / OSStatus error on start:_ the
  system-audio TCC category requires a signed binary. Dev builds are ad-hoc
  signed, which normally works; if not, run `npm run tauri build` once and
  launch the bundled app from `src-tauri/target/release/bundle/macos/`.
- _Re-test the prompts:_ `tccutil reset Microphone com.lilnotes` and
  `tccutil reset SystemAudioCaptureRequests com.lilnotes`.
- _system.wav is silent:_ make sure something is actually playing to the
  default output device (the tap follows the default output).

### Verifying milestone 3

1. In **Settings → Transcription**, download **Large v3 Turbo** (~1.6 GB;
   progress bar + cancel should work) and make sure it's selected. Leave
   "Live transcription" on.
2. Record a short session with speech on both channels (talk + play a video
   with speech). The transcript should fill in below the meters within
   ~5–15 s of each utterance, labeled Me / Speaker with timestamps.
3. Stop: the last chunk flushes, and `stop_recording` returns the full
   segment list. Timestamps should match the audio (`afplay` + spot-check).
4. Batch mode: toggle live transcription off, record again, then click
   **Transcribe recording** — same output, produced after the fact.

### Verifying milestone 4

1. Record a session where the system channel has **two or more distinct
   voices** (e.g. play a podcast/interview) while you also speak.
2. Stop. After transcription finishes, "Identifying speakers…" runs — the
   first time it downloads two small ONNX models (~34 MB total).
3. The generic "Speaker" chips become SPEAKER_00 / SPEAKER_01 (color-coded);
   your speech stays "Me". Spot-check that alternating voices in the
   recording alternate labels.
4. Click any SPEAKER_xx chip to rename it (e.g. "Priya") — the name applies
   across the transcript. (Renames persist per meeting from milestone 5.)

Diarization models: pyannote segmentation-3.0 + 3D-Speaker CAM++
embeddings, both ONNX via sherpa-onnx; clustering is threshold-based since
the speaker count is unknown. Rust tests: `cd src-tauri && cargo test`.

### Verifying milestone 5

1. Record a short meeting; after speakers are identified you land on the
   meeting's detail page automatically.
2. Rename a speaker and the meeting title, quit the app fully, relaunch —
   everything (transcript, labels, names, title) reloads from SQLite
   (`<app data>/lilnotes.sqlite3`).
3. Meetings shows the history; search matches titles _and_ transcript text;
   hovering a row reveals delete.
4. Settings → Storage: change the recordings folder (new sessions land
   there) and try "Delete audio after transcription" — after the next
   recording finishes processing, its WAVs are gone and the detail page
   shows an "audio deleted" badge.

### Verifying milestone 6

1. Install [Ollama](https://ollama.com/download) and launch it. (Quit it
   first to check the setup panel: Settings → Summaries should show "Ollama
   isn't running" with install guidance, and the rest of the app keeps
   working.)
2. Settings → Summaries: with Ollama running you see its version, the
   installed-model picker, and the curated suggestions (gemma4:26b
   recommended; gemma4:12b is the fast alternative if you want a quicker
   first test). Pull one — the progress bar should track real percent and
   cancel must work (a re-pull resumes where it left off).
3. Open a transcribed meeting → the Summary panel on the right →
   **Summarize**. Tokens should stream in live; the result is saved (check
   it survives an app restart) with model + timestamp shown.
4. Rename a speaker, hit **Regenerate** — action items should now use the
   new name. Try editing the prompt template in Settings and regenerating.

**Mic echo cancellation.** The mic channel uses macOS voice processing
(Apple's AEC + noise suppression, the FaceTime stack): speaker output and
steady room noise are removed from `mic.wav` at the driver level, so remote
voices shouldn't bleed into the "Me" track even without headphones. Ducking
of other audio is disabled so the system channel keeps its level. If voice
processing can't initialize on a device, capture automatically falls back to
the raw mic (a console line notes the fallback). Expect the voice-processed
mic to sound "thinner" than a raw recording — that is normal and fine for
ASR.

### Verifying milestone 9

1. Record/transcribe/diarize a meeting with a distinct remote speaker; click
   that speaker's chip → **Create new persona** "Priya" → confirm. Verify via
   the Personas view (shows "Priya — 1 voiceprint"). The DB is now encrypted
   with SQLCipher, so plain `sqlite3` reports "file is not a database" — that
   itself confirms encryption is active. To inspect rows, install
   `sqlcipher` (`brew install sqlcipher`), fetch the key from the Keychain
   (`security find-generic-password -s com.lilnotes -a db-key -w`),
   hex-encode it, and open with `PRAGMA key = "x'<64 hex chars>'"`.
2. Record a **second** meeting with the same person. After diarization the
   chip should pre-fill "Priya" with a confidence % (dashed ring = suggested).
   Confirm it → `SELECT COUNT(*) FROM voiceprints;` increments (gallery grew).
3. Negative: a brand-new voice stays `SPEAKER_xx` (no false auto-match).
4. Adaptive case: in a meeting where Priya is _not_ auto-matched (below
   threshold), manually assign her via the picker; confirm a new voiceprint
   row is enrolled, then re-run a similar later recording and check the
   confidence is higher / now clears the threshold.
5. `delete_persona("Priya")` (Personas view) removes her voiceprints and
   nulls links; the transcript falls back to the raw `SPEAKER_xx` label.
6. Encryption: on first launch a Keychain entry is created (service
   `com.lilnotes`, account `db-key` — verify with `security find-generic-password -s com.lilnotes -a db-key`). Quit, relaunch — the DB
   unlocks and all data reloads. `sqlite3` on the DB file reports "file is
   not a database" (encrypted). Deleting the Keychain item and relaunching
   makes the DB unreadable (intended factory-reset behavior). `cd src-tauri
&& cargo test` passes (pack/unpack round-trip, cosine, threshold
   classification, CRUD — all with the fixed test key, no Keychain access).

## Architecture

```
Mic ─────────► Capture (Rust/Core Audio) ── mic.wav ───► whisper-rs ─┐
System audio ► (process tap, separate)  ── system.wav ► whisper-rs ─┤
                                             │                       ├─► merge ─► SQLite ◄─► UI
                                             └─► sherpa-onnx diarize ┘             │
                                                (system channel only)              └─► Ollama (localhost)
```

Mic segments are labeled **Me**; system-channel segments get diarized speaker
labels (renamable per meeting). Channels are never mixed before ASR.

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
(service `com.lilnotes`); it is protected by your login keychain
(FileVault + user password). If the key is deleted, the database becomes
unreadable — effectively a factory reset. Nothing in the database or the
key ever leaves your Mac.

## Signing & notarization (milestone 8)

Documented once distribution lands. You provide your own Developer ID
identity; it is configured via environment variables, never committed.
