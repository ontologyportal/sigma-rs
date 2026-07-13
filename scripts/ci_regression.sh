#!/bin/bash
# scripts/ci_regression.sh — the regression battery with every input remote.
#
# The CI twin of scripts/regression.sh: same suites, same established
# expectations, but nothing is assumed on disk —
#
#   * the SUMO ontology is loaded through the CLI's own git feature
#     (`sumo -c --git <repo> load`, one sparse fetch for all constituents),
#   * each test case is fetched through the CLI's http feature
#     (`sumo test https://raw.githubusercontent.com/.../TQG1.kif.tq ...`),
#   * the TPTP smoke slice runs from a tree materialized by
#     scripts/fetch_tptp.py (tptp.org has no raw endpoint; run that first).
#
# Upstream's tests/ directory is flat, so suite membership (the local
# plain/typed/hard/higher split the expectations were gated on) comes from
# scripts/sumo_suites.tsv; upstream tests not in that map run as a
# report-only "new" suite.
#
# Environment (all optional):
#   SUMO_GIT     ontology repo          (default https://github.com/ontologyportal/sumo)
#   SUMO_BRANCH  branch to fetch/test   (default master)
#   TPTP         TPTP root from fetch_tptp.py (default ~/TPTP; smoke skipped if absent)
#   TPTP_BUDGET  per-problem seconds    (default 10)
#   EXT_BUDGET   external-backend cap   (default 60)
#   OUT          results directory      (default ./regression-out)
#   SKIP_BUILD   nonempty = use existing target/release/sumo
#   GITHUB_TOKEN used for the GitHub API test listing when set (rate limits)
#
# Outputs under $OUT:
#   suites.tsv           backend<TAB>suite<TAB>passed<TAB>graded<TAB>expected<TAB>status
#   logs/<backend>-<suite>.log   full per-test CLI output (--ugly)
#   tptp.tsv             name<TAB>szs<TAB>verdict (ok|SOUNDNESS|MUST_SOLVE|report)
#   meta.tsv             key<TAB>value run metadata
#   status               GREEN or FAIL
#
# Exit code: 0 iff every ESTABLISHED expectation holds (report-only cells
# never fail the run) — same contract as regression.sh.

set -u
cd "$(dirname "$0")/.."
REPO="$PWD"
SUMO_GIT="${SUMO_GIT:-https://github.com/ontologyportal/sumo}"
SUMO_BRANCH="${SUMO_BRANCH:-master}"
export TPTP="${TPTP:-$HOME/TPTP}"
TPTP_BUDGET="${TPTP_BUDGET:-10}"
EXT_BUDGET="${EXT_BUDGET:-60}"
OUT="${OUT:-$REPO/regression-out}"
BIN="$REPO/target/release/sumo"
SUITE_MAP="$REPO/scripts/sumo_suites.tsv"
FAIL=0

mkdir -p "$OUT/logs"
: > "$OUT/suites.tsv"
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
python3 - "$WORK/tree.json" "$SUITE_MAP" "$SUMO_GIT" "$SUMO_BRANCH" > "$WORK/tests.tsv" <<'PY'
import json, sys
tree, suite_map, repo, branch = sys.argv[1:5]
suites = {}
for line in open(suite_map):
    line = line.strip()
    if line and not line.startswith("#"):
        name, suite = line.split("\t")
        suites[name] = suite
data = json.load(open(tree))
if data.get("truncated"):
    sys.exit("GitHub tree listing was truncated")
print(f"# repo commit {data['sha']}", file=sys.stderr)
base = repo.removesuffix(".git").replace("https://github.com/",
                                         "https://raw.githubusercontent.com/")
for entry in data["tree"]:
    p = entry["path"]
    if p.startswith("tests/") and p.endswith(".kif.tq"):
        name = p.rsplit("/", 1)[1].removesuffix(".kif.tq")
        print(f"{name}\t{suites.get(name, 'new')}\t{base}/{branch}/{p}")
PY
[ -s "$WORK/tests.tsv" ] || { bad "no tests found under tests/ at $SUMO_BRANCH"; exit 1; }
sumo_commit=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['sha'])" "$WORK/tree.json")
note "$(wc -l < "$WORK/tests.tsv" | tr -d ' ') tests at $sumo_commit"

meta sigma_commit  "$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unknown)"
meta sumo_version  "$("$BIN" --version)"
meta sumo_repo     "$SUMO_GIT"
meta sumo_branch   "$SUMO_BRANCH"
meta sumo_commit   "$sumo_commit"
meta date_utc      "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
meta tptp_budget   "$TPTP_BUDGET"
meta ext_budget    "$EXT_BUDGET"

# ------------------------------------------- SUMO suites x backends
# Established expectations (empty = report-only) — regression.sh's table,
# with one delta: higher is 7 here vs. 8 locally, because the local baseline
# includes uncommitted ontology edits (Mid-level-ontology.kif et al.) that
# upstream master doesn't have yet (TQC2/TQM3/TQM10 are the affected tests;
# verified 2026-07-12).  Bump to 8 when those edits land upstream.
expect() { # expect <backend> <suite> -> count or ""
  case "$1/$2" in
    native/plain) echo 39 ;;  native/typed) echo 7 ;;  native/hard) echo 5 ;;
    native/higher) echo 7 ;;
    subprocess/plain|embedded/plain) echo 39 ;;
    subprocess/typed|embedded/typed) echo 7 ;;
    *) echo "" ;;
  esac
}
have_backend() {
  case "$1" in
    subprocess) command -v vampire >/dev/null ;;
    *)          true ;;
  esac
}
suite_urls() { # suite -> raw URLs on stdout
  awk -F'\t' -v s="$1" '$2 == s { print $3 }' "$WORK/tests.tsv"
}

