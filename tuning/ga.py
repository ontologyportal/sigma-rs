#!/usr/bin/env python3
"""Genetic-algorithm tuner for the sigma-rs native prover's `Strategy` genome.

Drives the Rust CLI as a black box: each generation writes the population as a
JSON array of partial `Strategy` specs and evaluates the whole population in ONE
parallel pass via

    sumo [kb-args] sweep <corpus> --configs pop.json --out res.csv ...

then reads the `config,problem,solved,status,steps,ms` matrix back.  Fitness is
**solved first, then less wall-time** (a genome that solves more wins; ties go
to the one that's faster on the problems it solves).

The gene set + value ranges mirror `Strategy::sample` in
crates/core/src/saturate/strategy.rs (the "viable region" — kept near the
shipping default per the IJCAI within-an-order-of-magnitude rule), so every
genome the GA emits is a sane, in-distribution config.  Fields not listed here
are omitted from the spec and fall back to `Strategy::base()` (serde default).

Build the CLI for this first (no CMake needed):
    cargo build --release -p sigmakee --no-default-features --features ask

Example:
    python3 tuning/ga.py \
        --sumo ./target/release/sumo --sumo-arg --no-db \
        --pop 24 --generations 15 --jobs 8 --timeout 8 \
        ~/Downloads/TPTP-v9.2.1/Problems/CSR/CSR025+1.p  <more .p / .kif.tq ...>
"""

import argparse, csv, json, os, random, subprocess, sys, time
from pathlib import Path

# ---------------------------------------------------------------------------
# Gene schema — mirrors Strategy::sample.  Each entry is either:
#   ("choice", [v0, v1, ...])  — pick uniformly (weighting via repeats)
#   ("bool",   pct)            — True with probability pct%
# `tier_weight` is three independent choice genes flattened into an array.
# ---------------------------------------------------------------------------
GENES = {
    "tier_weight0":  ("choice", [1, 1, 1, 2]),
    "tier_weight1":  ("choice", [1, 2, 2, 3, 4]),
    "tier_weight2":  ("choice", [1, 2, 2, 3, 4, 6, 8]),
    "pick_ratio":    ("choice", [2, 3, 4, 5, 6, 8, 10]),
    "goal_dist":     ("bool",   25),
    "goal_dist_w":   ("choice", [1, 2, 3, 4]),
    "cw_lits":       ("choice", [0, 1, 1, 2, 3]),
    "cw_size":       ("choice", [1, 1, 2, 3]),
    "cw_vars":       ("choice", [0, 1, 2, 2, 4]),
    "cw_skolem":     ("choice", [0, 1, 1, 2, 4]),
    "lit_select":    ("choice", [0, 0, 0, 1, 2]),
    "max_depth":     ("choice", [4, 5, 6, 7]),
    "max_term_size": ("choice", [48, 64, 96, 128]),
    "para_cap":      ("choice", [50, 100, 200, 400, 800]),
    "demod_cap":     ("choice", [32, 64, 128, 256]),
    "prec_seed":     ("choice", [0, 0, 0, 0xA5A51234, 0x13579BDF, 0xF00DCAFE]),
    "fc_max_premise_lits": ("choice", [4, 6, 8]),
    "fc_fanout":     ("choice", [8, 16, 32, 64]),
    "fc_flat_depth": ("choice", [1, 2, 3]),
    "fc_rounds":     ("choice", [2, 4, 6, 8, 10]),
    "fc_cap":        ("choice", [2000, 4000, 8000, 16000]),
    "fc_branch":     ("choice", [4, 8, 16]),
    "fc_max_pos":    ("choice", [1, 1, 2, 3]),
    "schema":        ("bool",   85),
    "decode":        ("bool",   90),
    "demod":         ("bool",   35),
    "ordered_resolution": ("bool", 15),
    "subsumption":   ("bool",   20),
    "superposition": ("bool",   10),
    "eq_factoring":  ("bool",   10),
    "bg_completion": ("bool",   10),
    "bg_completion_budget": ("choice", [128, 256, 512]),
    "liu_rescue":    ("bool",   85),
    "liu_rounds":    ("choice", [1, 1, 1, 2]),
    "liu_top_k":     ("choice", [16, 32, 64, 128]),
    "def_completion": ("bool",  85),
    "defcomp_rounds": ("choice", [2, 4, 6, 8]),
    "defcomp_max_adds": ("choice", [32, 64, 128, 256]),
    "defcomp_per_sym": ("choice", [4, 8, 16]),
    "head_filter":   ("bool",   15),
}
# Genes that fold into the `tier_weight` array in the emitted JSON.
TIER = ["tier_weight0", "tier_weight1", "tier_weight2"]


def random_gene(name, rng):
    kind, spec = GENES[name]
    return rng.choice(spec) if kind == "choice" else (rng.randrange(100) < spec)


def random_genome(rng):
    return {g: random_gene(g, rng) for g in GENES}


def mutate(genome, rng, rate):
    """Per-gene resample with probability `rate`."""
    out = dict(genome)
    for g in GENES:
        if rng.random() < rate:
            out[g] = random_gene(g, rng)
    return out


def crossover(a, b, rng):
    """Uniform crossover: each gene independently from either parent."""
    return {g: (a[g] if rng.random() < 0.5 else b[g]) for g in GENES}


def to_spec(genome, name):
    """Render a genome as a Strategy JSON spec (partial; rest default)."""
    spec = {"name": name}
    spec["tier_weight"] = [int(genome[t]) for t in TIER]
    for g, v in genome.items():
        if g in TIER:
            continue
        spec[g] = bool(v) if GENES[g][0] == "bool" else int(v)
    return spec


