# Contributing to LilNotes

Thanks for your interest. LilNotes is a small project with one maintainer,
so the rules below exist to keep reviews quick, not to add ceremony.

## Ground rules

- **Local-first is non-negotiable.** Nothing may send audio, transcripts,
  summaries or voiceprints off the user's Mac. The only network calls the app
  makes are model downloads the user starts and localhost calls to Ollama or
  the bundled llama.cpp sidecar.
- **License.** By contributing you agree that your contribution is licensed
  under the project's [AGPL-3.0](LICENSE) (inbound = outbound). There is no
  CLA.
- **Be kind.** See the [Code of Conduct](CODE_OF_CONDUCT.md).
- Security issues go through [SECURITY.md](SECURITY.md), not public issues.

## Prerequisites

Apple Silicon Mac running macOS 15 or newer, plus:

```sh
xcode-select --install                         # Xcode Command Line Tools
curl https://sh.rustup.rs -sSf | sh            # Rust stable (rust-toolchain.toml pins it)
brew install cmake meson ninja pkg-config      # llama.cpp sidecar + WebRTC AEC build
```

Node 24 (`.nvmrc`). If you use `nvm`, run `nvm use`.

## Building and running

```sh
npm install
npm run tauri:dev       # dev app window (builds the llama-server sidecar first)
npm run tauri:build     # release .app + .dmg (builds sidecar, copies dylibs, signs)
```

**Always use `npm run tauri:dev` (with the colon), never `npx tauri dev`.**
The npm script builds the `llama-server` sidecar first; without it,
summaries fail silently. The first build clones and compiles llama.cpp
(a few minutes), then compiles SQLCipher, OpenSSL and WebRTC APM from
source (a few more). Later builds are incremental.

`tauri.conf.json` declares the sidecar under `bundle.externalBin`, and the
`tauri_build` script checks that file exists, so `cargo test` or `cargo
clippy` in a fresh clone fail until you have either run
`npm run tauri:dev` once or created an empty stub:

```sh
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
mkdir -p src-tauri/binaries && : > "src-tauri/binaries/llama-server-$TRIPLE" && chmod +x "src-tauri/binaries/llama-server-$TRIPLE"
```

### Why `cargo run` looks unusual on this project

`src-tauri/.cargo/config.toml` routes `cargo run` through
`scripts/macos-dev-runner.sh`. It wraps the debug binary in a signed
`LilNotesDev.app` and launches it as its own "responsible process", because
macOS grants **System Audio Recording** permission to the responsible
process and keys it to the code signature. Without the wrapper, a dev build
launched from a terminal records silence with no error. The full explanation
lives in the "Critical gotchas" section of [CLAUDE.md](CLAUDE.md), which
doubles as the architecture notes for this repo.

Reset the permission prompts for a dev build with:

```sh
tccutil reset Microphone com.lilnotes.dev
tccutil reset SystemAudioCaptureRequests com.lilnotes.dev
```

## Checks before you push

```sh
npm run typecheck
npm run lint
npm run format:check          # or: npm run format
cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs exactly these, plus `cargo deny check licenses` and a check that
`THIRD_PARTY_NOTICES.md` is up to date. **If you add, remove or bump a
dependency**, run `bash scripts/gen-third-party-notices.sh` (needs
`brew install cargo-about`) and commit the result. A dependency whose
license is not on the allowlist in `src-tauri/deny.toml` fails CI; open an
issue first if you need one.

UI changes: check both light and dark themes, and use the shadcn/ui
primitives in `src/components/ui/` and CSS theme tokens rather than native
form controls or hardcoded colors.

## Commits and pull requests

Releases are cut automatically by release-please from
[Conventional Commits](https://www.conventionalcommits.org/), so the **PR
title** must use a type prefix:

| Prefix                                         | Effect                              |
| ---------------------------------------------- | ----------------------------------- |
| `feat: …`                                      | minor release                       |
| `fix: …`                                       | patch release                       |
| `feat!: …` / `fix!: …`                         | breaking change (minor while < 1.0) |
| `docs:`, `chore:`, `ci:`, `refactor:`, `test:` | no release                          |

PRs are squash-merged, so individual commit messages on the branch can be
informal; the PR title becomes the changelog line. A CI check (`PR title`)
fails until the title has a valid prefix; editing the title re-runs it. Never edit version
numbers or push `v*` tags by hand; release-please owns them.

Keep PRs focused. A PR that fixes a bug and also reformats unrelated files is
harder to review than two PRs.

## Where things live

| Path                                                       | What                                                                  |
| ---------------------------------------------------------- | --------------------------------------------------------------------- |
| `src-tauri/src/`                                           | Rust core: audio capture, ASR, diarization, DB, summaries, MCP server |
| `src/`                                                     | React + TypeScript frontend (`views/`, `components/`, `lib/ipc.ts`)   |
| `scripts/`                                                 | sidecar build, bundle fixup + signing, dev runner                     |
| `.github/workflows/`                                       | CI and the release pipeline                                           |
| `licenses/`, `src-tauri/about.toml`, `src-tauri/deny.toml` | third-party notice generation and license policy                      |

The IPC pattern, routing, state management and the list of things that look
wrong but are load-bearing (Core Audio tap recipe, sidecar process, TCC
handling, dylib fixup) are documented in [CLAUDE.md](CLAUDE.md). Read it
before touching `src-tauri/src/audio/` or the build scripts.

## Reporting bugs

Use the bug report template. Please never attach recordings or transcripts
that contain other people's voices or private information.
