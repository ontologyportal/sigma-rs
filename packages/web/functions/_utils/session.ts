// Session storage: one GitHub access token per browser, in KV, addressed by
// an HttpOnly session_id cookie. Centralizes everything the OAuth endpoints
// (github-auth.ts / github-auth-callback.ts / logout.ts) and the read
// endpoints (me/index.ts) need, so there is exactly one place that knows the
// KV key scheme, the record shape, and every cookie's attributes.

export interface SessionEnv {
  SESSIONS: KVNamespace;
}

interface SessionData {
  access_token: string;
  created_at: number;
  /** The GitHub login, filled lazily by /api/me (or the first caller that
   *  needs it) so per-user limits cost no GitHub call per request. */
  login?: string;
}

// 30 days. Drives both the session cookie's own Max-Age and the KV record's
// `expirationTtl`, so a session can never outlive the cookie that unlocks
// it -- KV stops returning the record once this elapses, with no manual
// expiry check (and no risk of the two drifting apart) needed on read.
const SESSION_MAX_AGE_S = 30 * 24 * 60 * 60;

export const SESSION_COOKIE = "session_id";

export function getCookie(request: Request, name: string): string | null {
  const cookie = request.headers.get("Cookie") || "";
  const match = cookie.match(new RegExp(`(?:^|; )${name}=([^;]*)`));
  return match ? decodeURIComponent(match[1]) : null;
}

/** One `Set-Cookie` value builder for every cookie this app sets (the
 *  session cookie, the OAuth CSRF state cookie) -- HttpOnly + Secure +
 *  Path=/ + SameSite=Lax everywhere. Pass `maxAgeSeconds: 0` to clear. */
export function cookieHeader(
  name: string,
  value: string,
  maxAgeSeconds: number,
): string {
  return `${name}=${value}; HttpOnly; Secure; Path=/; Max-Age=${maxAgeSeconds}; SameSite=Lax`;
}

function sessionKey(sessionId: string): string {
  return `session:${sessionId}`;
}

/** Create a session for `accessToken` and return the `Set-Cookie` header
 *  that hands its id to the browser. */
export async function createSession(
  accessToken: string,
  env: SessionEnv,
): Promise<string> {
  const sessionId = crypto.randomUUID();
  const data: SessionData = {
    access_token: accessToken,
    created_at: Date.now(),
  };
  await env.SESSIONS.put(sessionKey(sessionId), JSON.stringify(data), {
    expirationTtl: SESSION_MAX_AGE_S,
  });
  return cookieHeader(SESSION_COOKIE, sessionId, SESSION_MAX_AGE_S);
}

/** The current request's session: its id (so a caller like /api/logout can
 *  delete it) and its access token. Null for no session cookie, an
 *  unknown/expired session id, or a corrupt record -- a corrupt record is
 *  deleted rather than left to fail the same way on every future request. */
export async function getSession(
  request: Request,
  env: SessionEnv,
): Promise<{
  sessionId: string;
  accessToken: string;
  login: string | null;
} | null> {
  const sessionId = getCookie(request, SESSION_COOKIE);
  if (!sessionId) return null;

  const key = sessionKey(sessionId);
  const raw = await env.SESSIONS.get(key);
  if (!raw) return null;

  try {
    const data = JSON.parse(raw) as SessionData;
    return {
      sessionId,
      accessToken: data.access_token,
      login: data.login ?? null,
    };
  } catch {
    await env.SESSIONS.delete(key);
    return null;
  }
}

/** Record the GitHub login on an existing session. The record keeps its
 *  original expiry: it is rewritten with an absolute `expiration` derived
 *  from `created_at`, never a fresh TTL that would outlive the cookie. */
export async function setSessionLogin(
  sessionId: string,
  login: string,
  env: SessionEnv,
): Promise<void> {
  const key = sessionKey(sessionId);
  const raw = await env.SESSIONS.get(key);
  if (!raw) return;
  let data: SessionData;
  try {
    data = JSON.parse(raw) as SessionData;
  } catch {
    return;
  }
  if (data.login === login) return;
  const expiration = Math.floor(data.created_at / 1000) + SESSION_MAX_AGE_S;
  // KV rejects an expiration less than 60 s away; such a session is about
  // to lapse anyway, so skip the write.
  if (expiration - Date.now() / 1000 < 120) return;
  await env.SESSIONS.put(key, JSON.stringify({ ...data, login }), {
    expiration,
  });
}

/** Delete a session by id. The cookie itself is cleared separately -- see
 *  clearSessionCookie -- since a caller may want to delete without a
 *  Response to attach a Set-Cookie header to. */
export function deleteSession(
  sessionId: string,
  env: SessionEnv,
): Promise<void> {
  return env.SESSIONS.delete(sessionKey(sessionId));
}

/** The `Set-Cookie` header value that clears the session cookie. */
export function clearSessionCookie(): string {
  return cookieHeader(SESSION_COOKIE, "", 0);
}
