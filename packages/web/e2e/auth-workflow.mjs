// Exercises the real sign-in and settings dialogs and store with mocked provider responses.
// No real credentials, repository writes, or KB boot are involved.
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { createServer } from "vite";

const here = path.dirname(fileURLToPath(import.meta.url));
const entry = "\0auth-workflow";
const server = await createServer({
  root: path.join(here, ".."),
  logLevel: "error",
  server: { port: 0 },
  plugins: [
    {
      name: "auth-workflow-harness",
      resolveId(id) {
        if (id === "/__auth_workflow_entry.mjs") return entry;
      },
      load(id) {
        if (id !== entry) return;
        return `
        import { createApp, h } from 'vue';
        import { createPinia } from 'pinia';
        import LoginDialog from '/src/components/LoginDialog.vue';
        import LogoutDialog from '/src/components/LogoutDialog.vue';
        import SettingsDialog from '/src/components/SettingsDialog.vue';
        import { useAuthStore } from '/src/stores/auth.ts';
        import { useShellStore } from '/src/stores/shell.ts';
        import '/assets/styles.css';
        createApp({ setup() {
          const auth = useAuthStore();
          const shell = useShellStore();
          window.auth = auth;
          return () => h('main', { style: 'padding:32px' }, [
            h('button', { class: 'btn', onClick: () => auth.openLoginDialog() }, auth.githubSignedIn ? auth.accountName : 'Sign in'),
            h('button', { class: 'btn', onClick: () => shell.settingsOpen = true }, 'Settings'),
            h(LoginDialog), h(LogoutDialog), h(SettingsDialog)
          ]);
        }}).use(createPinia()).mount('#app');
      `;
      },
      configureServer(vite) {
        vite.middlewares.use(async (req, res, next) => {
          if (req.url !== "/__auth_workflow") return next();
          res.setHeader("Content-Type", "text/html");
          res.end(
            await vite.transformIndexHtml(
              req.url,
              '<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"></head><body><div id="app"></div><script type="module" src="/__auth_workflow_entry.mjs"></script></body></html>',
            ),
          );
        });
      },
    },
  ],
});
await server.listen();
let browser;
try {
  browser = await chromium.launch({
    executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
  });
  const page = await browser.newPage({
    viewport: { width: 1100, height: 900 },
    colorScheme: "light",
  });
  const problems = [];
  page.on("pageerror", (e) => problems.push(e.message));
  let gitlabUser = null;
  await page.route("https://api.github.com/user", (route) =>
    route.fulfill({
      json: { login: "octo", name: "Octo Cat", avatar_url: "" },
    }),
  );
  await page.route("**/api/logout", (route) => route.fulfill({ json: {} }));
  await page.route("**/api/gitlab", (route) => {
    const data = route.request().postDataJSON();
    if (data.operation === "capabilities")
      return route.fulfill({ json: { oauthConfigured: false } });
    if (data.operation === "session")
      return route.fulfill({ json: { user: gitlabUser } });
    if (data.operation === "logout") {
      gitlabUser = null;
      return route.fulfill({ json: {} });
    }
    if (data.operation === "login") {
      if (data.token === "invalid")
        return route.fulfill({
          status: 401,
          json: { error: "Token rejected" },
        });
      gitlabUser = { login: "company-user", server: data.server };
      return route.fulfill({ json: gitlabUser });
    }
    return route.fulfill({
      status: 400,
      json: { error: "Unexpected operation" },
    });
  });
  await page.goto(`${server.resolvedUrls.local[0]}__auth_workflow`);
  const signIn = page.getByRole("button", { name: "Sign in", exact: true });
  const githubAccount = page.getByRole("button", {
    name: "Octo Cat",
    exact: true,
  });
  const settingsButton = page.getByRole("button", {
    name: "Settings",
    exact: true,
  });
  const dialog = page.locator("dialog[open]");
  const shots = path.join(here, "shots");
  await fs.mkdir(shots, { recursive: true });

  async function connectGithub() {
    await signIn.click();
    assert.equal(await dialog.getByRole("tab").count(), 0);
    assert.equal(
      await dialog.getByLabel("GitLab personal access token").count(),
      0,
    );
    assert.equal(
      await dialog
        .getByRole("link", { name: "Continue with GitHub" })
        .getAttribute("href"),
      "/api/github-auth",
    );
    await page.screenshot({ path: path.join(shots, "auth-github.png") });
    await dialog
      .getByLabel("GitHub access token", { exact: true })
      .fill("github-example");
    await dialog
      .getByRole("button", { name: "Connect with token", exact: true })
      .click();
    await dialog.waitFor({ state: "hidden" });
    await githubAccount.waitFor();
  }

  await connectGithub();
  await settingsButton.click();
  const provider = dialog.getByRole("group", {
    name: "Sign in with an external provider",
  });
  await provider
    .getByLabel("GitLab server URL")
    .fill("https://gitlab.example.org");
  const token = provider.getByLabel("GitLab personal access token");
  const connect = provider.getByRole("button", {
    name: "Connect with token",
    exact: true,
  });
  await token.fill("invalid");
  await connect.click();
  await provider.getByText("Token rejected", { exact: true }).waitFor();
  assert.equal(await page.evaluate(() => window.auth.gitlabUser), null);
  assert.equal(await page.evaluate(() => window.auth.githubSignedIn), true);
  await token.fill("gitlab-example");
  await page.screenshot({ path: path.join(shots, "auth-gitlab.png") });
  await connect.click();
  await provider.getByText(/Connected to .* as company-user/).waitFor();
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  await githubAccount.click();
  assert.equal(
    await dialog.locator(".account-name strong").textContent(),
    "Octo Cat",
  );
  assert.equal(await dialog.locator(".account-name svg").count(), 0);
  assert.equal(await dialog.getByText(/company-user/).count(), 0);

  await dialog.getByRole("button", { name: "Sign out", exact: true }).click();
  await dialog.getByRole("button", { name: "Sign out", exact: true }).click();
  await signIn.waitFor();
  assert.equal(gitlabUser.login, "company-user");
  assert.equal(
    await page.evaluate(() => window.auth.gitlabUser.login),
    "company-user",
  );

  await connectGithub();
  await page.setViewportSize({ width: 390, height: 844 });
  await settingsButton.click();
  await provider
    .getByRole("button", { name: "Sign out of GitLab", exact: true })
    .click();
  await token.waitFor();
  assert.equal(gitlabUser, null);
  assert.equal(await page.evaluate(() => window.auth.githubSignedIn), true);
  assert.ok(
    await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth + 1),
  );
  await page.screenshot({ path: path.join(shots, "auth-mobile-gitlab.png") });
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  await githubAccount.click();
  assert.ok(
    await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth + 1),
  );
  await page.screenshot({ path: path.join(shots, "auth-mobile.png") });
  assert.deepEqual(problems, []);
  console.log(
    "Auth workflow passed: settings-based GitLab sign-in, independent sessions and sign-out, and mobile layout.",
  );
} finally {
  await browser?.close();
  await server.close();
}
