import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
// @ts-expect-error Build-only JS plugin; Node types stay out of the app.
import assetCache from "./scripts/asset-cache.mjs";

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

function proverBuilds() {
  return Object.fromEntries(
    [
      "vampire/vampire.wasm",
      "eprover/eprover.wasm",
      "eprover/e_axfilter.wasm",
    ].map((file) => {
      try {
        const bytes = process
          .getBuiltinModule("fs")
          .readFileSync(new URL(`./public/${file}`, import.meta.url));
        return [
          file,
          process
            .getBuiltinModule("crypto")
            .createHash("sha256")
            .update(bytes)
            .digest("hex"),
        ];
      } catch {
        return [file, null];
      }
    }),
  );
}

// vite.config.ts runs under Node, not the browser -- but this is a browser
// package, so pulling in @types/node globally would leak Node's `setTimeout`
// (returning `Timeout`) over DOM's (returning `number`) into every app file.
// `declare const` in a module file scopes the binding to this file only.
declare const process: {
  env: Record<string, string | undefined>;
  getBuiltinModule(name: "fs"): {
    readFileSync(path: URL, encoding: "utf8"): string;
    readFileSync(path: URL): Uint8Array;
  };
  getBuiltinModule(name: "crypto"): {
    createHash(algorithm: string): {
      update(bytes: Uint8Array): { digest(encoding: "hex"): string };
    };
  };
};

export default defineConfig({
  define: {
    __AUDIT_ENGINE__: JSON.stringify(auditEngine()),
    __PROVER_BUILDS__: JSON.stringify(proverBuilds()),
  },
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

  plugins: [vue(), assetCache()],

  // Serve index.html for unmatched paths: they are client-side routes (see
  // src/router.ts's vue-router instance), not missing assets.
  appType: "spa",

  worker: {
    format: "es",
  },
});
