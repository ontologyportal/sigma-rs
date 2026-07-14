#!/usr/bin/env python3
"""Render .github/scripts/ci_regression.sh results as a static Pages site.

Reads the structured output directory ci_regression.sh produced (meta.tsv,
sumo.tsv, tptp.tsv, logs/*.log) and writes:

    <out>/index.html    the report page (self-contained, no external assets)
    <out>/results.json  this run's full parsed results (also next run's diff base)
    <out>/history.json  compact per-run summaries, newest first
    <out>/badge.json    a shields.io endpoint (https://img.shields.io/endpoint)

Nothing is graded: the page prints each test's outcome as reported by the
CLI, and — when the previous deploy's results.json / history.json are
supplied — shows what changed since the last run and a history table.  The
workflow fetches those two files from the live Pages site before building,
so history accumulates across deploys with no storage beyond the site
itself.

Usage: regression_report.py --results regression-out --out site
           [--run-url URL] [--prev results.json] [--history history.json]
"""

import argparse
import html
import json
import re
import sys
from pathlib import Path

HISTORY_LIMIT = 300  # runs kept in history.json

VERDICT_RE = re.compile(
    r"^  (PASSED|FAILED|FALSE VERDICT|INFO|ERROR)\s*(?:\(total (\d+\.\d+)s\))?")
SZS_RE = re.compile(r"^  % SZS status (\S+) for (\S+)")


def read_tsv(path: Path):
    if not path.exists():
        return []
    rows = []
    for line in path.read_text().splitlines():
        if line.strip():
            rows.append(line.split("\t"))
    return rows


def parse_suite_log(path: Path):
    """Per-test cases from one `sumo test --ugly` transcript."""
    cases, case = [], None
    for line in path.read_text(errors="replace").splitlines():
        if line.startswith("Running test: "):
            case = {"label": line[len("Running test: "):].strip(),
                    "verdict": "ERROR", "secs": None, "szs": None, "detail": []}
            name = case["label"].rsplit("/", 1)[-1]
            case["name"] = re.sub(r"\.(kif\.tq|p|tptp)$", "", name)
            cases.append(case)
            continue
        if case is None:
            continue
        m = VERDICT_RE.match(line)
        if m:
            case["verdict"] = m.group(1)
            case["secs"] = float(m.group(2)) if m.group(2) else None
            continue
        m = SZS_RE.match(line)
        if m:
            case["szs"] = m.group(1)
            continue
        if line.startswith("    ") and line.strip():
            case["detail"].append(line.strip())
    return cases


def collect(results: Path):
    meta = {k: v for k, v in read_tsv(results / "meta.tsv")}
    status = (results / "status").read_text().strip() \
        if (results / "status").exists() else "FAIL"

    backends = []
    for backend, passed, graded, fv in read_tsv(results / "sumo.tsv"):
        log = results / "logs" / f"{backend}.log"
        backends.append({
            "backend": backend,
            "passed": int(passed) if passed else None,
            "graded": int(graded) if graded else None,
            "false_verdicts": int(fv) if fv else 0,
            "cases": parse_suite_log(log) if log.exists() else [],
        })

    tptp = [{"name": r[0], "szs": r[1]} for r in read_tsv(results / "tptp.tsv")]
    return {"meta": meta, "status": status, "backends": backends, "tptp": tptp}


# ------------------------------------------------------------------ diffs

def diff_runs(cur: dict, prev: dict | None):
    """Per-test changes between this run and the previous one.

    Returns {"backends": {name: {"flips": [(test, old, new)], "added": [...],
    "removed": [...]}}, "tptp": [(name, old, new)]} — or None when there is
    no comparable previous data.
    """
    if not prev or "backends" not in prev:
        return None
    prev_backends = {b["backend"]: {c["name"]: c["verdict"] for c in b.get("cases", [])}
                     for b in prev["backends"]}
    out = {"backends": {}, "tptp": []}
    for b in cur["backends"]:
        old = prev_backends.get(b["backend"])
        if old is None:
            continue
        new = {c["name"]: c["verdict"] for c in b["cases"]}
        out["backends"][b["backend"]] = {
            "flips":   [(n, old[n], v) for n, v in sorted(new.items())
                        if n in old and old[n] != v],
            "added":   sorted(set(new) - set(old)),
            "removed": sorted(set(old) - set(new)),
        }
    old_tptp = {t["name"]: t["szs"] for t in prev.get("tptp", [])}
    out["tptp"] = [(t["name"], old_tptp[t["name"]], t["szs"]) for t in cur["tptp"]
                   if t["name"] in old_tptp and old_tptp[t["name"]] != t["szs"]]
    return out


