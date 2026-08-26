// POST /api/logout
// Revokes the user's GitHub token (not just the local session), deletes
// the session from KV, and clears the session cookie.

import { getSession, deleteSession, clearSessionCookie } from "../_utils/session";

interface Env {
  GITHUB_OAUTH_CLIENT_ID: string;
  GITHUB_OAUTH_CLIENT_SECRET: string;
  SESSIONS: KVNamespace;
}

export const onRequestPost: PagesFunction<Env> = async (context) => {
  const session = await getSession(context.request, context.env);

  if (session) {
    // Revoke the token itself so it's actually dead on GitHub's side, not
    // just forgotten by us - this only revokes this one token, so other
    // devices/sessions for the same user stay logged in.
    await fetch(
      `https://api.github.com/applications/${context.env.GITHUB_OAUTH_CLIENT_ID}/token`,
      {
        method: "DELETE",
        headers: {
          Accept: "application/vnd.github+json",
          Authorization:
            "Basic " +
            btoa(`${context.env.GITHUB_OAUTH_CLIENT_ID}:${context.env.GITHUB_OAUTH_CLIENT_SECRET}`),
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ access_token: session.accessToken }),
      }
    ).catch(() => {
      // Best-effort: if GitHub's API is unreachable, still clear the
      // local session below rather than blocking logout entirely.
    });

    await deleteSession(session.sessionId, context.env);
  }

  const headers = new Headers({ "Content-Type": "application/json" });
  headers.append("Set-Cookie", clearSessionCookie());

  return new Response(JSON.stringify({ loggedOut: true }), { headers });
};
