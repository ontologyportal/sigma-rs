// Isolated visual fixture for the real Audit view. No GitHub calls, saved work,
// or real engine is touched. Run: node packages/web/e2e/audit-replay-preview.mjs
// Open http://localhost:8097/; add ?stale=1 to inspect a disabled stale report.
import { createServer } from "vite";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const axiom = {
  index: 0,
  rule: "axiom",
  premises: [],
  file: "Merge.kif",
  line: 1,
  kif: "(instance ReplayExample Entity)",
  tptp: null,
};
const replay = {
  config: {
    backend: "native",
    timeLimitSecs: 10,
    maxSteps: 500000,
    maxLits: 12,
    forwardClose: true,
    wantProof: true,
    profile: false,
    selectionTolerancePct: 0,
  },
  request: { count: 1, batch: 1, limit: 64 },
  findings: [42, 900].map((step) => ({ seed: 0, step, axioms: [axiom] })),
};
const markdown =
  "# Full SUMO contradiction report (test fixture)\n\n## Contradiction 1\n\n- Seed: `0`\n- Start step: `42`\n- Axioms to check: `1`\n- Axioms per subproblem: `1`\n\n## Contradiction 2\n\n- Seed: `0`\n- Start step: `900`\n";

const server = await createServer({
  root,
  server: { host: "127.0.0.1", port: 8097, strictPort: true },
  plugins: [
    {
      name: "isolated-audit-replay-preview",
      enforce: "pre",
      configureServer(server) {
        server.middlewares.use((req, res, next) => {
          if (req.url?.split("?")[0] !== "/") return next();
          res.setHeader("Content-Type", "text/html");
          res.end(
            '<!doctype html><html><head><title>Audit replay test fixture</title><meta name="viewport" content="width=device-width, initial-scale=1"></head><body><h1 style="font-size:16px;margin:16px">Audit replay test fixture</h1><div id="app"></div><script type="module" src="/@id/virtual:audit-preview"></script></body></html>',
          );
        });
      },
      resolveId(id) {
        if (id === "virtual:audit-preview") return id;
      },
      load(id) {
        if (id === "virtual:audit-preview")
          return `
        import { createApp, h } from 'vue';
        import { createPinia } from 'pinia';
        import { createRouter, createWebHistory, RouterView } from 'vue-router';
        import AuditTab from '/src/views/AuditTab.vue';
        import '/assets/styles.css';
        const router = createRouter({ history: createWebHistory(), routes: [{ path: '/', component: AuditTab }] });
        createApp({ render: () => h(RouterView) }).use(createPinia()).use(router).mount('#app');`;
        if (id.endsWith("/services/audit-replay.ts"))
          return `
        export async function latestAuditReport() { return { replay: ${JSON.stringify(replay)}, markdown: ${JSON.stringify(markdown)}, runUrl: '#', unavailable: location.search.includes('stale') ? 'Invalid report: SUMO master has changed since this audit. Wait for a new master audit.' : '' }; }
        export async function loadAuditReplay(replay, progress) { progress('Loading verified test inputs...'); }
        export async function checkAuditReplay() {}`;
        if (id.endsWith("/services/sigma.ts"))
          return `
        export const isWasmAbort = () => false;
        export function connectVampire() {}
        export function replaceWorker() {}
        export async function call(cmd, args) {
          if (cmd === 'audit') return { result: { total: 1000, next_step: args.request.step + 1, contradictions: [{ steps: [${JSON.stringify(axiom)}] }], batches: [{ status: 'Inconsistent', stop_reason: null, elapsed_ms: 10, focus: [${JSON.stringify(axiom)}] }], raw_output: 'Replayed test fixture step ' + args.request.step } };
          return { text: 'Test fixture', info: {}, terms: [] };
        }`;
      },
    },
  ],
});
await server.listen();
server.printUrls();
