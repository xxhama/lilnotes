# Security policy

## Reporting a vulnerability

Please **do not** open a public issue for security problems. Use GitHub's
private reporting form:

<https://github.com/xxhama/lilnotes/security/advisories/new>

You should get an acknowledgement within a week. Fixes for confirmed issues
ship as a new release; credit is given in the release notes unless you ask
otherwise.

## Supported versions

Only the latest release on the
[Releases page](https://github.com/xxhama/lilnotes/releases) receives
fixes. There is no auto-updater yet, so check that page or watch the repo
for releases.

## What LilNotes protects, and how

- **Everything stays on the Mac.** Audio, transcripts, summaries, notes and
  speaker voiceprints are stored under
  `~/Library/Application Support/com.lilnotes/`. The app makes no network
  requests except model downloads the user starts and localhost calls to
  Ollama or the bundled llama.cpp sidecar. There is no telemetry.
- **Database encryption.** The SQLite database is encrypted with SQLCipher.
  The 256-bit key is generated on first run and stored in the macOS login
  Keychain (service `com.lilnotes`). Deleting the key makes the database
  unreadable.
- **Voiceprints are biometric data.** CAM++ speaker embeddings live in the
  encrypted database and never leave the device. Users can delete them per
  persona or all at once from Settings.
- **MCP server.** Off by default. When enabled it binds to `127.0.0.1` only
  and requires a bearer token (constant-time compared) on every request. It
  is read-only and never exposes audio paths, settings or voiceprints.
- **Sidecar.** The bundled `llama-server` (llama.cpp) listens on localhost
  only. `scripts/build-llama-server.sh` pins the upstream ref; bumps are
  reviewed against upstream security fixes.
- **Code signing.** Release builds are signed with a Developer ID and
  notarized by Apple. SHA-256 checksums for every asset are published with
  each release.

## In scope

Anything that would let data leave the Mac without the user's action, bypass
the MCP token, read the database or Keychain key from another user account,
or execute code via a crafted audio file, model file, or MCP request.

## Out of scope

Issues that require an attacker to already have local admin access, denial
of service against localhost services, and vulnerabilities in third-party
models or Ollama itself (report those upstream).
