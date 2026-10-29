// Pure helpers for the proof-explanation format experiment (see README.md in
// this directory): loading the shared prompt module, top-level KIF forms with
// their lines and byte offsets, planted-fault candidates, answer analysis,
// and paired statistics. No SDK, no network -- unit-tested in lib.test.mjs.

import { readFileSync } from "node:fs";
import ts from "typescript";

/** Load a dependency-free TypeScript module (functions/_utils/explain.ts) as
 *  CommonJS exports, the same way the repo's node tests do. */
export function loadTs(url) {
  const out = ts.transpileModule(readFileSync(url, "utf8"), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
  const exports = {};
  new Function("exports", out)(exports);
  return exports;
}

export const explainModule = () =>
  loadTs(new URL("../../functions/_utils/explain.ts", import.meta.url));

export const CONDITIONS = ["kif", "paraphrase", "both", "narrative"];

// -- KIF forms ----------------------------------------------------------------

const utf8Len = (cp) => (cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4);

/** Top-level parenthesized forms of a KIF file: `{kif, line, offset, end}`
 *  with 1-based `line` and UTF-8 byte `offset`/`end` (what
 *  `auditConsistency({at})` takes), skipping `;` comments and strings. */
export function topLevelForms(text) {
  const forms = [];
  let depth = 0;
  let inString = false;
  let inComment = false;
  let line = 1;
  let bytes = 0;
  let start = null;
  for (let i = 0; i < text.length; i++) {
    const cp = text.codePointAt(i);
    const c = text[i];
    if (cp > 0xffff) i++;
    const here = bytes;
    bytes += utf8Len(cp);
    if (c === "\n") {
      line++;
      inComment = false;
      continue;
    }
    if (inComment) continue;
    if (inString) {
      if (c === "\\") {
        bytes += utf8Len(text.codePointAt(i + 1) ?? 0);
        i++;
      } else if (c === '"') inString = false;
      continue;
    }
    if (c === ";") inComment = true;
    else if (c === '"') inString = true;
    else if (c === "(") {
      if (depth === 0) start = { i, line, offset: here };
      depth++;
    } else if (c === ")" && depth > 0 && --depth === 0) {
      forms.push({
        kif: text.slice(start.i, i + 1),
        line: start.line,
        offset: start.offset,
        startIndex: start.i,
        endIndex: i + 1,
      });
    }
  }
  return forms;
}

/** `(head a b ...)` with only atom arguments (no nesting, variables or
 *  strings) as `{head, args}`, else null. */
export function flatAtom(kif) {
  const m = kif.match(/^\(\s*([^\s()"?@]+)((?:\s+[^\s()"?@]+)*)\s*\)$/);
  if (!m) return null;
  return { head: m[1], args: m[2].trim() ? m[2].trim().split(/\s+/) : [] };
}

const DISJOINT_LISTS = new Set(["partition", "disjointDecomposition"]);

/** Taxonomy facts the fault generator needs, from every file's forms. */
export function ontologyFacts(filesForms) {
  const parents = new Map();
  const classesOf = new Map();
  const disjoint = new Map();
  const domains = new Map();
  const add = (map, k, v) => {
    if (!map.has(k)) map.set(k, new Set());
    map.get(k).add(v);
  };
  const addDisjoint = (a, b) => {
    if (a === b) return;
    add(disjoint, a, b);
    add(disjoint, b, a);
  };
  for (const forms of filesForms) {
    for (const f of forms) {
      const a = flatAtom(f.kif);
      if (!a) continue;
      const [x, y, z] = a.args;
      if (a.head === "subclass" && a.args.length === 2) add(parents, x, y);
      else if (a.head === "instance" && a.args.length === 2)
        add(classesOf, x, y);
      else if (a.head === "disjoint" && a.args.length === 2) addDisjoint(x, y);
      else if (DISJOINT_LISTS.has(a.head) && a.args.length >= 3) {
        const parts = a.args.slice(1);
        for (let i = 0; i < parts.length; i++)
          for (let j = i + 1; j < parts.length; j++)
            addDisjoint(parts[i], parts[j]);
      } else if (a.head === "domain" && a.args.length === 3 && /^\d+$/.test(y))
        domains.set(`${x}#${y}`, z);
    }
  }
  return { parents, classesOf, disjoint, domains };
}

