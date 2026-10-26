/** Recorded contradictions and where they are watched for.
 *
 * The engine records every contradiction an audit finds, keyed by the
 * axioms it cites, and drops it the moment one of them leaves the KB. This
 * store mirrors that list for the views, keeps it across reloads (see
 * `findings-cache.ts`), and watches the configured *contradiction sources*
 * -- a GitHub issue tracker's `[Contradiction]` issues, a report URL such
 * as the nightly `latest-contradictions.json`, or a one-shot file upload --
 * with the same auto-update / auto-check / no-check preferences the
 * constituent Sources card uses. A source's new, valid contradictions
 * (every cited axiom present in this KB) are imported into the engine; the
 * rest are reported as not applicable. */

import { defineStore } from "pinia";
import type { AuditFinding, CitedAxiom, ImportOutcome } from "sigmakee/sdk";
import {
  CONTRADICTION_BASELINES_KEY,
  CONTRADICTION_PREFS_KEY,
  CONTRADICTION_SOURCES_KEY,
  SUMO,
} from "../constants";
import { fetchSumoIssue, searchSumoContradictions } from "../api/github";
import { call } from "../services/sigma";
import { fetchText } from "../services/sources";
import {
  loadFindingsMirror,
  mirrorReportsByOrigin,
  saveFindingsMirror,
} from "../services/findings-cache";
import { proofReportFromIssue } from "../utils/proofReport";
import { errMsg } from "../utils/format";
import { blobSha } from "./changes";
import type { UpdatePref } from "./kb";

export type ContradictionSourceKind = "github" | "url" | "file";

/** Somewhere contradictions are reported. `id` is the origin label every
 *  finding imported from it carries (GitHub issues append `#<number>`). */
export interface ContradictionSource {
  id: string;
  kind: ContradictionSourceKind;
  label: string;
  owner?: string;
  repo?: string;
  url?: string;
  name?: string;
  /** Epoch ms when the source was added. */
  added: number;
  /** Shipped by default; still removable. */
  builtin?: boolean;
}

/** Reports fetched from a source but not yet imported (an `auto-check`
 *  source's review step). `baseline` is recorded once they are imported. */
export interface PendingImport {
  source: ContradictionSource;
  reports: { json: string; origin: string }[];
  baseline?: string;
}

/** One check result, shown until dismissed. `pending` offers an Import
 *  action; `keys` are the findings a completed import recorded. */
export interface ContradictionAlert {
  key: string;
  message: string;
  pending?: PendingImport;
  keys?: string[];
}

/** One contradiction a source reported that this KB cannot record: a cited
 *  axiom is missing (edited, unloaded, or never loaded), or the entry cites
 *  none. Kept so the row can show what was reported and why it was skipped. */
export interface NotApplicable {
  /** The origin the finding would have carried (`<source id>` or
   *  `<source id>#<issue>`). */
  origin: string;
  axioms: CitedAxiom[];
  steps: ImportOutcome["rejected"][number]["steps"];
  reason: string;
  missing: CitedAxiom[];
}

/** What a source's last check found. In memory only: the boot-time check
 *  rebuilds it. */
export interface SourceCheck {
  at: number;
  /** Contradictions the source reported (applicable or not). */
  reported: number;
  /** Keys of the findings recorded (new or already present). */
  recorded: string[];
  notApplicable: NotApplicable[];
  /** Reports that could not be parsed at all. */
  unreadable: number;
  /** The fetch or listing failed; nothing below is meaningful. */
  error?: string;
  /** The fetched report text (URL and upload sources). */
  raw?: string;
}

/** The SUMO nightly audit's published report. */
export const NIGHTLY_REPORT_URL = `https://raw.githubusercontent.com/${SUMO.owner}/${SUMO.repo}/audit-state/.github/latest-contradictions.json`;

/** At most this many new issues are read per check (each is one API call). */
const ISSUES_PER_CHECK = 20;

const UPDATE_PREF_CYCLE: readonly UpdatePref[] = [
  "auto-update",
  "auto-check",
  "no-check",
];

export function githubSource(owner: string, repo: string): ContradictionSource {
  return {
    id: `github:${owner}/${repo}`,
    kind: "github",
    label: `${owner}/${repo} issues`,
    owner,
    repo,
    added: Date.now(),
  };
}

