import type { GitOrigin } from "../models/Origin";
import { blobSha } from "../stores/changes";

/** Same-origin requests keep GitLab credentials out of page JavaScript. */
export async function gitlabRequest<T>(
  operation: string,
  data: object = {},
): Promise<T> {
  const r = await fetch("/api/gitlab", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...data, operation }),
  });
  let result;
  try {
    result = await r.json();
  } catch {
    throw new Error(
      `GitLab sign-in service returned an empty or invalid response (HTTP ${r.status}). Please try again.`,
    );
  }
  if (!r.ok)
    throw new Error(
      typeof result?.error === "string"
        ? result.error
        : `GitLab request failed (${r.status}).`,
    );
  if (result === null || typeof result !== "object")
    throw new Error(
      `GitLab API returned an invalid response (HTTP ${r.status}).`,
    );
  return result as T;
}
export const coordinates = (
  g: { owner: string; repo: string; branch: string; server?: string },
  ref = g.branch,
) => ({
  owner: g.owner,
  repo: g.repo,
  ref,
  ...(g.server ? { server: g.server } : {}),
});
export async function gitlabTree(
  g: { owner: string; repo: string; branch: string; server?: string },
  ref = g.branch,
) {
  const tree = await gitlabRequest<
    { path: string; type: string; id: string; mode: string }[]
  >("tree", coordinates(g, ref));
  return tree.map((e) => ({
    path: e.path,
    type: e.type,
    mode: e.mode,
    sha: e.id,
    size: 0,
  }));
}
export async function gitlabHead(g: {
  owner: string;
  repo: string;
  branch: string;
  server?: string;
}) {
  const c = await gitlabRequest<{ sha: string; date: string }>(
    "head",
    coordinates(g),
  );
  return { sha: c.sha, date: new Date(c.date) };
}
export async function gitlabText(g: GitOrigin, path: string, ref = g.branch) {
  return (
    await gitlabRequest<{ text: string }>("file", {
      ...coordinates(g, ref),
      path,
    })
  ).text;
}
export async function gitlabPush(
  g: GitOrigin,
  files: { path: string; content: string; baseBlobSha: string | null }[],
  title: string,
) {
  const blobShas = await Promise.all(files.map((f) => blobSha(f.content)));
  if (blobShas.some((s) => !s))
    throw new Error("Cannot calculate file hashes in this browser.");
  const result = await gitlabRequest<{ url: string }>("push", {
    ...coordinates(g),
    files,
    title,
  });
  return { ...result, blobShas: blobShas as string[] };
}
