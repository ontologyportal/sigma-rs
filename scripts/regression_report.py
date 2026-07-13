#!/usr/bin/env python3
"""Render scripts/ci_regression.sh results as a static GitHub Pages site.

Reads the structured output directory ci_regression.sh produced (meta.tsv,
suites.tsv, tptp.tsv, logs/*.log) and writes:

    <out>/index.html    the report page (self-contained, no external assets)
    <out>/results.json  the parsed results, for machine consumption
    <out>/badge.json    a shields.io endpoint (https://img.shields.io/endpoint)

Usage: regression_report.py --results regression-out --out site [--run-url URL]
"""

import argparse
import html
import json
import re
import sys
from pathlib import Path

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

    suites = []
    for backend, suite, passed, graded, expected, verdict in read_tsv(results / "suites.tsv"):
        log = results / "logs" / f"{backend}-{suite}.log"
        suites.append({
            "backend": backend, "suite": suite,
            "passed": int(passed) if passed else None,
            "graded": int(graded) if graded else None,
            "expected": int(expected) if expected else None,
            "status": verdict,
            "cases": parse_suite_log(log) if log.exists() else [],
        })

    tptp = [{"name": n, "szs": s, "verdict": v}
            for n, s, v in read_tsv(results / "tptp.tsv")]
    return {"meta": meta, "status": status, "suites": suites, "tptp": tptp}


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
.state.green { color: var(--green); background: var(--green-bg); }
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
details { margin: .7rem 0; }
summary { cursor: pointer; padding: .55rem .9rem; background: var(--card);
  border: 1px solid var(--line); border-radius: 10px; font-weight: 600; }
summary .pill { margin-left: .6rem; }
details[open] summary { border-radius: 10px 10px 0 0; }
details .card { border-top: none; border-radius: 0 0 10px 10px; }
footer { margin-top: 3rem; color: var(--muted); font-size: .82rem; }
"""

PILL = {
    "ok": ("ok", "ok"), "FAIL": ("fail", "FAIL"), "report": ("info", "report-only"),
    "PASSED": ("ok", "passed"), "FAILED": ("fail", "failed"),
    "FALSE VERDICT": ("fail", "FALSE VERDICT"), "INFO": ("info", "info"),
    "ERROR": ("warn", "error"), "SOUNDNESS": ("fail", "SOUNDNESS"),
    "MUST_SOLVE": ("fail", "must-solve"),
}


def pill(key: str) -> str:
    cls, label = PILL.get(key, ("info", key))
    return f'<span class="pill {cls}">{html.escape(label)}</span>'


def esc(s) -> str:
    return html.escape(str(s))


def render(data: dict, run_url: str | None) -> str:
    meta, out = data["meta"], []
    w = out.append
    ok = data["status"] == "GREEN"
    date = meta.get("date_utc", "")
    sigma = meta.get("sigma_commit", "")
    sumo_repo = meta.get("sumo_repo", "https://github.com/ontologyportal/sumo")
    sumo_commit = meta.get("sumo_commit", "")
    raw_base = sumo_repo.removesuffix(".git")

    w("<main>")
    w("<h1>sigma-rs regression</h1>")
    w('<div class="banner">')
    w(f'<span class="state {"green" if ok else "fail"}">'
      f'{"ALL GREEN" if ok else "FAILURES"}</span>')
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

    # -- suite × backend summary ------------------------------------
    w("<h2>SUMO suites</h2>")
    w('<div class="card"><table>')
    w("<tr><th>backend</th><th>suite</th><th>result</th><th>expected</th>"
      "<th>status</th></tr>")
    for s in data["suites"]:
        res = "—" if s["passed"] is None else f'{s["passed"]} / {s["graded"]} passed'
        exp = "" if s["expected"] is None else str(s["expected"])
        w(f'<tr><td>{esc(s["backend"])}</td><td>{esc(s["suite"])}</td>'
          f'<td>{esc(res)}</td><td>{esc(exp)}</td><td>{pill(s["status"])}</td></tr>')
    w("</table></div>")

    # -- per-suite detail --------------------------------------------
    w("<h2>Per-test detail</h2>")
    for s in data["suites"]:
        if not s["cases"]:
            continue
        res = "" if s["passed"] is None else f' — {s["passed"]} / {s["graded"]}'
        w(f'<details><summary>{esc(s["backend"])} / {esc(s["suite"])}{esc(res)}'
          f'{pill(s["status"])}</summary>')
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
        w('<p class="meta">Theorem-rated slice from '
          '<a href="https://tptp.org">tptp.org</a> (fetched by '
          '<code>scripts/fetch_tptp.py</code>): a SAT/CSA verdict is a soundness '
          'failure; must-solve problems are marked when they stop solving.</p>')
        w('<div class="card"><table>')
        w("<tr><th>problem</th><th>SZS status</th><th>status</th></tr>")
        for t in data["tptp"]:
            m = re.match(r"[A-Z]+", t["name"])
            dom = m.group(0) if m else t["name"][:3]
            url = (f'https://tptp.org/cgi-bin/SeeTPTP?Category=Problems'
                   f'&Domain={dom}&File={t["name"]}.p')
            w(f'<tr><td><a href="{esc(url)}">{esc(t["name"])}</a></td>'
              f'<td><code>{esc(t["szs"])}</code></td><td>{pill(t["verdict"])}</td></tr>')
        w("</table></div>")

    w('<footer>Generated by <code>scripts/regression_report.py</code> from a '
      '<code>scripts/ci_regression.sh</code> run.</footer>')
    w("</main>")

    return ("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">"
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
            f"<title>sigma-rs regression</title><style>{CSS}</style></head><body>"
            + "\n".join(out) + "</body></html>\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--results", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--run-url", default=None)
    args = ap.parse_args()

    data = collect(args.results)
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "index.html").write_text(render(data, args.run_url))
    (args.out / "results.json").write_text(json.dumps(data, indent=2) + "\n")

    ok = data["status"] == "GREEN"
    (args.out / "badge.json").write_text(json.dumps({
        "schemaVersion": 1, "label": "regression",
        "message": "all green" if ok else "failures",
        "color": "brightgreen" if ok else "red",
    }) + "\n")
    print(f"wrote {args.out}/index.html ({data['status']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
