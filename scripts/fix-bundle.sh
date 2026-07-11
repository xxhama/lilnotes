#!/bin/bash
# Post-build fixup: copy sherpa-rs / onnxruntime dylibs into the .app bundle
# and fix rpath so the binary can find them at runtime.
#
# Tauri's bundler doesn't know about Rust dependency dylibs (sherpa-onnx,
# onnxruntime) — this script bridges that gap for production builds.
#
# Run automatically via: npm run tauri:build
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET="$ROOT/src-tauri/target/release"
BUNDLE_DIR="$TARGET/bundle/macos"

APP="$BUNDLE_DIR/LilNotes.app"
BIN="$APP/Contents/MacOS/lilnotes"
FRAMEWORKS="$APP/Contents/Frameworks"

if [ ! -d "$APP" ]; then
  echo "fix-bundle: LilNotes.app not found at $APP — was 'tauri build' run?" >&2
  exit 1
fi

# Dylibs shipped by sherpa-rs that the binary links via @rpath.
DYLIBS=(
  "libonnxruntime.1.17.1.dylib"
  "libsherpa-onnx-c-api.dylib"
)

echo "fix-bundle: copying dylibs into $APP/Contents/Frameworks/"
mkdir -p "$FRAMEWORKS"

for lib in "${DYLIBS[@]}"; do
  SRC="$TARGET/$lib"
  if [ ! -f "$SRC" ]; then
    echo "fix-bundle: $lib not found in $TARGET — skipping" >&2
    continue
  fi
  cp "$SRC" "$FRAMEWORKS/$lib"
  echo "  ✓ $lib"
done

# Ensure the binary has an rpath pointing at ../Frameworks.
if otool -l "$BIN" | grep -q "@executable_path/../Frameworks"; then
  echo "fix-bundle: rpath already set"
else
  install_name_tool -add_rpath @executable_path/../Frameworks "$BIN"
  echo "fix-bundle: added rpath @executable_path/../Frameworks"
fi

# Fix the install name of sherpa-onnx (it itself links onnxruntime via @rpath).
install_name_tool -id @rpath/libsherpa-onnx-c-api.dylib "$FRAMEWORKS/libsherpa-onnx-c-api.dylib" 2>/dev/null || true

# Re-sign the bundle ad-hoc. Tauri's bundler signed the app before we added
# dylibs and modified rpaths, which invalidated that signature. Without
# re-signing, Gatekeeper kills the app on launch with "Check with the
# developer to make sure LilNotes works with this version of macOS."
# For distribution, replace `-` with a Developer ID certificate.
echo "fix-bundle: re-signing bundle (ad-hoc)…"
codesign --force --deep --sign - "$APP"
echo "  ✓ signed"

# Re-create the .dmg from the fixed .app. Tauri's bundler creates the .dmg
# BEFORE this script runs, so the shipped .dmg would contain the unfixed
# app (no dylibs, no rpath). We rebuild it here from the corrected .app.
DMG_DIR="$TARGET/bundle/dmg"
DMG="$DMG_DIR/LilNotes_0.1.0_aarch64.dmg"
if [ -d "$DMG_DIR" ]; then
  echo "fix-bundle: re-creating .dmg from fixed .app…"
  rm -f "$DMG"
  # Staging dir with the app + an Applications symlink for drag-to-install.
  STAGE=$(mktemp -d)
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname "LilNotes" -srcfolder "$STAGE" -ov -quiet -format UDZO "$DMG"
  rm -rf "$STAGE"
  echo "  ✓ $DMG"
fi

echo "fix-bundle: done ✓"
echo ""
echo "Next steps for distribution:"
echo "  1. Code sign:    codesign --deep --force --sign 'Developer ID Application: Your Name' '$APP'"
echo "  2. Notarize:     xcrun notarytool submit '$APP' --apple-id ... --team-id ... --password ..."
echo "  3. Staple:       xcrun stapler staple '$APP'"