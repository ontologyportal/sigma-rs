/**
 * Tests: `.kif.tq` (SUO-KIF harness directives) and `.p`/`.tptp` (standalone
 * TPTP problems, parsed via `parseTptpTest`).
 *
 * Tests live in the same library as the constituents (repo catalogs, local
 * uploads, URLs) but are a separate collection: a test's (query ...) must
 * never be ingested as an axiom. Running one reuses the prove pipeline with
 * the test's own axioms, query, and (time N) budget. The Inference Tests tab
 * lists and runs them, the Ask/Tell tab opens a test into its panes.
 *
 * The library lists every test (`available`), but a test's text is only
 * fetched and parsed when it is needed -- run, opened, or edited (`ensure`);
 * `tests` holds the ones loaded so far. Boot (`load`) loads only what other
 * views read without asking: local test files (no download) and tests with
 * a tracked edit. A test that cannot be fetched or parsed is kept in
 * `failed` with the reason.
 */

import { defineStore } from "pinia";
import {
  LocalOrigin,
  Origin,
  OriginJson,
  RemoteOrigin,
  originId,
  serializeOrigin,
} from "../models/Origin";
import { call } from "../services/sigma";
import { fromOrigin } from "../services/sources";
import { errMsg } from "../utils/format";
import { useChangesStore } from "./changes";
import { originForRepo, useLibraryStore } from "./library";
import { useProverStore } from "./prover";
import { countForms, useTestHistoryStore } from "./runHistory";
import { AskResult, ParsedTest } from "sigmakee/sdk";

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
  parsed: ParsedTest;
  outcome: TestOutcome | null;
}

/** A test by name and origin (the open test, a save target). */
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

/** A test file the library offers that is not loaded yet. */
export interface AvailableTest {
  name: string;
  origin: Origin;
  size: number;
}

/** A library test that could not be fetched or parsed, and why. */
export interface FailedTest extends AvailableTest {
  error: string;
}

const testKey = (name: string, origin: Origin | OriginJson) =>
  `${originId(origin)}:${name}`;

/** Settles once boot's `load` has run (see `whenLoaded`). */
let markLoaded: () => void = () => {};
const loadedOnce = new Promise<void>((resolve) => {
  markLoaded = resolve;
});

