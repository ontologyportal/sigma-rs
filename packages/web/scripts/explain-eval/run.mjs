// Ask the model to explain every corpus item under each condition (how the
// proof is shown) a few times, through the Workers AI REST API. Appends to
// <out>/runs.jsonl and skips calls already recorded, so it resumes after a
// stop, a budget cutoff or a failure. --dry-run prints the prompts of the
// first item and the planned cost without calling anything.
//
//   CLOUDFLARE_ACCOUNT_ID=... CLOUDFLARE_API_TOKEN=... \
//   node scripts/explain-eval/run.mjs [--model @cf/zai-org/glm-5.3-flash]
//     [--conditions kif,paraphrase,both,narrative] [--samples 3]
//     [--max-neurons 9000] [--reasoning-effort low] [--retry-empty]
//     [--gateway ID] [--dry-run] [--out DIR]
//
// A call that comes back without an answer (a reasoning model that spent
// its whole token budget thinking) is a recorded outcome, not a failure, so
// a rerun skips it; --retry-empty sends those again. Only HTTP and network
// errors are retried automatically.

import { appendFileSync } from "node:fs";
import { join } from "node:path";
import { CONDITIONS, explainModule, parseArgs, readJsonl } from "./lib.mjs";

const opt = parseArgs(process.argv.slice(2), {
  model: "@cf/zai-org/glm-5.3-flash",
  conditions: CONDITIONS.join(","),
  samples: 3,
  temperature: 0.2,
  maxTokens: 1500,
  /** low | high | max for reasoning models; "" sends nothing. Defaults to
   *  low for reasoning models (GLM, gpt-oss, Qwen3, DeepSeek, Kimi). */
  reasoningEffort: "auto",
  retryEmpty: false,
  maxNeurons: 9000,
  concurrency: 2,
  kinds: "ask,contradiction",
  limit: 0,
  gateway: "",
  dryRun: false,
  out: new URL("./out", import.meta.url).pathname,
});
const ex = explainModule();
const reasoning = ex.isReasoningModel(opt.model);
const effort =
  opt.reasoningEffort === "auto"
    ? reasoning
      ? "low"
      : ""
    : opt.reasoningEffort;
/** What a run is keyed and scored by: the model plus its reasoning effort,
 *  so answers made under different settings never mix. */
const config = effort ? `${opt.model}@${effort}` : opt.model;
const conditions = opt.conditions.split(",").map((c) => c.trim());
for (const c of conditions)
  if (!CONDITIONS.includes(c)) throw new Error(`unknown condition ${c}`);
const kinds = new Set(opt.kinds.split(","));

let items = readJsonl(join(opt.out, "corpus.jsonl")).filter((i) =>
  kinds.has(i.kind),
);
if (opt.limit) items = items.slice(0, opt.limit);
if (!items.length)
  throw new Error(`no corpus items in ${opt.out}; run corpus.mjs`);

/** The messages one call sends: the production request (same clamping and
 *  validation as the web client) rendered in `condition`'s format. */
export function messagesFor(item, condition) {
  const req = ex.toExplainRequest(item.kind, item.steps, item.query);
  return ex.buildMessages(req, {
    format: condition,
    paraphrases: item.paraphrases,
    queryParaphrase: item.queryParaphrase,
    narrative: item.narrative,
  });
}

const runsPath = join(opt.out, "runs.jsonl");
const previous = readJsonl(runsPath);
/** Older records marked a missing answer as an error. */
const noAnswer = (r) => r.noAnswer || r.error === "empty response";
const done = new Set(
  previous
    .filter((r) => (noAnswer(r) ? !opt.retryEmpty : !r.error))
    .map((r) => r.key),
);
const jobs = [];
for (const item of items)
  for (const condition of conditions)
    for (let sample = 0; sample < opt.samples; sample++) {
      const key = `${item.id}|${condition}|${sample}|${config}`;
      if (!done.has(key)) jobs.push({ key, item, condition, sample });
    }

const promptChars = (msgs) => msgs.reduce((n, m) => n + m.content.length, 0);
const estimate = (job) =>
  ex.neurons(
    opt.model,
    ex.estimateTokens(promptChars(messagesFor(job.item, job.condition))),
    opt.maxTokens,
  );
