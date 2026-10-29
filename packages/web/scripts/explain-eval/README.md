# Explanation format experiment

Does the explanation model do better when the proof is shown as KIF steps
(what production sends), as English paraphrases, as both, or as the
engine's narrated prose? Every corpus item is explained under all four
conditions with the same model, prompt and settings; only the proof
rendering differs (`buildMessages(req, { format })` in
`functions/_utils/explain.ts`).

## Steps

```sh
# 1. Corpus: Ask/Tell proofs from the SUMO inference tests, plus planted
#    faults in real axioms that the prover confirms as contradictions.
#    Reads the KB from ~/.sigmakee/config.xml; ~10-40 min.
node scripts/explain-eval/corpus.mjs --ask 40 --contradictions 40

# 2. Check the prompts and the planned spend without calling anything.
node scripts/explain-eval/run.mjs --dry-run

# 3. Run the model. Resumable; stops at --max-neurons (default 9000, inside
#    the 10,000/day free allocation) and continues on the next run. The first
#    line it prints says how many calls are already recorded.
export CLOUDFLARE_ACCOUNT_ID=... CLOUDFLARE_API_TOKEN=...
node scripts/explain-eval/run.mjs --model @cf/zai-org/glm-5.3-flash --samples 3

# 4. Score and write out/report.md. Add --judge-model for 1-5 grades from a
#    stronger model that is not told the condition.
node scripts/explain-eval/score.mjs [--judge-model @cf/zai-org/glm-5.3]
```

Reasoning models (GLM, gpt-oss, Qwen3, ...) think before answering, and
GLM 5.3 Flash cannot turn that off. At its default `max` effort it often
spends the whole token budget thinking and returns no answer. The runner
therefore sends `reasoning_effort: low` by default (`--reasoning-effort high`
or `max` to change it, `""` to send none). Each model and effort is its own
setting: its calls are keyed and scored separately, and `score.mjs --config
@cf/zai-org/glm-5.3-flash@low` picks one (default: the setting with the most
calls). A call that returns no answer is recorded as an outcome and scored as
a failure (`answered` = 0), not retried; `--retry-empty` resends those.

Everything lands in `scripts/explain-eval/out/` (gitignored), or `--out DIR`.

## Metrics

Contradictions have ground truth: the planted axiom, its original text and
the corruption. `culprit_cited` checks the answer names the planted
`file:line`; `fix_exact` checks a proposed formula restores the original;
`fix_resolves` substitutes each proposed repair into the KB and re-audits
that line with the prover. For every answer: step references in range,
KB symbols the explanation uses that the proof never mentions, and invented
CamelCase names. The report gives per-condition means with bootstrap
intervals and paired sign tests against `kif`.

## Caveats

- Fault types differ in how often the prover confirms them within the audit
  time limit: `instance-class` and `relation-class` confirm readily,
  `subclass-parent`, `arg-swap` and `disjoint-ancestor` rarely. Each type is
  capped at an equal share, so a short corpus means the rare types ran out;
  raise `--max-candidates` or `--audit-time-limit` to dig further.
- Many inference tests return Unknown under the web app's prover profile;
  the corpus keeps only proved ones (at most 60 steps, the production cap).
- `narrative` numbers its statements itself, so step-reference metrics do
  not apply to it.
- GLM 5.3 Flash needs a paid billing method on Workers AI (the free daily
  neurons still apply on top). `@cf/zai-org/glm-4.7-flash` runs on the free
  plan.

Unit tests: `npm run test:explain-eval --workspace @sigma/web`.
