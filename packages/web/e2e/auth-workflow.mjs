// Exercises the real sign-in modal and store with mocked provider responses.
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
        import { useAuthStore } from '/src/stores/auth.ts';
        import '/assets/styles.css';
        createApp({ setup() {
          const auth = useAuthStore();
          window.auth = auth;
          return () => h('main', { style: 'padding:32px' }, [
            h('button', { class: 'btn', onClick: () => auth.openLoginDialog(auth.provider ?? 'github') }, auth.signedIn ? auth.accountName : 'Sign in'),
            h(LoginDialog), h(LogoutDialog)
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
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  const githubTab = page.getByRole("tab", { name: "GitHub", exact: true });
  const gitlabTab = page.getByRole("tab", { name: "GitLab", exact: true });
  assert.equal(await githubTab.getAttribute("aria-selected"), "true");
  assert.equal(await githubTab.locator("svg").count(), 1);
  assert.equal(await gitlabTab.locator("svg").count(), 1);
  assert.ok(
    await page.getByLabel("GitHub access token", { exact: true }).isVisible(),
  );
  assert.equal(
    await page
      .getByRole("link", { name: "Continue with GitHub" })
      .getAttribute("href"),
    "/api/github-auth",
  );
  await githubTab.focus();
  await page.keyboard.press("ArrowRight");
  assert.equal(await gitlabTab.getAttribute("aria-selected"), "true");
  assert.ok(await page.getByLabel("GitLab server URL").isVisible());
  assert.ok(await page.getByLabel("GitLab personal access token").isVisible());
  await page.keyboard.press("Home");
  assert.equal(await githubTab.getAttribute("aria-selected"), "true");
  const shots = path.join(here, "shots");
  await fs.mkdir(shots, { recursive: true });
  await page.screenshot({ path: path.join(shots, "auth-github.png") });

  await page
    .getByLabel("GitHub access token", { exact: true })
    .fill("github-example");
  await page
    .getByRole("tabpanel", { name: "GitHub", exact: true })
    .getByRole("button", { name: "Connect with token", exact: true })
    .click();
  await page.locator("dialog[open]").waitFor({ state: "hidden" });
  assert.equal(await page.evaluate(() => window.auth.provider), "github");
  await page.getByRole("button", { name: "Octo Cat", exact: true }).click();
  await gitlabTab.click();
  await page.getByLabel("GitLab server URL").fill("https://gitlab.example.org");
  await page.getByLabel("GitLab personal access token").fill("invalid");
  await page
    .getByRole("tabpanel", { name: "GitLab", exact: true })
    .getByRole("button", { name: "Connect with token", exact: true })
    .click();
  await page.getByText("Token rejected", { exact: true }).waitFor();
  assert.equal(await page.evaluate(() => window.auth.provider), "github");
  await page.getByLabel("GitLab personal access token").fill("gitlab-example");
  await page.screenshot({ path: path.join(shots, "auth-gitlab.png") });
  await page
    .getByRole("tabpanel", { name: "GitLab", exact: true })
    .getByRole("button", { name: "Connect with token", exact: true })
    .click();
  await page.locator("dialog[open]").waitFor({ state: "hidden" });
  assert.deepEqual(
    await page.evaluate(() => ({
      provider: window.auth.provider,
      github: !!window.auth.user,
      gitlab: !!window.auth.gitlabUser,
    })),
    { provider: "gitlab", github: false, gitlab: true },
  );

  await page.getByRole("button", { name: "company-user", exact: true }).click();
  assert.equal(
    await page.locator(".account-name strong").textContent(),
    "company-user",
  );
  assert.equal(
    await page.locator(".account-name svg").getAttribute("viewBox"),
    "0 0 24 24",
  );
  await page
    .getByRole("button", { name: "Choose repository", exact: true })
    .click();
  await page.waitForURL(
    (url) =>
      url.pathname === "/kb" && url.searchParams.get("chooseRepo") === "gitlab",
  );
  assert.equal(
    new URL(page.url()).searchParams.get("server"),
    "https://gitlab.example.org",
  );
  await page.getByRole("button", { name: "company-user", exact: true }).click();

  await githubTab.click();
  await page
    .getByLabel("GitHub access token", { exact: true })
    .fill("github-example");
  await page
    .getByRole("tabpanel", { name: "GitHub", exact: true })
    .getByRole("button", { name: "Connect with token", exact: true })
    .click();
  await page.locator("dialog[open]").waitFor({ state: "hidden" });
  assert.equal(gitlabUser, null);
  assert.equal(await page.evaluate(() => window.auth.gitlabUser), null);
  await page.getByRole("button", { name: "Octo Cat", exact: true }).click();
  await page.getByRole("button", { name: "Sign out", exact: true }).click();
  await page.getByRole("button", { name: "Sign out", exact: true }).click();
  await page.getByRole("button", { name: "Sign in", exact: true }).waitFor();
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  assert.ok(
    await page
      .locator("dialog[open]")
      .evaluate((el) => el.scrollWidth <= el.clientWidth + 1),
  );
  await page.screenshot({ path: path.join(shots, "auth-mobile.png") });
  await gitlabTab.click();
  assert.ok(await page.getByLabel("GitLab personal access token").isVisible());
  assert.ok(
    await page
      .locator("dialog[open]")
      .evaluate((el) => el.scrollWidth <= el.clientWidth + 1),
  );
  await page.screenshot({ path: path.join(shots, "auth-mobile-gitlab.png") });
  assert.deepEqual(problems, []);
  console.log(
    "Auth workflow passed: provider tabs, visible token forms, keyboard navigation, provider switching, failed-token preservation, sign-out, and mobile layout.",
  );
} finally {
  await browser?.close();
  await server.close();
}
