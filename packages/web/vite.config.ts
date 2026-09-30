import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

function auditEngine() {
  try {
    return JSON.parse(
      process
        .getBuiltinModule("fs")
        .readFileSync(
          new URL("../sigmakee/dist/build-info.json", import.meta.url),
          "utf8",
        ),
    );
  } catch {
    return null;
  }
}

// vite.config.ts runs under Node, not the browser -- but this is a browser
// package, so pulling in @types/node globally would leak Node's `setTimeout`
// (returning `Timeout`) over DOM's (returning `number`) into every app file.
// `declare const` in a module file scopes the binding to this file only.
declare const process: {
  env: Record<string, string | undefined>;
  getBuiltinModule(name: "fs"): {
    readFileSync(path: URL, encoding: "utf8"): string;
  };
};

export default defineConfig({
  define: { __AUDIT_ENGINE__: JSON.stringify(auditEngine()) },
  // Mount point, baked into every emitted asset URL: / for Cloudflare Pages,
  // /browse/ for GitHub Pages. Must stay ABSOLUTE -- with a relative base the
  // SPA fallback serves index.html at deeper paths (/edit/), where
  // './assets/main.js' resolves to '/edit/assets/main.js', comes back as
  // index.html, and is rejected on MIME type, so the page renders blank.
  base: process.env.VITE_BASE || "/",

  server: {
    port: 8080,
    // Cross-origin isolation, which is what grants SharedArrayBuffer to the
    // pthreads-built vampire.wasm. Mirrors public/_headers.
    headers: {
      "Cross-Origin-Opener-Policy": "same-origin",
      "Cross-Origin-Embedder-Policy": "require-corp",
    },
  },

  plugins: [vue()],

  // Serve index.html for unmatched paths: they are client-side routes (see
  // src/router.ts's vue-router instance), not missing assets.
  appType: "spa",

  worker: {
    format: "es",
  },
});