const planned = jobs.reduce((n, j) => n + estimate(j), 0);
const mine = previous.filter((r) => (r.config ?? r.model) === config);
const others = new Set(
  previous.map((r) => r.config ?? r.model).filter((c) => c !== config),
);
console.error(
  `${config}: ${new Set(mine.filter((r) => done.has(r.key)).map((r) => r.key)).size} calls already recorded (${new Set(mine.filter(noAnswer).map((r) => r.key)).size} without an answer${opt.retryEmpty ? ", retrying those" : ", kept; --retry-empty to resend"})${others.size ? `; other settings in this folder: ${[...others].join(", ")}` : ""}`,
);
console.error(
  `${items.length} items x ${conditions.length} conditions x ${opt.samples} samples: ${jobs.length} calls to make, at most ~${planned} neurons (budget ${opt.maxNeurons} this run)`,
);

if (opt.dryRun) {
  for (const condition of conditions) {
    console.log(`\n===== ${condition} (${items[0].id}) =====`);
    for (const m of messagesFor(items[0], condition))
      console.log(`--- ${m.role}\n${m.content}`);
  }
  process.exit(0);
}

const account = process.env.CLOUDFLARE_ACCOUNT_ID;
const token = process.env.CLOUDFLARE_API_TOKEN;
if (!account || !token)
  throw new Error(
    "set CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN (see README.md)",
  );
const url = opt.gateway
  ? `https://gateway.ai.cloudflare.com/v1/${account}/${opt.gateway}/workers-ai/${opt.model}`
  : `https://api.cloudflare.com/client/v4/accounts/${account}/ai/run/${opt.model}`;

async function call(messages) {
  for (let attempt = 0; ; attempt++) {
    const res = await fetch(url, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(
        ex.modelInputs(opt.model, messages, {
          maxTokens: opt.maxTokens,
          temperature: opt.temperature,
          reasoningEffort: effort,
        }),
      ),
    });
    const body = await res.json().catch(() => ({}));
    if (res.ok && body.success !== false) return body.result ?? body;
    const msg = JSON.stringify(body.errors ?? body).slice(0, 300);
    const retry = res.status === 429 || res.status >= 500;
    if (!retry || attempt >= 4) throw new Error(`HTTP ${res.status}: ${msg}`);
    await new Promise((r) => setTimeout(r, 2000 * 2 ** attempt));
  }
}

let spent = 0;
let made = 0;
let stopped = false;
async function worker() {
  while (jobs.length && !stopped) {
    const job = jobs.shift();
    const reserve = estimate(job);
    if (spent + reserve > opt.maxNeurons) {
      stopped = true;
      break;
    }
    spent += reserve;
    const messages = messagesFor(job.item, job.condition);
    const t0 = Date.now();
    const rec = {
      key: job.key,
      id: job.item.id,
      kind: job.item.kind,
      condition: job.condition,
      sample: job.sample,
      model: opt.model,
      config,
      promptChars: promptChars(messages),
    };
    try {
      const out = ex.readModelOutput(await call(messages));
      const cost = ex.neurons(
        opt.model,
        out.inputTokens ?? ex.estimateTokens(rec.promptChars),
        out.outputTokens ?? ex.estimateTokens(out.text.length),
      );
      spent += cost - reserve;
      Object.assign(rec, {
        text: out.text,
        inputTokens: out.inputTokens ?? null,
        outputTokens: out.outputTokens ?? null,
        neurons: cost,
      });
      rec.truncated = (out.outputTokens ?? 0) >= opt.maxTokens;
      if (!out.text) rec.noAnswer = true;
    } catch (e) {
      rec.error = String(e.message ?? e);
      if (/\b4006\b|free allocation/i.test(rec.error)) stopped = true;
    }
    rec.ms = Date.now() - t0;
    appendFileSync(runsPath, JSON.stringify(rec) + "\n");
    made++;
    console.error(
      `${made} ${job.key} ${rec.error ? `ERROR ${rec.error}` : rec.noAnswer ? `NO ANSWER (${rec.outputTokens ?? "?"} tokens spent reasoning)` : `${rec.outputTokens ?? "?"} out tokens${rec.truncated ? ", cut off" : ""}`} (~${spent} neurons)`,
    );
  }
}
await Promise.all(Array.from({ length: opt.concurrency }, worker));
console.error(
  `${made} calls, ~${spent} neurons${stopped ? `; stopped with ${jobs.length + (stopped ? 1 : 0)} left (budget or free allocation reached, rerun to resume)` : ""}`,
);
