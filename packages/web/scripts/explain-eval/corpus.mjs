// Build the experiment corpus: Ask/Tell proofs from the SUMO inference tests
// and contradictions from planted faults the prover confirms, each with the
// three renderings the conditions need (steps, per-step paraphrases, the
// engine's narrated prose). Writes <out>/corpus.jsonl; reruns append only
// items not already there.
//
//   node scripts/explain-eval/corpus.mjs [--ask 40] [--contradictions 40]
//     [--sumo DIR] [--files a.kif,b.kif] [--tests DIR] [--seed 1] [--out DIR]

import { appendFileSync, mkdirSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseTest } from "sigmakee/sdk";
import { configuredKb, loadKb, reingest } from "./kb.mjs";
import {
  balanced,
  explainModule,
  faultCandidates,
  ontologyFacts,
  parseArgs,
  readJsonl,
  rng,
  topLevelForms,
} from "./lib.mjs";

const conf = configuredKb();
const opt = parseArgs(process.argv.slice(2), {
  ask: 40,
  contradictions: 40,
  sumo: conf?.dir ?? "",
  files: conf?.files.join(",") ?? "Merge.kif,Mid-level-ontology.kif",
  tests: conf?.tests ?? "",
  faultFiles:
    "Merge.kif,Mid-level-ontology.kif,Geography.kif,CountriesAndRegions.kif,Economy.kif,Government.kif",
  maxCandidates: 400,
  timeLimit: 30,
  auditTimeLimit: 10,
  seed: 1,
  out: new URL("./out", import.meta.url).pathname,
});
if (!opt.sumo)
  throw new Error("--sumo DIR is required (no ~/.sigmakee/config.xml)");
mkdirSync(opt.out, { recursive: true });
const corpusPath = join(opt.out, "corpus.jsonl");
const have = new Set(readJsonl(corpusPath).map((i) => i.id));
const log = (...a) => console.error(...a);
const ex = explainModule();

const kb = await loadKb({
  dir: opt.sumo,
  files: opt.files
    .split(",")
    .map((f) => f.trim())
    .filter(Boolean),
  timeLimitSecs: opt.timeLimit,
  log,
});
const { session, language } = kb;

const steps = (proof) =>
  proof.map((s) => ({
    index: s.index,
    rule: s.rule,
    premises: s.premises,
    kif: s.kif,
    file: s.file ?? null,
    line: s.line ?? null,
  }));
const nl = (kif) => {
  try {
    return session.renderNl(kif, language, false).trim();
  } catch {
    return "";
  }
};
/** Same request the web client would send; null when it cannot be. */
const explainable = (kind, st, query) => {
  const req = ex.toExplainRequest(kind, st, query);
  return ex.validateExplainRequest(req).ok ? req : null;
};
const write = (item) => {
  appendFileSync(corpusPath, JSON.stringify(item) + "\n");
  have.add(item.id);
};

// -- Ask/Tell items -----------------------------------------------------------

let asks = [...have].filter((id) => id.startsWith("ask:")).length;
if (opt.ask > asks && opt.tests) {
  const names = readdirSync(opt.tests)
    .filter((n) => n.endsWith(".kif.tq"))
    .sort();
  for (const name of names) {
    if (asks >= opt.ask) break;
    const id = `ask:${name}`;
    if (have.has(id)) continue;
    const t = parseTest(name, readFileSync(join(opt.tests, name), "utf8"));
    if (!t.queryKif || t.expectedProof === false) continue;
    const tag = "eval-ask";
    session.flushSession(tag);
    const told = t.axiomKif?.trim()
      ? session.tell(t.axiomKif, tag)
      : { ok: true };
    if (!told.ok) continue;
    const t0 = Date.now();
    let r;
    try {
      r = session.ask(t.queryKif, { session: tag });
    } catch (e) {
      log(`${name}: ${e}`);
      continue;
    }
    const ms = Date.now() - t0;
    if (r.status !== "Proved" || !r.proof.length) {
      log(`${name}: ${r.status} (${ms} ms), skipped`);
      continue;
    }
    const st = steps(r.proof);
    if (!explainable("ask", st, t.queryKif)) {
      log(`${name}: ${st.length} steps, too large to explain, skipped`);
      continue;
    }
    write({
      id,
      kind: "ask",
      query: t.queryKif,
      queryParaphrase: nl(t.queryKif),
      steps: st,
      paraphrases: st.map((s) => nl(s.kif)),
      narrative: r.prose,
    });
    asks++;
    log(`${name}: proved, ${st.length} steps (${ms} ms) [${asks}/${opt.ask}]`);
  }
  session.flushSession("eval-ask");
}

