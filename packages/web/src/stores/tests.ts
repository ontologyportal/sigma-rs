/**
 * Tests: `.kif.tq` (SUO-KIF harness directives) and `.p`/`.tptp` (standalone
 * TPTP problems, parsed via `parseTptpTest`).
 *
 * Tests live in the same library as the constituents (repo catalogs, local
 * uploads, URLs) but are a separate collection: a test's (query ...) must
 * never be ingested as an axiom. Running one reuses the prove pipeline with
 * the test's own axioms, query, and (time N) budget. This store owns the
 * imported-tests collection and its localStorage persistence; the Problems
 * tab imports/removes/runs, the Ask/Tell tab opens a test into its panes.
 */

import { defineStore } from "pinia";
import { TQ_SETTING } from "../constants";
import {
  LocalOrigin,
  Origin,
  OriginJson,
  originId,
  parseOrigin,
  serializeOrigin,
} from "../models/Origin";
import { call } from "../services/sigma";
import { fromOrigin } from "../services/sources";
import { errMsg } from "../utils/format";
import { useLibraryStore } from "./library";
import { useProverStore } from "./prover";

export interface TestOutcome {
  cls: "ok" | "bad" | "mut";
  label: string;
  status?: string;
}

export interface TestEntry {
  name: string;
  origin: Origin;
  text: string;
  /** The worker's `TestCaseView`. */
  parsed: any;
  outcome: TestOutcome | null;
}

/** What's mirrored to localStorage -- enough to refetch the test's text on
 *  the next boot (the same shape the KB uses for constituents). */
export interface SavedTest {
  name: string;
  origin: OriginJson;
}

export type TestDialect = "kif" | "tptp";

export const isTestFile = (name: string) => /\.(tq|p|tptp)$/i.test(name);

/** `.kif.tq` files are KIF harness tests; `.p` / `.tptp` are TPTP problems. */
export const testDialect = (name: string): TestDialect =>
  /\.tq$/i.test(name) ? "kif" : "tptp";

/** The RPC that understands `name`'s dialect: `.kif.tq` (SUO-KIF harness
 *  directives) vs `.p`/`.tptp` (a standalone TPTP problem -- see
 *  `parse_tptp_test_content` in core). Both return the same `TestCaseView`
 *  shape (KIF text either way; a TPTP test's theory/conjecture come back
 *  translated). */
function testParseRpc(name: string) {
  return testDialect(name) === "kif" ? "parseTest" : "parseTptpTest";
}

function loadSavedTests(): SavedTest[] {
  try {
    const raw = JSON.parse(localStorage.getItem(TQ_SETTING) || "[]");
    if (!Array.isArray(raw)) return [];
    // Entries saved before the library carried a bare kind string.
    return raw
      .filter((t: any) => t && typeof t.name === "string")
      .map((t: any) => ({
        name: t.name,
        origin: serializeOrigin(parseOrigin(t.origin, t.name)),
      }));
  } catch {
    return [];
  }
}

function gradeTest(parsed: any, result: any): TestOutcome {
  const exp = parsed.expectedProof;
  const conclusiveNo = [
    "Disproved",
    "CounterSatisfiable",
    "Consistent",
  ].includes(result.status);
  if (exp === true) {
    return result.proved
      ? { cls: "ok", label: "pass" }
      : { cls: "bad", label: `no proof (${result.status})` };
  }
  if (exp === false) {
    if (result.proved) return { cls: "bad", label: "proved - expected no" };
    return conclusiveNo
      ? { cls: "ok", label: "pass" }
      : { cls: "mut", label: result.status };
  }
  return { cls: "mut", label: result.status }; // no yes/no expectation: informational
}

