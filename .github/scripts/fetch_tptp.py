#!/usr/bin/env python3
"""Materialize a $TPTP problem tree by scraping tptp.org's SeeTPTP pages.

The TPTP library has no raw-file endpoint — problems and axioms are only
served wrapped in HTML by the SeeTPTP CGI (e.g.
https://tptp.org/cgi-bin/SeeTPTP?Category=Problems&Domain=PUZ&File=PUZ001+1.p).
This script fetches each problem in a regression list (DOM/NAME lines, the
format of scripts/tptp_regression.list), strips the HTML wrapper, recursively
resolves include('Axioms/...') directives, and writes an ordinary TPTP
directory layout:

    <dest>/Problems/<DOM>/<NAME>.p
    <dest>/Axioms/<...>.ax

Files already present under <dest> are not re-fetched (their includes are
still walked), so pointing --dest at a CI cache makes reruns free until the
list changes.

Usage: fetch_tptp.py --list scripts/tptp_regression.list --dest ~/TPTP
"""

import argparse
import html
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

SEETPTP = "https://tptp.org/cgi-bin/SeeTPTP"
USER_AGENT = "sigma-rs-regression/1.0 (+https://github.com/ontologyportal/sigma-rs)"
FETCH_DELAY_SECS = 0.2
RETRIES = 3

INCLUDE_RE = re.compile(r"^\s*include\('([^']+)'", re.M)
TAG_RE = re.compile(r"<[^>]*>")


def seetptp_url(tptp_path: str) -> str:
    """Map a $TPTP-relative path to its SeeTPTP URL.

    Problems/PUZ/PUZ001+1.p -> ?Category=Problems&Domain=PUZ&File=PUZ001+1.p
    Axioms/GRP007+0.ax      -> ?Category=Axioms&File=GRP007+0.ax
    Axioms/SET007/SET007+1.ax -> ?Category=Axioms&File=SET007/SET007+1.ax
    """
    quote = lambda s: urllib.parse.quote(s, safe="/")
    category, _, rest = tptp_path.partition("/")
    if category == "Problems":
        domain, _, name = rest.partition("/")
        return f"{SEETPTP}?Category=Problems&Domain={quote(domain)}&File={quote(name)}"
    return f"{SEETPTP}?Category={quote(category)}&File={quote(rest)}"


def fetch_page(url: str) -> str:
    last_err = None
    for attempt in range(RETRIES):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(req, timeout=60) as resp:
                # SeeTPTP declares iso-8859-1.
                return resp.read().decode("latin-1")
        except (urllib.error.URLError, OSError, TimeoutError) as e:
            last_err = e
            time.sleep(2.0 * (attempt + 1))
    raise RuntimeError(f"failed to fetch {url}: {last_err}")


def extract_tptp(page: str, url: str) -> str:
    """Pull the TPTP text out of a SeeTPTP HTML page.

    The file body sits in the page's single <pre> block, decorated with
    <A NAME=...> anchors per formula and <a href=...> reference links, with
    `<` escaped as &lt; (so tag-stripping before unescaping is safe: every
    raw `<` opens a real tag).
    """
    if "ERROR: Cannot read" in page:
        raise RuntimeError(f"SeeTPTP has no such file: {url}")
    start = page.find("<pre>")
    end = page.rfind("</pre>")
    if start == -1 or end == -1 or end <= start:
        raise RuntimeError(f"no <pre> block in SeeTPTP page: {url}")
    body = page[start + len("<pre>"):end]
    text = html.unescape(TAG_RE.sub("", body)).strip() + "\n"
    if not re.search(r"^\s*(?:%|fof\(|cnf\(|tff\(|thf\(|tcf\(|include\()", text, re.M):
        raise RuntimeError(f"extracted text does not look like TPTP: {url}")
    return text


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--list", required=True, type=Path,
                    help="problem list, one DOM/NAME per line (# comments ok)")
    ap.add_argument("--dest", required=True, type=Path,
                    help="TPTP root to materialize (becomes $TPTP)")
    args = ap.parse_args()

    problems = []
    for line in args.list.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            problems.append(f"Problems/{line}.p")

    queue = list(problems)
    seen = set(queue)
    fetched = cached = 0
    while queue:
        tptp_path = queue.pop(0)
        out = args.dest / tptp_path
        if out.exists():
            text = out.read_text()
            cached += 1
        else:
            url = seetptp_url(tptp_path)
            text = extract_tptp(fetch_page(url), url)
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(text)
            fetched += 1
            print(f"  fetched {tptp_path}", flush=True)
            time.sleep(FETCH_DELAY_SECS)
        for inc in INCLUDE_RE.findall(text):
            if inc not in seen:
                seen.add(inc)
                queue.append(inc)

    print(f"TPTP tree at {args.dest}: {len(problems)} problems, "
          f"{len(seen) - len(problems)} axiom files ({fetched} fetched, {cached} already present)")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)
