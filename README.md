# LilNotes

Local-first meeting notes for Apple Silicon Macs. LilNotes records your
microphone and the other participants (system audio) as two separate tracks,
transcribes and identifies speakers entirely on-device, and writes a summary
with a language model running on your Mac. Nothing leaves the machine: no
cloud, no accounts, no telemetry.

<!-- Screenshots: docs/screenshot-meetings.png and docs/screenshot-detail.png -->

## Features

- **Two-channel capture.** Your mic and the meeting app's audio (Zoom, Teams,
  Meet, a browser tab, anything that plays sound) are recorded separately, so
  your voice is never mixed with theirs. Works with speakers or headphones;
  echo cancellation keeps remote voices out of your track.
- **On-device transcription** with [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
  on Metal, live while you record or after the fact.
- **Speaker identification.** The remote track is diarized
  ([sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx): pyannote
  segmentation + CAM++ embeddings). Name a speaker once and LilNotes
  recognises their voice in later meetings.
- **Summaries and action items** from a bundled
  [llama.cpp](https://github.com/ggml-org/llama.cpp) sidecar (Qwen3.5,
  downloaded in-app) or from [Ollama](https://ollama.com) if you already run
  it. Streams tokens live; prompt template is editable.
- **Notes alongside the transcript**, a per-customer view that groups
  meetings and summarises the relationship, and a menu-bar recorder so you
  can start and stop without opening the window.
- **Encrypted at rest.** Everything is stored in a SQLCipher database whose
  key lives in your macOS Keychain.
- **Read-only [MCP](https://modelcontextprotocol.io) server** (off by
  default) so local AI agents such as Claude Code can query your meetings.

## Requirements

- Apple Silicon Mac (M1 or later). Intel Macs are not supported.
- macOS 15 (Sequoia) or newer.
- Disk space for the models you pick: about 1.6 GB for the recommended
  Whisper model, 35 MB for speaker identification, and 2.7 GB (Qwen3.5 4B)
  or 5.7 GB (Qwen3.5 9B) for the bundled summariser. Ollama models are
  managed by Ollama.
- 16 GB of memory is comfortable; 8 GB works with the smaller models.

## Install

1. Download `LilNotes_<version>_aarch64.dmg` from the
   [latest release](https://github.com/xxhama/lilnotes/releases/latest).
2. Open the DMG and drag **LilNotes** to Applications.
3. Launch it. Releases are signed with a Developer ID and notarized by Apple,
   so there is nothing to click through.

To verify a download, compare it against the `checksums.txt` published with
the release:

```sh
shasum -a 256 -c checksums.txt
```

Building from source yourself? Unsigned local builds trigger Gatekeeper's
"damaged" warning on first launch. Either right-click → Open, or run
`xattr -cr /Applications/LilNotes.app` once.

## First run

The onboarding wizard walks through this, but here is what happens and why:

1. **Microphone** permission: your side of the meeting.
2. **System Audio Recording** permission: the other participants. macOS
   calls this "System Audio Recording Only" under System Settings → Privacy
   & Security → Screen & System Audio Recording. Nothing on screen is
   captured.
3. **Pick a transcription model** in Settings → Transcription. _Large v3
   Turbo_ is the default and the best trade-off for live transcription.
4. **Pick a summariser** in Settings → Summaries: download Qwen3.5 for the
   bundled engine, or point LilNotes at a running Ollama.

If a prompt never appeared, or you clicked the wrong button, reset it and
relaunch:

```sh
tccutil reset Microphone com.lilnotes
tccutil reset SystemAudioCaptureRequests com.lilnotes
```

## Privacy

- **Where data lives.** Recordings, models and the database are under
  `~/Library/Application Support/com.lilnotes/`. Audio can be deleted
  automatically after transcription (Settings → Recording), and you can move
  the recordings folder.
- **Encryption.** The database (meetings, transcripts, summaries, notes,
  customers, personas, voiceprints) is encrypted with SQLCipher. The 256-bit
  key is generated on first launch and stored in your login Keychain
  (service `com.lilnotes`). Deleting that Keychain item makes the database
  unreadable; this is the intended factory reset.
- **Voiceprints are biometric data.** They are speaker embeddings, not
  audio, stored only in the encrypted database, and only added when you
  confirm an identity. Delete them per persona in **Personas** or all at
  once in Settings → Personas & voiceprints.
- **Network.** LilNotes only talks to the network to download models you
  ask for (Hugging Face), and to `localhost` for Ollama or its own sidecar.
  There is no crash reporting, analytics or update check.
- **Recording other people** may require their consent where you live. That
  is on you.

## Summaries

LilNotes ships a `llama-server` sidecar (llama.cpp) and can download Qwen3.5
in 4B (fast, fits any Mac) or 9B (better, needs 16 GB+) quantized builds from
Settings → Summaries. If you already use Ollama, switch the backend to Ollama
and pick any installed model; LilNotes talks to it at `http://localhost:11434`
and can pull models with progress from the same screen. Summaries regenerate
on demand, so renaming a speaker and hitting **Regenerate** updates the
action items.

## Connect an AI agent (MCP)

LilNotes can expose your meetings to local AI agents over the Model Context
Protocol. Turn it on in **Settings → MCP server (AI agents)**; the app starts
a Streamable HTTP endpoint at `http://127.0.0.1:41777/mcp` (port
configurable) protected by a bearer token shown in the same panel, with
copy-ready snippets. For Claude Code:

```sh
claude mcp add --transport http lilnotes http://127.0.0.1:41777/mcp \
  --header "Authorization: Bearer <token>"
```

Tools (all read-only): `list_meetings`, `search_meetings`, `get_meeting`,
`get_transcript` (speaker names resolved to personas), `list_summaries`,
`list_customers`, `get_customer`, `list_customer_summaries`,
`list_personas`.

Privacy: off by default, binds to loopback only, every request needs the
token, and audio files, settings and voiceprints are never exposed. The
server only runs while LilNotes is open (including hidden in the menu bar).

## How it works

```
Mic ─────────► Capture (Rust/Core Audio) ── mic.flac ──► whisper.cpp ─┐
System audio ► (process tap, separate)  ── system.flac► whisper.cpp ─┤
                                             │                       ├─► merge ─► SQLite ◄─► UI
                                             └─► sherpa-onnx diarize ┘  (SQLCipher)  │
                                                (system channel only)               └─► llama.cpp sidecar
                                                                                        or Ollama (localhost)
```

- **Shell:** [Tauri v2](https://tauri.app) (Rust core; React + TypeScript +
  Tailwind + shadcn/ui front-end).
- **Capture:** a Core Audio process tap for system audio plus the mic, each
  written as 16 kHz mono FLAC (lossless). The two are never mixed before transcription.
  Mic segments are labelled **Me**; system-channel segments get diarized
  speaker labels, renamable per meeting.
- **Echo cancellation:** WebRTC AudioProcessing (AEC3), with the system
  track as the reference signal, so remote voices are removed from your
  track even on speakers.
- **Voiceprints & personas:** CAM++ embeddings computed during diarization
  are kept per persona as a small gallery. Matching is cosine similarity
  on-device; a gallery grows only when you confirm an identity, so a wrong
  suggestion never pollutes it.
- **Summaries:** the sidecar is a separate `llama-server` process (llama.cpp
  and whisper.cpp cannot share one process on Metal), spoken to over
  localhost HTTP.

## Building from source

```sh
git clone https://github.com/xxhama/lilnotes.git && cd lilnotes
brew install cmake meson ninja pkg-config   # plus Rust (rustup), Node 24, Xcode CLT
npm install
npm run tauri:dev       # dev window; builds the llama-server sidecar first
npm run tauri:build     # release .app + .dmg
```

Use `npm run tauri:dev` (with the colon), not `npx tauri dev`, or summaries
will not work. Everything else a contributor needs, including the checks CI
runs and the parts of the build that look odd but are load-bearing, is in
[CONTRIBUTING.md](CONTRIBUTING.md). Manual test walkthroughs for each
subsystem are in [docs/manual-testing.md](docs/manual-testing.md).

## License

LilNotes is free software under the
[GNU Affero General Public License v3.0](LICENSE). It builds on whisper.cpp,
llama.cpp, sherpa-onnx, ONNX Runtime, WebRTC AudioProcessing, SQLCipher and
others; their licenses are collected in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and shown in Settings →
About. Models are downloaded on demand under their own licenses (Whisper:
MIT; pyannote segmentation: MIT; 3D-Speaker CAM++: Apache-2.0; Qwen3.5:
Apache-2.0).
