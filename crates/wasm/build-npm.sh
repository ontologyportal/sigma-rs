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
# Publish with:  cd <out-dir> && npm publish
#
#   THREADED=1 ./build-npm.sh      # also builds crates/wasm/pkg-threaded/
#
# The threaded variant adds the `parallel` feature (rayon via
# wasm-bindgen-rayon), which needs a nightly toolchain + `rust-src` to
# `-Zbuild-std` the atomics-enabled std it depends on — a normal stable
# `rustup target add wasm32-unknown-unknown` install can't produce this.
# It's best-effort: if nightly/rust-src is missing, the normal pkg/ build
# above still completes and this step just prints why it skipped. It builds
# into its own `--target-dir` (target-threaded/) rather than sharing
# `target/wasm32-unknown-unknown` with the plain build above — the two use
# different RUSTFLAGS (atomics/bulk-memory), and sharing a target dir across
# differing RUSTFLAGS forces a full rebuild every time either variant runs.
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

# Optional size pass — only if wasm-opt (binaryen) is installed. Never fatal:
# older binaryen builds reject newer wasm opcodes (e.g. reference-types /
# bulk-memory: "invalid code after misc prefix: 17"), so we pass the feature
# flags and, if it still fails, keep the unoptimized .wasm rather than abort.
if command -v wasm-opt >/dev/null; then
  echo "==> Optimizing .wasm with wasm-opt -Oz"
  WASM_FILE="$OUT_PATH/${LIB_NAME}_bg.wasm"
  if wasm-opt -Oz \
       --enable-bulk-memory --enable-reference-types --enable-mutable-globals \
       --enable-nontrapping-float-to-int --enable-sign-ext \
       "$WASM_FILE" -o "$WASM_FILE.opt" 2>/dev/null; then
    mv "$WASM_FILE.opt" "$WASM_FILE"
    echo "    optimized"
  else
    rm -f "$WASM_FILE.opt"
    echo "    wasm-opt failed (likely an old binaryen); keeping the unoptimized .wasm"
  fi
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
echo "  Publish : cd '$OUT_PATH' && npm publish"

# -- Threaded variant (THREADED=1) -------------------------------------------
#
# Independent of the pkg/ build above: separate target dir, separate
# toolchain, separate output dir. Never touches pkg/.
if [ "${THREADED:-}" = "1" ]; then
  echo
  echo "==> THREADED=1: attempting pkg-threaded/ build"

  THREADED_OK=1
  if ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
    echo "    skip: no nightly toolchain installed (rustup toolchain install nightly)"
    THREADED_OK=0
  elif ! rustup component list --toolchain nightly 2>/dev/null | grep -q '^rust-src (installed)'; then
    echo "    skip: rust-src missing on nightly (rustup component add rust-src --toolchain nightly)"
    THREADED_OK=0
  fi

  if [ "$THREADED_OK" = "1" ]; then
    THREADED_OUT="$CRATE_DIR/pkg-threaded"
    THREADED_TARGET_DIR="$WORKSPACE_ROOT/target-threaded"

    echo "==> Compiling (release, threaded, $WASM_TRIPLE)"
    # --shared-memory/--max-memory must be passed to wasm-ld explicitly:
    # `+atomics` alone makes rustc EMIT atomic opcodes but (on current
    # nightlies) does not flip the linked memory to shared, and workers can't
    # share an unshared memory — wasm-bindgen-rayon's pool would silently get
    # a private copy. Shared memories require an explicit max; 1 GiB matches
    # the usual wasm-bindgen-rayon setup.
    RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--max-memory=1073741824 -C link-arg=--import-memory -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base -C link-arg=--export=__heap_base -C link-arg=--export=__data_end" \
      cargo +nightly build \
        --manifest-path "$CRATE_DIR/Cargo.toml" \
        --target "$WASM_TRIPLE" \
        --target-dir "$THREADED_TARGET_DIR" \
        -Zbuild-std=std,panic_abort \
        --release \
        --features parallel

    THREADED_WASM_IN="$THREADED_TARGET_DIR/$WASM_TRIPLE/release/$LIB_NAME.wasm"
    [ -f "$THREADED_WASM_IN" ] || { echo "error: expected $THREADED_WASM_IN"; exit 1; }

    echo "==> Generating bindings ($TARGET) into pkg-threaded/"
    rm -rf "$THREADED_OUT"
    mkdir -p "$THREADED_OUT"
    wasm-bindgen --target "$TARGET" --out-dir "$THREADED_OUT" "$THREADED_WASM_IN"

    # wasm-bindgen-rayon's worker helper imports the main module as
    # `import('../../..')` — a bare DIRECTORY url. Bundlers resolve that to
    # the package entry; a plain static server (serve.sh, GitHub Pages)
    # returns a directory listing / 404 instead, the import rejects
    # UNHANDLED inside the worker (no onerror fires), the worker never
    # posts wasm_bindgen_worker_ready, and initThreadPool() awaits forever.
    # Point it at the actual main-module file so no-bundler serving works.
    HELPER=$(find "$THREADED_OUT/snippets" -name workerHelpers.js 2>/dev/null | head -1)
    if [ -n "$HELPER" ]; then
      sed -i.bak "s|import('../../..')|import('../../../${LIB_NAME}.js')|" "$HELPER"
      rm -f "$HELPER.bak"
    fi

    if command -v wasm-opt >/dev/null; then
      echo "==> Optimizing .wasm with wasm-opt -Oz (threads-enabled)"
      THREADED_WASM_FILE="$THREADED_OUT/${LIB_NAME}_bg.wasm"
      if wasm-opt -Oz \
           --enable-threads --enable-bulk-memory --enable-reference-types --enable-mutable-globals \
           --enable-nontrapping-float-to-int --enable-sign-ext \
           "$THREADED_WASM_FILE" -o "$THREADED_WASM_FILE.opt" 2>/dev/null; then
        mv "$THREADED_WASM_FILE.opt" "$THREADED_WASM_FILE"
        echo "    optimized"
      else
        rm -f "$THREADED_WASM_FILE.opt"
        echo "    wasm-opt failed (likely an old binaryen); keeping the unoptimized .wasm"
      fi
    else
      echo "==> wasm-opt not found; skipping size optimization (optional)"
    fi

    echo "==> Assembling package metadata (pkg-threaded/)"
    cp "$CRATE_DIR/npm/package.json" "$THREADED_OUT/package.json"
    cp "$CRATE_DIR/README.md"        "$THREADED_OUT/README.md"
    cp "$CRATE_DIR/js/sdk.mjs"       "$THREADED_OUT/sdk.mjs"
    cp "$CRATE_DIR/js/sdk.d.ts"      "$THREADED_OUT/sdk.d.ts"
    if [ -f "$CRATE_DIR/LICENSE" ]; then
      cp "$CRATE_DIR/LICENSE" "$THREADED_OUT/LICENSE"
    elif [ -f "$WORKSPACE_ROOT/LICENSE" ]; then
      cp "$WORKSPACE_ROOT/LICENSE" "$THREADED_OUT/LICENSE"
    fi

    echo
    echo "Done. Threaded package is in: $THREADED_OUT"
  else
    echo "==> Skipping pkg-threaded/ build (normal pkg/ build above is unaffected)"
  fi
fi
