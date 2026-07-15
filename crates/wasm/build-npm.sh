#!/usr/bin/env bash
#
# Build a publishable npm package for sumo-parser-wasm WITHOUT wasm-pack —
# only cargo + wasm-bindgen-cli are required.
#
#   ./build-npm.sh [target] [out-dir]
#
#     target   wasm-bindgen target: web (default) | bundler | nodejs
#     out-dir  output directory, relative to this crate (default: pkg)
#
# Examples:
#   ./build-npm.sh                 # → crates/wasm/pkg/       (browser ESM)
#   ./build-npm.sh nodejs pkg-node # → crates/wasm/pkg-node/  (Node CommonJS)
#
# Publish with:  cd <out-dir> && npm publish --access public
#
set -euo pipefail

TARGET="${1:-web}"
OUT_DIR="${2:-pkg}"

# Resolve paths relative to this script so it works from any CWD.
CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
OUT_PATH="$CRATE_DIR/$OUT_DIR"
LIB_NAME="sumo_parser_wasm"
WASM_TRIPLE="wasm32-unknown-unknown"

echo "==> Checking toolchain"
command -v cargo >/dev/null || { echo "error: cargo not found"; exit 1; }
command -v wasm-bindgen >/dev/null || {
  echo "error: wasm-bindgen not found."
  echo "       cargo install wasm-bindgen-cli --version 0.2.121"
  exit 1
}
rustup target list --installed 2>/dev/null | grep -q "$WASM_TRIPLE" || {
  echo "==> Adding $WASM_TRIPLE target"
  rustup target add "$WASM_TRIPLE"
}

echo "==> Compiling (release, $WASM_TRIPLE)"
cargo build \
  --manifest-path "$CRATE_DIR/Cargo.toml" \
  --target "$WASM_TRIPLE" \
  --release

WASM_IN="$WORKSPACE_ROOT/target/$WASM_TRIPLE/release/$LIB_NAME.wasm"
[ -f "$WASM_IN" ] || { echo "error: expected $WASM_IN"; exit 1; }

echo "==> Generating bindings ($TARGET) into $OUT_DIR/"
rm -rf "$OUT_PATH"
mkdir -p "$OUT_PATH"
wasm-bindgen --target "$TARGET" --out-dir "$OUT_PATH" "$WASM_IN"

# Optional size pass — only if wasm-opt (binaryen) is installed.
if command -v wasm-opt >/dev/null; then
  echo "==> Optimizing .wasm with wasm-opt -Oz"
  wasm-opt -Oz "$OUT_PATH/${LIB_NAME}_bg.wasm" -o "$OUT_PATH/${LIB_NAME}_bg.wasm"
else
  echo "==> wasm-opt not found; skipping size optimization (optional)"
fi

echo "==> Assembling package metadata"
cp "$CRATE_DIR/npm/package.json" "$OUT_PATH/package.json"
cp "$CRATE_DIR/README.md"        "$OUT_PATH/README.md"
# SDK-shaped facade (Session/Source/Backend), published at the "./sdk" subpath.
cp "$CRATE_DIR/js/sdk.mjs"        "$OUT_PATH/sdk.mjs"
cp "$CRATE_DIR/js/sdk.d.ts"       "$OUT_PATH/sdk.d.ts"
# License: prefer a crate-local LICENSE, else the workspace root's.
if [ -f "$CRATE_DIR/LICENSE" ]; then
  cp "$CRATE_DIR/LICENSE" "$OUT_PATH/LICENSE"
elif [ -f "$WORKSPACE_ROOT/LICENSE" ]; then
  cp "$WORKSPACE_ROOT/LICENSE" "$OUT_PATH/LICENSE"
fi

echo
echo "Done. Package is in: $OUT_PATH"
echo "  Inspect : cd '$OUT_PATH' && npm publish --dry-run"
echo "  Publish : cd '$OUT_PATH' && npm publish --access public"
