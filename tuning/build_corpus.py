#!/usr/bin/env python3
"""Build a stratified, dead-TO-filtered tuning corpus from a `sumo sweep` matrix.

The GA tunes the *saturation* `Strategy`, so the corpus must have a gradient:
problems where parameters change the outcome.  Two traps to avoid:
  - trivial problems every config solves (no gradient), and
  - problems NO config solves (dead timeouts — pure cost; with CSR these are
    mostly the oracle-dependent ones the pure saturation prover can't do).

So the pool is the SATURATION-SOLVABLE problems (anything solved them in the
baseline sweep → every one yields gradient: a bad genome loses it, a fast genome
wins the ms tie-break), plus a small `--stretch` slice of currently-unsolved
ones (a better genome might crack them, raising the ceiling).  Seeded random
sample → reproducible, spread across the whole index.

Run a baseline sweep once to get the matrix, then build:

    sumo --no-db sweep <all CSR .p / dirs> --out base.csv --timeout 4
    python3 tuning/build_corpus.py --results base.csv \
        --problem-root ~/Downloads/TPTP-v9.2.1/Problems/CSR \
        --solved 100 --stretch 20 --out tuning/csr_corpus.txt
"""
import argparse, csv, random
from collections import defaultdict
from pathlib import Path


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--results", required=True,
                    help="a `sumo sweep --out` CSV (config,problem,solved,status,steps,ms)")
    ap.add_argument("--problem-root", required=True,
                    help="dir the `problem` column names are relative to (e.g. .../Problems/CSR)")
    ap.add_argument("--solved", type=int, default=100, help="N saturation-solvable problems")
    ap.add_argument("--stretch", type=int, default=20, help="N currently-unsolved 'stretch' problems")
    ap.add_argument("--seed", type=int, default=20240617)
    ap.add_argument("--out", default="tuning/csr_corpus.txt")
    args = ap.parse_args()

    # A problem is "solvable" if ANY config in the matrix solved it.
    any_solved = defaultdict(bool)
    seen = set()
    with open(args.results, newline="") as f:
        for row in csv.DictReader(f):
            p = row["problem"]
            seen.add(p)
            if row["solved"].strip().lower() == "true":
                any_solved[p] = True

    solvable = sorted(p for p in seen if any_solved[p])
    unsolved = sorted(p for p in seen if not any_solved[p])
    rng = random.Random(args.seed)
    rng.shuffle(solvable)
    rng.shuffle(unsolved)
    pick = solvable[:args.solved] + unsolved[:args.stretch]
    rng.shuffle(pick)

    root = Path(args.problem_root)
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text("\n".join(str(root / p) for p in pick) + "\n")
    print(f"matrix: {len(seen)} problems  ({len(solvable)} solvable, {len(unsolved)} unsolved)")
    print(f"corpus: {len(pick)} ({min(args.solved, len(solvable))} solvable + "
          f"{min(args.stretch, len(unsolved))} stretch) -> {args.out}")


if __name__ == "__main__":
    main()
