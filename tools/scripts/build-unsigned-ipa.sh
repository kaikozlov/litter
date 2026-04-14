#!/usr/bin/env bash
# Build an unsigned IPA from the current working tree.
#
# Usage:
#   ./tools/scripts/build-unsigned-ipa.sh [CONFIG]
#
#   CONFIG  — Xcode build configuration (default: Release)
#
# Prerequisites:
#   - Xcode with iOS SDK installed
#   - rustup-managed Rust toolchain with aarch64-apple-ios target
#   - xcodegen, meson, ninja (brew install xcodegen meson ninja)
#
# Output:
#   artifacts/litter-unsigned.ipa
#
# The IPA is unsigned. Sideload it with TrollStore, Sideloadly, altstore,
# or sign it manually with:
#   codesign -f -s "iPhone Developer" --entitlements ... Payload/Litter.app
#   zip -r ../signed.ipa Payload/

set -euo pipefail

# ── Config ───────────────────────────────────────────────────────────────────

CONFIG="${1:-Release}"
SCHEME="${SCHEME:-Litter}"
ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
IOS_DIR="$ROOT_DIR/apps/ios"
ARTIFACTS_DIR="$ROOT_DIR/artifacts"
IPA_NAME="litter-unsigned.ipa"
IPA_PATH="$ARTIFACTS_DIR/$IPA_NAME"

# Ensure rustup toolchain comes first for cross-compilation targets.
RUSTUP_BIN="$(dirname "$(rustup which cargo 2>/dev/null || echo "$HOME/.cargo/bin/cargo")")"
CARGO_BIN="$HOME/.cargo/bin"
export PATH="$RUSTUP_BIN:$CARGO_BIN:$PATH"

# ── Step 1: Build Rust for iOS device (arm64) ────────────────────────────────

echo "==> [1/5] Building Rust for iOS device (aarch64-apple-ios, $CONFIG)..."
cd "$ROOT_DIR"

# Sync submodule + apply patches if needed.
if [ ! -f .build-stamps/sync ]; then
  echo "    Syncing codex submodule..."
  "$IOS_DIR/scripts/sync-codex.sh" --preserve-current
  mkdir -p .build-stamps
  touch .build-stamps/sync
fi

# Generate Swift bindings if needed.
if [ ! -f .build-stamps/bindings-swift ]; then
  echo "    Generating Swift bindings..."
  cd shared/rust-bridge
  ./generate-bindings.sh --swift-only
  cd "$ROOT_DIR"
  mkdir -p .build-stamps
  touch .build-stamps/bindings-swift
fi

# Build Rust — use the fast device lane for raw staticlib output.
# For Release config we use the package lane (xcframework).
case "$CONFIG" in
  Debug)
    echo "    Using fast device lane (raw staticlib)..."
    make rust-ios-device-fast
    ;;
  Release|AdHoc|Distribution)
    echo "    Using package lane (xcframework)..."
    make rust-ios-package
    ;;
  *)
    echo "    Using fast device lane (raw staticlib)..."
    make rust-ios-device-fast
    ;;
esac

# ── Step 2: Download ios_system frameworks ───────────────────────────────────

echo "==> [2/5] Downloading ios_system frameworks..."
make ios-frameworks

# ── Step 3: Generate Xcode project ──────────────────────────────────────────

echo "==> [3/5] Generating Xcode project..."
make xcgen

# ── Step 4: Build .app (unsigned) ────────────────────────────────────────────

echo "==> [4/5] Building Litter.app ($CONFIG, unsigned, arm64)..."

BUILD_DIR="$ARTIFACTS_DIR/unsigned-build"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"

xcodebuild archive \
  -project "$IOS_DIR/Litter.xcodeproj" \
  -scheme "$SCHEME" \
  -configuration "$CONFIG" \
  -destination "generic/platform=iOS" \
  -archivePath "$BUILD_DIR/Litter.xcarchive" \
  CODE_SIGN_IDENTITY="-" \
  CODE_SIGNING_REQUIRED=NO \
  CODE_SIGNING_ALLOWED=NO \
  ENABLE_BITCODE=NO \
  DEVELOPMENT_TEAM="" \
  | tail -5

# Locate the .app inside the archive.
APP_PATH="$BUILD_DIR/Litter.xcarchive/Products/Applications/Litter.app"
if [ ! -d "$APP_PATH" ]; then
  # Fallback: check for any .app in the archive.
  APP_PATH="$(find "$BUILD_DIR/Litter.xcarchive/Products" -name "*.app" -maxdepth 3 | head -1)"
fi

if [ -z "$APP_PATH" ] || [ ! -d "$APP_PATH" ]; then
  echo "ERROR: Could not find Litter.app in the archive."
  echo "Looking inside: $BUILD_DIR/Litter.xcarchive/"
  find "$BUILD_DIR" -name "*.app" -maxdepth 5 2>/dev/null || true
  exit 1
fi

echo "    Found app bundle: $APP_PATH"

# ── Step 5: Package into IPA ────────────────────────────────────────────────

echo "==> [5/5] Packaging $IPA_NAME..."
rm -f "$IPA_PATH"
mkdir -p "$ARTIFACTS_DIR"

# IPA = zip file with Payload/ folder containing the .app.
STAGING="$BUILD_DIR/ipa-staging"
rm -rf "$STAGING"
mkdir -p "$STAGING/Payload"

# Copy the .app — use rsync to preserve symlinks and permissions.
rsync -a "$APP_PATH/" "$STAGING/Payload/Litter.app/"

# Create the IPA (zip with Payload/ at root).
cd "$STAGING"
zip -r --symlinks "$IPA_PATH" Payload/
cd "$ROOT_DIR"

# Clean up staging.
rm -rf "$STAGING"

# ── Done ─────────────────────────────────────────────────────────────────────

IPA_SIZE="$(du -h "$IPA_PATH" | cut -f1 | xargs)"
echo ""
echo "✅ Done! Unsigned IPA:"
echo "   $IPA_PATH ($IPA_SIZE)"
echo ""
echo "To sideload:"
echo "   • TrollStore: transfer the .ipa to your device and open with TrollStore"
echo "   • Sideloadly / altstore: drag the .ipa into the app"
echo "   • Manual signing:"
echo "       unzip $IPA_NAME"
echo "       codesign -f -s '<identity>' --entitlements <entitlements.plist> Payload/Litter.app"
echo "       zip -r ../signed.ipa Payload/"
echo ""
