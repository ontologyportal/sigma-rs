import { cookieHeader } from "../_utils/session";
import {
  type GitLabEnv,
  STATE_COOKIE,
  VERIFIER_COOKIE,
} from "../_utils/gitlab";

export const onRequestGet: PagesFunction<GitLabEnv> = async ({ env }) => {
  if (
    !env.GITLAB_OAUTH_CLIENT_ID ||
    !env.GITLAB_OAUTH_REDIRECT_URI ||
    !env.GITLAB_OAUTH_CLIENT_SECRET
  )
    return new Response(null, {
      status: 302,
      headers: { Location: "/", "Cache-Control": "no-store" },
    });
  const state = crypto.randomUUID();
  const verifier = crypto.randomUUID() + crypto.randomUUID();
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(verifier),
  );
  const challenge = btoa(String.fromCharCode(...new Uint8Array(digest)))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
  const url = new URL("https://gitlab.com/oauth/authorize");
  url.search = new URLSearchParams({
    client_id: env.GITLAB_OAUTH_CLIENT_ID,
    redirect_uri: env.GITLAB_OAUTH_REDIRECT_URI,
    response_type: "code",
    scope: "api",
    state,
    code_challenge: challenge,
    code_challenge_method: "S256",
  }).toString();
  const headers = new Headers({
    Location: url.toString(),
    "Cache-Control": "no-store",
  });
  headers.append("Set-Cookie", cookieHeader(STATE_COOKIE, state, 600));
  headers.append("Set-Cookie", cookieHeader(VERIFIER_COOKIE, verifier, 600));
  return new Response(null, { status: 302, headers });
};
