import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";

/** Load a sibling .ts module as CommonJS, resolving its relative imports the
 *  same way. */
const loaded = new Map();
function load(name) {
  if (loaded.has(name)) return loaded.get(name);
  const out = ts.transpileModule(
    readFileSync(new URL(`./${name}.ts`, import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText;
  const exports = {};
  loaded.set(name, exports);
  new Function("exports", "require", out)(exports, (spec) =>
    load(spec.replace(/^\.\//, "")),
  );
  return exports;
}
const { handleExplain, explainEnabled } = load("explain-handler");
const { createSession } = load("session");
const { cacheKey, validateExplainRequest, DEFAULT_MODEL } = load("explain");

class FakeKV {
  constructor() {
    this.map = new Map();
    this.puts = [];
  }
  async get(k) {
    return this.map.has(k) ? this.map.get(k) : null;
  }
  async put(k, v, opts) {
    this.map.set(k, v);
    this.puts.push({ k, v, opts });
  }
  async delete(k) {
    this.map.delete(k);
  }
}

function fakeAi(
  reply = {
    response: "**Conflict**: ok",
    usage: { prompt_tokens: 1000, completion_tokens: 100 },
  },
) {
  const ai = {
    calls: [],
    async run(model, inputs, options) {
      ai.calls.push({ model, inputs, options });
      if (reply instanceof Error) throw reply;
      return reply;
    },
  };
  return ai;
}

const ORIGIN = "https://sigmakee.dev";
const body = () => ({
  kind: "contradiction",
  steps: [
    {
      index: 0,
      rule: "axiom",
      premises: [],
      kif: "(subclass Dog Cat)",
      file: "Merge.kif",
      line: 1,
    },
    {
      index: 1,
      rule: "axiom",
      premises: [],
      kif: "(disjoint Dog Cat)",
      file: "Merge.kif",
      line: 2,
    },
    { index: 2, rule: "resolution", premises: [0, 1], kif: "False" },
  ],
});

async function setup(overrides = {}) {
  const env = {
    SESSIONS: new FakeKV(),
    EXPLAIN: new FakeKV(),
    AI: fakeAi(),
    ...overrides,
  };
  const cookie = (await createSession("gho_token", env)).split(";")[0];
  return { env, cookie };
}

function req(cookie, { origin = ORIGIN, json = body(), headers = {} } = {}) {
  const h = { "Content-Type": "application/json", ...headers };
  if (origin) h.Origin = origin;
  if (cookie) h.Cookie = cookie;
  return new Request(`${ORIGIN}/api/explain`, {
    method: "POST",
    headers: h,
    body: typeof json === "string" ? json : JSON.stringify(json),
  });
}

/** GitHub /user stub: `tokens` maps an access token to its login. */
function stubGithub(tokens = { gho_token: "octo" }) {
  const calls = [];
  globalThis.fetch = async (url, init) => {
    calls.push(url);
    const token = String(init?.headers?.Authorization ?? "").replace(
      "Bearer ",
      "",
    );
    const login = tokens[token];
    return login
      ? new Response(JSON.stringify({ login }), { status: 200 })
      : new Response("bad credentials", { status: 401 });
  };
  return calls;
}

const NOW = Date.UTC(2026, 9, 26, 12, 0, 0);

test("503 unavailable when the AI or EXPLAIN binding is missing", async () => {
  const { env, cookie } = await setup({ AI: undefined });
  assert.equal(explainEnabled(env), false);
  const res = await handleExplain(req(cookie), env, NOW);
  assert.equal(res.status, 503);
  assert.equal((await res.json()).error, "unavailable");
  const { env: env2 } = await setup({ EXPLAIN: undefined });
  assert.equal(explainEnabled(env2), false);
});

test("401 without a session or token", async () => {
  stubGithub();
  const { env } = await setup();
  const res = await handleExplain(req(null), env, NOW);
  assert.equal(res.status, 401);
  assert.equal(env.AI.calls.length, 0);
});

test("403 on a foreign or missing Origin", async () => {
  stubGithub();
  const { env, cookie } = await setup();
  for (const origin of ["https://evil.example", null, "not a url"]) {
    const res = await handleExplain(req(cookie, { origin }), env, NOW);
    assert.equal(res.status, 403);
  }
  // A dev proxy that terminates TLS: same host, other scheme, is allowed.
  stubGithub();
  const ok = await handleExplain(
    req(cookie, { origin: "http://sigmakee.dev" }),
    env,
    NOW,
  );
  assert.equal(ok.status, 200);
});

test("400 with a reason for an invalid body", async () => {
  stubGithub();
  const { env, cookie } = await setup();
  const bad = body();
  bad.steps[2].premises = [7];
  const res = await handleExplain(req(cookie, { json: bad }), env, NOW);
  assert.equal(res.status, 400);
  assert.match((await res.json()).message, /missing step/);
  const notJson = await handleExplain(req(cookie, { json: "{" }), env, NOW);
  assert.equal(notJson.status, 400);
  const prompt = await handleExplain(
    req(cookie, { json: { kind: "contradiction", prompt: "hi", steps: [] } }),
    env,
    NOW,
  );
  assert.equal(prompt.status, 400);
  assert.equal(env.AI.calls.length, 0);
});

test("success calls the model once, caches, and charges the budget", async () => {
  const gh = stubGithub();
  const { env, cookie } = await setup({ EXPLAIN_GATEWAY: "gw" });
  const res = await handleExplain(req(cookie), env, NOW);
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), {
    explanation: "**Conflict**: ok",
    cached: false,
    model: DEFAULT_MODEL,
  });
  assert.equal(env.AI.calls.length, 1);
  const call = env.AI.calls[0];
  assert.equal(call.model, DEFAULT_MODEL);
  assert.equal(call.inputs.max_completion_tokens, 1200);
  assert.equal(call.inputs.reasoning_effort, "low");
  assert.equal(call.inputs.max_tokens, undefined);
  assert.deepEqual(call.options, { gateway: { id: "gw" } });
  assert.match(
    call.inputs.messages[1].content,
    /\[3\] resolution from \[1, 2\]: False/,
  );
  // 1000 * 13636 / 1e6 + 100 * 45455 / 1e6 = 18.2 -> 19 neurons.
  assert.equal(await env.EXPLAIN.get("budget:2026-10-26"), "19");
  assert.equal(await env.EXPLAIN.get("rl:octo:2026-10-26"), "1");
  const key = await cacheKey(
    validateExplainRequest(body()).request,
    DEFAULT_MODEL,
  );
  assert.ok(await env.EXPLAIN.get(key));
  // The login was resolved once and stored on the session record.
  assert.equal(gh.length, 1);
  const [record] = [...env.SESSIONS.map.values()];
  assert.equal(JSON.parse(record).login, "octo");

  // A whitespace-different repeat is a cache hit: no model call, no charge.
  const again = body();
  again.steps[0].kif = "(subclass\n  Dog Cat)";
  const hit = await handleExplain(req(cookie, { json: again }), env, NOW);
  assert.equal(hit.status, 200);
  assert.equal((await hit.json()).cached, true);
  assert.equal(env.AI.calls.length, 1);
  assert.equal(await env.EXPLAIN.get("budget:2026-10-26"), "19");
  assert.equal(gh.length, 1, "session login is cached");
});

test("a personal token works without a session, resolved once per hour", async () => {
  const gh = stubGithub({ ghp_pat: "patuser" });
  const { env } = await setup();
  const headers = { Authorization: "Bearer ghp_pat" };
  const res = await handleExplain(req(null, { headers }), env, NOW);
  assert.equal(res.status, 200);
  const other = body();
  other.steps[0].kif = "(subclass Dog Mouse)";
  await handleExplain(req(null, { headers, json: other }), env, NOW);
  assert.equal(gh.length, 1);
  assert.equal(await env.EXPLAIN.get("rl:patuser:2026-10-26"), "2");
  const tok = env.EXPLAIN.puts.find((p) => p.k.startsWith("tok:"));
  assert.equal(tok.opts.expirationTtl, 3600);
  assert.doesNotMatch(tok.k, /ghp_pat/, "the token itself is never a key");
  const bad = await handleExplain(
    req(null, { headers: { Authorization: "Bearer nope" } }),
    env,
    NOW,
  );
  assert.equal(bad.status, 401);
});

test("429 from the per-minute limiters and the daily per-user count", async () => {
  stubGithub();
  const ipKeys = [];
  const { env, cookie } = await setup({
    EXPLAIN_RL_IP: {
      async limit({ key }) {
        ipKeys.push(key);
        return { success: false };
      },
    },
  });
  const ipRes = await handleExplain(
    req(cookie, { headers: { "CF-Connecting-IP": "203.0.113.9" } }),
    env,
    NOW,
  );
  assert.equal(ipRes.status, 429);
  assert.equal(ipRes.headers.get("Retry-After"), "60");
  assert.deepEqual(ipKeys, ["ip:203.0.113.9"]);

  const userKeys = [];
  const { env: env2, cookie: c2 } = await setup({
    EXPLAIN_RL_IP: {
      async limit() {
        return { success: true };
      },
    },
    EXPLAIN_RL_USER: {
      async limit({ key }) {
        userKeys.push(key);
        return { success: false };
      },
    },
  });
  const userRes = await handleExplain(req(c2), env2, NOW);
  assert.equal(userRes.status, 429);
  assert.deepEqual(userKeys, ["user:octo"]);
  assert.equal(env2.AI.calls.length, 0);

  const { env: env3, cookie: c3 } = await setup({ EXPLAIN_USER_DAILY: "2" });
  await env3.EXPLAIN.put("rl:octo:2026-10-26", "2");
  const daily = await handleExplain(req(c3), env3, NOW);
  assert.equal(daily.status, 429);
  const { retryAfter } = await daily.json();
  assert.equal(retryAfter, 12 * 60 * 60);
  assert.equal(env3.AI.calls.length, 0);
});

test("503 paused once the daily budget would be exceeded", async () => {
  stubGithub();
  const { env, cookie } = await setup({ EXPLAIN_DAILY_CAP: "1000" });
  await env.EXPLAIN.put("budget:2026-10-26", "990");
  const res = await handleExplain(req(cookie), env, NOW);
  assert.equal(res.status, 503);
  assert.equal((await res.json()).error, "paused");
  assert.equal(env.AI.calls.length, 0);
  // The next UTC day has its own budget.
  const tomorrow = await handleExplain(req(cookie), env, NOW + 24 * 3600e3);
  assert.equal(tomorrow.status, 200);
});

test("a failed model call charges the reservation and caches nothing", async () => {
  stubGithub();
  const { env, cookie } = await setup({ AI: fakeAi(new Error("boom")) });
  const res = await handleExplain(req(cookie), env, NOW);
  assert.equal(res.status, 502);
  assert.ok(Number(await env.EXPLAIN.get("budget:2026-10-26")) > 0);
  assert.equal(
    [...env.EXPLAIN.map.keys()].some((k) => k.startsWith("explain:")),
    false,
  );

  const { env: env2, cookie: c2 } = await setup({
    AI: fakeAi({ response: "  " }),
  });
  const empty = await handleExplain(req(c2), env2, NOW);
  assert.equal(empty.status, 502);
});

test("Workers AI's free-allocation refusal pauses the rest of the day", async () => {
  stubGithub();
  const { env, cookie } = await setup({
    AI: fakeAi(
      new Error(
        "4006: you have used up your daily free allocation of 10,000 neurons",
      ),
    ),
  });
  const res = await handleExplain(req(cookie), env, NOW);
  assert.equal(res.status, 503);
  assert.equal((await res.json()).error, "paused");
  assert.equal(await env.EXPLAIN.get("budget:2026-10-26"), "9000");
  const other = body();
  other.steps[0].kif = "(subclass Dog Mouse)";
  const next = await handleExplain(req(cookie, { json: other }), env, NOW);
  assert.equal(next.status, 503);
  assert.equal(env.AI.calls.length, 1, "no model call once paused");
});

test("defaults stay inside the free allocation", async () => {
  stubGithub();
  const { env, cookie } = await setup();
  await env.EXPLAIN.put("budget:2026-10-26", "8990");
  const res = await handleExplain(req(cookie), env, NOW);
  assert.equal(res.status, 503);
  const { env: env2, cookie: c2 } = await setup();
  await env2.EXPLAIN.put("rl:octo:2026-10-26", "20");
  const daily = await handleExplain(req(c2), env2, NOW);
  assert.equal(daily.status, 429);
});

test("a non-reasoning model override gets max_tokens and no effort", async () => {
  stubGithub();
  const { env, cookie } = await setup({
    EXPLAIN_MODEL: "@cf/meta/llama-3.1-8b-instruct-fp8-fast",
  });
  const res = await handleExplain(req(cookie), env, NOW);
  assert.equal(res.status, 200);
  const { inputs, model } = env.AI.calls[0];
  assert.equal(model, "@cf/meta/llama-3.1-8b-instruct-fp8-fast");
  assert.equal(inputs.max_tokens, 1200);
  assert.equal(inputs.reasoning_effort, undefined);
  assert.equal(inputs.max_completion_tokens, undefined);
});

test("OpenAI-style responses and missing usage are handled", async () => {
  stubGithub();
  const { env, cookie } = await setup({
    AI: fakeAi({ choices: [{ message: { content: "Because." } }] }),
  });
  const res = await handleExplain(req(cookie), env, NOW);
  assert.equal(res.status, 200);
  assert.equal((await res.json()).explanation, "Because.");
  assert.ok(Number(await env.EXPLAIN.get("budget:2026-10-26")) > 0);
});
