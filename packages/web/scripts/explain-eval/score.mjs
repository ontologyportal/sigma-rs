// Score every recorded explanation and write <out>/scores.jsonl and
// <out>/report.md. Automatic metrics need no model; contradiction fixes are
// re-verified with the prover (substitute the proposed formula for the
// planted axiom, re-audit that line). --judge-model adds 1-5 grades from a
// stronger model that is not told the condition (resumable, <out>/judge.jsonl).
//
//   node scripts/explain-eval/score.mjs [--config MODEL[@EFFORT]] [--no-verify]
//     [--judge-model @cf/zai-org/glm-5.3]
//
// Runs are scored per setting (model plus reasoning effort, see run.mjs);
// --config picks one, defaulting to the setting with the most calls. A call
// that returned no answer scores as a failure on every task metric.

import { appendFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
  CONDITIONS,
  bootstrapCi,
  citedLocations,
  explainModule,
  extractKif,
  inventedSymbols,
  isRepairOf,
  kbSymbolSet,
  mean,
  paired,
  parseArgs,
  readJsonl,
  rng,
  sameKif,
  stepRefs,
  topLevelForms,
  unsupportedSymbols,
} from "./lib.mjs";
import { configuredKb, loadKb, reingest, webConfig } from "./kb.mjs";

const conf = configuredKb();
const opt = parseArgs(process.argv.slice(2), {
  config: "",
  verify: true,
  judgeModel: "",
  judgeMaxNeurons: 9000,
  sumo: conf?.dir ?? "",
  files: conf?.files.join(",") ?? "Merge.kif,Mid-level-ontology.kif",
  auditTimeLimit: 10,
  seed: 7,
  out: new URL("./out", import.meta.url).pathname,
});
const log = (...a) => console.error(...a);
const ex = explainModule();
const corpus = new Map(
  readJsonl(join(opt.out, "corpus.jsonl")).map((i) => [i.id, i]),
);
const allRuns = readJsonl(join(opt.out, "runs.jsonl")).filter((r) =>
  corpus.has(r.id),
);
const configOf = (r) => r.config ?? r.model;
const counts = new Map();
for (const r of allRuns)
  counts.set(configOf(r), (counts.get(configOf(r)) ?? 0) + 1);
const config =
  opt.config || [...counts].sort((a, b) => b[1] - a[1])[0]?.[0] || "";
if (!counts.has(config))
  throw new Error(
    `no runs for ${config}; have ${[...counts.keys()].join(", ")}`,
  );
if (counts.size > 1)
  log(
    `scoring ${config}; also present: ${[...counts.keys()].filter((c) => c !== config).join(", ")} (--config to choose)`,
  );
// The latest record per key wins (a failed call retried later).
const byKey = new Map(
  allRuns.filter((r) => configOf(r) === config).map((r) => [r.key, r]),
);
const isNoAnswer = (r) => r.noAnswer || r.error === "empty response";
/** Calls with an outcome: an answer, or a recorded failure to answer. HTTP
 *  and network errors are not outcomes and are left out. */
const outcomes = [...byKey.values()].filter((r) => !r.error || isNoAnswer(r));
const answered = outcomes.filter((r) => !isNoAnswer(r) && r.text);

// -- KB for symbols and fix verification ---------------------------------------

let kb = null;
let kbSymbols = new Set();
const needKb = answered.some((r) => corpus.get(r.id).kind === "contradiction");
if (opt.sumo && (opt.verify || needKb)) {
  kb = await loadKb({
    dir: opt.sumo,
    files: opt.files
      .split(",")
      .map((f) => f.trim())
      .filter(Boolean),
    timeLimitSecs: opt.auditTimeLimit,
    log,
  });
  kb.session.configure(webConfig(opt.auditTimeLimit));
  kbSymbols = kbSymbolSet(kb.texts.values());
}

const verifyPath = join(opt.out, "verify.jsonl");
const verified = new Map(
  readJsonl(verifyPath).map((v) => [`${v.id}|${v.formula}`, v.resolves]),
);

/** Does replacing the planted axiom with `formula` remove the contradiction
 *  at that line (and is `formula` well-formed)? Cached in verify.jsonl. */
