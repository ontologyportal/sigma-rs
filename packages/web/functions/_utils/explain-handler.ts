// The handler behind /api/explain (functions/api/explain.ts): a short English
// explanation of one proof (Ask/Tell or an audit contradiction), from
// Workers AI.
//
// Cost control is the design constraint: a hijacked endpoint may only spend a
// known, bounded amount. There is no prompt field -- the prompt is built here
// from validated proof steps (_utils/explain.ts) -- and the checks run
// cheapest first: origin, per-IP rate, GitHub identity, validation, the
// content-hash cache (a repeat costs nothing), per-user rate and daily count,
// then the global daily neuron budget, which fails closed.
//
// Bindings (wrangler.jsonc; missing AI or EXPLAIN = disabled, 503):
//   AI                 Workers AI
//   EXPLAIN            KV: cache + counters (own key scheme, not SESSIONS)
//   EXPLAIN_RL_USER/IP rate-limit bindings (optional; skipped when absent)
//   EXPLAIN_MODEL      model id (default DEFAULT_MODEL)
//   EXPLAIN_DAILY_CAP  global neurons per UTC day (default 9000, inside the
//                      10,000/day Workers AI free allocation)
//   EXPLAIN_USER_DAILY explanations per user per UTC day (default 20)
//   EXPLAIN_GATEWAY    optional AI Gateway id

import { getSession, setSessionLogin, type SessionEnv } from "./session";
import {
  DEFAULT_MODEL,
  LIMITS,
  buildMessages,
  cacheKey,
  estimateTokens,
  modelInputs,
  neurons,
  readModelOutput,
  sha256Hex,
  validateExplainRequest,
  type ExplainErrorBody,
  type ExplainErrorCode,
  type ExplainResponse,
} from "./explain";

interface ExplainAi {
  run(
    model: string,
    inputs: Record<string, unknown>,
    options?: Record<string, unknown>,
  ): Promise<unknown>;
}

interface RateLimiter {
  limit(options: { key: string }): Promise<{ success: boolean }>;
}

export interface ExplainEnv extends SessionEnv {
  AI?: ExplainAi;
  EXPLAIN?: KVNamespace;
  EXPLAIN_RL_USER?: RateLimiter;
  EXPLAIN_RL_IP?: RateLimiter;
  EXPLAIN_MODEL?: string;
  EXPLAIN_DAILY_CAP?: string;
  EXPLAIN_USER_DAILY?: string;
  EXPLAIN_GATEWAY?: string;
}

const DEFAULT_DAILY_CAP = 9_000;
const DEFAULT_USER_DAILY = 20;
/** Workers AI's refusal once the account's free daily neurons are spent
 *  (error 4006 on the Workers Free plan). */
const ALLOCATION_SPENT = /\b4006\b|free allocation/i;
/** The rate-limit bindings' period (wrangler.jsonc `simple.period`). */
const RATE_PERIOD_S = 60;
const COUNTER_TTL_S = 2 * 24 * 60 * 60;
const CACHE_TTL_S = 30 * 24 * 60 * 60;
/** How long a personal token's resolved login is trusted. */
const TOKEN_LOGIN_TTL_S = 60 * 60;

function json(status: number, body: unknown, headers?: HeadersInit): Response {
  const h = new Headers(headers);
  h.set("Content-Type", "application/json");
  h.set("Cache-Control", "no-store");
  return new Response(JSON.stringify(body), { status, headers: h });
}

function fail(
  status: number,
  error: ExplainErrorCode,
  message: string,
  retryAfter?: number,
): Response {
  const body: ExplainErrorBody = { error, message };
  if (retryAfter !== undefined) body.retryAfter = retryAfter;
  return json(
    status,
    body,
    retryAfter !== undefined ? { "Retry-After": String(retryAfter) } : {},
  );
}

function positiveInt(raw: string | undefined, fallback: number): number {
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : fallback;
}

async function githubLogin(token: string): Promise<string | null> {
  try {
    const res = await fetch("https://api.github.com/user", {
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "sigmakee.dev",
      },
    });
    if (!res.ok) return null;
    const user = (await res.json()) as { login?: unknown };
    return typeof user.login === "string" && user.login ? user.login : null;
  } catch {
    return null;
  }
}

/** The caller's GitHub login: from the OAuth session (cached on the session
 *  record), else from an `Authorization: Bearer` personal token (cached in
 *  EXPLAIN by token hash). Null = not signed in. */
async function resolveLogin(
  request: Request,
  env: ExplainEnv,
  kv: KVNamespace,
): Promise<string | null> {
  const session = await getSession(request, env);
  if (session) {
    if (session.login) return session.login;
    const login = await githubLogin(session.accessToken);
    if (login) await setSessionLogin(session.sessionId, login, env);
    return login;
  }
  const m = (request.headers.get("Authorization") ?? "").match(
    /^Bearer\s+(\S+)$/,
  );
  if (!m) return null;
  const key = `tok:${await sha256Hex(m[1])}`;
  const cached = await kv.get(key);
  if (cached) return cached;
  const login = await githubLogin(m[1]);
  if (login) await kv.put(key, login, { expirationTtl: TOKEN_LOGIN_TTL_S });
  return login;
}

async function readCounter(kv: KVNamespace, key: string): Promise<number> {
  const n = Number(await kv.get(key));
  return Number.isFinite(n) && n > 0 ? n : 0;
}