/** `cls` and all its `subclass` ancestors. */
export function ancestors(facts, cls) {
  const seen = new Set([cls]);
  const stack = [cls];
  while (stack.length) {
    for (const p of facts.parents.get(stack.pop()) ?? [])
      if (!seen.has(p)) {
        seen.add(p);
        stack.push(p);
      }
  }
  return seen;
}

/** Classes disjoint from `cls` or from one of its ancestors. */
export function disjointFrom(facts, cls) {
  const out = new Set();
  for (const a of ancestors(facts, cls))
    for (const d of facts.disjoint.get(a) ?? []) out.add(d);
  return out;
}

/** Deterministic PRNG (mulberry32). */
export function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function shuffle(xs, random) {
  const a = [...xs];
  for (let i = a.length - 1; i > 0; i--) {
    const j = Math.floor(random() * (i + 1));
    [a[i], a[j]] = [a[j], a[i]];
  }
  return a;
}

const pick = (set, random) => {
  const xs = [...set];
  return xs.length ? xs[Math.floor(random() * xs.length)] : null;
};

export const FAULT_TYPES = [
  "instance-class",
  "relation-class",
  "subclass-parent",
  "arg-swap",
  "disjoint-ancestor",
];

/**
 * Planted-fault candidates for one file: single ground facts rewritten the
 * way a careless edit would, each `{type, line, offset, startIndex, endIndex,
 * original, mutated}`. Whether a candidate actually yields a contradiction is
 * for the prover to decide (corpus.mjs keeps only those it confirms).
 *
 * - instance-class: `(instance I C)` -> `(instance I D)`, D disjoint from C's
 *   lineage; `relation-class` when C is a kind of Relation (an arity or
 *   relation-type slip, by far the most common confirmed kind, so it is
 *   balanced separately).
 * - subclass-parent: `(subclass A B)` -> `(subclass A D)`, same.
 * - arg-swap: `(R a b)` -> `(R b a)` where R's two domains are disjoint.
 * - disjoint-ancestor: `(disjoint A B)` -> `(disjoint A P)`, P an ancestor of
 *   A, emptying A.
 */
export function faultCandidates(forms, facts, random) {
  const out = [];
  for (const f of forms) {
    const a = flatAtom(f.kif);
    if (!a || a.args.length !== 2) continue;
    const [x, y] = a.args;
    let type = null;
    let mutated = null;
    if (a.head === "instance" || a.head === "subclass") {
      const d = pick(disjointFrom(facts, y), random);
      if (d) {
        type =
          a.head === "subclass"
            ? "subclass-parent"
            : ancestors(facts, y).has("Relation")
              ? "relation-class"
              : "instance-class";
        mutated = `(${a.head} ${x} ${d})`;
      }
    } else if (a.head === "disjoint") {
      const anc = [...ancestors(facts, x)].filter((p) => p !== x && p !== y);
      const p = pick(new Set(anc), random);
      if (p) {
        type = "disjoint-ancestor";
        mutated = `(disjoint ${x} ${p})`;
      }
    } else if (x !== y) {
      /** Does `atom`, placed where `domain` is required, contradict a
       *  class it is already known to have? */
      const misfits = (atom, domain) => {
        if (!domain) return false;
        const need = ancestors(facts, domain);
        return [domain, ...(facts.classesOf.get(atom) ?? [])].some(
          (k, i) =>
            i > 0 && [...disjointFrom(facts, k)].some((d) => need.has(d)),
        );
      };
      const d1 = facts.domains.get(`${a.head}#1`);
      const d2 = facts.domains.get(`${a.head}#2`);
      const clash = d1 && d2 && disjointFrom(facts, d1);
      if (
        (clash && [...ancestors(facts, d2)].some((c) => clash.has(c))) ||
        misfits(x, d2) ||
        misfits(y, d1)
      ) {
        type = "arg-swap";
        mutated = `(${a.head} ${y} ${x})`;
      }
    }
    if (type)
      out.push({
        type,
        line: f.line,
        offset: f.offset,
        startIndex: f.startIndex,
        endIndex: f.endIndex,
        original: f.kif,
        mutated,
      });
  }
  return out;
}

/** Candidates interleaved across fault types, each type in seeded order, so
 *  a capped corpus stays balanced. */
