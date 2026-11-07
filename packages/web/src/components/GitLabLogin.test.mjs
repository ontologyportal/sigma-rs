import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse, compileScript } from "@vue/compiler-sfc";
import * as vue from "vue";
import ts from "typescript";

const { descriptor } = parse(
  readFileSync(new URL("./GitLabLogin.vue", import.meta.url), "utf8"),
);
const source = ts.transpileModule(
  compileScript(descriptor, { id: "gitlab-login-test" }).content,
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture({
  fail = false,
  oauthConfigured = false,
  server = "https://gitlab.com",
} = {}) {
  const calls = [];
  const auth = vue.reactive({ gitlabUser: null, initGitlab: async () => {} });
  let cleanup;
  let mount;
  Object.assign(auth, {
    async loginWithGitlabToken(server, token) {
      calls.push(["login", { server, token }]);
      if (fail) throw new Error("Token rejected");
      auth.gitlabUser = { login: "alice", server };
      return auth.gitlabUser;
    },
    async logout() {
      auth.gitlabUser = null;
    },
  });
  const imports = {
    vue: {
      ...vue,
      onMounted: (fn) => {
        mount = fn;
      },
      onBeforeUnmount: (fn) => {
        cleanup = fn;
      },
    },
    "../stores/auth": { useAuthStore: () => auth },
    "../api/gitlab": {
      gitlabRequest: async (...args) => {
        calls.push(args);
        if (args[0] === "capabilities") return { oauthConfigured };
        if (fail) throw new Error("Token rejected");
        return { login: "alice" };
      },
    },
    "../composables/useStatus": {
      useStatus: () => ({
        set: (...args) => calls.push(["status", ...args]),
        fail: (e) => calls.push(["error", e.message]),
      }),
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    URL,
    AbortController,
    require: (name) => imports[name] ?? {},
  });
  const scope = vue.effectScope();
  const ui = scope.run(() =>
    exports.default.setup(
      { server, dialog: true },
      { expose: () => {}, emit: (...args) => calls.push(["emit", ...args]) },
    ),
  );
  return {
    ui,
    calls,
    auth,
    mount: () => mount(),
    stop: () => {
      cleanup();
      scope.stop();
    },
  };
}

test("personal token form submits once and clears its password before waiting for the server", async () => {
  const f = fixture();
  try {
    f.ui.token.value = " glpat-example ";
    const pending = f.ui.connect();
    assert.equal(f.ui.token.value, "");
    assert.equal(f.ui.busy.value, true);
    await f.ui.connect();
    await pending;
    assert.equal(f.calls.filter((c) => c[0] === "login").length, 1);
    assert.equal(
      f.calls.find((c) => c[0] === "login")[1].token,
      "glpat-example",
    );
    assert.ok(f.calls.some((c) => c[0] === "status" && c[1].includes("alice")));
    assert.equal(f.ui.busy.value, false);
  } finally {
    f.stop();
  }
});

test("rejected and empty tokens do not linger in the form", async () => {
  const f = fixture({ fail: true });
  try {
    await f.ui.connect();
    assert.ok(!f.calls.some((c) => c[0] === "login"));
    f.ui.token.value = "glpat-bad";
    await f.ui.connect();
    assert.equal(f.ui.token.value, "");
    assert.ok(f.calls.some((c) => c[0] === "error"));
    assert.equal(f.ui.busy.value, false);
  } finally {
    f.stop();
  }
});

test("OAuth is hidden until the backend confirms it is configured", async () => {
  for (const configured of [false, true]) {
    const f = fixture({ oauthConfigured: configured });
    try {
      assert.equal(f.ui.oauthConfigured.value, false);
      await f.mount();
      assert.equal(f.ui.oauthConfigured.value, configured);
      f.ui.token.value = "example";
      await f.ui.connect();
      assert.ok(f.calls.some((c) => c[0] === "login"));
    } finally {
      f.stop();
    }
  }
});

test("token input is visible without opening a disclosure or following an OAuth link", () => {
  assert.doesNotMatch(descriptor.template.content, /<details|8788|localHttp/);
  assert.match(descriptor.template.content, /v-if="\s*oauthConfigured/);
  assert.match(descriptor.template.content, /v-model="token"/);
});

test("custom GitLab URL is typed, normalized, and sent with the token", async () => {
  const f = fixture({ server: "https://gitlab.nps.edu/fsg/" });
  try {
    f.ui.token.value = "example";
    await f.ui.connect();
    const request = f.calls.find((c) => c[0] === "login");
    assert.equal(request[1].server, "https://gitlab.nps.edu");
    assert.equal(f.ui.serverUrl.value, "https://gitlab.nps.edu");
    assert.ok(
      f.calls.some((c) => c[0] === "emit" && c[2] === "https://gitlab.nps.edu"),
    );
  } finally {
    f.stop();
  }
});

test("server URL has no preselected company and must be supplied for token login", async () => {
  const f = fixture({ server: "" });
  try {
    f.ui.token.value = "example";
    await f.ui.connect();
    assert.equal(f.ui.serverUrl.value, "");
    assert.ok(!f.calls.some((c) => c[0] === "login"));
    assert.doesNotMatch(
      descriptor.template.content,
      /<select|gitlab\.nps\.edu/,
    );
    assert.match(descriptor.template.content, /v-model="serverUrl"/);
  } finally {
    f.stop();
  }
});

test("connecting and disconnecting update the global GitLab account", async () => {
  const f = fixture({ server: "https://gitlab.nps.edu" });
  try {
    f.ui.token.value = "example";
    await f.ui.connect();
    assert.equal(f.auth.gitlabUser.login, "alice");
    assert.equal(f.auth.gitlabUser.server, "https://gitlab.nps.edu");
    assert.equal(f.ui.connected.value.login, "alice");
    await f.auth.logout();
    assert.equal(f.auth.gitlabUser, null);
    assert.equal(f.ui.connected.value, null);
  } finally {
    f.stop();
  }
});
