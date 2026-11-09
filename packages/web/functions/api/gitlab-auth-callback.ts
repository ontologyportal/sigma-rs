import { cookieHeader, getCookie } from "../_utils/session";
import {
  type GitLabEnv,
  STATE_COOKIE,
  VERIFIER_COOKIE,
  saveToken,
} from "../_utils/gitlab";

export const onRequestGet: PagesFunction<GitLabEnv> = async ({
  request,
  env,
}) => {
  const url = new URL(request.url);
  const state = getCookie(request, STATE_COOKIE);
  const verifier = getCookie(request, VERIFIER_COOKIE);
  const headers = new Headers({
    "Cache-Control": "no-store",
    "Referrer-Policy": "no-referrer",
  });
  headers.append("Set-Cookie", cookieHeader(STATE_COOKIE, "", 0));
  headers.append("Set-Cookie", cookieHeader(VERIFIER_COOKIE, "", 0));
  if (
    !state ||
    state !== url.searchParams.get("state") ||
    !verifier ||
    !url.searchParams.get("code")
  )
    return new Response("Invalid GitLab login state.", {
      status: 400,
      headers,
    });
  try {
    const response = await fetch("https://gitlab.com/oauth/token", {
      method: "POST",
      redirect: "manual",
      body: new URLSearchParams({
        grant_type: "authorization_code",
        code: url.searchParams.get("code")!,
        code_verifier: verifier,
        client_id: env.GITLAB_OAUTH_CLIENT_ID,
        client_secret: env.GITLAB_OAUTH_CLIENT_SECRET,
        redirect_uri: env.GITLAB_OAUTH_REDIRECT_URI,
      }),
    });
    if (!response.ok) throw new Error("Token exchange failed");
    const data = (await response.json()) as {
      access_token: string;
      expires_in: number;
    };
    headers.append(
      "Set-Cookie",
      await saveToken(data.access_token, data.expires_in, env),
    );
    headers.set("Location", "/");
    return new Response(null, { status: 302, headers });
  } catch {
    return new Response("GitLab login failed. Please try signing in again.", {
      status: 502,
      headers,
    });
  }
};