note ""
note "== SUMO suites =="
printf '%-11s %-7s %-9s %s\n' backend suite result expectation
for backend in native subprocess embedded; do
  if ! have_backend "$backend"; then
    note "$backend: prover binary not on PATH — skipped"
    continue
  fi
  suites="plain typed hard"
  # higher is established on native only; broken and upstream-new tests are
  # report-only and run once, on native, with a hard cap.
  [ "$backend" = native ] && suites="$suites higher broken new"
  for suite in $suites; do
    urls=$(suite_urls "$suite")
    [ -z "$urls" ] && continue
    # External backends default to 300s/attempt — cap them.  Native keeps
    # each test's own (time N) directive, except the ungated broken/new
    # suites, which get a cap so one divergent test can't burn minutes.
    cap=""
    [ "$backend" != native ] && cap="--timeout $EXT_BUDGET"
    case "$suite" in broken|new) cap="--timeout 30" ;; esac
    # The typed suite runs under --lang tff on the translation backends:
    # its arithmetic needs TPTP theory semantics, which the FOF default
    # deliberately hides.  Native needs no flag.
    lang=""
    [ "$backend" != native ] && [ "$suite" = typed ] && lang="--lang tff"
    log="$OUT/logs/$backend-$suite.log"
    # shellcheck disable=SC2086
    "$BIN" --ugly --config "$CFG" test --backend "$backend" $cap $lang $urls \
      > "$log" 2>&1
    got=$(grep -oE '[0-9]+ / [0-9]+ passed' "$log" | head -1)
    passed=${got%% *}
    graded=$(printf '%s' "$got" | awk '{print $3}')
    want=$(expect "$backend" "$suite")
    if [ -n "$want" ]; then
      if [ "${passed:-x}" = "$want" ]; then
        printf '%-11s %-7s %-9s %s\n' "$backend" "$suite" "${got:-none}" "ok"
        status=ok
      else
        printf '%-11s %-7s %-9s %s\n' "$backend" "$suite" "${got:-none}" "EXPECTED $want"
        bad "$backend/$suite: got '${got:-none}', expected $want passed"
        status=FAIL
      fi
    else
      printf '%-11s %-7s %-9s %s\n' "$backend" "$suite" "${got:-none}" "(report-only)"
      status=report
    fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$backend" "$suite" "${passed:-}" "${graded:-}" "${want:-}" "$status" >> "$OUT/suites.tsv"
  done
done

# ------------------------------------------- TPTP smoke (native CLI)
# Same list, gates, and semantics as regression.sh: any SAT/CSA verdict on
# this Theorem-rated slice is a soundness failure; MUST_SOLVE must stay
# solved at the default budget.  The tree under $TPTP comes from
# scripts/fetch_tptp.py.
LIST="$REPO/scripts/tptp_regression.list"
MUST_SOLVE="GRP001+6 RNG047+2 RNG050+1 PUZ001+1"
note ""
if [ ! -d "$TPTP/Problems" ]; then
  note "== TPTP smoke skipped ($TPTP has no Problems/ — run scripts/fetch_tptp.py) =="
  bad "TPTP tree missing at $TPTP"
else
  note "== TPTP smoke (native, ${TPTP_BUDGET}s each) =="
  [ -f "$LIST" ] || { bad "missing $LIST"; exit $((FAIL)); }
  declare -a RESULTS=()
  while read -r entry; do
    [ -z "$entry" ] && continue; case "$entry" in \#*) continue ;; esac
    f="$TPTP/Problems/$entry.p"
    name="${entry##*/}"
    if [ ! -f "$f" ]; then bad "missing problem file $f"; continue; fi
    szs=$(timeout $((TPTP_BUDGET + 15)) "$BIN" --no-db test "$f" --timeout "$TPTP_BUDGET" 2>/dev/null \
          | grep -m1 -oE 'SZS status [A-Za-z]+' | awk '{print $3}')
    RESULTS+=("$name ${szs:-none}")
    printf '  %-16s %s\n' "$name" "${szs:-none}"
  done < "$LIST"

  for r in "${RESULTS[@]}"; do
    name=${r%% *}; szs=${r##* }
    verdict=report
    case "$szs" in
      CounterSatisfiable|Satisfiable)
        bad "SOUNDNESS: $name returned $szs on a Theorem-rated problem"
        verdict=SOUNDNESS ;;
      Theorem|Unsatisfiable) verdict=ok ;;
    esac
    for m in $MUST_SOLVE; do
      if [ "$name" = "$m" ] && [ "$verdict" = report ]; then
        bad "must-solve $m came back ${szs} (expected solved at ${TPTP_BUDGET}s)"
        verdict=MUST_SOLVE
      fi
    done
    printf '%s\t%s\t%s\n' "$name" "$szs" "$verdict" >> "$OUT/tptp.tsv"
  done
  for m in $MUST_SOLVE; do
    grep -q "^$m	" "$OUT/tptp.tsv" || bad "must-solve $m not present in list/results"
  done
fi

note ""
if [ "$FAIL" = 0 ]; then
  note "== REGRESSION: ALL GREEN =="; echo GREEN > "$OUT/status"
else
  note "== REGRESSION: FAILURES (see FAIL lines) =="; echo FAIL > "$OUT/status"
fi
meta result "$(cat "$OUT/status")"
exit "$FAIL"
