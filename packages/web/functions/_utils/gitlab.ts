import { cookieHeader, getCookie } from "./session";

export interface GitLabEnv {
  SESSIONS: KVNamespace;
  GITLAB_OAUTH_CLIENT_ID: string;
  GITLAB_OAUTH_CLIENT_SECRET: string;
  /** Exact public callback URL registered with GitLab. */
  GITLAB_OAUTH_REDIRECT_URI: string;
}

export const SESSION_COOKIE = "gitlab_session";
export const STATE_COOKIE = "gitlab_state";
export const VERIFIER_COOKIE = "gitlab_verifier";

/** Normalize a GitLab website URL to its HTTPS origin, keeping groups separate. */
export function normalizeServer(value: unknown = "https://gitlab.com"): string {
  try {
    if (typeof value !== "string" || !value.trim()) throw new Error();
    const url = new URL(value.trim());
    const host = url.hostname.toLowerCase().replace(/\.$/, "");
    if (
      url.protocol !== "https:" ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      url.port ||
      !/^(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z][a-z0-9-]*$/.test(host) ||
      /(?:^|\.)(?:localhost|local|internal|lan|home|test|invalid)$/.test(host)
    )
      throw new Error();
    return `https://${host}`;
  } catch {
    throw Object.assign(
      new Error(
        "Enter an HTTPS GitLab server URL without credentials, a query, or a fragment.",
      ),
      { status: 400 },
    );
  }
}

/** GitLab credentials never leave the server; expired tokens require sign-in. */
export async function sessionFor(
  request: Request,
  env: GitLabEnv,
  server?: string,
) {
  const id = getCookie(request, SESSION_COOKIE);
  if (!id || !/^[a-f0-9-]{36}$/.test(id)) return null;
  const raw = await env.SESSIONS.get(`gitlab:${id}`);
  if (!raw) return null;
  try {
    const data = JSON.parse(raw) as {
      token: string;
      expires: number;
      kind?: "oauth" | "personal";
      server?: string;
      login?: string;
    };
    const storedServer = normalizeServer(data.server ?? "https://gitlab.com");
    if (server && storedServer !== server) return null;
    if (
      typeof data.token === "string" &&
      data.token &&
      data.expires > Date.now()
    )
      return {
        token: data.token,
        server: storedServer,
        login: typeof data.login === "string" ? data.login : null,
        kind:
          data.kind === "personal" ? ("personal" as const) : ("oauth" as const),
      };
  } catch {
    /* An invalid session is treated as signed out. */
  }
  await env.SESSIONS.delete(`gitlab:${id}`);
  return null;
}

/** Store only an expiring access token, without retaining a refresh token. */
export async function saveToken(
  token: string,
  seconds: number,
  env: GitLabEnv,
  kind: "oauth" | "personal" = "oauth",
  server = "https://gitlab.com",
  login?: string,
) {
  const id = crypto.randomUUID();
  const ttl = Math.min(Math.floor(seconds), 7200);
  if (typeof token !== "string" || !token || !Number.isFinite(ttl) || ttl < 60)
    throw new Error("Invalid token expiry");
  await env.SESSIONS.put(
    `gitlab:${id}`,
    JSON.stringify({
      token,
      kind,
      login,
      server: normalizeServer(server),
      expires: Date.now() + ttl * 1000,
    }),
    { expirationTtl: ttl },
  );
  return cookieHeader(SESSION_COOKIE, id, ttl);
}

/** Validated HTTPS origins and manual redirects keep credentials on their server. */
export async function gitlabFetch(
  path: string,
  token: string | null,
  init: RequestInit = {},
  server = "https://gitlab.com",
) {
  const response = await fetch(`${normalizeServer(server)}/api/v4${path}`, {
    ...init,
    redirect: "manual",
    headers: {
      Accept: "application/json",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...init.headers,
    },
  });
  if (response.status >= 300 && response.status < 400) {
    throw Object.assign(
      new Error(
        "GitLab redirected the request. Credentials were not forwarded.",
      ),
      { status: 502 },
    );
  }
  return response;
}

/** Browser mutations require an exact same-origin request. */
export function sameOrigin(request: Request) {
  return request.headers.get("Origin") === new URL(request.url).origin;
}

/** Forget this browser's GitLab connection without revoking a personal token. */
export async function endGitlabSession(
  request: Request,
  env: Pick<GitLabEnv, "SESSIONS">,
): Promise<string> {
  const id = getCookie(request, SESSION_COOKIE);
  if (id) await env.SESSIONS.delete(`gitlab:${id}`);
  return cookieHeader(SESSION_COOKIE, "", 0);
}
