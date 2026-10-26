/**
 * Recorded-contradiction mirror (OPFS).
 *
 * The engine keeps every contradiction an audit found (or a report
 * imported) in its findings cache, dropping each one the moment a cited
 * axiom leaves the KB. That cache lives inside the worker's KB, so it
 * survives a reload only when the KB snapshot cache (`kb-cache.ts`) is
 * eligible and hits. This mirror keeps the findings independently, as the
 * same report shape `sumo audit --import` reads, and is re-imported after
 * every boot: an import re-resolves each cited axiom against the live KB,
 * so a finding whose axioms were edited or unloaded meanwhile simply fails
 * to come back.
 */

import type { AuditFinding } from "sigmakee/sdk";
import { useBootStore } from "../stores/boot";

const SUMO_CACHE_DIR = "sumo-cache";
const FINDINGS_FILE = "findings.json";

/** One recorded contradiction as the mirror stores it. */
export interface MirroredFinding {
  origin: string;
  found_at_ms: number;
  axioms: { file: string | null; line: number | null; kif: string }[];
  steps: {
    rule: string;
    premises: number[];
    kif: string;
    file: string | null;
    line: number | null;
  }[];
}

export interface FindingsMirror {
  version: 1;
  findings: MirroredFinding[];
}

/** The mirror shape of the engine's current findings. */
export function mirrorFindings(findings: AuditFinding[]): FindingsMirror {
  return {
    version: 1,
    findings: findings.map((f) => ({
      origin: f.origin,
      found_at_ms: f.found_at_ms,
      axioms: f.culprits.map((c) => ({
        file: c.file,
        line: c.line,
        kif: c.kif,
      })),
      steps: f.contradiction.steps.map((s) => ({
        rule: s.rule,
        premises: s.premises,
        kif: s.kif,
        file: s.file,
        line: s.line,
      })),
    })),
  };
}

/** Whether an untrusted parsed value is a usable mirror. */
export function isFindingsMirror(v: unknown): v is FindingsMirror {
  const m = v as FindingsMirror | null;
  return (
    !!m &&
    typeof m === "object" &&
    m.version === 1 &&
    Array.isArray(m.findings) &&
    m.findings.every(
      (f) =>
        f &&
        typeof f.origin === "string" &&
        Array.isArray(f.axioms) &&
        f.axioms.every((a) => a && typeof a.kif === "string"),
    )
  );
}

/** The mirror regrouped into one importable report per origin (the engine
 *  records one origin per import call). */
export function mirrorReportsByOrigin(m: FindingsMirror): Map<string, string> {
  const groups = new Map<string, MirroredFinding[]>();
  for (const f of m.findings) {
    const list = groups.get(f.origin) ?? [];
    list.push(f);
    groups.set(f.origin, list);
  }
  return new Map(
    [...groups].map(([origin, findings]) => [
      origin,
      JSON.stringify({
        contradictions: findings.map((f) => ({
          axioms: f.axioms,
          steps: f.steps ?? [],
          found_at_ms: f.found_at_ms,
        })),
      }),
    ]),
  );
}

async function cacheDir(): Promise<FileSystemDirectoryHandle> {
  const boot = useBootStore();
  if (!boot.opfsRoot) throw new Error("File system not initialized yet");
  return boot.opfsRoot.getDirectoryHandle(SUMO_CACHE_DIR, { create: true });
}

/** Write the mirror; a failure (no OPFS, quota) is swallowed -- the engine
 *  still holds the findings for this session. */
export async function saveFindingsMirror(
  findings: AuditFinding[],
): Promise<void> {
  try {
    const dir = await cacheDir();
    const handle = await dir.getFileHandle(FINDINGS_FILE, { create: true });
    const w = await handle.createWritable();
    await w.write(JSON.stringify(mirrorFindings(findings)));
    await w.close();
  } catch {
    /* no OPFS, or out of quota */
  }
}

/** The stored mirror, or null when there is none (or it is unreadable). */
export async function loadFindingsMirror(): Promise<FindingsMirror | null> {
  try {
    const dir = await cacheDir();
    const text = await (
      await (await dir.getFileHandle(FINDINGS_FILE)).getFile()
    ).text();
    const parsed: unknown = JSON.parse(text);
    return isFindingsMirror(parsed) ? parsed : null;
  } catch {
    return null;
  }
}
