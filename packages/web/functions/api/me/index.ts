// GET /api/me
// Returns the logged-in user's profile (display name + avatar) AND their
// GitHub access token in one call -- src/auth.ts needs both on every page
// load, and splitting them across two endpoints cost a second Functions
// invocation and a second KV read to answer the same "who is logged in"
// question.

import { getGithubToken } from "../../_utils/session";

interface Env {
  SESSIONS: KVNamespace;
}

interface GithubUser {
  login: string;
  name: string | null;
  avatar_url: string;
}

export const onRequestGet: PagesFunction<Env> = async (context) => {
  const token = await getGithubToken(context.request, context.env);
  if (!token) {
    return new Response("Not logged in", { status: 401 });
  }

  const res = await fetch("https://api.github.com/user", {
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      // GitHub rejects requests without a User-Agent.
      "User-Agent": "sigmakee.dev",
    },
  });

  if (!res.ok) {
    // Covers a revoked/expired token as well as any GitHub-side failure.
    return new Response("Could not fetch GitHub profile", {
      status: res.status === 401 ? 401 : 502,
    });
  }

  const user = (await res.json()) as GithubUser;

  return new Response(
    JSON.stringify({
      // Many users never set a display name - fall back to their username.
      name: user.name || user.login,
      login: user.login,
      avatarUrl: user.avatar_url,
      token,
    }),
    { headers: { "Content-Type": "application/json" } }
  );
};