export function balanced(candidates, random) {
  const byType = new Map(FAULT_TYPES.map((t) => [t, []]));
  for (const c of candidates) byType.get(c.type).push(c);
  const queues = [...byType.values()].map((xs) => shuffle(xs, random));
  const out = [];
  for (let i = 0; queues.some((q) => i < q.length); i++)
    for (const q of queues) if (i < q.length) out.push(q[i]);
  return out;
}

// -- answer analysis ----------------------------------------------------------

/** Balanced `(head ...)` expressions in free text, outermost only. */
export function extractKif(text) {
  const out = [];
  for (let i = 0; i < text.length; i++) {
    if (text[i] !== "(" || !/[A-Za-z]/.test(text[i + 1] ?? "")) continue;
    let depth = 0;
    let j = i;
    for (; j < text.length; j++) {
      if (text[j] === "(") depth++;
      else if (text[j] === ")" && --depth === 0) break;
      else if (text[j] === "\n" && text[j + 1] === "\n") break;
    }
    if (depth !== 0 || j >= text.length) continue;
    const kif = text.slice(i, j + 1);
    const bare = kif.replace(/"(?:[^"\\]|\\.)*"/g, '""');
    if (
      /^\([A-Za-z][\w-]*\s+\S/.test(kif) &&
      /^[A-Za-z0-9_?@\s()=<>.+*"-]*$/.test(bare) &&
      !/\.\s/.test(bare)
    ) {
      out.push(
        kif.replace(/\s+/g, " ").replace(/\( /g, "(").replace(/ \)/g, ")"),
      );
      i = j;
    }
  }
  return [...new Set(out)];
}

/** `file:line` (or `file line N`) citations in the answer. */
export function citedLocations(text) {
  const out = new Set();
  for (const m of text.matchAll(
    /([\w.()-]+\.kif)`?\s*(?::|,?\s*(?:at\s+)?line\s+)(\d+)/gi,
  ))
    out.add(`${m[1]}:${m[2]}`);
  return out;
}

/** Step numbers the answer cites: `step 3`, `steps 4 and 5`, `steps 2-4`,
 *  `[7]`. */
export function stepRefs(text) {
  const out = new Set();
  for (const m of text.matchAll(
    /\bsteps?\s+(\d+(?:\s*(?:,|and|&|-|–|to)\s*\d+)*)/gi,
  )) {
    const nums = m[1].split(/\s*(?:,|and|&)\s*/);
    for (const part of nums) {
      const r = part.split(/\s*(?:-|–|to)\s*/).map(Number);
      if (r.length === 2 && r[1] >= r[0] && r[1] - r[0] < 100)
        for (let n = r[0]; n <= r[1]; n++) out.add(n);
      else r.forEach((n) => out.add(n));
    }
  }
  for (const m of text.matchAll(/\[(\d+)\]/g)) out.add(Number(m[1]));
  return out;
}

/** Text before the fixes section, where every claim should come from the
 *  proof (proposed fixes may legitimately name new symbols). */
export function explanationPart(text) {
  const i = text.search(/\*\*\s*Possible fixes/i);
  return i >= 0 ? text.slice(0, i) : text;
}

const SYMBOL = /[A-Za-z][A-Za-z0-9_-]*/g;
const withoutStrings = (kif) => kif.replace(/"(?:[^"\\]|\\.)*"/g, " ");

/** Every symbol used in a formula (comments and string literals excluded). */
export function kbSymbolSet(texts) {
  const out = new Set();
  for (const text of texts)
    for (const f of topLevelForms(text))
      for (const t of withoutStrings(f.kif).match(SYMBOL) ?? []) out.add(t);
  return out;
}

/** Symbol-like tokens of an explanation (before the fixes, file citations
 *  removed): CamelCase or lowerCamel names anywhere, and any name written as
 *  code in backticks. A plain capitalized word ("Park", "View") is left out:
 *  it is as likely English as a SUMO class. */
function symbolTokens(text) {
  const body = explanationPart(text).replace(/[\w.()-]+\.kif(?::\d+)?/g, " ");
  const out = new Set(
    body.match(/\b[A-Za-z][a-z0-9]+(?:[A-Z][a-z0-9]*)+\b/g) ?? [],
  );
  for (const m of body.matchAll(/`([^`\n]+)`/g))
    for (const t of m[1].match(SYMBOL) ?? []) if (t.length > 1) out.add(t);
  return out;
}

const proofSymbols = (proofText) =>
  new Set(withoutStrings(proofText).match(SYMBOL) ?? []);

/** KB symbols the explanation names that appear nowhere in the proof or
 *  query: claims from outside the proof. */
export function unsupportedSymbols(text, kbSymbols, proofText) {
  const inProof = proofSymbols(proofText);
  return new Set(
    [...symbolTokens(text)].filter((t) => kbSymbols.has(t) && !inProof.has(t)),
  );
}

/** CamelCase names in the explanation that are neither KB symbols nor in
 *  the proof: invented terms. */
export function inventedSymbols(text, kbSymbols, proofText = "") {
  const inProof = proofSymbols(proofText);
  return new Set(
    [...symbolTokens(text)].filter(
      (t) => /[A-Z]/.test(t.slice(1)) && !kbSymbols.has(t) && !inProof.has(t),
    ),
  );
}

/** Whether `kif` is a plausible repair of `mutated`: the same predicate,
 *  sharing at least one argument with the planted fact or the original. */
export function isRepairOf(kif, mutated, original) {
  const k = flatAtom(kif);
  const m = flatAtom(mutated);
  if (!k || !m || k.head !== m.head) return false;
  const known = new Set([...m.args, ...(flatAtom(original)?.args ?? [])]);
  return k.args.some((a) => known.has(a));
}

export const sameKif = (a, b) =>
  a.replace(/\s+/g, " ").replace(/\( /g, "(").replace(/ \)/g, ")").trim() ===
  b.replace(/\s+/g, " ").replace(/\( /g, "(").replace(/ \)/g, ")").trim();

// -- statistics ---------------------------------------------------------------

export const mean = (xs) =>
  xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : NaN;

/** 95% percentile-bootstrap interval of the mean. */
export function bootstrapCi(xs, random, rounds = 2000) {
  if (xs.length < 2) return [NaN, NaN];
  const means = [];
  for (let r = 0; r < rounds; r++) {
    let s = 0;
    for (let i = 0; i < xs.length; i++)
      s += xs[Math.floor(random() * xs.length)];
    means.push(s / xs.length);
  }
  means.sort((a, b) => a - b);
  return [means[Math.floor(rounds * 0.025)], means[Math.floor(rounds * 0.975)]];
}

/** Two-sided exact sign test p-value for `wins` vs `losses` (ties dropped). */
export function signTest(wins, losses) {
  const n = wins + losses;
  if (!n) return 1;
  const k = Math.min(wins, losses);
  let logC = 0;
  let tail = 0;
  for (let i = 0; i <= k; i++) {
    if (i > 0) logC += Math.log(n - i + 1) - Math.log(i);
    tail += Math.exp(logC - n * Math.LN2);
  }
  return Math.min(1, 2 * tail);
}

/** Paired comparison of `a` against `b` over shared keys of two
 *  `Map<item, value>`: wins/losses/ties of a over b and the sign-test p. */
export function paired(a, b) {
  let wins = 0;
  let losses = 0;
  let ties = 0;
  for (const [k, va] of a) {
    if (!b.has(k)) continue;
    const vb = b.get(k);
    if (Number.isNaN(va) || Number.isNaN(vb)) continue;
    if (va > vb + 1e-9) wins++;
    else if (vb > va + 1e-9) losses++;
    else ties++;
  }
  return { wins, losses, ties, p: signTest(wins, losses) };
}

// -- files --------------------------------------------------------------------

export function readJsonl(path) {
  try {
    return readFileSync(path, "utf8")
      .split("\n")
      .filter(Boolean)
      .map((l) => JSON.parse(l));
  } catch (e) {
    if (e.code === "ENOENT") return [];
    throw e;
  }
}

/** `--name value` / `--flag` command-line options into an object. */
export function parseArgs(argv, defaults) {
  const out = { ...defaults };
  for (let i = 0; i < argv.length; i++) {
    const m = argv[i].match(/^--([\w-]+)(?:=(.*))?$/);
    if (!m) throw new Error(`unexpected argument: ${argv[i]}`);
    const key = m[1].replace(/-(\w)/g, (_, c) => c.toUpperCase());
    if (!(key in defaults)) throw new Error(`unknown option --${m[1]}`);
    if (typeof defaults[key] === "boolean") out[key] = m[2] !== "false";
    else {
      const v = m[2] ?? argv[++i];
      if (v === undefined) throw new Error(`--${m[1]} needs a value`);
      out[key] = typeof defaults[key] === "number" ? Number(v) : v;
    }
  }
  return out;
}