def summarize(data: dict) -> dict:
    """The compact per-run record appended to history.json."""
    solved = sum(t["szs"] in ("Theorem", "Unsatisfiable") for t in data["tptp"])
    return {
        "date": data["meta"].get("date_utc", ""),
        "sigma_commit": data["meta"].get("sigma_commit", "")[:12],
        "sumo_commit": data["meta"].get("sumo_commit", "")[:12],
        "backends": {b["backend"]: {"passed": b["passed"], "graded": b["graded"],
                                    "false_verdicts": b["false_verdicts"]}
                     for b in data["backends"]},
        "tptp": {"solved": solved, "total": len(data["tptp"])},
    }


# ---------------------------------------------------------------- HTML

CSS = """
:root {
  --bg: #f6f7f9; --card: #ffffff; --ink: #1a2027; --muted: #67707c;
  --line: #e3e6ea; --green: #1a7f37; --green-bg: #e6f4ea; --red: #c62828;
  --red-bg: #fdecea; --amber: #9a6700; --amber-bg: #fff3d6; --gray: #57606a;
  --gray-bg: #eef1f4; --link: #0b62c4; --mono: ui-monospace, SFMono-Regular,
  Menlo, Consolas, monospace;
}
@media (prefers-color-scheme: dark) { :root {
  --bg: #14181d; --card: #1c2229; --ink: #e8ecf1; --muted: #98a2ad;
  --line: #2c343d; --green: #4ecb71; --green-bg: #16301f; --red: #ff7369;
  --red-bg: #3a1f1d; --amber: #e3b341; --amber-bg: #35301a; --gray: #9aa4af;
  --gray-bg: #262d35; --link: #58a6ff;
}}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--ink);
  font: 15px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; }
a { color: var(--link); text-decoration: none; }
a:hover { text-decoration: underline; }
main { max-width: 980px; margin: 0 auto; padding: 2rem 1.25rem 4rem; }
h1 { font-size: 1.5rem; margin: 0; }
h2 { font-size: 1.1rem; margin: 2.2rem 0 .8rem; }
.banner { display: flex; flex-wrap: wrap; align-items: center; gap: .8rem;
  margin: 1.2rem 0 .4rem; }
.state { font-weight: 700; padding: .35rem .9rem; border-radius: 999px; }
.state.fail { color: var(--red); background: var(--red-bg); }
.meta { color: var(--muted); font-size: .86rem; }
.meta code, td code, .mono { font-family: var(--mono); font-size: .86em; }
.card { background: var(--card); border: 1px solid var(--line);
  border-radius: 10px; padding: .4rem 1rem; overflow-x: auto; }
table { border-collapse: collapse; width: 100%; }
th, td { text-align: left; padding: .45rem .7rem; white-space: nowrap; }
th { color: var(--muted); font-size: .78rem; text-transform: uppercase;
  letter-spacing: .04em; border-bottom: 1px solid var(--line); }
tr + tr td { border-top: 1px solid var(--line); }
td.detail { white-space: normal; color: var(--muted); font-size: .86rem; }
.pill { display: inline-block; padding: .1rem .55rem; border-radius: 999px;
  font-size: .8rem; font-weight: 600; }
.pill.ok    { color: var(--green); background: var(--green-bg); }
.pill.fail  { color: var(--red);   background: var(--red-bg); }
.pill.warn  { color: var(--amber); background: var(--amber-bg); }
.pill.info  { color: var(--gray);  background: var(--gray-bg); }
.delta-up   { color: var(--green); }
.delta-down { color: var(--red); }
details { margin: .7rem 0; }
summary { cursor: pointer; padding: .55rem .9rem; background: var(--card);
  border: 1px solid var(--line); border-radius: 10px; font-weight: 600; }
details[open] summary { border-radius: 10px 10px 0 0; }
details .card { border-top: none; border-radius: 0 0 10px 10px; }
footer { margin-top: 3rem; color: var(--muted); font-size: .82rem; }
"""

PILL = {
    "PASSED": ("ok", "passed"), "FAILED": ("fail", "failed"),
    "FALSE VERDICT": ("fail", "FALSE VERDICT"), "INFO": ("info", "info"),
    "ERROR": ("warn", "error"),
}


def pill(key: str) -> str:
    cls, label = PILL.get(key, ("info", key))
    return f'<span class="pill {cls}">{html.escape(label)}</span>'


def esc(s) -> str:
    return html.escape(str(s))


