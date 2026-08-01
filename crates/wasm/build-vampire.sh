#!/usr/bin/env bash
#
# Build vampire.wasm/.js for the in-browser "Vampire (WASM)" prover backend
# — an alternative to the native Rust prover, selectable in the Ask/Tell and
# Audit settings.
#
# Nothing Vampire-related is vendored into this repo: both the Vampire C++
# source and the vprover/vampireGuide build scripts that know how to compile
# it for Emscripten are fetched fresh into a gitignored cache directory,
# pinned to specific commits for reproducibility (bump VAMPIRE_GUIDE_REF /
# the guide's own vampire-version.env pin to update). This mirrors how
# serve.sh already treats the Rust→wasm build: nothing built is committed.
#
# Requires:
#   - the Emscripten SDK (emcc / emcmake / emmake on PATH)
#   - GNU awk (gawk). macOS's bundled `awk` (BWK awk) cannot parse
#     editCMakeLists.sh's multi-line `-v` string assignments ("awk: newline
#     in string") and SILENTLY produces a no-op patch instead of erroring —
#     `command -v gawk` is checked explicitly so a missing gawk fails loud
#     here instead of shipping an unpatched (broken) CMakeLists.txt.
#     `brew install gawk` on macOS.
#   - git, cmake, python3, make — the ordinary Vampire native-build deps.
#
# Output: web/vampire/{vampire.js,vampire.wasm,vampire-runner.js} (gitignored,
# same treatment as web/pkg/) — serve.sh copies nothing further; this script
# writes directly into web/.
#
#   ./build-vampire.sh                # build once, skip on a later call at
#                                      # the same pin (see BUILT_STAMP below)
#   VAMPIRE_RECLONE=1 ./build-vampire.sh   # force a clean rebuild
#   SKIP_VAMPIRE=1 ./build-vampire.sh      # no-op (serve.sh's opt-out)
set -euo pipefail

if [ "${SKIP_VAMPIRE:-}" = "1" ]; then
  echo "==> SKIP_VAMPIRE=1: not building the Vampire WASM backend"
  exit 0
fi

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE_DIR="${VAMPIRE_CACHE_DIR:-$CRATE_DIR/.vampire-cache}"
GUIDE_DIR="$CACHE_DIR/guide"
VAMPIRE_DIR="$CACHE_DIR/vampire"
OUT_DIR="$CRATE_DIR/web/vampire"

