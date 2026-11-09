import { getCookie } from "../_utils/session";
import {
  type GitLabEnv,
  endGitlabSession,
  sessionFor,
  normalizeServer,
  saveToken,
  gitlabFetch,
  sameOrigin,
  SESSION_COOKIE,
} from "../_utils/gitlab";

function reply(data: unknown, status = 200) {
  return Response.json(data, {
    status,
    headers: { "Cache-Control": "no-store", Vary: "Cookie" },
  });
}
function validPath(path: unknown): path is string {
  return (
    typeof path === "string" &&
    path.length > 0 &&
    !path.includes("\\") &&
    !Array.from(path).some((c) => c.charCodeAt(0) < 32) &&
    path.split("/").every((p) => p && p !== "." && p !== "..")
  );
}

export const onRequestPost: PagesFunction<GitLabEnv> = async ({
  request,
  env,
}) => {
  if (!sameOrigin(request))
    return reply({ error: "Invalid request origin." }, 403);
  if (!request.headers.get("Content-Type")?.startsWith("application/json"))
    return reply({ error: "JSON required." }, 415);
  try {
    const reader = request.body?.getReader();
    if (!reader) return reply({ error: "JSON required." }, 400);
    const decoder = new TextDecoder();
    let raw = "";
    let size = 0;
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) break;
      size += chunk.value.byteLength;
      if (size > 5_000_000) {
        await reader.cancel();
        return reply({ error: "Request too large." }, 413);
      }
      raw += decoder.decode(chunk.value, { stream: true });
    }
    raw += decoder.decode();
    let input;
    try {
      input = JSON.parse(raw);
    } catch {
      return reply({ error: "Invalid JSON." }, 400);
    }
    if (!input || typeof input !== "object")
      return reply({ error: "Invalid request." }, 400);
    if (input.operation === "session") {
      const current = await sessionFor(request, env);
      if (!current) return reply({ user: null });
      let login = current.login;
      if (!login) {
        const profile = await gitlabFetch(
          "/user",
          current.token,
          {},
          current.server,
        );
        if (profile.status === 401) return reply({ user: null });
        if (!profile.ok)
          return reply({ error: "Cannot check the GitLab connection." }, 502);
        const user = (await profile.json()) as { username?: string };
        login = typeof user.username === "string" ? user.username : null;
      }
      return reply({ user: login ? { login, server: current.server } : null });
    }
    const server = normalizeServer(
      input.server ??
        (input.operation === "logout"
          ? (await sessionFor(request, env))?.server
          : undefined),
    );
    const fetchGitlab = (
      path: string,
      token: string | null,
      init: RequestInit = {},
    ) => gitlabFetch(path, token, init, server);
    if (input.operation === "capabilities") {
      return reply({
        oauthConfigured: Boolean(
          server === "https://gitlab.com" &&
          env.GITLAB_OAUTH_CLIENT_ID &&
          env.GITLAB_OAUTH_CLIENT_SECRET &&
          env.GITLAB_OAUTH_REDIRECT_URI,
        ),
      });
    }
    const session = await sessionFor(request, env, server);
    const token = session?.token ?? null;
    if (input.operation === "login") {
      if (
        typeof input.token !== "string" ||
        !input.token.trim() ||
        input.token.length > 4096 ||
        /\s/.test(input.token.trim()) ||
        Array.from(input.token.trim() as string).some(
          (c) => c.charCodeAt(0) < 32,
        )
      )
        return reply(
          { error: "Enter a valid GitLab personal access token." },
          400,
        );
      const personalToken = input.token.trim();
      const check = await fetchGitlab("/user", personalToken);
      if (!check.ok)
        return reply(
          {
            error:
              "GitLab rejected this token. Check that it is active and has API access.",
          },
          check.status === 401 || check.status === 403 ? check.status : 502,
        );
      const user = (await check.json()) as { username: string };
      if (typeof user.username !== "string")
        return reply({ error: "Cannot verify the GitLab account." }, 502);
      const cookie = await saveToken(
        personalToken,
        7200,
        env,
        "personal",
        server,
        user.username,
      );
      const oldId = getCookie(request, SESSION_COOKIE);
      if (oldId) await env.SESSIONS.delete(`gitlab:${oldId}`);
      const headers = new Headers({
        "Cache-Control": "no-store",
        Vary: "Cookie",
      });
      headers.append("Set-Cookie", cookie);
      return Response.json({ login: user.username, server }, { headers });
    }
    if (input.operation === "logout") {
      if (token && session?.kind === "oauth")
        await fetch("https://gitlab.com/oauth/revoke", {
          method: "POST",
          redirect: "manual",
          body: new URLSearchParams({
            client_id: env.GITLAB_OAUTH_CLIENT_ID,
            client_secret: env.GITLAB_OAUTH_CLIENT_SECRET,
            token,
          }),
        }).catch(() => {});
      if (!session) return reply({});
      return new Response("{}", {
        headers: {
          "Set-Cookie": await endGitlabSession(request, env),
          "Cache-Control": "no-store",
        },
      });
    }
    if (
      !validPath(input.owner) ||
      !validPath(input.repo) ||
      input.repo.includes("/") ||
      typeof input.ref !== "string" ||
      !input.ref ||
      input.ref.length > 1024
    )
      return reply({ error: "Invalid repository or branch." }, 400);
    const project = `/projects/${encodeURIComponent(`${input.owner}/${input.repo}`)}`;
    const query = `ref=${encodeURIComponent(input.ref)}`;
    const read = async (path: string) => {
      const r = await fetchGitlab(path, token);
      if (!r.ok)
        throw Object.assign(
          new Error(
            r.status === 401
              ? "Sign in to GitLab again."
              : `GitLab request failed (${r.status}). Check access and branch protection.`,
          ),
          { status: r.status },
        );
      return r.json();
    };
    if (input.operation === "head") {
      const commit = (await read(
        `${project}/repository/commits/${encodeURIComponent(input.ref)}`,
      )) as { id: string; committed_date: string };
      return reply({ sha: commit.id, date: commit.committed_date });
    }
    if (input.operation === "branches") {
      const branches: { name: string }[] = [];
      for (let page = 1; page <= 100; page++) {
        const r = await fetchGitlab(
          `${project}/repository/branches?per_page=100&page=${page}`,
          token,
        );
        if (!r.ok)
          return reply(
            {
              error:
                r.status === 404
                  ? `Project ${input.owner}/${input.repo} was not found on ${new URL(server).host}, or your token cannot access it. Enter a project path including its name; groups do not have branches.${token ? "" : " Connect to this GitLab server to access private projects."}`
                  : r.status === 401
                    ? "Your GitLab connection has expired. Connect again."
                    : `Cannot list branches for ${input.owner}/${input.repo} (${r.status}). Check your token's project access.`,
            },
            r.status,
          );
        branches.push(...((await r.json()) as { name: string }[]));
        if (!r.headers.get("x-next-page"))
          return reply(branches.map((b) => b.name));
      }
      return reply({ error: "Too many branches." }, 413);
    }
    if (input.operation === "tree") {
      const entries: unknown[] = [];
      for (let page = 1; page <= 1000; page++) {
        const r = await fetchGitlab(
          `${project}/repository/tree?${query}&recursive=true&per_page=100&page=${page}`,
          token,
        );
        if (!r.ok)
          return reply(
            {
              error: `Cannot list GitLab repository (${r.status}). Sign in if it is private.`,
            },
            r.status,
          );
        entries.push(...((await r.json()) as unknown[]));
        if (!r.headers.get("x-next-page")) return reply(entries);
      }
      return reply({ error: "Repository listing is too large." }, 413);
    }
    if (input.operation === "file") {
      if (!validPath(input.path))
        return reply({ error: "Invalid file path." }, 400);
      const r = await fetchGitlab(
        `${project}/repository/files/${encodeURIComponent(input.path)}/raw?${query}`,
        token,
      );
      if (!r.ok)
        return reply(
          {
            error: `Cannot read GitLab file (${r.status}). Sign in if it is private.`,
          },
          r.status,
        );
      return reply({ text: await r.text() });
    }
    if (input.operation !== "push")
      return reply({ error: "Unknown operation." }, 400);
    if (!token)
      return reply({ error: "Sign in to GitLab before pushing." }, 401);
    if (
      !Array.isArray(input.files) ||
      !input.files.length ||
      input.files.length > 100 ||
      typeof input.title !== "string" ||
      !input.title.trim()
    )
      return reply({ error: "Choose saved files and a commit title." }, 400);
    const paths = new Set<string>();
    for (const f of input.files) {
      if (
        !validPath(f.path) ||
        paths.has(f.path) ||
        typeof f.content !== "string" ||
        !(
          f.baseBlobSha === null ||
          (typeof f.baseBlobSha === "string" &&
            /^[a-f0-9]{40}$/.test(f.baseBlobSha))
        )
      )
        return reply({ error: "Invalid or duplicate file." }, 400);
      paths.add(f.path);
    }
    const actions = [];
    for (const f of input.files) {
      const r = await fetchGitlab(
        `${project}/repository/files/${encodeURIComponent(f.path)}?${query}`,
        token,
      );
      if (r.status === 404 && f.baseBlobSha === null) {
        actions.push({
          action: "create",
          file_path: f.path,
          content: f.content,
        });
      } else {
        if (!r.ok)
          return reply(
            { error: `Cannot verify ${f.path}. Reload before pushing.` },
            409,
          );
        const file = (await r.json()) as {
          blob_id: string;
          last_commit_id: string;
        };
        if (
          !f.baseBlobSha ||
          file.blob_id !== f.baseBlobSha ||
          !file.last_commit_id
        )
          return reply(
            {
              error: `${f.path} changed remotely. Reload or resolve changes before pushing.`,
            },
            409,
          );
        actions.push({
          action: "update",
          file_path: f.path,
          content: f.content,
          last_commit_id: file.last_commit_id,
        });
      }
    }
    const r = await fetchGitlab(`${project}/repository/commits`, token, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        branch: input.ref,
        commit_message: input.title.trim(),
        actions,
      }),
    });
    if (!r.ok)
      return reply(
        {
          error: `GitLab rejected the commit (${r.status}). Check remote changes, permissions and branch protection. Your local edits are retained.`,
        },
        409,
      );
    const commit = (await r.json()) as { id: string };
    return reply({
      url: `${server}/${input.owner}/${input.repo}/-/commit/${commit.id}`,
    });
  } catch (e) {
    const status = (e as { status?: number }).status;
    return reply(
      {
        error: status
          ? (e as Error).message
          : "GitLab request failed. Please retry.",
      },
      status || 502,
    );
  }
};