const progress = { pending: 0, checked: 0, started: 0 };
async function resolves(item, formula) {
  const key = `${item.id}|${formula}`;
  if (verified.has(key)) return verified.get(key);
  progress.checked++;
  const { file, line, original } = item.truth;
  const text = kb.texts.get(file);
  const form = topLevelForms(text).find(
    (f) => f.line === line && sameKif(f.kif, original),
  );
  let ok = false;
  if (
    form &&
    !kb.session.validateFormula(formula).some((d) => d.severity === "error")
  ) {
    const edited =
      text.slice(0, form.startIndex) + formula + text.slice(form.endIndex);
    await reingest(kb, file, edited);
    try {
      kb.session.clearAuditFindings();
      const res = kb.session.auditConsistency({
        at: { file, offset: form.offset },
        limit: 3,
      });
      ok = !res.contradictions.some((c) =>
        c.steps.some((s) => s.file === file && s.line === line),
      );
    } catch {
      ok = false;
    }
    await reingest(kb, file, text);
  }
  verified.set(key, ok);
  const secs = (Date.now() - progress.started) / 1000;
  const left = Math.max(0, progress.pending - progress.checked);
  log(
    `fix check ${progress.checked}/${progress.pending}: ${ok ? "resolves" : "does not resolve"} (${item.id}); ~${Math.ceil((left * secs) / progress.checked / 60)} min left`,
  );
  appendFileSync(
    verifyPath,
    JSON.stringify({ id: item.id, formula, resolves: ok }) + "\n",
  );
  return ok;
}

// -- automatic metrics ---------------------------------------------------------

const proofText = (item) =>
  [item.query ?? "", ...item.steps.map((s) => s.kif)].join("\n");

/** A call without an answer fails every task metric; descriptive ones
 *  (words, step validity, symbol counts) have nothing to measure. */
function noAnswerMetrics(run) {
  const item = corpus.get(run.id);
  const m = { answered: 0 };
  if (run.condition !== "narrative") m.cites_steps = 0;
  if (item.kind === "contradiction") {
    m.culprit_cited = 0;
    m.repair_proposed = 0;
    m.fix_exact = 0;
    if (opt.verify && kb) m.fix_resolves = 0;
  }
  return m;
}

async function metrics(run) {
  if (isNoAnswer(run) || !run.text) return noAnswerMetrics(run);
  const item = corpus.get(run.id);
  const text = run.text;
  const m = {
    answered: 1,
    words: text.split(/\s+/).filter(Boolean).length,
  };
  if (run.truncated != null) m.cut_off = run.truncated ? 1 : 0;
  const refs = [...stepRefs(text)];
  if (run.condition !== "narrative") {
    m.cites_steps = refs.length ? 1 : 0;
    m.step_refs_valid = refs.length
      ? refs.filter((n) => n >= 1 && n <= item.steps.length).length /
        refs.length
      : NaN;
  }
  if (kbSymbols.size) {
    m.unsupported_symbols = unsupportedSymbols(
      text,
      kbSymbols,
      proofText(item),
    ).size;
    m.invented_symbols = inventedSymbols(text, kbSymbols, proofText(item)).size;
  }
  if (item.kind === "contradiction") {
    const t = item.truth;
    m.culprit_cited = citedLocations(text).has(`${t.file}:${t.line}`) ? 1 : 0;
    const formulas = extractKif(text);
    const repairs = formulas.filter((f) =>
      isRepairOf(f, t.mutated, t.original),
    );
    m.repair_proposed = repairs.length ? 1 : 0;
    m.fix_exact = repairs.some((f) => sameKif(f, t.original)) ? 1 : 0;
    if (opt.verify && kb) {
      let ok = false;
      for (const f of repairs) if ((ok = await resolves(item, f))) break;
      m.fix_resolves = ok ? 1 : 0;
    }
  }
  return m;
}

// -- judge ----------------------------------------------------------------------

const judgePath = join(opt.out, "judge.jsonl");
const judged = new Map(
  readJsonl(judgePath).map((j) => [`${j.key}|${j.model}`, j]),
);

