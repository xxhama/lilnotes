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

**Target:** Apple Silicon, macOS 26 (Tahoe)+. Bundle ID: `co.elastic.lilnote`.

## Development

Prerequisites: Rust (stable, via rustup), Node 20+, Xcode Command Line Tools.

```sh
npm install
npm run tauri dev     # dev app window
npm run tauri build   # release .app/.dmg
```

## Milestone status

| # | Milestone | Status |
|---|-----------|--------|
| 1 | Scaffold: app shell, IPC round-trip | ✅ done |
| 2 | Dual-source capture → two 16 kHz WAVs, level meters, permissions | ✅ done |
| 3 | Transcription (whisper-rs Metal), model download, near-live chunks | ⬜ |
| 4 | Diarization + merged speaker-labeled transcript | ⬜ |
| 5 | SQLite persistence, history/detail UI, speaker rename | ⬜ |
| 6 | Ollama summaries + in-app model manager (pull w/ progress) | ⬜ |
| 7 | Export: Markdown, PDF, clipboard | ⬜ |
| 8 | Signing, notarization, .dmg, first-run flow | ⬜ |

### Verifying milestone 1

`npm install && npm run tauri dev` — the window should open with the LilNotes
sidebar; on the Meetings screen, click **Test backend connection**: it should
show the backend version, round-trip latency, and echoed message.

### Verifying milestone 2

1. Go to **Record**, hit the record button. Approve the Microphone prompt,
   then the System Audio Recording prompt (first run only).
2. Talk, and play something (music, a video) so both meters move.
3. Stop. The card shows the two WAV paths under
   `~/Library/Application Support/co.elastic.lilnote/recordings/<session>/`.
4. Inspect: `afinfo mic.wav system.wav` (both 16 kHz mono 16-bit) and play
   them — mic.wav has only your voice, system.wav only the playback.

Rust unit tests (resampler + limiter): `cd src-tauri && cargo test`.

**Troubleshooting capture**

- *No system-audio prompt appears / OSStatus error on start:* the
  system-audio TCC category requires a signed binary. Dev builds are ad-hoc
  signed, which normally works; if not, run `npm run tauri build` once and
  launch the bundled app from `src-tauri/target/release/bundle/macos/`.
- *Re-test the prompts:* `tccutil reset Microphone co.elastic.lilnote` and
  `tccutil reset SystemAudioCaptureRequests co.elastic.lilnote`.
- *system.wav is silent:* make sure something is actually playing to the
  default output device (the tap follows the default output).

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

## Signing & notarization (milestone 8)

Documented once distribution lands. You provide your own Developer ID
identity; it is configured via environment variables, never committed.