def evaluate(genomes, names, args, gen):
    """Run `sumo sweep` over the population; return {name: (solved, total_ms)}.

    total_ms is summed over SOLVED problems only (the 'fast on its wins' signal).
    """
    pop_path = Path(args.out_dir) / f"gen{gen:03d}_pop.json"
    csv_path = Path(args.out_dir) / f"gen{gen:03d}_res.csv"
    specs = [to_spec(g, n) for g, n in zip(genomes, names)]
    pop_path.write_text(json.dumps(specs, indent=0))

    cmd = [args.sumo, *args.sumo_arg, "sweep", *args.corpus,
           "--configs", str(pop_path), "--out", str(csv_path),
           "--timeout", str(args.timeout), "--budget", str(args.budget),
           "--steps", str(args.steps), "--seed", str(args.seed)]
    if args.jobs:
        cmd += ["--jobs", str(args.jobs)]
    subprocess.run(cmd, check=True,
                   stdout=(None if args.verbose else subprocess.DEVNULL),
                   stderr=(None if args.verbose else subprocess.DEVNULL))

    stats = {n: [0, 0] for n in names}  # name -> [solved, total_ms_on_solved]
    with open(csv_path, newline="") as f:
        for row in csv.DictReader(f):
            n = row["config"]
            if n not in stats:
                continue
            if row["solved"].strip().lower() == "true":
                stats[n][0] += 1
                try:
                    stats[n][1] += int(row["ms"])
                except ValueError:
                    pass
    return {n: tuple(v) for n, v in stats.items()}


def fitness_key(stat):
    """Sort key, BEST first: most solved, then least wall-time."""
    solved, ms = stat
    return (-solved, ms)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("corpus", nargs="*", help=".p / .kif.tq files or dirs")
    ap.add_argument("--corpus-file", help="file of corpus paths, one per line (# comments ok)")
    ap.add_argument("--sumo", default="./target/release/sumo")
    ap.add_argument("--sumo-arg", action="append", default=[],
                    help="passed to sumo before the subcommand (e.g. --no-db). repeatable")
    ap.add_argument("--pop", type=int, default=24)
    ap.add_argument("--generations", type=int, default=15)
    ap.add_argument("--elite", type=int, default=3, help="genomes copied verbatim each gen")
    ap.add_argument("--tournament-k", type=int, default=3)
    ap.add_argument("--mutation-rate", type=float, default=0.15, help="per-gene resample prob")
    ap.add_argument("--crossover-rate", type=float, default=0.8)
    ap.add_argument("--timeout", type=int, default=8, help="per-run wall cap (s)")
    ap.add_argument("--budget", type=int, default=2000, help="SInE axiom budget")
    ap.add_argument("--steps", type=int, default=200000, help="given-clause step cap")
    ap.add_argument("--jobs", type=int, default=0, help="sweep worker threads (0=auto)")
    ap.add_argument("--seed", type=int, default=0xC0FFEE)
    ap.add_argument("--out-dir", default="tuning/runs")
    ap.add_argument("--verbose", action="store_true", help="show sweep output")
    args = ap.parse_args()
    if args.corpus_file:
        for line in Path(args.corpus_file).read_text().splitlines():
            line = line.strip()
            if line and not line.startswith("#"):
                args.corpus.append(line)
    if not args.corpus:
        sys.exit("no corpus: pass paths and/or --corpus-file")
    if not args.jobs:
        args.jobs = None
    Path(args.out_dir).mkdir(parents=True, exist_ok=True)
    rng = random.Random(args.seed)

    if not Path(args.sumo).exists():
        sys.exit(f"sumo binary not found: {args.sumo}\n"
                 f"build it: cargo build --release -p sigmakee --no-default-features --features ask")

    # Generation 0: the shipping default (all-default genome) + random population.
    pop = [{g: random_gene(g, rng) for g in GENES} for _ in range(args.pop - 1)]
    pop.insert(0, {g: random_gene(g, rng) for g in GENES})  # keep pop size
    best_overall, best_stat = None, (-1, 0)

    for gen in range(args.generations):
        names = [f"g{gen:03d}-{i:03d}" for i in range(len(pop))]
        t0 = time.time()
        stats = evaluate(pop, names, args, gen)
        ranked = sorted(zip(pop, names), key=lambda pn: fitness_key(stats[pn[1]]))
        top_g, top_n = ranked[0]
        s, ms = stats[top_n]
        if (s, -ms) > (best_stat[0], -best_stat[1]):
            best_overall, best_stat = dict(top_g), (s, ms)
        print(f"gen {gen:3d}  best: solved={s:3d} ms={ms:7d}  "
              f"pop_best_solved={max(stats[n][0] for n in names):3d}  "
              f"({time.time()-t0:.0f}s)")
        Path(args.out_dir, "best.json").write_text(
            json.dumps([to_spec(best_overall, "ga-best")], indent=2))

        # Next generation: elitism + tournament selection + crossover + mutation.
        elites = [dict(g) for g, _ in ranked[:args.elite]]
        nxt = list(elites)
        while len(nxt) < args.pop:
            def pick():
                cand = rng.sample(list(zip(pop, names)), min(args.tournament_k, len(pop)))
                return min(cand, key=lambda pn: fitness_key(stats[pn[1]]))[0]
            child = crossover(pick(), pick(), rng) if rng.random() < args.crossover_rate else dict(pick())
            nxt.append(mutate(child, rng, args.mutation_rate))
        pop = nxt

    print(f"\nbest overall: solved={best_stat[0]} ms={best_stat[1]}")
    print(f"written: {Path(args.out_dir, 'best.json')}  "
          f"(feed back via `sumo sweep --configs` or the portfolio runner)")


if __name__ == "__main__":
    main()