export function urlSource(url: string): ContradictionSource {
  let label = url;
  try {
    const u = new URL(url);
    label = `${u.host}${u.pathname}`;
  } catch {
    /* shown verbatim */
  }
  return { id: `url:${url}`, kind: "url", label, url, added: Date.now() };
}

export function fileSource(
  name: string,
  added = Date.now(),
): ContradictionSource {
  return {
    id: `file:${name}@${added}`,
    kind: "file",
    label: name,
    name,
    added,
  };
}

function defaultSources(): ContradictionSource[] {
  return [
    {
      ...urlSource(NIGHTLY_REPORT_URL),
      label: "SUMO nightly audit",
      builtin: true,
    },
    { ...githubSource(SUMO.owner, SUMO.repo), builtin: true },
  ];
}

function defaultPref(kind: ContradictionSourceKind): UpdatePref {
  return kind === "url"
    ? "auto-update"
    : kind === "github"
      ? "auto-check"
      : "no-check";
}

function loadJson<T>(
  key: string,
  fallback: T,
  check: (v: unknown) => v is T,
): T {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(key) || "null");
    if (check(raw)) return raw;
  } catch {
    /* corrupt or absent */
  }
  return fallback;
}

const isRecord = (v: unknown): v is Record<string, string> =>
  !!v && typeof v === "object" && !Array.isArray(v);

function isSourceList(v: unknown): v is ContradictionSource[] {
  return (
    Array.isArray(v) &&
    v.every(
      (s) =>
        s &&
        typeof s.id === "string" &&
        ["github", "url", "file"].includes(s.kind) &&
        typeof s.label === "string",
    )
  );
}

/** Whether a stored-sources entry exists at all (first run vs. the user
 *  having removed every source). */
function hasStoredSources(): boolean {
  return localStorage.getItem(CONTRADICTION_SOURCES_KEY) !== null;
}

