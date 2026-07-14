# CLAUDE.md

Local-first macOS meeting recorder. Records mic + system audio as separate
channels, transcribes and diarizes entirely on-device, and summarizes with a
locally running Ollama model. No cloud, no telemetry, no Python at runtime.

**Stack:** Tauri v2 (Rust core, React 19 + TypeScript + Tailwind + shadcn/ui
frontend), SQLite with SQLCipher encryption, whisper.cpp (Metal), sherpa-onnx
diarization, llama.cpp sidecar + Ollama for summaries.

**Target:** Apple Silicon, macOS 26 (Tahoe)+. Bundle ID: `com.lilnotes`.

## Build / run / test commands

```sh
npm install                  # first-time setup
npm run tauri:dev            # dev app window (builds sidecar first)
npm run tauri:build           # release .app/.dmg (builds sidecar + fixes dylibs)
npm run typecheck             # TypeScript typecheck (no bundling)
npm run lint                  # ESLint
npm run lint:fix              # ESLint --fix
npm run format                # Prettier --write
npm run format:check          # Prettier --check (CI)
npm test                      # Rust unit tests (cd src-tauri && cargo test)
cd src-tauri && cargo clippy  # Rust lints
cd src-tauri && cargo fmt --check  # Rust format check
```

> **Do NOT use `npx tauri dev`** — it skips the sidecar build and
> summarization breaks silently. Always use `npm run tauri:dev` (colon).

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

### Rust backend (`src-tauri/src/`)

| Module        | Purpose                                                                                                    |
| ------------- | ---------------------------------------------------------------------------------------------------------- |
| `commands`    | All Tauri IPC commands (thin wrappers — parse/validate, call module, map errors to strings)                |
| `audio`       | Dual-source capture: mic + Core Audio process tap, 16 kHz mono WAV per channel                             |
| `permissions` | TCC status/request helpers for microphone + system audio                                                   |
| `asr`         | whisper-rs transcription (Metal), context reuse, silence-based chunking                                    |
| `models`      | ML model registry + downloader with progress events                                                        |
| `diarize`     | sherpa-onnx speaker diarization (pyannote + CAM++)                                                         |
| `voiceprint`  | CAM++ speaker embeddings for cross-meeting identity                                                        |
| `personas`    | Named identity layer + voiceprint matching + enrollment                                                    |
| `transcript`  | Merge ASR + diarization, overlap-based speaker assignment                                                  |
| `db`          | SQLite persistence (SQLCipher encrypted), single `Mutex<Connection>`, migrations via `PRAGMA user_version` |
| `keystore`    | macOS Keychain key for SQLCipher                                                                           |
| `summary`     | Summarization via Ollama (localhost) + bundled llama.cpp sidecar                                           |
| `tray`        | Menu bar tray icon — "pure remote control" via events, no second recording path                            |

### Frontend (`src/`)

| Directory        | Purpose                                                                                        |
| ---------------- | ---------------------------------------------------------------------------------------------- |
| `views/`         | Top-level pages: Home, Recording, MeetingDetail, Settings, Personas, Customers, CustomerDetail |
| `components/`    | Reusable widgets: TranscriptPane, SummaryPanel, AudioPlayer, LevelMeter, NotesEditor, etc.     |
| `components/ui/` | shadcn/ui primitives (Button, Dialog, etc.)                                                    |
| `lib/`           | `ipc.ts` (typed IPC wrappers), `useTauriEvent.ts` (event hook), `utils.ts` (`cn()`)            |
| `hooks/`         | `useSystemTheme.ts`                                                                            |

## Key patterns

**IPC pattern:** `#[tauri::command]` in `commands.rs` → register in `lib.rs`
`invoke_handler!` → typed wrapper in `src/lib/ipc.ts` → call from views.
Events: `app.emit("event:name", payload)` on Rust side → `useTauriEvent(hook,
cb)` in React.

**Routing:** Custom `Route` discriminated union in `App.tsx` (no
react-router). Views rendered conditionally based on `route.name`.

**State management:** Plain `useState` + `useRef` + Tauri IPC. No
Redux/Zustand. Do not add one.

**Error handling:** All Tauri commands return `Result<T, String>`. Command
bodies stay thin — parse/validate, call into the module, map errors to
strings.

**DB access:** `Arc<Db>` in Tauri state, `State<'_, Arc<Db>>` in command
handlers. Single `Mutex<Connection>`. Migrations via `PRAGMA user_version`.

**Serde convention:** All IPC structs use `#[serde(rename_all =
"camelCase")]` for JS interop.

## Critical gotchas

