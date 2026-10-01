#!/usr/bin/env bash
# Build E and e_axfilter for WASM (default) or native (--native OUTPUT_DIR).
# Requires an activated emsdk, git, GNU make, Python 3, and tar.
# Output: dist/{eprover,e_axfilter}.{js,wasm}, plus upstream COPYING.
set -euo pipefail

if [ "${SKIP_EPROVER:-0}" = "1" ]; then
  echo "==> SKIP_EPROVER=1: not building E"
  exit 0
fi

PKG_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE_DIR="$PKG_DIR/.eprover-cache"
SOURCE_DIR="$CACHE_DIR/source"
MODE=wasm
OUT_DIR="$PKG_DIR/dist"
if [ "${1:-}" = "--native" ] && [ "$#" = 2 ]; then
  MODE=native
  mkdir -p "$2"
  OUT_DIR="$(cd "$2" && pwd)"
elif [ "$#" != 0 ]; then
  echo "usage: build.sh [--native OUTPUT_DIR]" >&2
  exit 1
fi
if [ "$MODE" = native ]; then
  COMPILER=cc
  TOOLS=(cc ar ranlib)
  OUTPUTS=(eprover e_axfilter eprover-COPYING)
else
  COMPILER=emcc
  TOOLS=(emcc emar emranlib)
  OUTPUTS=(eprover.js eprover.wasm e_axfilter.js e_axfilter.wasm COPYING runner.mjs)
fi
EPROVER_REF=292e0a8336654931d9a3746dc1353bc0dde970d4
JOBS=${EPROVER_JOBS:-4}

[[ "$JOBS" =~ ^[1-9][0-9]*$ ]] || { echo "error: EPROVER_JOBS must be a positive integer" >&2; exit 1; }
for tool in "${TOOLS[@]}" git make python3 tar cksum; do
  command -v "$tool" >/dev/null || { echo "error: $tool not found; install the $MODE build prerequisites" >&2; exit 1; }
done

BUILD_KEY="$( { echo "$EPROVER_REF $MODE"; uname -sm; "$COMPILER" --version; cat "$PKG_DIR/build.sh" "$PKG_DIR/runner.mjs"; } | cksum)"
STAMP="$OUT_DIR/.eprover-built-key"
complete=true
for output in "${OUTPUTS[@]}"; do
  [ -s "$OUT_DIR/$output" ] || complete=false