export const useContradictionsStore = defineStore("contradictions", {
  state: () => ({
    sources: hasStoredSources()
      ? loadJson(CONTRADICTION_SOURCES_KEY, [], isSourceList)
      : defaultSources(),
    prefs: loadJson<Record<string, string>>(
      CONTRADICTION_PREFS_KEY,
      {},
      isRecord,
    ) as Record<string, UpdatePref>,
    baselines: loadJson<Record<string, string>>(
      CONTRADICTION_BASELINES_KEY,
      {},
      isRecord,
    ),
    /** The engine's recorded contradictions, newest first. */
    findings: [] as AuditFinding[],
    alerts: [] as ContradictionAlert[],
    /** Alerts the boot-time check added, for the sources-changed dialog. */
    changedOnLoad: [] as ContradictionAlert[],
    /** Source ids with a check in flight. */
    checking: [] as string[],
    /** Each source's last check, by source id. */
    checks: {} as Record<string, SourceCheck>,
    /** The finding the Audit tab should scroll to / expand, if any. */
    focusKey: null as string | null,
  }),
  getters: {
    /** Findings a source contributed (its own label, or an issue of it). */
    findingsFor: (state) => (source: ContradictionSource) =>
      state.findings.filter(
        (f) => f.origin === source.id || f.origin.startsWith(`${source.id}#`),
      ),
    /** Findings the local provers recorded (not imported). */
    localFindings: (state) =>
      state.findings.filter((f) => !/^(github|url|file):/.test(f.origin)),
  },
  actions: {
    persistSources() {
      localStorage.setItem(
        CONTRADICTION_SOURCES_KEY,
        JSON.stringify(this.sources),
      );
    },
    persistPrefs() {
      localStorage.setItem(CONTRADICTION_PREFS_KEY, JSON.stringify(this.prefs));
    },
    setBaseline(id: string, value: string) {
      this.baselines[id] = value;
      localStorage.setItem(
        CONTRADICTION_BASELINES_KEY,
        JSON.stringify(this.baselines),
      );
    },

    prefFor(source: ContradictionSource): UpdatePref {
      return this.prefs[source.id] ?? defaultPref(source.kind);
    },
    cyclePref(source: ContradictionSource) {
      const cur = this.prefFor(source);
      this.prefs[source.id] =
        UPDATE_PREF_CYCLE[
          (UPDATE_PREF_CYCLE.indexOf(cur) + 1) % UPDATE_PREF_CYCLE.length
        ];
      this.persistPrefs();
    },

    /** Register a source; a duplicate id is a no-op. Returns the stored one. */
    addSource(source: ContradictionSource): ContradictionSource {
      const existing = this.sources.find((s) => s.id === source.id);
      if (existing) return existing;
      this.sources.push(source);
      this.persistSources();
      return source;
    },
    /** Forget a source and its watch state. Its findings stay recorded:
     *  they are contradictions of this KB regardless of who reported them. */
    removeSource(id: string) {
      this.sources = this.sources.filter((s) => s.id !== id);
      delete this.prefs[id];
      delete this.baselines[id];
      this.persistSources();
      this.persistPrefs();
      localStorage.setItem(
        CONTRADICTION_BASELINES_KEY,
        JSON.stringify(this.baselines),
      );
      this.alerts = this.alerts.filter((a) => a.key !== id);
    },

    pushAlert(alert: ContradictionAlert) {
      this.alerts = [...this.alerts.filter((a) => a.key !== alert.key), alert];
    },
    dismissAlert(key: string) {
      this.alerts = this.alerts.filter((a) => a.key !== key);
      this.changedOnLoad = this.changedOnLoad.filter((a) => a.key !== key);
    },

    /** Re-read the engine's findings (an audit or an edit may have changed
     *  them) and refresh the reload mirror. */
    async refresh() {
      this.findings = await call("auditFindings");
      await saveFindingsMirror(this.findings);
    },

    /** Import one report into the engine under `origin`. */
    async importJson(json: string, origin: string): Promise<ImportOutcome> {
      const outcome = await call("importContradictions", { json, origin });
      await this.refresh();
      return outcome;
    },

    /** Re-import the reload mirror, origin by origin. Findings whose axioms
     *  are no longer in the KB fail to resolve and stay dropped. */
    async restoreFindings() {
      const mirror = await loadFindingsMirror();
      if (mirror)
        for (const [origin, json] of mirrorReportsByOrigin(mirror)) {
          try {
            await call("importContradictions", { json, origin });
          } catch {
            /* an unreadable group: the rest still restore */
          }
        }
      await this.refresh();
    },

    /** Import `pending`'s reports, record the baseline, and alert with what
     *  changed. A background check stays quiet when nothing new arrived. */
    async applyImport(pending: PendingImport, manual: boolean) {
      const before = new Set(this.findings.map((f) => f.key));
      const check: SourceCheck = {
        at: Date.now(),
        reported: 0,
        recorded: [],
        notApplicable: [],
        unreadable: 0,
        raw:
          pending.source.kind !== "github" && pending.reports.length === 1
            ? pending.reports[0].json
            : undefined,
      };
      for (const r of pending.reports) {
        try {
          const o = await call("importContradictions", {
            json: r.json,
            origin: r.origin,
          });
          check.reported += o.imported.length + o.rejected.length;
          check.recorded.push(...o.imported.map((f) => f.key));
          check.notApplicable.push(
            ...o.rejected.map((x) => ({ origin: r.origin, ...x })),
          );
        } catch {
          check.unreadable++;
        }
      }
      this.checks[pending.source.id] = check;
      await this.refresh();
      if (pending.baseline)
        this.setBaseline(pending.source.id, pending.baseline);
      const fresh = this.findings
        .filter((f) => !before.has(f.key))
        .map((f) => f.key);
      if (!fresh.length && !manual) return;
      const rejected = check.notApplicable.length;
      const parts = [
        `${fresh.length} new contradiction${fresh.length === 1 ? "" : "s"} recorded`,
      ];
      if (rejected)
        parts.push(`${rejected} not applicable to this knowledge base`);
      if (check.unreadable)
        parts.push(`${check.unreadable} report(s) unreadable`);
      this.pushAlert({
        key: pending.source.id,
        message: `${pending.source.label}: ${parts.join(", ")}.`,
        keys: fresh,
      });
    },

    /** Record that a source's check could not even reach its reports. */
    failCheck(source: ContradictionSource, error: string) {
      this.checks[source.id] = {
        at: Date.now(),
        reported: 0,
        recorded: [],
        notApplicable: [],
        unreadable: 0,
        error,
      };
    },

    /** Import an `auto-check` alert's pending reports -- its Import action. */
    async applyPending(alert: ContradictionAlert) {
      if (!alert.pending) return;
      this.dismissAlert(alert.key);
      await this.applyImport(alert.pending, true);
    },

    /** Check one source: fetch what it reports now, and import (or, under
     *  `auto-check` in the background, offer to import) anything newer than
     *  the recorded baseline. `manual` always imports and always alerts. */
    async checkSource(source: ContradictionSource, manual = false) {
      if (source.kind === "file" || this.checking.includes(source.id)) return;
      const pref = this.prefFor(source);
      this.checking = [...this.checking, source.id];
      try {
        if (source.kind === "url") {
          let text: string;
          try {
            text = await fetchText(source.url ?? source.label);
          } catch (e) {
            this.failCheck(source, `couldn't fetch the report (${errMsg(e)})`);
            if (manual)
              this.pushAlert({
                key: source.id,
                message: `${source.label}: couldn't fetch the report (${errMsg(e)}).`,
              });
            return;
          }
          const hash = (await blobSha(text)) ?? `len:${text.length}`;
          const baseline = this.baselines[source.id];
          const pending: PendingImport = {
            source,
            reports: [{ json: text, origin: source.id }],
            baseline: hash,
          };
          // Unchanged since the last import: re-import quietly anyway. It
          // is idempotent, and it is what tells the row how much of the
          // report applies to the KB loaded now.
          if (!manual && baseline === hash) {
            await this.applyImport(pending, false);
            return;
          }
          if (!manual && pref === "auto-check" && baseline) {
            this.pushAlert({
              key: source.id,
              message: `${source.label}: the report changed since it was last imported.`,
              pending,
            });
            return;
          }
          await this.applyImport(pending, manual);
          return;
        }

        // GitHub: open `[Contradiction]` issues newer than the last one seen.
        let issues;
        try {
          issues = (
            await searchSumoContradictions(1, source.owner, source.repo)
          ).issues.filter((i) => i.state === "open");
        } catch (e) {
          this.failCheck(source, `couldn't list issues (${errMsg(e)})`);
          if (manual)
            this.pushAlert({
              key: source.id,
              message: `${source.label}: couldn't list issues (${errMsg(e)}).`,
            });
          return;
        }
        const baseline = Number(this.baselines[source.id] ?? 0);
        const newest = issues.reduce((m, i) => Math.max(m, i.number), baseline);
        const fresh = issues
          .filter((i) => i.number > baseline)
          .sort((a, b) => a.number - b.number)
          .slice(0, ISSUES_PER_CHECK);
        if (!fresh.length) {
          this.setBaseline(source.id, String(newest));
          this.checks[source.id] ??= {
            at: Date.now(),
            reported: 0,
            recorded: [],
            notApplicable: [],
            unreadable: 0,
          };
          if (manual)
            this.pushAlert({
              key: source.id,
              message: `${source.label}: no new contradiction issues.`,
            });
          return;
        }
        const reports: PendingImport["reports"] = [];
        for (const issue of fresh) {
          try {
            const report = proofReportFromIssue(
              await fetchSumoIssue(issue.number, source.owner, source.repo),
            );
            if (report.problem.command !== "audit") continue;
            reports.push({
              json: JSON.stringify(report),
              origin: `${source.id}#${issue.number}`,
            });
          } catch {
            /* an issue without a readable report */
          }
        }
        const pending: PendingImport = {
          source,
          reports,
          baseline: String(fresh[fresh.length - 1].number),
        };
        if (!manual && pref === "auto-check") {
          this.pushAlert({
            key: source.id,
            message: `${source.label}: ${fresh.length} new contradiction issue${fresh.length === 1 ? "" : "s"}.`,
            pending,
          });
          return;
        }
        await this.applyImport(pending, manual);
      } finally {
        this.checking = this.checking.filter((id) => id !== source.id);
      }
    },

    /** The boot-time pass over every watched source; alerts it adds are
     *  collected for the sources-changed dialog. */
    async checkForUpdates() {
      const earlier = new Set(this.alerts);
      for (const source of this.sources) {
        if (source.kind === "file" || this.prefFor(source) === "no-check")
          continue;
        await this.checkSource(source);
      }
      this.changedOnLoad = this.alerts.filter((a) => !earlier.has(a));
    },

    /** A one-shot upload: import the file's report now and list it as a
     *  source for provenance; there is nothing to watch afterwards. */
    async importFile(file: File) {
      const text = await file.text();
      const source = this.addSource(fileSource(file.name));
      await this.applyImport(
        { source, reports: [{ json: text, origin: source.id }] },
        true,
      );
    },

    async clearFindings() {
      await call("clearAuditFindings");
      await this.refresh();
    },
  },
});