def render(data: dict, deltas: dict | None, history: list, run_url: str | None) -> str:
    meta, out = data["meta"], []
    w = out.append
    date = meta.get("date_utc", "")
    sigma = meta.get("sigma_commit", "")
    sumo_repo = meta.get("sumo_repo", "https://github.com/ontologyportal/sumo")
    sumo_commit = meta.get("sumo_commit", "")
    raw_base = sumo_repo.removesuffix(".git")

    w("<main>")
    w("<h1>sigma-rs regression</h1>")
    w('<div class="banner">')
    if data["status"] != "GREEN":
        w('<span class="state fail">RUN INCOMPLETE</span>')
    w(f'<span class="meta">{esc(date)}</span>')
    w("</div>")
    w('<p class="meta">')
    w(f'{esc(meta.get("sumo_version", ""))}<br>')
    w(f'sigma-rs <a href="https://github.com/ontologyportal/sigma-rs/commit/{esc(sigma)}">'
      f'<code>{esc(sigma[:12])}</code></a> &nbsp;·&nbsp; ')
    w(f'ontology <a href="{esc(raw_base)}/commit/{esc(sumo_commit)}">'
      f'<code>{esc(sumo_commit[:12])}</code></a> '
      f'(<a href="{esc(raw_base)}">{esc(raw_base.split("github.com/")[-1])}</a> '
      f'@ {esc(meta.get("sumo_branch", ""))}, loaded via the CLI git feature; '
      f'tests fetched per-case over raw HTTP)')
    if run_url:
        w(f' &nbsp;·&nbsp; <a href="{esc(run_url)}">workflow run</a>')
    w("</p>")

    # -- per-backend summary -------------------------------------------
    w("<h2>SUMO tests</h2>")
    w('<div class="card"><table>')
    w("<tr><th>backend</th><th>result</th><th>false verdicts</th></tr>")
    for s in data["backends"]:
        res = "—" if s["passed"] is None else f'{s["passed"]} / {s["graded"]} passed'
        fv = s["false_verdicts"]
        fv_cell = pill("FALSE VERDICT") + f" × {fv}" if fv else "0"
        w(f'<tr><td>{esc(s["backend"])}</td><td>{esc(res)}</td>'
          f'<td>{fv_cell}</td></tr>')
    w("</table></div>")

    # -- changes since the previous run ---------------------------------
    w("<h2>Changes since previous run</h2>")
    if deltas is None:
        w('<p class="meta">No previous run data to compare against.</p>')
    else:
        prev_date = history[1]["date"] if len(history) > 1 else ""
        rows = []
        for backend, d in deltas["backends"].items():
            for name, old, new in d["flips"]:
                rows.append((backend, name, pill(old) + " → " + pill(new)))
            if d["added"]:
                rows.append((backend, f'{len(d["added"])} new test(s)',
                             esc(", ".join(d["added"][:8])
                                 + ("…" if len(d["added"]) > 8 else ""))))
            if d["removed"]:
                rows.append((backend, f'{len(d["removed"])} removed test(s)',
                             esc(", ".join(d["removed"][:8])
                                 + ("…" if len(d["removed"]) > 8 else ""))))
        for name, old, new in deltas["tptp"]:
            rows.append(("tptp", name,
                         f"<code>{esc(old)}</code> → <code>{esc(new)}</code>"))
        if not rows:
            w(f'<p class="meta">No changes since {esc(prev_date)}.</p>')
        else:
            w(f'<p class="meta">Compared against {esc(prev_date)}.</p>')
            w('<div class="card"><table>')
            w("<tr><th>where</th><th>test</th><th>change</th></tr>")
            for where, name, change in rows:
                w(f"<tr><td>{esc(where)}</td><td>{esc(name)}</td>"
                  f"<td>{change}</td></tr>")
            w("</table></div>")

    # -- per-backend detail ---------------------------------------------
    w("<h2>Per-test detail</h2>")
    for s in data["backends"]:
        if not s["cases"]:
            continue
        res = "" if s["passed"] is None else f' — {s["passed"]} / {s["graded"]}'
        w(f'<details><summary>{esc(s["backend"])}{esc(res)}</summary>')
        w('<div class="card"><table>')
        w("<tr><th>test</th><th>verdict</th><th>time</th><th>SZS</th><th>notes</th></tr>")
        for c in s["cases"]:
            secs = "" if c["secs"] is None else f'{c["secs"]:.2f}s'
            detail = "; ".join(c["detail"][:2])
            w(f'<tr><td><a href="{esc(c["label"])}">{esc(c["name"])}</a></td>'
              f'<td>{pill(c["verdict"])}</td><td>{esc(secs)}</td>'
              f'<td><code>{esc(c["szs"] or "")}</code></td>'
              f'<td class="detail">{esc(detail)}</td></tr>')
        w("</table></div></details>")

    # -- TPTP smoke ----------------------------------------------------
    if data["tptp"]:
        w(f'<h2>TPTP smoke (native, {esc(meta.get("tptp_budget", "?"))}s each)</h2>')
        w('<p class="meta">Problems from <a href="https://tptp.org">tptp.org</a> '
          '(fetched by <code>fetch_tptp.py</code>); each row is the SZS '
          'status the prover reported.</p>')
        w('<div class="card"><table>')
        w("<tr><th>problem</th><th>SZS status</th></tr>")
        for t in data["tptp"]:
            m = re.match(r"[A-Z]+", t["name"])
            dom = m.group(0) if m else t["name"][:3]
            url = (f'https://tptp.org/cgi-bin/SeeTPTP?Category=Problems'
                   f'&Domain={dom}&File={t["name"]}.p')
            w(f'<tr><td><a href="{esc(url)}">{esc(t["name"])}</a></td>'
              f'<td><code>{esc(t["szs"])}</code></td></tr>')
        w("</table></div>")

    # -- history ---------------------------------------------------------
    if len(history) > 1:
        w("<h2>History</h2>")
        w('<div class="card"><table>')
        backends = sorted({b for h in history for b in h.get("backends", {})})
        w("<tr><th>date</th><th>sigma-rs</th><th>ontology</th>"
          + "".join(f"<th>{esc(b)}</th>" for b in backends)
          + "<th>tptp solved</th></tr>")
        for i, h in enumerate(history[:60]):
            cells = []
            for b in backends:
                r = h.get("backends", {}).get(b)
                cur = f'{r["passed"]}/{r["graded"]}' if r and r["passed"] is not None else "—"
                arrow = ""
                if r and i + 1 < len(history):
                    p = history[i + 1].get("backends", {}).get(b)
                    if p and p.get("passed") is not None and r["passed"] is not None:
                        if r["passed"] > p["passed"]:
                            arrow = ' <span class="delta-up">▲</span>'
                        elif r["passed"] < p["passed"]:
                            arrow = ' <span class="delta-down">▼</span>'
                cells.append(cur + arrow)
            t = h.get("tptp", {})
            tp = f'{t.get("solved", "—")}/{t.get("total", "—")}' if t else "—"
            w(f'<tr><td>{esc(h.get("date", ""))}</td>'
              f'<td><code>{esc(h.get("sigma_commit", ""))}</code></td>'
              f'<td><code>{esc(h.get("sumo_commit", ""))}</code></td>'
              + "".join(f"<td>{c}</td>" for c in cells)
              + f"<td>{esc(tp)}</td></tr>")
        w("</table></div>")

    w('<footer>Generated by <code>regression_report.py</code> from a '
      '<code>ci_regression.sh</code> run.</footer>')
    w("</main>")

    return ("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">"
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
            f"<title>sigma-rs regression</title><style>{CSS}</style></head><body>"
            + "\n".join(out) + "</body></html>\n")


