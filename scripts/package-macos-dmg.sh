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

mkdir -p "$DMG_DIR"
rm -f "$DMG_PATH"

# Clean up incomplete rw.* images left by failed Tauri dmg packaging attempts.
find "$(dirname "$APP_PATH")" -maxdepth 1 -type f -name "rw.*.dmg" -delete || true

hdiutil create \
  -volname "$APP_NAME" \
  -srcfolder "$APP_PATH" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

echo "Created DMG: $DMG_PATH"
