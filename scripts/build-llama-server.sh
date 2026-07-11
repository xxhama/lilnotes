#!/bin/bash
# Build llama-server (llama.cpp's HTTP server) and copy it to
# src-tauri/binaries/ with the target-triple suffix that Tauri's sidecar
# mechanism expects.
#
# llama-server provides a clean HTTP API with chat_template_kwargs
# support (enable_thinking: false) — the model-agnostic way to disable
# thinking mode that works across Qwen3.5, DeepSeek, Gemma, etc.
# without per-model hardcoded token IDs.
#
# Run before `tauri build` (production) or `tauri dev` (development) so the
# sidecar binary exists for the shell plugin to spawn.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SRC_TAURI="$ROOT/src-tauri"

# Get the Rust target triple (e.g. aarch64-apple-darwin on Apple Silicon).
TARGET=$(rustc -vV | grep '^host:' | awk '{print $2}')
echo "build-llama-server: target triple = $TARGET"

# Vendored llama.cpp source. Cloned shallow on first build, updated on
# subsequent builds. Override the ref with LLAMA_REF (tag, branch, or commit).
LLAMA_DIR="$ROOT/.llama.cpp"
BUILD_DIR="$LLAMA_DIR/build"
# Pinned to release b9957 (2026-07-10), which includes the CVE-2026-21869
# patch (commit c78fb90, 2026-04-23). Bump manually after reviewing upstream
# changes; override at invocation with LLAMA_REF=<ref>.
LLAMA_REF="${LLAMA_REF:-b9957}"

if [ ! -d "$LLAMA_DIR" ]; then
  echo "build-llama-server: cloning llama.cpp (ref $LLAMA_REF)…"
  git clone --depth 1 --branch "$LLAMA_REF" \
    https://github.com/ggml-org/llama.cpp "$LLAMA_DIR"
elif [ "${SKIP_PULL:-0}" != "1" ]; then
  echo "build-llama-server: updating llama.cpp (ref $LLAMA_REF)…"
  cd "$LLAMA_DIR"
  git fetch --depth 1 origin "$LLAMA_REF"
  git checkout "$LLAMA_REF"
fi

# Build with CMake. Static linking so the binary is self-contained (no
# @rpath dylib dependencies). Metal is enabled by default on Apple Silicon.
# Disable OpenSSL (we only use HTTP on localhost) and the web UI (not needed).
echo "build-llama-server: cmake configure…"
cmake -B "$BUILD_DIR" -S "$LLAMA_DIR" \
  -DCMAKE_BUILD_TYPE=Release \
  -DGGML_METAL=ON \
  -DBUILD_SHARED_LIBS=OFF \
  -DLLAMA_OPENSSL=OFF \
  -DLLAMA_BUILD_UI=OFF \
  -DLLAMA_BUILD_TESTS=OFF \
  -DLLAMA_BUILD_EXAMPLES=OFF

echo "build-llama-server: building llama-server (this takes a few minutes)…"
cmake --build "$BUILD_DIR" --config Release -j 8 --target llama-server

# The binary lands at build/bin/llama-server.
BINARY="$BUILD_DIR/bin/llama-server"
if [ ! -f "$BINARY" ]; then
  echo "build-llama-server: binary not found at $BINARY" >&2
  echo "  (in some versions the binary may be named 'server' — check $BUILD_DIR/bin/)" >&2
  # Fall back to 'server' name (older llama.cpp versions).
  BINARY="$BUILD_DIR/bin/server"
  if [ ! -f "$BINARY" ]; then
    echo "build-llama-server: neither llama-server nor server found" >&2
    exit 1
  fi
fi

# Copy to src-tauri/binaries/ with the target-triple suffix.
mkdir -p "$SRC_TAURI/binaries"
DEST="$SRC_TAURI/binaries/llama-server-$TARGET"
cp "$BINARY" "$DEST"
echo "build-llama-server: copied $BINARY → $DEST ✓"