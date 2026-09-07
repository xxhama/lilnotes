# Manual test walkthroughs

End-to-end checks for each subsystem, for use before a release or after
touching the relevant code. Automated tests cover the pure logic (resampler,
AEC, transcript merge, DB, MCP, voiceprint math); everything below needs a
real Mac with permissions granted.

## App shell and IPC

`npm run tauri:dev` opens the window with the LilNotes sidebar. On the
Meetings screen, **Test backend connection** shows the backend version,
round-trip latency and echoed message.

## Capture

1. Go to **Record** and hit the record button. Approve the Microphone prompt,
   then the System Audio Recording prompt (first run only).
2. Talk, and play something (music, a video) so both meters move.
3. Stop. The recording card shows the two WAV paths under
   `~/Library/Application Support/com.lilnotes/recordings/<session>/`.
4. Inspect: `afinfo mic.wav system.wav` (both 16 kHz mono 16-bit) and play
   them. `mic.wav` should contain only your voice, `system.wav` only the
   playback.

Troubleshooting:

- _No system-audio prompt / OSStatus error on start:_ system-audio TCC
  requires a signed, self-responsible process. Dev builds go through
  `scripts/macos-dev-runner.sh` for exactly this reason; do not launch the
  raw binary from `target/debug`. Headless diagnosis:
  `cd src-tauri && cargo run --example tap_probe` prints a SILENCE/OK verdict.
- _Re-test the prompts:_ `tccutil reset Microphone com.lilnotes.dev` and
  `tccutil reset SystemAudioCaptureRequests com.lilnotes.dev` (release
  builds: `com.lilnotes`).
- _system.wav is silent:_ something must actually be playing to the default
  output device (the tap follows the default output).

Echo cancellation: with speakers on, a remote voice should not appear in
`mic.wav`. The AEC uses the system track as its reference; if it cannot
initialise, capture falls back to the raw mic and logs a line saying so.

## Transcription

1. In **Settings → Transcription**, download **Large v3 Turbo** (~1.6 GB;
   the progress bar and cancel should work) and make sure it is selected.
   Leave "Live transcription" on.
2. Record a short session with speech on both channels (talk, and play a
   video with speech). The transcript should fill in below the meters within
   5–15 s of each utterance, labelled Me / Speaker with timestamps.
3. Stop: the last chunk flushes and the full segment list appears.
   Timestamps should match the audio (`afplay` and spot-check).
4. Batch mode: toggle live transcription off, record again, then click
   **Transcribe recording**. Same output, produced after the fact.

## Speaker identification

1. Record a session where the system channel has **two or more distinct
   voices** (a podcast or interview) while you also speak.
2. Stop. After transcription, "Identifying speakers…" runs; the first time it
   downloads two small ONNX models (~35 MB total).
3. Generic "Speaker" chips become SPEAKER_00 / SPEAKER_01 (colour-coded);
   your speech stays "Me". Alternating voices should alternate labels.
4. Click a SPEAKER_xx chip to rename it. The name applies across the
   transcript and persists per meeting.

## Persistence

1. Record a short meeting; after speakers are identified you land on the
   meeting's detail page.
2. Rename a speaker and the meeting title, quit the app fully, relaunch.
   Everything reloads from the database.
3. Meetings shows the history; search matches titles _and_ transcript text;
   hovering a row reveals delete.
4. Settings → Recording: change the recordings folder (new sessions land
   there) and enable "Delete audio after transcription". After the next
   recording finishes processing, its WAVs are gone and the detail page
   shows an "audio deleted" badge.

## Summaries

Bundled engine:

1. Settings → Summaries → Bundled: download **Qwen3.5 4B**. Progress and
   cancel should work.
2. Open a transcribed meeting → Summary panel → **Summarize**. Tokens stream
   in live; the result is saved with model + timestamp and survives a
   restart.
3. Rename a speaker, hit **Regenerate**: action items use the new name. Edit
   the prompt template in Settings and regenerate again.

Ollama:

1. Quit Ollama first: Settings → Summaries → Ollama should say it is not
   running, with install guidance, and the rest of the app keeps working.
2. Start Ollama. The panel shows its version, installed models and the
   curated suggestions. Pull one: the progress bar tracks real percent and
   cancel works (a re-pull resumes).
3. Repeat the summarize / regenerate steps above with an Ollama model.

## Voiceprints and personas

1. Record, transcribe and diarize a meeting with a distinct remote speaker;
   click that speaker's chip → **Create new persona** "Priya" → confirm.
   The Personas view shows "Priya — 1 voiceprint".
2. Record a **second** meeting with the same person. After diarization the
   chip should pre-fill "Priya" with a confidence % (dashed ring =
   suggested). Confirm it; the gallery grows to 2.
3. Negative: a brand-new voice stays `SPEAKER_xx` (no false auto-match).
4. Adaptive case: in a meeting where Priya is _not_ auto-matched (below
   threshold), assign her manually via the picker; a new voiceprint is
   enrolled. A later similar recording should now clear the threshold.
5. Delete the persona from the Personas view: her voiceprints are removed
   and transcripts fall back to the raw `SPEAKER_xx` label.

## Encryption

On first launch a Keychain entry is created (service `com.lilnotes`, account
`db-key`): `security find-generic-password -s com.lilnotes -a db-key`. Quit
and relaunch: the DB unlocks and all data reloads. Plain `sqlite3` on the DB
file reports "file is not a database", which is the expected sign that
encryption is on. Deleting the Keychain item and relaunching makes the DB
unreadable (intended factory-reset behaviour).

To inspect rows, install `sqlcipher` (`brew install sqlcipher`), fetch the
key (`security find-generic-password -s com.lilnotes -a db-key -w`),
hex-encode it, and open with `PRAGMA key = "x'<64 hex chars>'"`.

## MCP server

Enable it in Settings → MCP server. Then:

```sh
curl -i -X POST http://127.0.0.1:41777/mcp -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
```

Expect 200 with server info; the same request without the header must be 401. Regenerate the token in Settings and confirm the old one stops working.
