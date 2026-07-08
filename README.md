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
| 2 | Dual-source capture → two 16 kHz WAVs, level meters, permissions | ⬜ |
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
