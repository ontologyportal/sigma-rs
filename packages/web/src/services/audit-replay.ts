import { fetchLatestMasterAudit, fetchAuditMasterSha } from "../api/github";
import { AUDIT_ENGINE, SUMO } from "../constants";
import { GitOrigin } from "../models/Origin";
import { useKBStore } from "../stores/kb";
import { useChangesStore } from "../stores/changes";
import {
  assertReplayCompatible,
  parseAuditReplay,
  verifyReplayFiles,
  type AuditReplay,
} from "../utils/auditReplay";

const raw = (ref: string, path: string) =>
  `https://raw.githubusercontent.com/${SUMO.owner}/${SUMO.repo}/${ref}/${path}`;

async function currentContext() {
  const [master, run] = await Promise.all([
    fetchAuditMasterSha(),
    fetchLatestMasterAudit(),
  ]);
  return { master, run };
}

/** Read the exact Markdown published with the workflow artifact. */
export async function latestAuditReport() {
  const context = await currentContext();
  const response = await fetch(
    raw("audit-state", ".github/latest-contradictions.md"),
    { cache: "no-store" },
  );
  if (!response.ok)
    throw new Error(
      "No replay report has been published yet. Run the updated master audit first.",
    );
  const markdown = await response.text();
  const replay = parseAuditReplay(markdown);
  let unavailable = "";
  try {
    assertReplayCompatible(replay, context.master, AUDIT_ENGINE, context.run);
  } catch (e) {
    unavailable = e instanceof Error ? e.message : String(e);
  }
  return {
    markdown,
    replay,
    runUrl: `https://github.com/${SUMO.owner}/${SUMO.repo}/actions/runs/${replay.run_id}/attempts/${replay.run_attempt}`,
    unavailable,
  };
}

/** Revalidate against live master and the latest completed run, without cached refs. */
export async function checkAuditReplay(replay: AuditReplay) {
  const { master, run } = await currentContext();
  assertReplayCompatible(replay, master, AUDIT_ENGINE, run);
}

/** Called only after confirmation. Verify all downloads before replacing saved work. */
export async function loadAuditReplay(
  replay: AuditReplay,
  progress: (message: string) => void,
) {
  await checkAuditReplay(replay);
  const files = new Array<{ name: string; bytes: Uint8Array<ArrayBuffer> }>(
    replay.constituents.length,
  );
  let next = 0;
  let done = 0;
  await Promise.all(
    Array.from({ length: Math.min(6, files.length) }, async () => {
      for (let i = next++; i < files.length; i = next++) {
        const { name } = replay.constituents[i];
        const response = await fetch(raw(replay.sumo_commit, name), {
          cache: "no-store",
        });
        if (!response.ok)
          throw new Error(`Cannot load ${name}: HTTP ${response.status}.`);
        files[i] = {
          name,
          bytes: new Uint8Array(await response.arrayBuffer()),
        };
        progress(
          `Downloading verified audit inputs: ${++done}/${files.length}`,
        );
      }
    }),
  );
  const texts = await verifyReplayFiles(replay, files);
  const manifestResponse = await fetch(
    raw(replay.sumo_commit, ".github/full-sumo.txt"),
    { cache: "no-store" },
  );
  if (!manifestResponse.ok)
    throw new Error("Cannot verify the audited Full SUMO constituent list.");
  const names = (await manifestResponse.text())
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter((s) => s && !s.startsWith("#"));
  if (JSON.stringify(names) !== JSON.stringify(files.map((f) => f.name)))
    throw new Error(
      "The report does not match master's Full SUMO constituent list.",
    );
  await checkAuditReplay(replay);
  progress("Replacing the KB with the verified audit inputs...");
  const kb = useKBStore();
  const changes = useChangesStore();
  await kb.replaceAll();
  const origin = new GitOrigin(
    "github",
    SUMO.owner,
    SUMO.repo,
    replay.sumo_commit,
  );
  for (let i = 0; i < files.length; i++) {
    // Also discard saved edits for audited files that were not previously loaded.
    await changes.forget(files[i].name, "sumo");
    const result = await kb.ingest(files[i].name, texts[i], origin);
    if (!result.added) throw new Error(`Failed to ingest ${files[i].name}.`);
    progress(`Loading audit constituents: ${i + 1}/${files.length}`);
  }
  await kb.reprocess();
  await verifyReplayFiles(
    replay,
    kb.constituents.map((c) => ({
      name: c.file,
      bytes: new TextEncoder().encode(c.text),
    })),
  );
  await checkAuditReplay(replay);
}
