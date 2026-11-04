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

yarn tauri build --target "$TARGET" --bundles app
bash ./scripts/package-macos-dmg.sh "$TARGET"
