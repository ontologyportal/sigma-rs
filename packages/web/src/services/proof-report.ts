import { call, type CmdArgs, type CmdResult } from "./sigma";
import { lspSyncedText } from "./lsp";
import { GitOrigin } from "../models/Origin";
import { useKBStore } from "../stores/kb";
import { useShellStore } from "../stores/shell";
import { AUDIT_ENGINE, PROVER_BUILDS } from "../constants";
import { fetchBranchHead } from "../api/github";
import { useChangesStore } from "../stores/changes";
import { useProverStore } from "../stores/prover";
import { replayFileManifest, verifyReplayFiles } from "../utils/auditReplay";
import {
  assertProofCompatible,
  formatProofReport,
  parseProofReport,
  reportRepository,
  reportPath,
  reportBranch,
  type ProofReport,
  type SharedProofReport,
  type PinnedConstituent,
} from "../utils/proofReport";

type ReportCommand = "prove" | "audit" | "recheckAudit";

/** Capture inputs now; never reconstruct an old proof from current UI settings. */
export async function runWithReport<C extends ReportCommand>(
  cmd: C,
  args: CmdArgs<C>,
) {
  const kb = useKBStore();
  const files = kb.constituents.map((c) => ({
    name: c.file,
    text: lspSyncedText(c.file) ?? c.text,
    source:
      c.origin instanceof GitOrigin
        ? {
            repository: `${c.origin.service}:${c.origin.owner}/${c.origin.repo}`,
            branch: c.origin.branch,
            path: c.origin.pathOf(c.name),
          }
        : null,
  }));
  const context = {
    format: "sigma-proof-report" as const,
    version: 1 as const,
    created: new Date().toISOString(),
    engine: AUDIT_ENGINE,
    app: useShellStore().version ? { ...useShellStore().version } : null,
    problem: JSON.parse(JSON.stringify({ command: cmd, ...args })),
    constituents: files,
  };
  const response = await call(cmd, { ...args, reportFiles: files });
  const backend = response.reportContext?.config.backend ?? "native";
  const banner =
    response.result.raw_output?.match(
      /^.*(?:Vampire\s+\d|E(?:-?prover)?\s+(?:version\s+)?\d|Version\s*:).*$/im,
    )?.[0] ?? null;
  const report: ProofReport = {
    ...context,
    prover: {
      backend,
      version: backend === "native" ? (AUDIT_ENGINE?.commit ?? null) : banner,
      binaries: Object.fromEntries(
        Object.entries(PROVER_BUILDS).filter(([file]) =>
          file.startsWith(backend === "e" ? "eprover/" : `${backend}/`),
        ),
      ),
    },
    config: response.reportContext?.config ?? {},
    defaults: response.reportContext?.defaults,
    problem: response.reportContext?.problem ?? context.problem,
    result: response.result,
    unavailable:
      response.reportContext?.error ??
      "This run has no verified input snapshot.",
  };
  return {
    ...response,
    result: { ...response.result, shareReport: report },
  } as CmdResult<C> & {
    result: CmdResult<C>["result"] & { shareReport: ProofReport };
  };
}

const rawReportUrl = (source: PinnedConstituent["source"]) => {
  const { owner, repo } = reportRepository(source.repository);
  return `https://raw.githubusercontent.com/${owner}/${repo}/${source.commit}/${source.path.split("/").map(encodeURIComponent).join("/")}`;
};

async function downloadProofFiles(
  files: PinnedConstituent[],
  progress: (message: string) => void,
) {
  const result = new Array<{ name: string; bytes: Uint8Array<ArrayBuffer> }>(
    files.length,
  );
  let next = 0;
  let done = 0;
  let total = 0;
  const downloads = await Promise.allSettled(
    Array.from({ length: Math.min(6, files.length) }, async () => {
      for (let i = next++; i < files.length; i = next++) {
        const response = await fetch(rawReportUrl(files[i].source), {
          cache: "no-store",
          credentials: "omit",
        });
        if (!response.ok)
          throw new Error(
            `Cannot download ${files[i].name} at the pinned commit: HTTP ${response.status}.`,
          );
        if (Number(response.headers.get("content-length")) > 64_000_000)
          throw new Error("Report constituent exceeds the 64 MB limit.");
        const bytes = new Uint8Array(await response.arrayBuffer());
        total += bytes.length;
        if (bytes.length > 64_000_000 || total > 256_000_000)
          throw new Error("Report inputs exceed the download size limit.");
        result[i] = { name: files[i].name, bytes };
        progress(`Verifying committed inputs: ${++done}/${files.length}`);
      }
    }),
  );
  for (const download of downloads)
    if (download.status === "rejected") throw download.reason;
  return result;
}