done
if [ "${EPROVER_REBUILD:-0}" != "1" ] && $complete &&
   [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$BUILD_KEY" ]; then
  echo "==> E $MODE already built at $EPROVER_REF (EPROVER_REBUILD=1 to rebuild)"
  exit 0
fi

mkdir -p "$CACHE_DIR"
if [ ! -d "$SOURCE_DIR/.git" ]; then
  git clone --no-checkout https://github.com/eprover/eprover.git "$SOURCE_DIR"
fi
if ! git -C "$SOURCE_DIR" cat-file -e "$EPROVER_REF^{commit}" 2>/dev/null; then
  git -C "$SOURCE_DIR" fetch origin "$EPROVER_REF"
fi

BUILD_DIR="$(mktemp -d "$CACHE_DIR/build.XXXXXX")"
trap 'rm -rf -- "$BUILD_DIR"' EXIT
git -C "$SOURCE_DIR" archive "$EPROVER_REF" | tar -x -C "$BUILD_DIR"
cd "$BUILD_DIR"

# PicoSAT hardcodes native archive tools; use the Emscripten equivalents.
python3 - "$MODE" <<'PY'
from pathlib import Path
import sys
if sys.argv[1] == "wasm":
    p = Path("CONTRIB/picosat-965/makefile.in")
    text = p.read_text()
    assert "\tar rc " in text and "\tranlib " in text
    p.write_text(text.replace("\tar rc ", "\temar rc ").replace("\tranlib ", "\temranlib "))
    # E's generated strategies contain host-sized LONG_MAX literals.
    p = Path("HEURISTICS/schedule.vars")
    text = p.read_text()
    assert "9223372036854775807" in text
    p.write_text(text.replace("9223372036854775807", "2147483647"))
    p = Path("PROVER/eprover.c")
    text = p.read_text()
    assert "LLONG_MAX, LONG_MAX);" in text
    p.write_text(text.replace("LLONG_MAX, LONG_MAX);", "LONG_MAX, LONG_MAX);"))
# Sigma selects premises; single-strategy auto mode must not filter them again.
p = Path("PROVER/eprover.c")
text = p.read_text()
auto_sine = '               h_parms->sine = "Auto";\n               auto_conf = true;'
assert auto_sine in text
p.write_text(text.replace(auto_sine, '               auto_conf = true;'))
# The pinned upstream printer emits debug S-expressions inside negated TPTP.
p = Path("CLAUSES/ccl_tformulae.c")
text = p.read_text()
debug = '      printf("#### ");\n      TermPrintSExpr(out, form, bank->sig);\n      printf("\\n");\n'
assert debug in text
p.write_text(text.replace(debug, ""))
# Symbol codes start at 1; entry 0 is uninitialized.
p = Path("TERMS/cte_signature.c")
text = p.read_text()
assert "for(i=0; i< junk->f_count; i++)" in text
p.write_text(text.replace("for(i=0; i< junk->f_count; i++)",
                        "for(i=1; i<= junk->f_count; i++)"))
PY

make links
for part in BASICS INOUT TERMS ORDERINGS CLAUSES PROPOSITIONAL LEARN PCL2 HEURISTICS CONTROL PROVER; do
  touch "$part/Makefile.dependencies"
done
(
  cd CONTRIB/picosat-965
  CC="$COMPILER" CFLAGS="-O3 -DNDEBUG -DTRACE -DNGETRUSAGE -DNALLSIGNALS" ./configure.sh -t
  make -j "$JOBS" libpicosat.a
)
ln -s picosat-965/libpicosat.a CONTRIB/CONTRIB.a

# Preserve E's domain flags, but omit native stack-limit manipulation.
ARCHIVER=ar
[ "$MODE" = native ] || ARCHIVER=emar
MAKE_ARGS=("CC=$COMPILER" "AR=$ARCHIVER rcs" "OPTFLAGS=-O3"
  "BUILDFLAGS=-DCLAUSE_PERM_IDENT -DPRINT_TSTP_STATUS -DUSE_SYSTEM_MEM"
  "NODEBUG=-DNDEBUG" "LTOFLAGS=")
for part in BASICS INOUT TERMS ORDERINGS CLAUSES PROPOSITIONAL LEARN PCL2 HEURISTICS CONTROL; do
  make -C "$part" -j "$JOBS" "${MAKE_ARGS[@]}"
done
make -C PROVER -j "$JOBS" "${MAKE_ARGS[@]}" eprover.o e_axfilter.o

mkdir -p output
LINK_FLAGS=(-O3 -sMODULARIZE=1 -sEXPORT_ES6=1 -sENVIRONMENT=web,worker,node
  -sALLOW_MEMORY_GROWTH=1 -sSTACK_SIZE=8388608 -sFORCE_FILESYSTEM=1
  -sEXPORTED_RUNTIME_METHODS=FS,callMain -sEXIT_RUNTIME=1
  -sINCOMING_MODULE_JS_API=wasmBinary,noInitialRun,print,printErr,onExit,locateFile,arguments,thisProgram)
LIBRARIES=(lib/CONTROL.a lib/HEURISTICS.a lib/LEARN.a lib/CLAUSES.a
  lib/ORDERINGS.a lib/TERMS.a lib/INOUT.a lib/BASICS.a lib/CONTRIB.a)
for program in eprover e_axfilter; do
  if [ "$MODE" = native ]; then
    cc -O3 "PROVER/$program.o" "${LIBRARIES[@]}" -lm -o "output/$program"
  else
    emcc "${LINK_FLAGS[@]}" "PROVER/$program.o" "${LIBRARIES[@]}" -lm -o "output/$program.js"
  fi
done
if [ "$MODE" = native ]; then
  cp COPYING output/eprover-COPYING
else
  cp COPYING output/COPYING
  cp "$PKG_DIR/runner.mjs" output/runner.mjs
fi
mkdir -p "$OUT_DIR"
cp output/* "$OUT_DIR/"
echo "$BUILD_KEY" > "$STAMP"
echo "==> Built eprover and e_axfilter in $OUT_DIR"
