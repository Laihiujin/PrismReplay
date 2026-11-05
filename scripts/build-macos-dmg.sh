#!/usr/bin/env bash

set -euo pipefail

TARGET="${1:?target is required}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

export CMAKE_PROJECT_INCLUDE="$ROOT_DIR/scripts/macos-portable.cmake"
export MACOSX_DEPLOYMENT_TARGET="13.3"

export SDKROOT="${SDKROOT:-$(xcrun --sdk macosx --show-sdk-path)}"
export CMAKE_OSX_DEPLOYMENT_TARGET="${CMAKE_OSX_DEPLOYMENT_TARGET:-13.3}"
export LIBCLANG_PATH="${LIBCLANG_PATH:-$(brew --prefix llvm)/lib}"

if [[ "$TARGET" == "aarch64-apple-darwin" ]]; then
  export GGML_METAL_EMBED_LIBRARY="${GGML_METAL_EMBED_LIBRARY:-1}"
fi

BUILD_ARGS=(tauri build --target "$TARGET" --bundles app)
if [[ -z "${APPLE_SIGNING_IDENTITY:-}" || "$APPLE_SIGNING_IDENTITY" == "-" ]]; then
  # Tauri treats an exported empty APPLE_CERTIFICATE as a certificate to import.
  # The DMG packager signs the app ad-hoc; do not touch a keychain in this mode.
  export APPLE_SIGNING_IDENTITY="-"
  unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD
  BUILD_ARGS+=(--no-sign)
elif [[ -z "${APPLE_CERTIFICATE:-}" ]]; then
  # A real identity may already be installed in the runner's keychain.
  unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD
fi

yarn "${BUILD_ARGS[@]}"
bash ./scripts/package-macos-dmg.sh "$TARGET"
