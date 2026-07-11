#!/bin/bash
# Build the llm-sidecar binary and copy it to src-tauri/binaries/ with the
# target-triple suffix that Tauri's sidecar mechanism expects.
#
# Run before `tauri build` (production) or `tauri dev` (development) so the
# sidecar binary exists for the shell plugin to spawn.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SRC_TAURI="$ROOT/src-tauri"

# Get the Rust target triple (e.g. aarch64-apple-darwin on Apple Silicon).
TARGET=$(rustc -vV | grep '^host:' | awk '{print $2}')

echo "build-sidecar: target triple = $TARGET"

# Build the sidecar in release mode for production, dev for development.
# "debug" is a reserved Cargo profile name — map it to "dev".
PROFILE="${1:-release}"
CARGO_PROFILE="$PROFILE"
OUT_DIR="$PROFILE"
if [ "$PROFILE" = "debug" ]; then
  CARGO_PROFILE="dev"
  OUT_DIR="debug"
fi
echo "build-sidecar: cargo build --profile $CARGO_PROFILE -p llm-sidecar"
cargo build --manifest-path "$SRC_TAURI/Cargo.toml" --profile "$CARGO_PROFILE" -p llm-sidecar

# Copy to src-tauri/binaries/ with the target-triple suffix.
mkdir -p "$SRC_TAURI/binaries"
SRC="$SRC_TAURI/target/$OUT_DIR/llm-sidecar"
DEST="$SRC_TAURI/binaries/llm-sidecar-$TARGET"

if [ ! -f "$SRC" ]; then
  echo "build-sidecar: binary not found at $SRC" >&2
  exit 1
fi

cp "$SRC" "$DEST"
echo "build-sidecar: copied $SRC → $DEST ✓"