function judgeMessages(item, run) {
  const req = ex.toExplainRequest(item.kind, item.steps, item.query);
  const proof = ex
    .buildMessages(req, {
      format: "both",
      paraphrases: item.paraphrases,
      queryParaphrase: item.queryParaphrase,
    })[1]
    .content.replace(/^PROOF DATA\n/, "");
  const truth = item.truth
    ? `\nGround truth: the axiom at ${item.truth.file}:${item.truth.line} was corrupted from ${item.truth.original} to ${item.truth.mutated}. A good fix restores or equivalently repairs it.\n`
    : "";
  return [
    {
      role: "system",
      content: [
        "You grade explanations of machine-generated SUMO/SUO-KIF proofs. Be strict and consistent.",
        "Score each criterion from 1 (poor) to 5 (excellent):",
        "faithfulness: every claim is supported by the proof; no invented axioms, steps or symbols.",
        "coverage: the key premises and the decisive inference are explained.",
        "clarity: a reader who knows SUMO can follow it.",
        item.kind === "contradiction"
          ? "fix_quality: the proposed fixes target the corrupted axiom and would repair it with minimal change."
          : "fix_quality: null (not applicable).",
        'Reply with only a JSON object: {"faithfulness": n, "coverage": n, "clarity": n, "fix_quality": n or null}.',
      ].join("\n"),
    },
    {
      role: "user",
      content: `Proof (${item.kind}):\n${proof}\n${truth}\nExplanation to grade:\n<<<\n${run.text}\n>>>`,
    },
  ];
}

async function judge(item, run, budget) {
  const id = `${run.key}|${opt.judgeModel}`;
  if (judged.has(id)) return judged.get(id);
  const account = process.env.CLOUDFLARE_ACCOUNT_ID;
  const token = process.env.CLOUDFLARE_API_TOKEN;
  if (!account || !token)
    throw new Error("set CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN");
  const messages = judgeMessages(item, run);
  const chars = messages.reduce((n, m) => n + m.content.length, 0);
  const reserve = ex.neurons(opt.judgeModel, ex.estimateTokens(chars), 1000);
  if (budget.spent + reserve > opt.judgeMaxNeurons) return null;
  const res = await fetch(
    `https://api.cloudflare.com/client/v4/accounts/${account}/ai/run/${opt.judgeModel}`,
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ messages, max_tokens: 1000, temperature: 0 }),
    },
  );
  const body = await res.json().catch(() => ({}));
  if (!res.ok || body.success === false) {
    log(
      `judge ${run.key}: HTTP ${res.status} ${JSON.stringify(body.errors ?? "").slice(0, 200)}`,
    );
    return null;
  }
  const out = ex.readModelOutput(body.result ?? body);
  budget.spent += ex.neurons(
    opt.judgeModel,
    out.inputTokens ?? ex.estimateTokens(chars),
    out.outputTokens ?? ex.estimateTokens(out.text.length),
  );
  let grades = null;
  try {
    grades = JSON.parse(out.text.match(/\{[\s\S]*\}/)?.[0] ?? "");
  } catch {
    /* unparseable: recorded as null */
  }
  const rec = {
    key: run.key,
    model: opt.judgeModel,
    grades,
    raw: grades ? undefined : out.text,
  };
  judged.set(id, rec);
  appendFileSync(judgePath, JSON.stringify(rec) + "\n");
  return rec;
}

// -- score ------------------------------------------------------------------------

if (opt.verify && kb) {
  const pending = new Set();
  for (const run of answered) {
    const item = corpus.get(run.id);
    if (item.kind !== "contradiction") continue;
    for (const f of extractKif(run.text))
      if (isRepairOf(f, item.truth.mutated, item.truth.original))
        pending.add(`${item.id}|${f}`);
  }
  progress.pending = [...pending].filter((k) => !verified.has(k)).length;
  progress.started = Date.now();
  log(
    `${pending.size} distinct fixes to verify, ${pending.size - progress.pending} cached; each new one re-audits for up to ${opt.auditTimeLimit}s (--no-verify to skip)`,
  );
}
log(`scoring ${outcomes.length} calls`);
const scores = [];
const budget = { spent: 0 };
for (const run of outcomes) {
  const item = corpus.get(run.id);
  const m = await metrics(run);
  if (opt.judgeModel && m.answered) {
    const j = await judge(item, run, budget);
    for (const [k, v] of Object.entries(j?.grades ?? {}))
      if (typeof v === "number") m[`judge_${k}`] = v;
  }
  scores.push({
    key: run.key,
    id: run.id,
    kind: item.kind,
    condition: run.condition,
    metrics: m,
  });
}
writeFileSync(
  join(opt.out, "scores.jsonl"),
  scores.map((s) => JSON.stringify(s)).join("\n") + "\n",
);

