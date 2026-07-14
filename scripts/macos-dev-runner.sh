#!/bin/bash
# Cargo runner for macOS dev builds (wired up via src-tauri/.cargo/config.toml).
#
# Why this exists: macOS TCC identifies an app by its code signature. The
# release .app is a signed *bundle* with a stable identifier (com.lilnotes),
# so its System Audio Recording grant survives rebuilds. A bare `cargo run`
# binary is only linker-ad-hoc-signed — its sole identity is a cdhash of the
# compiled binary, which changes on EVERY rebuild. macOS then silently drops
# the system-audio grant after any recompile, and on macOS 26 the TCC prompt
# will not re-fire for a non-bundled binary. Result: system audio capture
# works in `npm run tauri:build` but dies in `npm run tauri:dev`.
#
# Fix: wrap the freshly built binary in a minimal LilNotesDev.app (stable
# identifier com.lilnotes.dev), ad-hoc sign the bundle, and exec the binary
# from inside it — the exact shape TCC sees for the release build. The
# llama-server sidecar is cloned in beside it because tauri-plugin-shell
# resolves sidecars relative to the running executable.
#
# Anything that is not the app binary (unit-test binaries, examples) is
# exec'd unchanged, so `cargo test` is unaffected.
set -euo pipefail

BIN="$1"
shift

# Normalize to an absolute path; cargo may hand us a cwd-relative one.
case "$BIN" in
  /*) ;;
  *) BIN="$(pwd)/$BIN" ;;
esac

OUT_DIR="$(dirname "$BIN")"

# Cargo sets DYLD_FALLBACK_LIBRARY_PATH so binaries find the sherpa-rs /
# onnxruntime dylibs in the target dir — but macOS strips DYLD_* env across
# the SIP-protected bash running this script. Reconstruct it for everything
# we launch (app, test binaries, examples), keeping dyld's default fallbacks.
export DYLD_FALLBACK_LIBRARY_PATH="$OUT_DIR:$OUT_DIR/deps:$(dirname "$OUT_DIR"):$HOME/lib:/usr/local/lib:/lib:/usr/lib"

if [ "$(basename "$BIN")" != "lilnotes" ]; then
  exec "$BIN" "$@"
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
APP="$OUT_DIR/LilNotesDev.app"
APP_BIN="$APP/Contents/MacOS/lilnotes"
SIDECAR="$OUT_DIR/llama-server"

# Rebuild + re-sign the wrapper only when an input changed; a plain re-run
# (no recompile) launches instantly and keeps the exact signature TCC
# already approved.
needs_build=0
[ -f "$APP_BIN" ] || needs_build=1
[ "$BIN" -nt "$APP_BIN" ] && needs_build=1
[ "$ROOT/src-tauri/Info.plist" -nt "$APP/Contents/Info.plist" ] && needs_build=1
if [ -f "$SIDECAR" ] && [ "$SIDECAR" -nt "$APP/Contents/MacOS/llama-server" ]; then
  needs_build=1
fi

if [ "$needs_build" = 1 ]; then
  echo "dev-runner: wrapping $(basename "$BIN") in LilNotesDev.app (stable TCC identity)…" >&2
  mkdir -p "$APP/Contents/MacOS"

  # Start from the repo Info.plist (mic + system-audio usage strings) and add
  # the bundle keys. A stable CFBundleIdentifier distinct from the release
  # app keeps the dev TCC record separate and self-consistent.
  cp -f "$ROOT/src-tauri/Info.plist" "$APP/Contents/Info.plist"
  PB=/usr/libexec/PlistBuddy
  for entry in \
    "CFBundleIdentifier string com.lilnotes.dev" \
    "CFBundleName string LilNotes Dev" \
    "CFBundleExecutable string lilnotes" \
    "CFBundlePackageType string APPL" \
    "CFBundleShortVersionString string 0.0.0-dev" \
    "CFBundleVersion string 0.0.0-dev"; do
    key="${entry%% *}"
    $PB -c "Delete :$key" "$APP/Contents/Info.plist" 2>/dev/null || true
    $PB -c "Add :$entry" "$APP/Contents/Info.plist"
  done

  # APFS clones (cp -c): instant, no extra disk for the big debug binary.
  rm -f "$APP_BIN"
  cp -c "$BIN" "$APP_BIN" 2>/dev/null || cp -f "$BIN" "$APP_BIN"

  # The dev binary has no LC_RPATH and normally finds the sherpa/onnxruntime
  # dylibs via the DYLD_FALLBACK_LIBRARY_PATH cargo sets — but macOS strips
  # DYLD_* env across the SIP-protected bash running this script. Point rpath
  # at the target dir instead (same trick as scripts/fix-bundle.sh).
  install_name_tool -add_rpath "$OUT_DIR" "$APP_BIN"
  install_name_tool -add_rpath "$OUT_DIR/deps" "$APP_BIN"
  if [ -f "$SIDECAR" ]; then
    rm -f "$APP/Contents/MacOS/llama-server"
    cp -c "$SIDECAR" "$APP/Contents/MacOS/llama-server" 2>/dev/null \
      || cp -f "$SIDECAR" "$APP/Contents/MacOS/llama-server"
  fi

  codesign --force --deep --sign - "$APP"
  echo "dev-runner: signed $APP" >&2
fi

# Launch through the disclaim shim: a plain exec would leave the terminal as
# the process's TCC "responsible process", and macOS silently denies System
# Audio Recording evaluated against the terminal — the tap runs but records
# zeros. The shim makes the app responsible for itself (like a Finder/`open`
# launch) and then exec's in place, so pid, stdio, Ctrl-C, and the tauri dev
# watcher all behave exactly as before. See scripts/disclaim.c.
DISCLAIM="$OUT_DIR/dev-disclaim"
if [ ! -x "$DISCLAIM" ] || [ "$ROOT/scripts/disclaim.c" -nt "$DISCLAIM" ]; then
  if cc -o "$DISCLAIM" "$ROOT/scripts/disclaim.c"; then
    echo "dev-runner: built disclaim shim" >&2
  else
    echo "dev-runner: WARNING: could not build disclaim shim; system audio will record silence in dev" >&2
    exec "$APP_BIN" "$@"
  fi
fi
echo "dev-runner: launching $APP_BIN via disclaim shim (self-responsible for TCC)" >&2
exec "$DISCLAIM" "$APP_BIN" "$@"
