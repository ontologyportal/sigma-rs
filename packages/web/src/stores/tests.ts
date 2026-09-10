/**
 * Tests: `.kif.tq` (SUO-KIF harness directives) and `.p`/`.tptp` (standalone
 * TPTP problems, parsed via `parseTptpTest`).
 *
 * Tests share the constituent import channels (GitHub picker / URL) but are a
 * separate collection: a test's (query ...) must never be ingested as an
 * axiom. Running one reuses the prove pipeline with the test's own axioms,
 * query, and (time N) budget. This store owns the imported-tests collection
 * and its OPFS/localStorage persistence; the Ask/Tell tab owns loading a
 * test's text into its panes.
 */

import { defineStore } from "pinia";
import { TQ_SETTING } from "../constants";
import { OriginKind, originForKind } from "../models/Origin";
import { call } from "../services/sigma";
import { fromOrigin } from "../services/sources";
import { errMsg } from "../utils/format";
import { useBootStore } from "./boot";
import { useProverStore } from "./prover";

export interface TestOutcome {
  cls: "ok" | "bad" | "mut";
  label: string;
  status?: string;
}

export interface TestEntry {
  name: string;
  origin: OriginKind;
  text: string;
  /** The worker's `TestCaseView`. */
  parsed: any;
  outcome: TestOutcome | null;
}

export interface SavedTest {
  name: string;
  origin: OriginKind;
}

export const isTestFile = (name: string) => /\.(tq|p|tptp)$/i.test(name);

/** The RPC that understands `name`'s dialect: `.kif.tq` (SUO-KIF harness
 *  directives) vs `.p`/`.tptp` (a standalone TPTP problem -- see
 *  `parse_tptp_test_content` in core). Both return the same `TestCaseView`
 *  shape (KIF text either way; a TPTP test's theory/conjecture come back
 *  translated). */
function testParseRpc(name: string) {
  return /\.tq$/i.test(name) ? "parseTest" : "parseTptpTest";
}

function loadSavedTests(): SavedTest[] {
  try {
    const raw = JSON.parse(localStorage.getItem(TQ_SETTING) || "[]");
    return Array.isArray(raw) ? raw : [];
  } catch {
    return [];
  }
}

/** Write `text` to `name` in OPFS, creating or overwriting it -- shared by
 *  test upload and save-back. */
