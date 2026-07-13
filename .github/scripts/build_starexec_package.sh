#!/usr/bin/env bash
# Package the `sumo` prover as a StarExec-compatible solver archive.
#
# Follows the layout StarExec's solver upload expects (see
# https://starexec.acorn.miami.edu/starexec/secure/add/solver.help):
#
#   <archive>/
#     starexec_description.txt   (optional; solver description)
#     bin/
#       starexec_run_default      (required; one starexec_run_<config> per config)
#       sumo                      (the release binary the run script execs)
#
# StarExec extracts the archive, cds into bin/, and on each job runs
# `bin/starexec_run_<config> <benchmark-file>` with STAREXEC_WALLCLOCK_LIMIT
# (and friends) set in the environment. Archive format is tar.gz.
#
# StarExec's execution nodes are Linux x86_64. Run this script on (or
# cross-compile for) that target -- a macOS/arm64 binary will not run there.
#
# The `sumo casc` codepath this package runs (see starexec_run_default
# below) never opens a database, so `integrated-prover` (embedded Vampire
# C++ via CMake) is left out of the default feature list -- it's a heavy
# build-time-only dependency this package gets no runtime benefit from.
# `persist` (LMDB via heed/bincode) stays in: it's cheap and untangling it
# from the CLI's shared dispatch path (crates/cli/src/main.rs) isn't worth
# it for a build that just never exercises that code.
#
# LMDB (persist, always) still links C, so cross-compiling from macOS needs
# a real cross toolchain, not just `rustup target add`. When --target names
# a non-host triple and `cross` (https://github.com/cross-rs/cross, Docker
# required: `cargo install cross --git https://github.com/cross-rs/cross
# --locked`) is on PATH, this script builds with it automatically instead
# of plain cargo.
#
# Usage:
#   .github/scripts/build_starexec_package.sh [--target TRIPLE] [--features LIST] [--out PATH]

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

target=""
features="ask,parallel,alloc-mi"
out="$repo_root/target/starexec/sigma-rs-starexec.tar.gz"

while [ $# -gt 0 ]; do
    case "$1" in
        --target)   target="$2"; shift 2 ;;
        --features) features="$2"; shift 2 ;;
        --out)      out="$2"; shift 2 ;;
        -h|--help)
            sed -n '2,36p' "$0"
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

host_triple="$(rustc -vV | sed -n 's/^host: //p')"

builder="cargo"
build_args=(build --release -p sigmakee --bin sumo --no-default-features --features "$features")
bin_subdir="release"
if [ -n "$target" ]; then
    build_args+=(--target "$target")
    bin_subdir="$target/release"
    if [ "$target" != "$host_triple" ]; then
        if command -v cross >/dev/null 2>&1; then
            builder="cross"
        else
            echo "warning: cross-compiling to '$target' from host '$host_triple' with plain" >&2
            echo "         cargo -- this will likely fail to link the crate's C deps (LMDB," >&2
            echo "         via the 'persist' feature; add integrated-prover and it also" >&2
            echo "         needs Vampire's CMake build)." >&2
            echo "         Install 'cross' for a working cross toolchain:" >&2
            echo "           cargo install cross --git https://github.com/cross-rs/cross --locked" >&2
        fi
    fi
fi

echo "==> $builder ${build_args[*]}"
"$builder" "${build_args[@]}"

built_bin="$repo_root/target/$bin_subdir/sumo"
if [ ! -x "$built_bin" ]; then
    echo "error: expected binary not found at $built_bin" >&2
    exit 1
fi

package_triple="${target:-$host_triple}"
case "$package_triple" in
    *linux*) : ;;
    *)
        echo "warning: packaging a '$package_triple' binary -- StarExec's execution" >&2
        echo "         nodes are Linux x86_64 and will not run this. Re-run with" >&2
        echo "         --target x86_64-unknown-linux-gnu (or -musl) from a Linux" >&2
        echo "         host / cross toolchain." >&2
        ;;
esac

stage="$repo_root/target/starexec/stage"
rm -rf "$stage"
mkdir -p "$stage/bin"

cp "$built_bin" "$stage/bin/sumo"
chmod +x "$stage/bin/sumo"

cat > "$stage/bin/starexec_run_default" <<'EOF'
#!/bin/sh
# StarExec invokes this as `starexec_run_default <benchmark-file>` with cwd
# set to this bin/ directory. STAREXEC_WALLCLOCK_LIMIT (seconds) is set by
# the job's configured wallclock limit; fall back to 300s if absent (e.g.
# manual invocation outside StarExec).
set -e
here="$(cd "$(dirname "$0")" && pwd)"
bench="$1"

# `sumo casc` classifies a single-file argument as one problem only when its
# name ends in `.p`/`.tptp` -- anything else it treats as a *list file* of
# one-path-per-line problems. StarExec benchmark filenames aren't guaranteed
# to carry either extension, so force the classification via a `.p`-suffixed
# symlink rather than trusting the name StarExec handed us.
work="$(mktemp -d "${TMPDIR:-/tmp}/starexec-sumo.XXXXXX")"
trap 'rm -rf "$work"' EXIT
bench_dir="$(cd "$(dirname "$bench")" && pwd)"
ln -s "$bench_dir/$(basename "$bench")" "$work/problem.p"

wall="${STAREXEC_WALLCLOCK_LIMIT:-300}"
# Shave a margin off the wallclock limit so `sumo` can finish printing its
# SZS status line before StarExec's own harness SIGKILLs the process at the
# limit -- otherwise a run that legitimately uses the full budget risks
# losing its result to the race.
if [ "$wall" -gt 10 ]; then timeout=$((wall - 5)); else timeout="$wall"; fi

"$here/sumo" casc "$work/problem.p" --timeout "$timeout" --jobs 1
EOF
chmod +x "$stage/bin/starexec_run_default"

cat > "$stage/starexec_description.txt" <<'EOF'
sigma-rs (sumo) -- native Rust theorem prover for SUO-KIF/TPTP problems,
part of the SigmaKEE-rs project (https://github.com/ontologyportal/sigma-rs).
Each problem is proved on a fresh, self-contained knowledge base (full
saturation + a budget-adaptive strategy portfolio) and reports SZS status.
EOF

mkdir -p "$(dirname "$out")"
tar -czf "$out" -C "$stage" .

echo "==> wrote $out"
tar -tzf "$out"
