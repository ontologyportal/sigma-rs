#!/bin/bash
# .github/scripts/ci_regression.sh — the regression battery with every input
# remote.
#
# The CI twin of scripts/regression.sh (the gitignored local dev script),
# with nothing assumed on disk —
#
#   * the SUMO ontology is loaded through the CLI's own git feature
#     (`sumo -c --git <repo> load`, one sparse fetch for all constituents),
#   * each test case is fetched through the CLI's http feature
#     (`sumo test https://raw.githubusercontent.com/.../TQG1.kif.tq ...`),
#   * the TPTP smoke slice runs from a tree materialized by
#     .github/scripts/fetch_tptp.py (tptp.org has no raw endpoint; run
#     that first).
#
# Unlike regression.sh, nothing here is graded against expectations: every
# tests/**/*.kif.tq at the branch head runs as one pool per backend, the TPTP
# slice records each problem's SZS status, and all of it lands on the report
# page as plain results (the page diffs them against the previous published
# run).  The exit code only reflects infrastructure health — build, ontology
# load, test enumeration, missing TPTP files.
#
# Environment (all optional):
#   SUMO_GIT     ontology repo          (default https://github.com/ontologyportal/sumo)
#   SUMO_BRANCH  branch to fetch/test   (default master)
#   TPTP         TPTP root from fetch_tptp.py (default ~/TPTP)
#   TPTP_BUDGET  per-problem seconds    (default 10)
#   EXT_BUDGET   external-backend cap   (default 60; native keeps each
#                test's own `(time N)` directive)
#   OUT          results directory      (default ./regression-out)
#   SKIP_BUILD   nonempty = use existing target/release/sumo
#   GITHUB_TOKEN used for the GitHub API test listing when set (rate limits)
#
# Outputs under $OUT:
#   sumo.tsv             backend<TAB>passed<TAB>graded<TAB>false_verdicts
#   logs/<backend>.log   full per-test CLI output (--ugly)
#   tptp.tsv             name<TAB>szs
#   meta.tsv             key<TAB>value run metadata
#   status               GREEN or FAIL (infrastructure only)
#
# Exit code: 0 iff the run itself was healthy (results are never graded).

set -u
cd "$(dirname "$0")/../.."
REPO="$PWD"
SUMO_GIT="${SUMO_GIT:-https://github.com/ontologyportal/sumo}"
SUMO_BRANCH="${SUMO_BRANCH:-master}"
export TPTP="${TPTP:-$HOME/TPTP}"
TPTP_BUDGET="${TPTP_BUDGET:-10}"
EXT_BUDGET="${EXT_BUDGET:-60}"
OUT="${OUT:-$REPO/regression-out}"
BIN="$REPO/target/release/sumo"
FAIL=0

mkdir -p "$OUT/logs"
: > "$OUT/sumo.tsv"
: > "$OUT/tptp.tsv"
: > "$OUT/meta.tsv"

note() { printf '%s\n' "$*"; }
bad()  { printf 'FAIL  %s\n' "$*"; FAIL=1; }
meta() { printf '%s\t%s\n' "$1" "$2" >> "$OUT/meta.tsv"; }

# ---------------------------------------------------------------- build
if [ -z "${SKIP_BUILD:-}" ]; then
  note "== build =="
  cargo build --release --bin sumo || { bad "build"; exit 1; }
fi
[ -x "$BIN" ] || { bad "missing $BIN"; exit 1; }

# ------------------------------------------------ scratch config + load
# A self-contained SigmaKEE home: config.xml declares the canonical SUMO
# constituent set (the same 49 files the established baselines load), then
# one `-c --git load` sparse-fetches them all from $SUMO_GIT and compiles
# the LMDB store next to the config (editDir).
WORK="$OUT/work"
rm -rf "$WORK" && mkdir -p "$WORK"
CFG="$WORK/config.xml"
CONSTITUENTS="Merge.kif Mid-level-ontology.kif english_format.kif domainEnglishFormat.kif
ArabicCulture.kif Anatomy.kif arteries.kif Biography.kif Cars.kif Catalog.kif
Communications.kif ComputerInput.kif ComputingBrands.kif CountriesAndRegions.kif
Dining.kif Economy.kif emotion.kif engineering.kif Facebook.kif FinancialOntology.kif
Food.kif Geography.kif Government.kif Hotel.kif Justice.kif Languages.kif Law.kif
Media.kif Medicine.kif MilitaryDevices.kif Military.kif MilitaryPersons.kif
MilitaryProcesses.kif Music.kif development/Muscles.kif naics.kif People.kif
pictureList.kif pictureList-ImageNet.kif QoSontology.kif Sports.kif
TransnationalIssues.kif Transportation.kif TransportDetail.kif UXExperimentalTerms.kif
VirusProteinAndCellPart.kif Weather.kif WMD.kif capabilities.kif"

note "== load ontology ($SUMO_GIT @ $SUMO_BRANCH) =="
declare_args=()
for f in $CONSTITUENTS; do declare_args+=(-f "$f"); done
"$BIN" --ugly --config "$CFG" config --declare --kb SUMO "${declare_args[@]}" >/dev/null \
  || { bad "config --declare"; exit 1; }
"$BIN" --ugly --config "$CFG" config --edit-dir "$WORK" >/dev/null \
  || { bad "config --edit-dir"; exit 1; }
"$BIN" --ugly --config "$CFG" -c --git "$SUMO_GIT" --branch "$SUMO_BRANCH" load \
  2>&1 | tee "$OUT/logs/load.log" | tail -2
grep -q "load succeeded" "$OUT/logs/load.log" || { bad "ontology load"; exit 1; }