export const useTestsStore = defineStore("tests", {
  state: () => ({
    tests: [] as TestEntry[],
    /** Mirrored to localStorage -- what the next boot re-imports. */
    saved: loadSavedTests(),
    /** The test currently loaded into the Ask/Tell panes, if any. Set by
     *  opening or saving-as a test; cleared if that test is removed. */
    openTest: null as SavedTest | null,
  }),
  getters: {
    /** The editor mode a test opens in: `.tq` = kif, else tptp. */
    dialect: () => testDialect,
    find:
      (state) =>
      (name: string): TestEntry | undefined =>
        state.tests.find((t) => t.name === name),
  },
  actions: {
    persistSaved() {
      localStorage.setItem(TQ_SETTING, JSON.stringify(this.saved));
    },

    /** Parse and import a test. Nothing is added on a parse failure. */
    async add(
      name: string,
      text: string,
      origin: Origin,
    ): Promise<{ added: boolean; notices: string[] }> {
      if (this.tests.some((t) => t.name === name)) {
        return { added: false, notices: [`${name}: already imported`] };
      }
      const { test } = await call(testParseRpc(name), { name, text });
      this.tests.push({ name, origin, text, parsed: test, outcome: null });
      const id = originId(origin);
      if (
        !this.saved.some((t) => t.name === name && originId(t.origin) === id)
      ) {
        this.saved.push({ name, origin: serializeOrigin(origin) });
        this.persistSaved();
      }
      return { added: true, notices: [] };
    },

    /** Drop a test from the imported set. Its library copy stays (removing
     *  is not deleting). */
    async remove(name: string, id: string) {
      const gone = (t: { name: string; origin: Origin | OriginJson }) =>
        t.name === name && originId(t.origin) === id;
      this.tests = this.tests.filter((t) => !gone(t));
      this.saved = this.saved.filter((t) => !gone(t));
      this.persistSaved();
      if (this.openTest && gone(this.openTest)) this.openTest = null;
    },

    /** Boot: re-import every saved test, best-effort per test. */
    async restore() {
      for (const { name, origin: json } of this.saved) {
        try {
          const origin = parseOrigin(json, name);
          await this.add(name, await fromOrigin(name, origin), origin);
        } catch (e) {
          console.warn(`test ${name}: ${errMsg(e)}`);
        }
      }
    },

    setOpen(t: { name: string; origin: Origin } | null) {
      this.openTest = t
        ? { name: t.name, origin: serializeOrigin(t.origin) }
        : null;
    },

    /** Save the Ask/Tell panes as a test: `text` is the `.kif.tq` the caller
     *  built (via `formatTest`) or the raw TPTP problem, per `dialect`.
     *  Overwrites the currently open test in place when it is a local
     *  library file of the same dialect (round-trips cleanly, same format);
     *  otherwise -- nothing open, a different dialect, or a read-only origin
     *  (`sumo`/`url`) that can't be written back -- prompts for a new file
     *  name and saves as a new local test. `saved: false` means the user
     *  cancelled the name prompt. */
    async saveCurrent(
      text: string,
      dialect: TestDialect,
    ): Promise<{
      saved: boolean;
      name?: string;
      overwritten?: boolean;
      notices?: string[];
    }> {
      const open = this.openTest;
      const canOverwrite =
        !!open &&
        open.origin.kind === "file" &&
        testDialect(open.name) === dialect;
      const ext = dialect === "kif" ? ".kif.tq" : ".p";
      const matches = (n: string) =>
        testDialect(n) === dialect && isTestFile(n);
      let name = open
        ? open.name
        : dialect === "kif"
          ? "test.kif.tq"
          : "problem.p";
      if (!canOverwrite) {
        if (!matches(name))
          name = name.replace(/\.(kif\.tq|tq|p|tptp)$/i, "") + ext;
        const chosen = (window.prompt("Save test as:", name) || "").trim();
        if (!chosen) return { saved: false };
        name = matches(chosen) ? chosen : `${chosen}${ext}`;
      }
      const origin = new LocalOrigin();
      await useLibraryStore().writeLocal(name, text);
      const existing = this.tests.find(
        (t) => t.name === name && t.origin.kind === "file",
      );
      if (existing) {
        const { test } = await call(testParseRpc(name), { name, text });
        existing.text = text;
        existing.parsed = test;
        existing.outcome = null;
        this.setOpen({ name, origin });
        return { saved: true, name, overwritten: true };
      }
      const r = await this.add(name, text, origin);
      if (r.added) this.setOpen({ name, origin });
      return { saved: r.added, name, overwritten: false, notices: r.notices };
    },

    /** Prove one test with the prover's settings (its own `(time N)` budget
     *  taking precedence) and grade the result into `t.outcome`. */
    async run(t: TestEntry) {
      const config = useProverStore().config(
        t.parsed.timeout ? { timeLimitSecs: t.parsed.timeout } : {},
      );
      const { result } = await call("prove", {
        assertions: t.parsed.axiomKif,
        query: t.parsed.queryKif,
        config,
        session: "__tq_test__",
      });
      t.outcome = { ...gradeTest(t.parsed, result), status: result.status };
    },

    /** Run `list` (default: every imported test) in order, skipping tests
     *  without a query; a throwing run counts as a failure rather than
     *  aborting the rest. */
    async runAll(
      onProgress?: (t: TestEntry) => void,
      list: TestEntry[] = this.tests,
    ): Promise<{ pass: number; ran: number }> {
      let pass = 0;
      let ran = 0;
      for (const t of list) {
        if (!t.parsed.queryKif) continue;
        onProgress?.(t);
        try {
          await this.run(t);
        } catch (err) {
          t.outcome = { cls: "bad", label: errMsg(err).slice(0, 60) };
        }
        ran += 1;
        if (t.outcome?.cls === "ok") pass += 1;
      }
      return { pass, ran };
    },
  },
});