/** Verify all run-time bytes against current branch tips; no local content is exported. */
export async function exportProofReport(
  report: ProofReport,
  progress: (message: string) => void = () => {},
) {
  if (report.unavailable) throw new Error(report.unavailable);
  if (!report.engine)
    throw new Error(
      "This build has no engine identity. Rebuild the engine before sharing.",
    );
  const heads = new Map<string, PinnedConstituent["source"]>();
  const pinned: PinnedConstituent[] = [];
  for (const file of report.constituents) {
    const source = file.source;
    if (!source || !reportBranch(source.branch) || !reportPath(source.path))
      throw new Error(
        `${file.name}: load this file from a pushed GitHub branch, then rerun the proof before sharing. Local uploads and detached revisions cannot be shared.`,
      );
    const { owner, repo } = reportRepository(source.repository);
    const key = `${source.repository}@${source.branch}`;
    let pin = heads.get(key);
    if (!pin) {
      progress(`Checking HEAD of ${owner}/${repo}@${source.branch}...`);
      const commit = await fetchBranchHead(owner, repo, source.branch);
      if (!/^[0-9a-f]{40}$/.test(commit))
        throw new Error("Could not resolve branch HEAD.");
      pin = { ...source, commit };
      heads.set(key, pin);
    }
    pinned.push({
      name: file.name,
      sha256: "",
      source: { ...pin, path: source.path },
    });
  }
  assertProofCompatible(
    { engine: report.engine, prover: report.prover } as SharedProofReport,
    AUDIT_ENGINE,
    PROVER_BUILDS,
  );
  const downloaded = await downloadProofFiles(pinned, progress);
  const snapshot = await replayFileManifest(
    report.constituents.map((f) => ({
      name: f.name,
      bytes: new TextEncoder().encode(f.text),
    })),
  );
  try {
    await verifyReplayFiles(snapshot, downloaded);
  } catch {
    throw new Error(
      "Cannot export this proof: its local input files do not match the pushed HEAD of their GitHub branch. Either push your changes to the head of the branch or reset your local files to the head of the branch, then rerun the proof before exporting.",
    );
  }
  for (const source of heads.values()) {
    const { owner, repo } = reportRepository(source.repository);
    if ((await fetchBranchHead(owner, repo, source.branch)) !== source.commit)
      throw new Error(
        "The branch moved while verifying inputs. Try sharing again.",
      );
  }
  return formatProofReport(report, {
    fingerprint: snapshot.fingerprint,
    constituents: pinned.map((f, i) => ({
      ...f,
      sha256: snapshot.constituents[i].sha256,
    })),
  });
}

/** Called after explicit confirmation. Downloads and validates everything before replacement. */
export async function importProofReport(
  input: SharedProofReport,
  progress: (message: string) => void = () => {},
) {
  const report = parseProofReport(JSON.stringify(input));
  assertProofCompatible(report, AUDIT_ENGINE, PROVER_BUILDS);
  const downloaded = await downloadProofFiles(report.constituents, progress);
  const texts = await verifyReplayFiles(report, downloaded);
  const files = report.constituents.map((f, i) => ({
    name: f.name,
    text: texts[i],
  }));
  await call("validateProofSetup", { files, config: report.config });
  const kb = useKBStore();
  const changes = useChangesStore();
  progress("Replacing the KB with verified pinned inputs...");
  await kb.replaceAll();
  for (let i = 0; i < files.length; i++) {
    const f = report.constituents[i];
    const { owner, repo } = reportRepository(f.source.repository);
    await changes.forget(f.name, "sumo");
    const loaded = await kb.ingest(
      f.name,
      texts[i],
      new GitOrigin("github", owner, repo, f.source.commit),
    );
    if (!loaded.added) throw new Error(`Failed to load ${f.name}.`);
    progress(`Loading pinned constituents: ${i + 1}/${files.length}`);
  }
  await kb.reprocess();
  await verifyReplayFiles(
    report,
    kb.constituents.map((c) => ({
      name: c.file,
      bytes: new TextEncoder().encode(c.text),
    })),
  );
  const prover = useProverStore();
  await prover.loadDefaults();
  prover.adoptConfig(
    report.problem.command === "prove" ? "ask" : "audit",
    report.config,
  );
  progress("Replaying the recorded problem...");
  const p = report.problem;
  const response =
    p.command === "prove"
      ? await call("prove", { ...p, config: report.config, reportFiles: files })
      : await call("audit", {
          request: p.request,
          config: report.config,
          reportFiles: files,
        });
  if (response.reportContext?.error)
    throw new Error(response.reportContext.error);
  return response.result;
}