# ------------------------------------------------ enumerate remote tests
# One GitHub trees-API call lists tests/**/*.kif.tq at the branch head; each
# becomes a raw.githubusercontent.com URL for `sumo test` to fetch itself.
note ""
note "== enumerate tests ($SUMO_BRANCH) =="
api="${SUMO_GIT#https://github.com/}"; api="${api%.git}"
auth=(); [ -n "${GITHUB_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
# (`${auth[@]+...}` keeps bash 3.2's `set -u` happy when the array is empty.)
curl -fsSL ${auth[@]+"${auth[@]}"} \
  "https://api.github.com/repos/$api/git/trees/$SUMO_BRANCH?recursive=1" \
  > "$WORK/tree.json" || { bad "GitHub tree listing"; exit 1; }
python3 - "$WORK/tree.json" "$SUMO_GIT" "$SUMO_BRANCH" > "$WORK/tests.txt" <<'PY'
import json, sys
tree, repo, branch = sys.argv[1:4]
data = json.load(open(tree))
if data.get("truncated"):
    sys.exit("GitHub tree listing was truncated")
base = repo.removesuffix(".git").replace("https://github.com/",
                                         "https://raw.githubusercontent.com/")
for entry in data["tree"]:
    p = entry["path"]
    if p.startswith("tests/") and p.endswith(".kif.tq"):
        print(f"{base}/{branch}/{p}")
PY
[ -s "$WORK/tests.txt" ] || { bad "no tests found under tests/ at $SUMO_BRANCH"; exit 1; }
sumo_commit=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['sha'])" "$WORK/tree.json")
note "$(wc -l < "$WORK/tests.txt" | tr -d ' ') tests at $sumo_commit"

meta sigma_commit  "$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unknown)"
meta sumo_version  "$("$BIN" --version)"
meta sumo_repo     "$SUMO_GIT"
meta sumo_branch   "$SUMO_BRANCH"
meta sumo_commit   "$sumo_commit"
meta date_utc      "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
meta tptp_budget   "$TPTP_BUDGET"
meta ext_budget    "$EXT_BUDGET"

# ------------------------------------------- SUMO tests x backends
# The whole pool runs on every available backend, report-only.  External
# backends default to 300s/attempt — cap them so one unprovable test can't
# burn minutes; native keeps each test's own `(time N)` directive.
have_backend() {
  case "$1" in
    subprocess) command -v vampire >/dev/null ;;
    *)          true ;;
  esac
}

note ""
note "== SUMO tests =="
printf '%-11s %s\n' backend result
urls=$(cat "$WORK/tests.txt")
for backend in native subprocess embedded; do
  if ! have_backend "$backend"; then
    note "$backend: prover binary not on PATH — skipped"
    continue
  fi
  cap=""
  [ "$backend" != native ] && cap="--timeout $EXT_BUDGET"
  log="$OUT/logs/$backend.log"
  # shellcheck disable=SC2086
  "$BIN" --ugly --config "$CFG" test --backend "$backend" $cap $urls \
    > "$log" 2>&1
  got=$(grep -oE '[0-9]+ / [0-9]+ passed' "$log" | head -1)
  passed=${got%% *}
  graded=$(printf '%s' "$got" | awk '{print $3}')
  fv=$(grep -oE '[0-9]+ FALSE VERDICT' "$log" | head -1 | awk '{print $1}')
  if [ -z "$got" ]; then
    printf '%-11s %s\n' "$backend" "none"
    bad "$backend: no test summary (harness error — see logs/$backend.log)"
    continue
  fi
  printf '%-11s %s%s\n' "$backend" "$got" "${fv:+  ($fv false verdicts)}"
  printf '%s\t%s\t%s\t%s\n' "$backend" "$passed" "$graded" "${fv:-0}" >> "$OUT/sumo.tsv"
done

# ------------------------------------------- TPTP smoke (native CLI)
# Each listed problem's SZS status is recorded as-is — no grading; the
# report page shows the statuses and their drift from the previous run.
# The tree under $TPTP comes from .github/scripts/fetch_tptp.py.
LIST="$REPO/.github/scripts/tptp_regression.list"
note ""
if [ ! -d "$TPTP/Problems" ]; then
  note "== TPTP smoke skipped ($TPTP has no Problems/ — run .github/scripts/fetch_tptp.py) =="
  bad "TPTP tree missing at $TPTP"
else
  note "== TPTP smoke (native, ${TPTP_BUDGET}s each) =="
  [ -f "$LIST" ] || { bad "missing $LIST"; exit $((FAIL)); }
  while read -r entry; do
    [ -z "$entry" ] && continue; case "$entry" in \#*) continue ;; esac
    f="$TPTP/Problems/$entry.p"
    name="${entry##*/}"
    if [ ! -f "$f" ]; then bad "missing problem file $f"; continue; fi
    szs=$(timeout $((TPTP_BUDGET + 15)) "$BIN" --no-db test "$f" --timeout "$TPTP_BUDGET" 2>/dev/null \
          | grep -m1 -oE 'SZS status [A-Za-z]+' | awk '{print $3}')
    printf '  %-16s %s\n' "$name" "${szs:-none}"
    printf '%s\t%s\n' "$name" "${szs:-none}" >> "$OUT/tptp.tsv"
  done < "$LIST"
fi

note ""
if [ "$FAIL" = 0 ]; then
  note "== RUN OK =="; echo GREEN > "$OUT/status"
else
  note "== RUN FAILURES (infrastructure — see FAIL lines) =="; echo FAIL > "$OUT/status"
fi
meta result "$(cat "$OUT/status")"
exit "$FAIL"
