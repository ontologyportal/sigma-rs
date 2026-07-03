# Solver-parameter tuning (genetic algorithm)

`ga.py` tunes the native prover's `Strategy` genome by driving the Rust CLI as a
black box. It evolves a population of `Strategy` specs and evaluates each
generation in one parallel pass via `sumo sweep`, optimising **solved first,
then less wall-time**.

## Build the CLI (no CMake / no embedded Vampire needed)

```sh
cargo build --release -p sigmakee --no-default-features --features ask
```

This builds `./target/release/sumo` with the native saturation prover and the
`sweep` subcommand — no C++ Vampire, no CMake.

## Run

```sh
export TPTP=$HOME/Downloads/TPTP-v9.2.1          # if the corpus is TPTP with includes
python3 tuning/ga.py \
    --sumo ./target/release/sumo --sumo-arg=--no-db \
    --pop 24 --generations 15 --jobs 8 --timeout 8 \
    <corpus.p / corpus.kif.tq / dirs ...>
```

Note: pass-through flags use the `=` form (`--sumo-arg=--no-db`) because argparse
otherwise mistakes a leading `--` for an option.

### Key flags
- `--pop`, `--generations`, `--elite`, `--tournament-k`, `--mutation-rate`,
  `--crossover-rate` — the GA shape.
- `--timeout` (per-run wall cap), `--budget` (SInE axioms), `--steps`
  (given-clause cap) — passed straight to `sumo sweep`.
- `--jobs` — sweep worker threads.
- `--out-dir` (default `tuning/runs/`) — per-generation `popNNN.json` /
  `resNNN.csv`, plus a live `best.json`.

## Output

`best.json` is a one-element array holding the best genome found so far,
re-readable by the solver:

```sh
sumo --no-db sweep --configs tuning/runs/best.json  <corpus ...>
```

(or feed it to the portfolio runner). It is rewritten every generation, so an
interrupted run still leaves the best-so-far on disk.

## How it works

- **Genome / ranges** mirror `Strategy::sample` in
  `crates/core/src/saturate/strategy.rs` — the viable region near the shipping
  default. Fields not in the genome fall back to `Strategy::base()` (the spec is
  partial; serde fills the rest).
- **Fitness** parses the `config,problem,solved,status,steps,ms` matrix
  `sweep --out` writes: primary = number solved (correct SZS verdict); tie-break
  = total `ms` over the problems it solved.
- **Evolution**: elitism + tournament selection + uniform crossover + per-gene
  mutation. Deterministic from `--seed`.

## Picking a corpus

The GA only has a gradient if genomes *differ* in what they solve — choose a
corpus that's hard/diverse enough that the default doesn't already solve
everything (e.g. a mix of CSR/TPTP problems near the prover's current frontier),
not a set every config trivially proves.

## Building a gradient-rich corpus (`build_corpus.py`)

The GA needs a corpus with a gradient (problems params actually change) and as
little dead-timeout waste as possible. `build_corpus.py` constructs one from a
baseline `sumo sweep` matrix: it pools the *saturation-solvable* problems
(anything solved them → each yields gradient) plus a small `--stretch` slice of
currently-unsolved ones, then takes a seeded random sample.

```sh
# 1. baseline matrix over the whole CSR set (one config = the default)
sumo --no-db sweep ~/Downloads/TPTP-v9.2.1/Problems/CSR/*+*.p --out base.csv --timeout 4
# 2. sample a 120-problem corpus (100 solvable + 20 stretch), reproducible
python3 tuning/build_corpus.py --results base.csv \
    --problem-root ~/Downloads/TPTP-v9.2.1/Problems/CSR \
    --solved 100 --stretch 20 --out tuning/csr_corpus.txt
# 3. tune
python3 tuning/ga.py --sumo ./target/release/sumo --sumo-arg=--no-db \
    --corpus-file tuning/csr_corpus.txt --pop 24 --generations 20 --jobs 8
```

Corpus `.txt` files hold absolute paths (machine-specific) and are gitignored;
the builder + seed make them reproducible. Confirmed gradient on a 120-problem
CSR sample: genome solved-counts spanned 74..101, and the GA found a genome
solving 101 vs the shipping default's 99 (+2) — in generation 0.