async function writeOpfsFile(name: string, text: string) {
  const boot = useBootStore();
  if (!boot.opfsRoot) throw new Error("File system not yet initialized");
  const handle = await boot.opfsRoot.getFileHandle(name, { create: true });
  const stream = await handle.createWritable();
  await stream.write(text);
  await stream.close();
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
     *  opening, loading, or saving-as a test; cleared if that test is
     *  removed. */
    openTest: null as SavedTest | null,
  }),
  getters: {
    /** Names of the imported tests that came from the upstream repo -- the
     *  KB tab's picker hides them alongside the loaded constituents. */
    sumoTestNames: (state) =>
      state.tests.filter((t) => t.origin === "sumo").map((t) => t.name),
  },
  actions: {
    persistSaved() {
      localStorage.setItem(TQ_SETTING, JSON.stringify(this.saved));
    },

    /** Parse and import a test. Nothing is added on a parse failure. */
    async add(
      name: string,
      text: string,
      origin: OriginKind,
    ): Promise<{ added: boolean; notices: string[] }> {
      if (this.tests.some((t) => t.name === name)) {
        return { added: false, notices: [`${name}: already imported`] };
      }
      const { test } = await call(testParseRpc(name), { name, text });
      this.tests.push({ name, origin, text, parsed: test, outcome: null });
      if (!this.saved.some((t) => t.name === name && t.origin === origin)) {
        this.saved.push({ name, origin });
        this.persistSaved();
      }
      return { added: true, notices: [] };
    },

    /** Drop a test; a `file`-origin test's OPFS copy goes with it. */
    async remove(name: string, origin: OriginKind) {
      this.tests = this.tests.filter(
        (t) => t.name !== name || t.origin !== origin,
      );
      this.saved = this.saved.filter(
        (t) => t.name !== name || t.origin !== origin,
      );
      this.persistSaved();
      if (origin === "file") {
        const boot = useBootStore();
        try {
          await boot.opfsRoot?.removeEntry(name);
        } catch {
          /* already gone */
        }
      }
      if (
        this.openTest &&
        this.openTest.name === name &&
        this.openTest.origin === origin
      ) {
        this.openTest = null;
      }
    },

    /** Boot: re-import every saved test, best-effort per test. */
    async restore() {
      for (const { name, origin } of this.saved) {
        try {
          await this.add(
            name,
            await fromOrigin(name, originForKind(origin)),
            origin,
          );
        } catch (e) {
          console.warn(`test ${name}: ${errMsg(e)}`);
        }
      }
    },

    setOpen(t: SavedTest | null) {
      this.openTest = t ? { name: t.name, origin: t.origin } : null;
    },

    /** Import a freshly uploaded test file (Ask/Tell's "Load test"): persist
     *  it to OPFS, parse + add it to the imported-tests list, and mark it as
     *  the currently open test. Throws on a non-test extension or a parse
     *  failure (nothing is left half-added since `add` only pushes after a
     *  successful parse). Returns `parsed` so the caller can load it into
     *  the panes. */
    async loadFile(
      name: string,
      text: string,
    ): Promise<{ added: boolean; notices: string[]; parsed?: any }> {
      if (!isTestFile(name))
        throw new Error(`${name}: not a .kif.tq / .p / .tptp test file`);
      await writeOpfsFile(name, text);
      const r = await this.add(name, text, "file");
      if (!r.added) return r;
      const t = this.tests.find((x) => x.name === name && x.origin === "file");
      this.openTest = { name, origin: "file" };
      return { ...r, parsed: t?.parsed };
    },

    /** Save the Ask/Tell assertions/query as a test. `formattedTq` is the
     *  `.kif.tq` text the caller built (via `formatTest`) from the current
     *  panes. Overwrites the currently open test in place when it's a `.tq`
     *  file-origin import (round-trips cleanly, same format); otherwise --
     *  nothing open, or the open test is a `.p`/`.tptp` import or came from
     *  a read-only origin (`sumo`/`url`) that can't be written back --
     *  prompts for a new file name and saves as a new local test.
     *  `saved: false` means the user cancelled the name prompt. */
    async saveCurrent(formattedTq: string): Promise<{
      saved: boolean;
      name?: string;
      overwritten?: boolean;
      notices?: string[];
    }> {
      const open = this.openTest;
      const canOverwrite =
        !!open && open.origin === "file" && /\.tq$/i.test(open.name);
      let name = open ? open.name : "test.kif.tq";
      if (!canOverwrite) {
        name = /\.tq$/i.test(name)
          ? name
          : name.replace(/\.(p|tptp)$/i, "") + ".kif.tq";
        const chosen = (window.prompt("Save test as:", name) || "").trim();
        if (!chosen) return { saved: false };
        name = /\.tq$/i.test(chosen) ? chosen : `${chosen}.kif.tq`;
      }
      await writeOpfsFile(name, formattedTq);
      const existing = this.tests.find(
        (t) => t.name === name && t.origin === "file",
      );
      if (existing) {
        existing.text = formattedTq;
        const { test } = await call("parseTest", { name, text: formattedTq });
        existing.parsed = test;
        existing.outcome = null;
        this.openTest = { name, origin: "file" };
        return { saved: true, name, overwritten: true };
      }
      const r = await this.add(name, formattedTq, "file");
      if (r.added) this.openTest = { name, origin: "file" };
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

    /** Run every test that has a query, in order; a throwing run counts as
     *  a failure rather than aborting the rest. */
    async runAll(
      onProgress?: (t: TestEntry) => void,
    ): Promise<{ pass: number; ran: number }> {
      let pass = 0;
      let ran = 0;
      for (const t of this.tests) {
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
