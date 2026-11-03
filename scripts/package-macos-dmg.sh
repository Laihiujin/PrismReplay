#!/usr/bin/env bash

set -euo pipefail

TARGET="${1:?target is required}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

APP_NAME="${APP_NAME:-PrismReplay}"
VERSION="${VERSION:-$(node -p "require('./package.json').version")}"

case "$TARGET" in
  aarch64-apple-darwin)
    SUFFIX="aarch64"
    ;;
  x86_64-apple-darwin)
    SUFFIX="x86_64"
    ;;
  universal-apple-darwin)
    SUFFIX="universal"
    ;;
  *)
    echo "Unsupported macOS target: $TARGET" >&2
    exit 1
    ;;
esac

APP_PATH="src-tauri/target/${TARGET}/release/bundle/macos/${APP_NAME}.app"
DMG_DIR="src-tauri/target/${TARGET}/release/bundle/dmg"
DMG_PATH="${DMG_DIR}/${APP_NAME}_${VERSION}_${SUFFIX}.dmg"

if [[ ! -d "$APP_PATH" ]]; then
  echo "App bundle not found: $APP_PATH" >&2
  exit 1
fi

codesign --force --deep --sign "${APPLE_SIGNING_IDENTITY:--}" "$APP_PATH"
codesign --verify --deep --strict "$APP_PATH"

mkdir -p "$DMG_DIR"
rm -f "$DMG_PATH"

# Clean up incomplete rw.* images left by failed Tauri dmg packaging attempts.
find "$(dirname "$APP_PATH")" -maxdepth 1 -type f -name "rw.*.dmg" -delete || true

STAGING_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGING_DIR"' EXIT
ditto "$APP_PATH" "$STAGING_DIR/$APP_NAME.app"
ln -s /Applications "$STAGING_DIR/Applications"

hdiutil create \
  -volname "$APP_NAME" \
  -srcfolder "$STAGING_DIR" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

hdiutil verify "$DMG_PATH"
MOUNT_DIR="$(mktemp -d)"
trap 'hdiutil detach "$MOUNT_DIR" >/dev/null 2>&1 || true; rm -rf "$MOUNT_DIR" "$STAGING_DIR"' EXIT
hdiutil attach "$DMG_PATH" -readonly -nobrowse -mountpoint "$MOUNT_DIR"
INSTALLED_APP="$STAGING_DIR/installed/$APP_NAME.app"
mkdir -p "$STAGING_DIR/installed"
ditto "$MOUNT_DIR/$APP_NAME.app" "$INSTALLED_APP"
codesign --verify --deep --strict "$INSTALLED_APP"
PLIST="$INSTALLED_APP/Contents/Info.plist"
ACTUAL_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$PLIST")"
[[ "$ACTUAL_VERSION" == "$VERSION" ]] || { echo "Version mismatch: $ACTUAL_VERSION != $VERSION" >&2; exit 1; }
EXECUTABLE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$PLIST")"
case "$TARGET" in
  aarch64-apple-darwin) lipo -verify_arch arm64 "$INSTALLED_APP/Contents/MacOS/$EXECUTABLE" ;;
  x86_64-apple-darwin) lipo -verify_arch x86_64 "$INSTALLED_APP/Contents/MacOS/$EXECUTABLE" ;;
  universal-apple-darwin) lipo -verify_arch arm64 x86_64 "$INSTALLED_APP/Contents/MacOS/$EXECUTABLE" ;;
esac
hdiutil detach "$MOUNT_DIR"
rmdir "$MOUNT_DIR"
trap 'rm -rf "$STAGING_DIR"' EXIT
shasum -a 256 "$DMG_PATH" > "$DMG_PATH.sha256"
echo "Verified DMG layout, app signature, version and architecture: $DMG_PATH"