# Pinned to a specific vampireGuide commit — NOT `main` — so this build is
# reproducible; vampireGuide's own vampire-version.env (fetched below, at
# this same pin) in turn pins the Vampire source commit.
VAMPIRE_GUIDE_REPO=${VAMPIRE_GUIDE_REPO:-https://raw.githubusercontent.com/vprover/vampireGuide}
VAMPIRE_GUIDE_REF=${VAMPIRE_GUIDE_REF:-e46d374b9ef9f2073d5ac80cd692974f3dd23b39}

BUILT_STAMP="$CACHE_DIR/BUILT_REF"
if [ "${VAMPIRE_RECLONE:-0}" != "1" ] && [ -f "$BUILT_STAMP" ] \
   && [ "$(cat "$BUILT_STAMP")" = "$VAMPIRE_GUIDE_REF" ] \
   && [ -f "$OUT_DIR/vampire.wasm" ] && [ -f "$OUT_DIR/vampire.js" ]; then
  echo "==> Vampire WASM already built at guide ref $VAMPIRE_GUIDE_REF — skipping"
  echo "    (VAMPIRE_RECLONE=1 to force a rebuild)"
  exit 0
fi

echo "==> Checking toolchain"
command -v emcc    >/dev/null || { echo "error: emcc not found — install the Emscripten SDK (emsdk) or 'brew install emscripten'"; exit 1; }
command -v emcmake >/dev/null || { echo "error: emcmake not found (Emscripten SDK incomplete)"; exit 1; }
command -v emmake  >/dev/null || { echo "error: emmake not found (Emscripten SDK incomplete)"; exit 1; }
command -v cmake   >/dev/null || { echo "error: cmake not found"; exit 1; }
command -v python3 >/dev/null || { echo "error: python3 not found"; exit 1; }
# `editCMakeLists.sh` invokes plain `awk`, not `gawk` — if the system `awk`
# isn't GNU awk (macOS: BWK awk, which mis-parses the script's multi-line
# `-v` strings), shim `awk` -> gawk via a scratch dir prepended to PATH.
# Avoids depending on Homebrew's `gnubin` layout, which differs between
# Intel/ARM Macs and isn't there at all on Linux (whose awk usually already
# IS gawk).
AWK_SHIM=""
if awk --version 2>&1 | head -1 | grep -qi "GNU Awk"; then
  : # system awk is already gawk
else
  GAWK=$(command -v gawk || true)
  [ -n "$GAWK" ] || { echo "error: gawk not found (system awk isn't GNU awk) — 'brew install gawk' (see this script's header)"; exit 1; }
  AWK_SHIM=$(mktemp -d)
  ln -s "$GAWK" "$AWK_SHIM/awk"
  trap 'rm -rf "$AWK_SHIM"' EXIT
fi

mkdir -p "$GUIDE_DIR"
echo "==> Fetching vampireGuide build scripts (pinned $VAMPIRE_GUIDE_REF)"
for f in editCMakeLists.sh smallWasmBuild.sh patchVampireInteractive.sh vampire-runner.js vampire-version.env; do
  curl -fsSL "$VAMPIRE_GUIDE_REPO/$VAMPIRE_GUIDE_REF/$f" -o "$GUIDE_DIR/$f"
done
chmod +x "$GUIDE_DIR"/*.sh

# shellcheck disable=SC1090
source "$GUIDE_DIR/vampire-version.env"
VAMPIRE_REF=${VAMPIRE_REF:?vampire-version.env did not set VAMPIRE_REF}
echo "    vampire pinned at $VAMPIRE_REF ($VAMPIRE_TAG)"

if [ "${VAMPIRE_RECLONE:-0}" = "1" ] && [ -d "$VAMPIRE_DIR" ]; then
  echo "==> VAMPIRE_RECLONE=1: removing existing checkout"
  rm -rf "$VAMPIRE_DIR"
fi

if [ ! -d "$VAMPIRE_DIR" ]; then
  echo "==> Cloning vampire ($VAMPIRE_REF)"
  git clone --quiet https://github.com/vprover/vampire.git "$VAMPIRE_DIR"
  git -C "$VAMPIRE_DIR" checkout --quiet "$VAMPIRE_REF"
else
  echo "==> Reusing existing checkout at $VAMPIRE_DIR"
  current_ref=$(git -C "$VAMPIRE_DIR" rev-parse HEAD)
  if [ "$current_ref" != "$VAMPIRE_REF" ]; then
    echo "error: '$VAMPIRE_DIR' is at $current_ref, expected pinned $VAMPIRE_REF." >&2
    echo "       VAMPIRE_RECLONE=1 to reclone." >&2
    exit 1
  fi
fi

echo "==> Copying smallWasmBuild.sh into the vampire tree"
cp -f "$GUIDE_DIR/smallWasmBuild.sh" "$VAMPIRE_DIR/"
chmod +x "$VAMPIRE_DIR/smallWasmBuild.sh"

echo "==> Patching CMakeLists.txt for Emscripten (GNU awk)"
PATH="${AWK_SHIM:+$AWK_SHIM:}$PATH" "$GUIDE_DIR/editCMakeLists.sh" "$VAMPIRE_DIR/CMakeLists.txt"

echo "==> Applying interactive stdin/stdout WASM patch"
"$GUIDE_DIR/patchVampireInteractive.sh" "$VAMPIRE_DIR"

echo "==> Building (emcmake + emmake — this is the slow step, several minutes)"
BUILD_DIR=${VAMPIRE_BUILD_DIR:-build-ems}
( cd "$VAMPIRE_DIR" && BUILD_DIR="$BUILD_DIR" ./smallWasmBuild.sh )

echo "==> Staging output into $OUT_DIR"
mkdir -p "$OUT_DIR"
cp "$VAMPIRE_DIR/$BUILD_DIR/vampire.js" "$VAMPIRE_DIR/$BUILD_DIR/vampire.wasm" "$OUT_DIR/"
cp "$GUIDE_DIR/vampire-runner.js" "$OUT_DIR/"

echo "$VAMPIRE_GUIDE_REF" > "$BUILT_STAMP"
echo "==> Done. $(du -h "$OUT_DIR/vampire.wasm" | cut -f1) vampire.wasm in $OUT_DIR"