// -- report -----------------------------------------------------------------------

/** Metrics where a smaller value is the better outcome. */
const LOWER_IS_BETTER = new Set([
  "unsupported_symbols",
  "invented_symbols",
  "cut_off",
]);
const fmt = (x, digits = 2) => (Number.isFinite(x) ? x.toFixed(digits) : "-");
const random = rng(opt.seed);
const lines = [
  `# Explanation format experiment`,
  ``,
  `Setting: ${config}.${opt.judgeModel ? ` Judge: ${opt.judgeModel}.` : ""}`,
  `${corpus.size} corpus items; ${outcomes.length} calls: ${answered.length} answered, ${outcomes.length - answered.length} without an answer (scored as failures); ${byKey.size - outcomes.length} HTTP/network errors left out.`,
  ``,
  `Each cell is the mean over items of the per-item mean over samples, with a 95% bootstrap interval. "vs kif" is wins/losses/ties of that condition over kif per item (a win is the better value; lower is better for the *_symbols counts; words is descriptive), and the two-sided sign-test p-value.`,
];
for (const kind of ["contradiction", "ask"]) {
  const ks = scores.filter((s) => s.kind === kind);
  if (!ks.length) continue;
  const conds = CONDITIONS.filter((c) => ks.some((s) => s.condition === c));
  const metricNames = [...new Set(ks.flatMap((s) => Object.keys(s.metrics)))];
  /** Map<item, mean over samples> for one condition and metric. */
  const perItem = (cond, metric) => {
    const acc = new Map();
    for (const s of ks)
      if (s.condition === cond && Number.isFinite(s.metrics[metric])) {
        if (!acc.has(s.id)) acc.set(s.id, []);
        acc.get(s.id).push(s.metrics[metric]);
      }
    return new Map([...acc].map(([k, v]) => [k, mean(v)]));
  };
  lines.push(``, `## ${kind} (${new Set(ks.map((s) => s.id)).size} items)`, ``);
  lines.push(
    `| metric | ${conds.join(" | ")} |`,
    `|---|${conds.map(() => "---").join("|")}|`,
  );
  for (const metric of metricNames) {
    const base = perItem("kif", metric);
    const cells = conds.map((c) => {
      const v = perItem(c, metric);
      const xs = [...v.values()];
      const [lo, hi] = bootstrapCi(xs, random);
      let cell = `${fmt(mean(xs))} [${fmt(lo)}, ${fmt(hi)}]`;
      if (c !== "kif" && base.size) {
        const p = LOWER_IS_BETTER.has(metric)
          ? paired(base, v)
          : paired(v, base);
        cell += ` (${p.wins}/${p.losses}/${p.ties}, p=${fmt(p.p, 3)})`;
      }
      return cell;
    });
    lines.push(`| ${metric} | ${cells.join(" | ")} |`);
  }
  const cost = conds.map((c) => {
    const rs = outcomes.filter(
      (r) => r.condition === c && corpus.get(r.id).kind === kind,
    );
    return `${fmt(mean(rs.map((r) => r.inputTokens ?? NaN).filter(Number.isFinite)), 0)} in / ${fmt(mean(rs.map((r) => r.outputTokens ?? NaN).filter(Number.isFinite)), 0)} out tokens, ${fmt(mean(rs.map((r) => r.neurons)), 1)} neurons`;
  });
  lines.push(`| cost per call | ${cost.join(" | ")} |`);
}
lines.push(
  ``,
  `Metrics: answered = returned an answer at all (a reasoning model can spend its whole budget thinking); cut_off = the answer hit the token cap; culprit_cited = names the planted axiom's file:line; repair_proposed = a formula with the planted axiom's predicate and an argument in common; fix_exact = restores the original; fix_resolves = substituting a proposed repair removes the contradiction at that line (prover-checked); cites_steps / step_refs_valid = step numbers used and in range (not applicable to narrative, which has its own numbering); unsupported_symbols = KB symbols in the explanation (before the fixes) that appear nowhere in the proof; invented_symbols = CamelCase names there that are not KB symbols at all; judge_* = 1-5 grades.`,
);
const report = lines.join("\n") + "\n";
writeFileSync(join(opt.out, "report.md"), report);
console.log(report);
