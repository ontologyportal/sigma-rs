// GET /api/github-auth-callback
// GitHub redirects here after the user approves (or denies) access.
// Exchanges the code for an access token and stores it server-side, in a KV
// record keyed by a random session id -- see _utils/session.ts. The token
// itself does reach the browser afterward, on demand: /api/me hands it to
// page JS on every load so it can be sent as an Authorization header
// straight from the browser to api.github.com (see src/auth.ts). What this
// endpoint keeps off the wire is the OAuth *exchange* -- the client secret
// and the code-for-token trade never touch the browser.

import { getCookie, cookieHeader, createSession } from "../_utils/session";

interface Env {
  GITHUB_OAUTH_CLIENT_ID: string;
  GITHUB_OAUTH_CLIENT_SECRET: string;
  // Bind a KV namespace named SESSIONS in your Pages project settings
  // (Settings > Functions > KV namespace bindings).
  SESSIONS: KVNamespace;
}

interface TokenResponse {
  access_token?: string;
  scope?: string;
  token_type?: string;
  error?: string;
  error_description?: string;
}

export const onRequestGet: PagesFunction<Env> = async (context) => {
  const url = new URL(context.request.url);
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  const savedState = getCookie(context.request, "oauth_state");

  if (!code || !state || !savedState || state !== savedState) {
    return new Response("Invalid or missing OAuth state.", { status: 400 });
  }

  // Same derivation as in github-auth.ts - since this request landed on
  // whatever host GitHub redirected back to, it naturally matches.
  const redirectUri = new URL("/api/github-auth-callback", context.request.url);
  redirectUri.protocol = "https";

  const tokenRes = await fetch("https://github.com/login/oauth/access_token", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Accept: "application/json",
    },
    body: JSON.stringify({
      client_id: context.env.GITHUB_OAUTH_CLIENT_ID,
      client_secret: context.env.GITHUB_OAUTH_CLIENT_SECRET,
      code,
      redirect_uri: redirectUri.toString(),
    }),
  });

  const tokenData = (await tokenRes.json()) as TokenResponse;

  if (!tokenData.access_token) {
    return new Response(
      `GitHub auth failed: ${tokenData.error_description || tokenData.error || "unknown error"}`,
      { status: 400 }
    );
  }

  const sessionCookie = await createSession(tokenData.access_token, context.env);

  const headers = new Headers();
  headers.set("Location", "/"); // send them back to the app
  headers.append("Set-Cookie", sessionCookie);
  // Clear the now-used state cookie.
  headers.append("Set-Cookie", cookieHeader("oauth_state", "", 0));

  return new Response(null, { status: 302, headers });
};
