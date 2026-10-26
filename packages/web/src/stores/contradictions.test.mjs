import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createPinia, setActivePinia, defineStore } from "pinia";

const source = ts.transpileModule(
  readFileSync(new URL("./contradictions.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

const NIGHTLY =
  "https://raw.githubusercontent.com/ontologyportal/sumo/audit-state/.github/latest-contradictions.json";

/** A report in `sumo audit --json` shape; an axiom whose text says MISSING
 *  is one the fake engine treats as absent from the KB. */
const report = (...kifs) =>
  JSON.stringify({
    contradictions: kifs.map((group) => ({
      axioms: group.map((kif) => ({ file: "Merge.kif", line: 1, kif })),
    })),
  });

function fixture({
  urlText = report(["(a)", "(b)"]),
  issues = [],
  issueBodies = {},
  storage = {},
  mirror = null,
} = {}) {
  setActivePinia(createPinia());
  const calls = [];
  // A fake engine: findings keyed by their axiom texts, idempotent import.
  const findings = [];
  const engine = {
    import(json, origin) {
      const obj = JSON.parse(json);
      const entries = obj.contradictions ?? obj.findings ?? [obj];
      const imported = [];
      const rejected = [];
      for (const e of entries) {
        const axioms = e.axioms ?? [];
        const missing = axioms.filter((a) => /MISSING/.test(a.kif));
        if (!axioms.length || missing.length) {
          rejected.push({ axioms, reason: "missing", missing });
          continue;
        }
        const key = axioms
          .map((a) => a.kif)
          .sort()
          .join("+");
        let f = findings.find((x) => x.key === key);
        if (!f) {
          f = {
            key,
            origin,
            culprits: axioms.map((a) => ({ sid: a.kif, ...a })),
            complete: true,
            session: null,
            found_at_ms: e.found_at_ms ?? Date.now(),
            contradiction: { steps: [] },
          };
          findings.unshift(f);
        }
        imported.push(f);
      }
      return { imported, rejected };
    },
  };
  const localStorage = {
    getItem: (k) => (k in storage ? storage[k] : null),
    setItem: (k, v) => {
      storage[k] = v;
    },
  };
  const imports = {
    pinia: { defineStore },
    "../constants": {
      CONTRADICTION_BASELINES_KEY: "baselines",
      CONTRADICTION_PREFS_KEY: "prefs",
      CONTRADICTION_SOURCES_KEY: "sources",
      SUMO: { owner: "ontologyportal", repo: "sumo", branch: "master" },
    },
    "../api/github": {
      searchSumoContradictions: async (page, owner, repo) => {
        calls.push(["search", page, owner, repo]);
        return { issues, hasMore: false, incomplete: false };
      },
      fetchSumoIssue: async (number, owner, repo) => {
        calls.push(["issue", number, owner, repo]);
        if (!(number in issueBodies)) throw new Error("no body");
        return issueBodies[number];
      },
    },
    "../services/sigma": {
      call: async (cmd, args) => {
        calls.push([cmd, args]);
        if (cmd === "auditFindings") return [...findings];
        if (cmd === "importContradictions")
          return engine.import(args.json, args.origin);
        if (cmd === "clearAuditFindings") {
          findings.length = 0;
          return undefined;
        }
        throw new Error(`unexpected ${cmd}`);
      },
    },
    "../services/sources": {
      fetchText: async (url) => {
        calls.push(["fetch", url]);
        if (urlText instanceof Error) throw urlText;
        return urlText;
      },
    },
    "../services/findings-cache": {
      loadFindingsMirror: async () => mirror,
      mirrorReportsByOrigin: (m) => {
        const groups = new Map();
        for (const f of m.findings)
          groups.set(f.origin, [...(groups.get(f.origin) ?? []), f]);
        return new Map(
          [...groups].map(([origin, list]) => [
            origin,
            JSON.stringify({
              contradictions: list.map((f) => ({ axioms: f.axioms })),
            }),
          ]),
        );
      },
      saveFindingsMirror: async (list) => {
        calls.push(["mirror", list.length]);
      },
    },
    // An issue body is the report JSON itself here.
    "../utils/proofReport": {
      proofReportFromIssue: (body) => JSON.parse(body),
    },
    "../utils/format": { errMsg: (e) => e.message },
    "./changes": {
      blobSha: async (text) => createHash("sha1").update(text).digest("hex"),
    },
    "./kb": {},
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name],
    localStorage,
    URL,
    Date,
    console,
  });
  const store = exports.useContradictionsStore();
  const url = (text) => {
    urlText = text;
  };
  return { store, calls, storage, findings, exports, setUrlText: url };
}

const imports = (calls) => calls.filter(([c]) => c === "importContradictions");

test("ships the nightly report and the SUMO issue tracker as watched sources", () => {
  const f = fixture();
  assert.deepEqual(
    JSON.parse(JSON.stringify(f.store.sources.map((s) => [s.kind, s.id]))),
    [
      ["url", `url:${NIGHTLY}`],
      ["github", "github:ontologyportal/sumo"],
    ],
  );
  const [nightly, issues] = f.store.sources;
  assert.equal(f.store.prefFor(nightly), "auto-update");
  assert.equal(f.store.prefFor(issues), "auto-check");
  f.store.cyclePref(nightly);
  assert.equal(f.store.prefFor(nightly), "auto-check");
  assert.match(f.storage.prefs, /auto-check/);
});

test("a URL source imports on first check, then only when the report changes", async () => {
  const f = fixture();
  const [nightly] = f.store.sources;
  await f.store.checkSource(nightly);
  assert.equal(imports(f.calls).length, 1);
  assert.equal(imports(f.calls)[0][1].origin, nightly.id);
  assert.equal(f.store.findings.length, 1);
  assert.ok(f.store.baselines[nightly.id]);
  assert.equal(f.store.alerts.length, 1);
  assert.match(f.store.alerts[0].message, /1 new contradiction recorded/);

  // Same report again: nothing fetched beyond the text, nothing imported.
  f.calls.length = 0;
  await f.store.checkSource(nightly);
  assert.equal(imports(f.calls).length, 0);

  // A changed report with one applicable and one inapplicable entry.
  f.setUrlText(report(["(a)", "(b)"], ["(c)"], ["(MISSING)"]));
  f.calls.length = 0;
  await f.store.checkSource(nightly);
  assert.equal(imports(f.calls).length, 1);
  assert.equal(f.store.findings.length, 2);
  assert.match(f.store.alerts.at(-1).message, /1 new contradiction recorded/);
  assert.match(f.store.alerts.at(-1).message, /1 not applicable/);

  // A manual check always re-imports and always reports.
  f.calls.length = 0;
  f.store.alerts = [];
  await f.store.checkSource(nightly, true);
  assert.equal(imports(f.calls).length, 1);
  assert.match(f.store.alerts[0].message, /0 new contradictions recorded/);
});

test("an auto-check URL source offers the changed report instead of importing it", async () => {
  const f = fixture();
  const [nightly] = f.store.sources;
  f.store.prefs[nightly.id] = "auto-check";
  // The first check has no baseline to compare against and imports.
  await f.store.checkSource(nightly);
  assert.equal(imports(f.calls).length, 1);
  f.setUrlText(report(["(a)", "(b)"], ["(d)"]));
  f.calls.length = 0;
  await f.store.checkSource(nightly);
  assert.equal(imports(f.calls).length, 0);
  const alert = f.store.alerts.find((a) => a.pending);
  assert.ok(alert);
  assert.match(alert.message, /changed since it was last imported/);
  await f.store.applyPending(alert);
  assert.equal(imports(f.calls).length, 1);
  assert.equal(f.store.findings.length, 2);
  assert.ok(!f.store.alerts.some((a) => a.pending));
});

test("a GitHub source reads the open [Contradiction] issues newer than the last seen", async () => {
  const f = fixture({
    issues: [
      { number: 7, state: "open", title: "[Contradiction] x", url: "" },
      { number: 5, state: "open", title: "[Contradiction] y", url: "" },
      { number: 9, state: "closed", title: "[Contradiction] z", url: "" },
    ],
    issueBodies: {
      5: JSON.stringify({
        problem: { command: "audit" },
        axioms: [{ file: "Merge.kif", line: 1, kif: "(e)" }],
      }),
      7: JSON.stringify({
        problem: { command: "prove" },
        axioms: [{ file: "Merge.kif", line: 1, kif: "(f)" }],
      }),
    },
  });
  const tracker = f.store.sources[1];
  f.store.prefs[tracker.id] = "auto-update";
  await f.store.checkSource(tracker);
  assert.deepEqual(
    f.calls.filter(([c]) => c === "issue").map(([, n]) => n),
    [5, 7],
  );
  // Only the audit report is imported, labelled by its issue.
  assert.deepEqual(
    imports(f.calls).map(([, a]) => a.origin),
    ["github:ontologyportal/sumo#5"],
  );
  assert.equal(f.store.baselines[tracker.id], "7");
  f.calls.length = 0;
  await f.store.checkSource(tracker);
  assert.equal(f.calls.filter(([c]) => c === "issue").length, 0);
  assert.equal(imports(f.calls).length, 0);
});

test("an auto-check GitHub source alerts with the new issues pending", async () => {
  const f = fixture({
    issues: [{ number: 3, state: "open", title: "[Contradiction] a", url: "" }],
    issueBodies: {
      3: JSON.stringify({
        problem: { command: "audit" },
        axioms: [{ file: "Merge.kif", line: 1, kif: "(g)" }],
      }),
    },
  });
  const tracker = f.store.sources[1];
  await f.store.checkForUpdates();
  assert.equal(imports(f.calls).length, 1); // the nightly URL, auto-update
  const alert = f.store.alerts.find((a) => a.key === tracker.id);
  assert.match(alert.message, /1 new contradiction issue\./);
  assert.equal(alert.pending.reports.length, 1);
  assert.equal(f.store.changedOnLoad.length, 2);
});

test("sources are added once, removed with their watch state, and persisted", () => {
  const f = fixture();
  const s = f.exports.githubSource("me", "fork");
  f.store.addSource(s);
  f.store.addSource({ ...s });
  assert.equal(f.store.sources.filter((x) => x.id === s.id).length, 1);
  assert.match(f.storage.sources, /github:me\/fork/);
  f.store.prefs[s.id] = "no-check";
  f.store.setBaseline(s.id, "4");
  f.store.removeSource(s.id);
  assert.ok(!f.store.sources.some((x) => x.id === s.id));
  assert.equal(f.store.prefs[s.id], undefined);
  assert.equal(f.store.baselines[s.id], undefined);
  assert.doesNotMatch(f.storage.sources, /github:me\/fork/);
});

test("an empty stored source list is kept, not replaced by the defaults", () => {
  const f = fixture({ storage: { sources: "[]" } });
  assert.equal(f.store.sources.length, 0);
});

test("a one-shot upload imports its report and is listed for provenance", async () => {
  const f = fixture();
  const file = {
    name: "contradictions.json",
    text: async () => report(["(h)"], ["(MISSING)"]),
  };
  await f.store.importFile(file);
  const upload = f.store.sources.find((s) => s.kind === "file");
  assert.equal(upload.label, "contradictions.json");
  assert.equal(f.store.prefFor(upload), "no-check");
  assert.equal(imports(f.calls)[0][1].origin, upload.id);
  assert.equal(f.store.findingsFor(upload).length, 1);
  assert.match(f.store.alerts[0].message, /1 new contradiction recorded/);
  assert.match(f.store.alerts[0].message, /1 not applicable/);
});

test("restoring the mirror re-imports each origin and mirrors the result", async () => {
  const f = fixture({
    mirror: {
      version: 1,
      findings: [
        {
          origin: "native",
          found_at_ms: 1,
          axioms: [{ kif: "(m)" }],
          steps: [],
        },
        {
          origin: "url:x",
          found_at_ms: 2,
          axioms: [{ kif: "(n)" }],
          steps: [],
        },
        {
          origin: "url:x",
          found_at_ms: 3,
          axioms: [{ kif: "(MISSING)" }],
          steps: [],
        },
      ],
    },
  });
  await f.store.restoreFindings();
  assert.deepEqual(
    imports(f.calls)
      .map(([, a]) => a.origin)
      .sort(),
    ["native", "url:x"],
  );
  assert.equal(f.store.findings.length, 2);
  assert.ok(f.calls.some(([c, n]) => c === "mirror" && n === 2));
  assert.equal(f.store.localFindings.length, 1);
});

test("clearing forgets every finding", async () => {
  const f = fixture();
  await f.store.checkSource(f.store.sources[0]);
  assert.equal(f.store.findings.length, 1);
  await f.store.clearFindings();
  assert.equal(f.store.findings.length, 0);
});