function secondsUntilUtcMidnight(now: number): number {
  const d = new Date(now);
  const midnight = Date.UTC(
    d.getUTCFullYear(),
    d.getUTCMonth(),
    d.getUTCDate() + 1,
  );
  return Math.max(1, Math.ceil((midnight - now) / 1000));
}

export function explainEnabled(env: ExplainEnv): boolean {
  return !!env.AI && !!env.EXPLAIN;
}

export async function handleExplain(
  request: Request,
  env: ExplainEnv,
  now = Date.now(),
): Promise<Response> {
  const ai = env.AI;
  const kv = env.EXPLAIN;
  if (!ai || !kv)
    return fail(503, "unavailable", "Explanations are not enabled here.");

  // CSRF: a cross-site form or fetch carries the attacker's Origin. Compare
  // hosts only -- a TLS-terminating dev proxy changes the scheme.
  const origin = request.headers.get("Origin");
  let sameHost: boolean;
  try {
    sameHost = !!origin && new URL(origin).host === new URL(request.url).host;
  } catch {
    sameHost = false;
  }
  if (!sameHost) return fail(403, "forbidden", "Cross-origin request.");

  const ip = request.headers.get("CF-Connecting-IP") ?? "unknown";
  if (
    env.EXPLAIN_RL_IP &&
    !(await env.EXPLAIN_RL_IP.limit({ key: `ip:${ip}` })).success
  )
    return fail(429, "rate_limited", "Too many requests.", RATE_PERIOD_S);

  const login = await resolveLogin(request, env, kv);
  if (!login)
    return fail(401, "unauthorized", "Sign in with GitHub to explain proofs.");

  const declared = Number(request.headers.get("Content-Length") ?? 0);
  if (declared > LIMITS.maxBodyBytes * 2)
    return fail(400, "invalid", "Request body too large.");
  const raw = await request.text();
  if (raw.length > LIMITS.maxBodyBytes * 2)
    return fail(400, "invalid", "Request body too large.");
  let body: unknown;
  try {
    body = JSON.parse(raw);
  } catch {
    return fail(400, "invalid", "Body is not JSON.");
  }
  const checked = validateExplainRequest(body);
  if (!checked.ok) return fail(400, "invalid", checked.reason);
  const req = checked.request;

  const model = env.EXPLAIN_MODEL || DEFAULT_MODEL;
  const key = await cacheKey(req, model);
  const hit = await kv.get(key);
  if (hit) {
    try {
      const stored = JSON.parse(hit) as { explanation: string; model: string };
      const res: ExplainResponse = { ...stored, cached: true };
      return json(200, res);
    } catch {
      await kv.delete(key);
    }
  }

  if (
    env.EXPLAIN_RL_USER &&
    !(await env.EXPLAIN_RL_USER.limit({ key: `user:${login}` })).success
  )
    return fail(429, "rate_limited", "Too many requests.", RATE_PERIOD_S);

  const day = new Date(now).toISOString().slice(0, 10);
  const userKey = `rl:${login}:${day}`;
  const used = await readCounter(kv, userKey);
  if (used >= positiveInt(env.EXPLAIN_USER_DAILY, DEFAULT_USER_DAILY))
    return fail(
      429,
      "rate_limited",
      "Daily explanation limit reached.",
      secondsUntilUtcMidnight(now),
    );

  const messages = buildMessages(req);
  const promptTokens = estimateTokens(
    messages.reduce((n, m) => n + m.content.length, 0),
  );
  const reserve = neurons(model, promptTokens, LIMITS.maxOutputTokens);
  const budgetKey = `budget:${day}`;
  const spent = await readCounter(kv, budgetKey);
  const cap = positiveInt(env.EXPLAIN_DAILY_CAP, DEFAULT_DAILY_CAP);
  if (spent + reserve > cap)
    return fail(503, "paused", "Explanations are paused for today.");

  await kv.put(userKey, String(used + 1), { expirationTtl: COUNTER_TTL_S });

  let output: ReturnType<typeof readModelOutput>;
  try {
    const options = env.EXPLAIN_GATEWAY
      ? { gateway: { id: env.EXPLAIN_GATEWAY } }
      : undefined;
    output = readModelOutput(
      await ai.run(model, modelInputs(model, messages), options),
    );
  } catch (e) {
    // The account's free allocation ran out (other use, or estimates that
    // undercounted): close today's budget so later requests stop before the
    // model call.
    if (ALLOCATION_SPENT.test(e instanceof Error ? e.message : String(e))) {
      await kv.put(budgetKey, String(Math.max(cap, spent)), {
        expirationTtl: COUNTER_TTL_S,
      });
      return fail(503, "paused", "Explanations are paused for today.");
    }
    // The call may still have been billed: charge the reservation.
    await kv.put(budgetKey, String(spent + reserve), {
      expirationTtl: COUNTER_TTL_S,
    });
    return fail(502, "failed", "The model call failed.");
  }

  const cost = neurons(
    model,
    output.inputTokens ?? promptTokens,
    output.outputTokens ?? estimateTokens(output.text.length),
  );
  await kv.put(budgetKey, String(spent + cost), {
    expirationTtl: COUNTER_TTL_S,
  });
  if (!output.text)
    return fail(502, "failed", "The model returned no explanation.");

  await kv.put(key, JSON.stringify({ explanation: output.text, model }), {
    expirationTtl: CACHE_TTL_S,
  });
  const res: ExplainResponse = {
    explanation: output.text,
    cached: false,
    model,
  };
  return json(200, res);
}