1. **Sidecar process — ggml-metal symbol collision.** `llama-server`
   (llama.cpp's HTTP server, Metal) and `whisper-rs` (Metal, ggml 0.9.5)
   cannot coexist in one binary. The LLM runs in a separate `llama-server`
   process, spawned via `tauri-plugin-shell`'s sidecar mechanism. IPC is
   HTTP/SSE to `http://127.0.0.1:<port>/v1/chat/completions`. Thinking mode
   is disabled via `chat_template_kwargs: {"enable_thinking": false}` — the
   model-agnostic approach that works across Qwen3.5, DeepSeek, Gemma, etc.
   Build it with `scripts/build-llama-server.sh` (clones + CMake-builds
   llama.cpp). Source is vendored at `.llama.cpp/`.

2. **`tauri:dev` vs `tauri dev`.** Always use `npm run tauri:dev` (colon) —
   it builds the llama-server sidecar first. `npx tauri dev` skips the
   sidecar build and summarization breaks silently.

3. **CoreAudio process tap recipe.** `src/audio/system_tap.rs` has a 6-step
   recipe with load-bearing warnings: do NOT touch `isExclusive`, must use
   IOProc not AVAudioEngine, must build a private aggregate device. Read the
   module doc comment before touching audio capture.

4. **RecordingView stays mounted.** `App.tsx` keeps `RecordingView` always
   mounted (hidden via CSS `display:none`) so recording state (segments,
   notes, live transcript) survives navigation. Do not "optimize" by
   unmounting inactive views — it will lose live recording state.

5. **SQLCipher encrypted DB.** Plain `sqlite3` reports "file is not a
   database." To inspect: `brew install sqlcipher`, get key from Keychain
   (`security find-generic-password -s com.lilnotes -a db-key -w`),
   hex-encode it, open with `PRAGMA key = "x'<64 hex chars>'"`.

6. **TCC permissions — dev system audio records SILENCE without the
   runner.** Two traps, both dev-only, both silent (tap creates fine, IOProc
   runs, every sample is zero — no error anywhere):
   - TCC evaluates System Audio Recording against the process's
     **responsible process**. Anything spawned from a terminal pipeline
     (cargo/npm/tauri dev) is attributed to the _terminal_, which has no
     `NSAudioCaptureUsageDescription` → denied with no prompt. The dev
     runner launches the app through `scripts/disclaim.c`
     (`responsibility_spawnattrs_setdisclaim` + `POSIX_SPAWN_SETEXEC`, the
     Chromium/VS Code trick) so the app is self-responsible, as if launched
     from Finder.
   - The grant is keyed to the code signature. A bare `cargo run` binary is
     only linker-signed (identity = per-build cdhash), so any grant dies on
     rebuild. The runner wraps the binary in a signed
     `target/debug/LilNotesDev.app` (stable id `com.lilnotes.dev`, same
     shape as the release bundle) before launching.

   Wiring: `src-tauri/.cargo/config.toml` sets
   `scripts/macos-dev-runner.sh` as the cargo runner; `tauri dev` uses
   `cargo run`, so it flows through automatically. Do not bypass it.
   Diagnose capture headlessly with `cargo run --example tap_probe`
   (prints a SILENCE/OK verdict). Reset prompts:
   `tccutil reset SystemAudioCaptureRequests com.lilnotes.dev` (release:
   `com.lilnotes`).

7. **Post-build dylib fixup.** `scripts/fix-bundle.sh` copies
   sherpa-rs/onnxruntime dylibs into the `.app` bundle and fixes rpath.
   Tauri's bundler doesn't know about these. Do not skip this step.

8. **`useTauriEvent` hook.** Use `src/lib/useTauriEvent.ts`, not raw
   `listen()`. The hook handles a React StrictMode double-mount race.

9. **macOS 26+ only.** `tauri.conf.json` sets `minimumSystemVersion: "26.0"`.

10. **AEC architecture.** WebRTC APM is used (not Apple's AEC) because
    Apple's voice processing can't reference other apps' audio. The
    system-audio stream is the render reference. Both signals must be
    16 kHz mono, 10 ms frames.

## How to add a new Tauri command

1. Write `#[tauri::command] fn my_cmd(...) -> Result<T, String>` in
   `commands.rs`
2. Register `my_cmd` in the `invoke_handler!` list in `lib.rs`
3. Add a typed wrapper in `src/lib/ipc.ts`
4. Call it from a view

## How to add a new view/route

1. Add a variant to the `Route` union in `App.tsx`
2. Add it to the `NAV` array (with icon + label)
3. Create `src/views/MyView.tsx` and render it in `App.tsx`'s main content area