/** Grade a prove result against a test's `(answer yes|no)` expectation. */
export function gradeTest(
  parsed: Pick<ParsedTest, "expectedProof">,
  result: AskResult,
): TestOutcome {
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

/** Add a test run to the Inference Tests history (`parsed` null when the
 *  test could not be loaded). */
function recordTestRun(
  name: string,
  parsed: ParsedTest | null,
  status: string,
): void {
  useTestHistoryStore().record({
    lang: testDialect(name),
    assertions: "",
    query: "",
    goal: parsed?.queryKif ?? "",
    title: name,
    told: countForms(parsed?.axiomKif ?? ""),
    status,
    backend: useProverStore().backendLabel,
  });
}

export const useTestsStore = defineStore("tests", {
  state: () => ({
    /** The tests loaded so far (text fetched and parsed). */
    tests: [] as TestEntry[],
    /** Library tests that could not be loaded this session. */
    failed: [] as FailedTest[],
    /** A `runAll` is in progress. */
    running: false,
    /** Boot's `load` finished: the library is listed. */
    loaded: false,
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
    /** Every test file in the library (local entries and repo catalogs)
     *  that is not loaded. */
    available(state): AvailableTest[] {
      const library = useLibraryStore();
      const loaded = new Set(state.tests.map((t) => testKey(t.name, t.origin)));
      const out: AvailableTest[] = [];
      const offer = (name: string, origin: Origin, size: number) => {
        if (!loaded.has(testKey(name, origin)))
          out.push({ name, origin, size });
      };
      for (const e of library.entries)
        if (isTestFile(e.name))
          offer(
            e.name,
            e.kind === "url" ? new RemoteOrigin(e.url) : new LocalOrigin(),
            e.size,
          );
      for (const repo of library.repos) {
        const origin = originForRepo(repo);
        for (const e of library.catalogs[library.repoId(repo)] ?? [])
          if (isTestFile(e.path)) offer(origin.nameFor(e.path), origin, e.size);
      }
      return out;
    },
  },
  actions: {
    /** Parse and add a test. Nothing is added on a parse failure. */
    async add(
      name: string,
      text: string,
      origin: Origin,
    ): Promise<{ added: boolean; notices: string[] }> {
      if (this.tests.some((t) => t.name === name)) {
        return { added: false, notices: [`${name}: already loaded`] };
      }
      const { test } = await call(testParseRpc(name), { name, text });
      this.tests.push({ name, origin, text, parsed: test, outcome: null });
      useLibraryStore().ensureEntry(name, origin, text.length);
      const key = testKey(name, origin);
      this.failed = this.failed.filter(
        (f) => testKey(f.name, f.origin) !== key,
      );
      return { added: true, notices: [] };
    },

    /** The loaded test `name` (from `origin`, when given), fetching and
     *  parsing it from the library first if needed. Throws when the library
     *  has no such test or it cannot be loaded (recorded in `failed`). */
    async ensure(name: string, origin?: Origin): Promise<TestEntry> {
      const id = origin && originId(origin);
      const same = (t: { name: string; origin: Origin }) =>
        t.name === name && (!id || originId(t.origin) === id);
      const have = this.tests.find(same);
      if (have) return have;
      const known = this.failed.find(same);
      if (known) throw new Error(known.error);
      const entry = this.available.find(same);
      if (!entry) throw new Error(`No test named ${name} in the library.`);
      try {
        await this.add(
          name,
          await fromOrigin(name, entry.origin),
          entry.origin,
        );
      } catch (e) {
        this.failed.push({ ...entry, error: errMsg(e) });
        throw e;
      }
      const t = this.tests.find(same);
      if (!t) throw new Error(`${name}: already loaded from another source`);
      return t;
    },

    /** Drop a test (its library file was deleted). */
    async remove(name: string, id: string) {
      const gone = (t: { name: string; origin: Origin | OriginJson }) =>
        t.name === name && originId(t.origin) === id;
      this.tests = this.tests.filter((t) => !gone(t));
      this.failed = this.failed.filter((t) => !gone(t));
      if (this.openTest && gone(this.openTest)) this.openTest = null;
    },

    /** Boot: list the library, then load the tests other views read without
     *  asking -- local files (no download) and tests with a tracked edit.
     *  Best-effort; `loaded` / `whenLoaded` flip when it is done either way. */
    async load(): Promise<void> {
      try {
        await useLibraryStore().loadCatalogs();
        const edited = new Set(
          Object.values(useChangesStore().index).map(
            (r) => `${r.origin}:${r.name}`,
          ),
        );
        for (const t of this.available)
          if (
            t.origin.kind === "file" ||
            edited.has(`${t.origin.kind}:${t.name}`)
          )
            await this.ensure(t.name, t.origin).catch(() => {});
      } catch {
        /* a catalog listing failure leaves its tests unlisted */
      } finally {
        this.loaded = true;
        markLoaded();
      }
    },

    /** Resolves once boot's `load` has finished (at once if it has). */
    whenLoaded(): Promise<void> {
      return loadedOnce;
    },

    setOpen(t: { name: string; origin: Origin } | null) {
      this.openTest = t
        ? { name: t.name, origin: serializeOrigin(t.origin) }
        : null;
    },

    /** Save the Ask/Tell panes as a test: `text` is the `.kif.tq` the caller
     *  built (via `formatTest`) or the raw TPTP problem, per `dialect`.
     *  Overwrites the currently open test in place when it has the same
     *  dialect: a local library file is rewritten, and one imported from a
     *  repo or URL keeps a local edit in the edit store (as a KB constituent
     *  does), which outranks its source on later loads. Otherwise -- nothing
     *  open, or a different dialect -- prompts for a new file name and saves
     *  as a new local test. `saved: false` means the user cancelled the name
     *  prompt. `target` lets the raw editor save a file independently of the
     *  test currently open in Ask/Tell. */
    async saveCurrent(
      text: string,
      dialect: TestDialect,
      target?: SavedTest | null,
    ): Promise<{
      saved: boolean;
      name?: string;
      overwritten?: boolean;
      notices?: string[];
    }> {
      const open = target === undefined ? this.openTest : target;
      const remote =
        open &&
        (open.origin.kind === "sumo" || open.origin.kind === "url") &&
        testDialect(open.name) === dialect
          ? this.tests.find(
              (t) => t.name === open.name && t.origin.kind === open.origin.kind,
            )
          : undefined;
      if (remote) {
        const { test } = await call(testParseRpc(remote.name), {
          name: remote.name,
          text,
        });
        await useChangesStore().recordSave(
          remote.name,
          remote.origin.kind,
          text,
          remote.text,
        );
        remote.text = text;
        remote.parsed = test;
        remote.outcome = null;
        this.setOpen({ name: remote.name, origin: remote.origin });
        return { saved: true, name: remote.name, overwritten: true };
      }
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
      if (this.tests.some((t) => t.name === name && t.origin.kind !== "file"))
        throw new Error(
          `${name}: already imported from another source; choose a different filename`,
        );
      const { test } = await call(testParseRpc(name), { name, text });
      await useLibraryStore().writeLocal(name, text);
      const existing = this.tests.find(
        (t) => t.name === name && t.origin.kind === "file",
      );
      if (existing) {
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

    /** Prove one test with the `test` prover profile (its own `(time N)`
     *  budget taking precedence) and grade the result into `t.outcome`. */
    async run(t: TestEntry): Promise<void> {
      // Axioms-only test: nothing to prove, and `runAll` skips these too.
      const query = t.parsed.queryKif;
      if (!query) return;
      const prover = useProverStore();
      await prover.loadDefaults();
      const config = prover.config(
        "test",
        t.parsed.timeGiven ? { timeLimitSecs: t.parsed.timeout } : {},
      );
      const { result } = await call("prove", {
        assertions: t.parsed.axiomKif,
        query,
        config,
        session: "__tq_test__",
      });
      t.outcome = { ...gradeTest(t.parsed, result), status: result.status };
      recordTestRun(t.name, t.parsed, result.status);
    },

    /** Run `list` (default: every loaded test) in order, loading each first
     *  and skipping tests without a query; one that cannot be loaded or
     *  throws counts as a failure rather than aborting the rest. */
    async runAll(
      onProgress?: (t: { name: string }) => void,
      list?: { name: string; origin: Origin }[],
    ): Promise<{ pass: number; ran: number }> {
      let pass = 0;
      let ran = 0;
      this.running = true;
      try {
        for (const ref of [...(list ?? this.tests)]) {
          onProgress?.(ref);
          let t: TestEntry;
          try {
            t = await this.ensure(ref.name, ref.origin);
          } catch {
            recordTestRun(ref.name, null, "Error");
            ran += 1;
            continue;
          }
          if (!t.parsed.queryKif) continue;
          try {
            await this.run(t);
          } catch (err) {
            t.outcome = { cls: "bad", label: errMsg(err).slice(0, 60) };
            recordTestRun(t.name, t.parsed, "Error");
          }
          ran += 1;
          if (t.outcome?.cls === "ok") pass += 1;
        }
      } finally {
        this.running = false;
      }
      return { pass, ran };
    },
  },
});
