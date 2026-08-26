// GET /api/github-auth
// Starts the login flow: redirects the user to GitHub to approve access,
// then GitHub sends them back to /api/github-auth-callback.

import { cookieHeader } from "../_utils/session";

interface Env {
  GITHUB_OAUTH_CLIENT_ID: string;
}

// 'repo' gives read/write on the user's repos (needed to push commits and
// open PRs). If SUMO is public and you never need private-repo access,
// 'public_repo' is a narrower, less alarming alternative.
const SCOPE = "repo";

export const onRequestGet: PagesFunction<Env> = async (context) => {
  const redirectUri = new URL("/api/github-auth-callback", context.request.url);
  redirectUri.protocol = "https";

  // Random value to prevent CSRF - verified again in the callback.
  const state = crypto.randomUUID();

  const authorizeUrl = new URL("https://github.com/login/oauth/authorize");
  authorizeUrl.searchParams.set("client_id", context.env.GITHUB_OAUTH_CLIENT_ID);
  authorizeUrl.searchParams.set("redirect_uri", redirectUri.toString());
  authorizeUrl.searchParams.set("scope", SCOPE);
  authorizeUrl.searchParams.set("state", state);

  const headers = new Headers();
  headers.set("Location", authorizeUrl.toString());
  // Short-lived cookie just to survive the round trip to GitHub and back.
  headers.append("Set-Cookie", cookieHeader("oauth_state", state, 600));

  return new Response(null, { status: 302, headers });
};