// -- planted contradictions ---------------------------------------------------

/** The first audit contradiction that cites `file:line`, or null. */
function contradictionAt(file, text, offset, line) {
  session.clearAuditFindings();
  const res = session.auditConsistency({ at: { file, offset }, limit: 3 });
  return (
    res.contradictions.find((c) =>
      c.steps.some((s) => s.file === file && s.line === line),
    ) ?? null
  );
}

let planted = [...have].filter((id) => id.startsWith("fault:")).length;
if (opt.contradictions > planted) {
  session.configure((await import("./kb.mjs")).webConfig(opt.auditTimeLimit));
  const facts = ontologyFacts([...kb.texts.values()].map(topLevelForms));
  const random = rng(opt.seed);
  const pool = [];
  for (const file of opt.faultFiles.split(",").map((f) => f.trim())) {
    const text = kb.texts.get(file);
    if (!text) continue;
    for (const c of faultCandidates(topLevelForms(text), facts, random))
      pool.push({ ...c, file });
  }
  const queue = balanced(pool, random).slice(0, opt.maxCandidates);
  log(`${pool.length} fault candidates; trying up to ${queue.length}`);
  const typesInPool = new Set(pool.map((c) => c.type));
  const perType = Math.ceil(opt.contradictions / Math.max(1, typesInPool.size));
  const confirmedByType = {};
  for (const id of have)
    if (id.startsWith("fault:")) {
      const type = id.split(":").at(-1);
      confirmedByType[type] = (confirmedByType[type] ?? 0) + 1;
    }
  log(
    `pool by type: ${JSON.stringify(Object.fromEntries([...typesInPool].map((t) => [t, pool.filter((c) => c.type === t).length])))}; at most ${perType} per type`,
  );
  let tried = 0;
  const tally = {};
  for (const c of queue) {
    if (planted >= opt.contradictions) break;
    const id = `fault:${c.file}:${c.line}:${c.type}`;
    if (have.has(id) || (confirmedByType[c.type] ?? 0) >= perType) continue;
    tried++;
    const t = (tally[c.type] ??= { tried: 0, preexisting: 0, confirmed: 0 });
    t.tried++;
    const original = kb.texts.get(c.file);
    // A line that is already part of a contradiction is no clean ground truth.
    if (contradictionAt(c.file, original, c.offset, c.line)) {
      t.preexisting++;
      continue;
    }
    const text =
      original.slice(0, c.startIndex) + c.mutated + original.slice(c.endIndex);
    await reingest(kb, c.file, text);
    let found = null;
    try {
      found = contradictionAt(c.file, text, c.offset, c.line);
    } catch (e) {
      log(`${id}: ${e}`);
    }
    if (found) {
      const st = steps(found.steps);
      if (explainable("contradiction", st)) {
        t.confirmed++;
        confirmedByType[c.type] = (confirmedByType[c.type] ?? 0) + 1;
        write({
          id,
          kind: "contradiction",
          steps: st,
          paraphrases: st.map((s) => nl(s.kif)),
          narrative: found.prose,
          truth: {
            type: c.type,
            file: c.file,
            line: c.line,
            original: c.original,
            mutated: c.mutated,
          },
        });
        planted++;
        log(
          `${id}: ${c.original} -> ${c.mutated}, ${st.length} steps [${planted}/${opt.contradictions}]`,
        );
      }
    }
    await reingest(kb, c.file, original);
  }
  session.clearAuditFindings();
  log(`tried ${tried} candidates: ${JSON.stringify(tally)}`);
}
log(`corpus: ${asks} ask, ${planted} contradiction items -> ${corpusPath}`);