def load_json(path: Path | None):
    if path and path.exists():
        try:
            return json.loads(path.read_text())
        except json.JSONDecodeError:
            return None
    return None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--results", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--run-url", default=None)
    ap.add_argument("--prev", type=Path, default=None,
                    help="previous deploy's results.json (for the changes section)")
    ap.add_argument("--history", type=Path, default=None,
                    help="previous deploy's history.json (appended to)")
    args = ap.parse_args()

    data = collect(args.results)
    prev = load_json(args.prev)
    deltas = diff_runs(data, prev)

    prev_history = load_json(args.history) or []
    if not isinstance(prev_history, list):
        prev_history = []
    history = ([summarize(data)] + prev_history)[:HISTORY_LIMIT]

    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "index.html").write_text(render(data, deltas, history, args.run_url))
    (args.out / "results.json").write_text(json.dumps(data, indent=2) + "\n")
    (args.out / "history.json").write_text(json.dumps(history, indent=2) + "\n")

    native = next((b for b in data["backends"] if b["backend"] == "native"), None)
    msg = (f'{native["passed"]}/{native["graded"]}'
           if native and native["passed"] is not None else "no results")
    ok_run = data["status"] == "GREEN"
    (args.out / "badge.json").write_text(json.dumps({
        "schemaVersion": 1, "label": "regression",
        "message": msg if ok_run else "run incomplete",
        "color": "blue" if ok_run else "red",
    }) + "\n")
    print(f"wrote {args.out}/index.html "
          f"({len(history)} run(s) in history, deltas: {'yes' if deltas else 'none'})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
