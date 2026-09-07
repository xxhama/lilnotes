#!/bin/bash
# Post-build fixup: copy sherpa-rs / onnxruntime dylibs into the .app bundle,
# fix rpath so the binary can find them at runtime, sign the bundle, rebuild
# the DMG, and (when credentials are present) notarize + staple both.
#
# Tauri's bundler doesn't know about Rust dependency dylibs (sherpa-onnx,
# onnxruntime) — this script bridges that gap for production builds. Because
# adding those dylibs invalidates whatever signature `tauri build` produced,
# this script owns signing entirely. Do NOT set Tauri's own signing env vars
# (APPLE_SIGNING_IDENTITY, APPLE_ID, APPLE_API_KEY…): they would make
# `tauri build` sign and notarize a bundle we throw away.
#
# Signing modes (chosen by environment):
#   MACOS_SIGN_IDENTITY unset      → ad-hoc signature. Local/fork builds.
#                                    Gatekeeper shows "damaged" on first
#                                    launch; `xattr -cr` or right-click→Open.
#   MACOS_SIGN_IDENTITY set        → Developer ID + hardened runtime.
#     + NOTARY_KEY_P8_PATH,          → …and notarized with an App Store
#       NOTARY_KEY_ID,                  Connect API key, then stapled.
#       NOTARY_ISSUER_ID
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
SIDECAR="$APP/Contents/MacOS/llama-server"
ENTITLEMENTS="$ROOT/src-tauri/entitlements.plist"

# --- Signing mode ------------------------------------------------------------
IDENTITY="${MACOS_SIGN_IDENTITY:-}"
NOTARIZE=0
if [ -n "$IDENTITY" ]; then
  # Hardened runtime + secure timestamp are both required by notarization.
  SIGN=(codesign --force --sign "$IDENTITY" --options runtime --timestamp)
  if [ -n "${NOTARY_KEY_P8_PATH:-}" ] && [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER_ID:-}" ]; then
    NOTARIZE=1
  else
    echo "fix-bundle: WARNING: signing with '$IDENTITY' but NOTARY_* not set — skipping notarization" >&2
  fi
else
  # Ad-hoc: no Team ID, so the hardened runtime's library validation would
  # reject our own dylibs. Sign plainly, no timestamp.
  SIGN=(codesign --force --sign - --timestamp=none)
  echo "fix-bundle: WARNING: MACOS_SIGN_IDENTITY not set — ad-hoc signing (Gatekeeper will warn on first launch)" >&2
fi

# notarize <file>: submit to Apple, wait, fail loudly with the log on rejection.
notarize() {
  local file="$1" out status id
  echo "fix-bundle: notarizing $(basename "$file") (usually 1–5 min)…"
  out=$(xcrun notarytool submit "$file" \
    --key "$NOTARY_KEY_P8_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID" \
    --wait --timeout 30m --output-format json)
  status=$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))')
  id=$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')
  if [ "$status" != "Accepted" ]; then
    echo "fix-bundle: ERROR: notarization of $(basename "$file") ended with status '$status' (submission $id)" >&2
    [ -n "$id" ] && xcrun notarytool log "$id" \
      --key "$NOTARY_KEY_P8_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID" >&2 || true
    exit 1
  fi
  echo "  ✓ accepted (submission $id)"
}

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
    # Hard error: the app cannot run without these. A restored CI cache can
    # leave the sherpa-rs build script "fresh" without re-copying its dylibs
    # into target/release — shipping without them means a crash on launch.
    echo "fix-bundle: ERROR: $lib not found in $TARGET" >&2
    exit 1
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

# Sign inside-out: every nested Mach-O first (dylibs, sidecar), then the app
# bundle itself, which seals Contents/ and the main executable. `--deep` is
# deprecated for signing and would also apply the app's entitlements to the
# dylibs, so each is signed explicitly. Tauri's bundler signed the app before
# we added dylibs and modified rpaths, which invalidated that signature.
echo "fix-bundle: signing ($( [ -n "$IDENTITY" ] && echo "$IDENTITY" || echo ad-hoc ))…"
for lib in "$FRAMEWORKS"/*.dylib; do
  "${SIGN[@]}" "$lib"
  echo "  ✓ $(basename "$lib")"
done
if [ -f "$SIDECAR" ]; then
  "${SIGN[@]}" "$SIDECAR"
  echo "  ✓ $(basename "$SIDECAR")"
fi
"${SIGN[@]}" --entitlements "$ENTITLEMENTS" "$APP"
echo "  ✓ $(basename "$APP")"
codesign --verify --deep --strict --verbose=2 "$APP" 2>&1 | sed 's/^/  /'

if [ "$NOTARIZE" = 1 ]; then
  # Notarize the app first so the DMG's ticket covers a stapled app.
  ZIP=$(mktemp -d)/LilNotes.zip
  ditto -c -k --keepParent "$APP" "$ZIP"
  notarize "$ZIP"
  rm -f "$ZIP"
  xcrun stapler staple "$APP"
  echo "  ✓ stapled $(basename "$APP")"
fi

# Re-create the .dmg from the fixed .app. Tauri's bundler creates the .dmg
# BEFORE this script runs, so the shipped .dmg would contain the unfixed
# app (no dylibs, no rpath). We rebuild it here from the corrected .app.
DMG_DIR="$TARGET/bundle/dmg"
VERSION=$(node -p "require('$ROOT/src-tauri/tauri.conf.json').version")
DMG="$DMG_DIR/LilNotes_${VERSION}_aarch64.dmg"
if [ -d "$DMG_DIR" ]; then
  echo "fix-bundle: re-creating .dmg from fixed .app…"
  rm -f "$DMG"
  # Staging dir with the app + an Applications symlink for drag-to-install.
  STAGE=$(mktemp -d)
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  # License texts travel with the distribution (BSD/Apache notice clauses).
  for f in LICENSE THIRD_PARTY_NOTICES.md; do
    [ -f "$ROOT/$f" ] && cp "$ROOT/$f" "$STAGE/$f"
  done
  hdiutil create -volname "LilNotes" -srcfolder "$STAGE" -ov -quiet -format UDZO "$DMG"
  rm -rf "$STAGE"
  echo "  ✓ $DMG"

  if [ -n "$IDENTITY" ]; then
    codesign --force --sign "$IDENTITY" --timestamp "$DMG"
    echo "  ✓ signed $(basename "$DMG")"
  fi
  if [ "$NOTARIZE" = 1 ]; then
    notarize "$DMG"
    xcrun stapler staple "$DMG"
    echo "  ✓ stapled $(basename "$DMG")"
    echo "fix-bundle: Gatekeeper assessment"
    spctl -a -vv -t open --context context:primary-signature "$DMG" 2>&1 | sed 's/^/  /'
    spctl -a -vv -t exec "$APP" 2>&1 | sed 's/^/  /'
  fi
fi

echo "fix-bundle: done ✓"
