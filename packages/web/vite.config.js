import { defineConfig } from 'vite';

export default defineConfig({
  // Mount point, baked into every emitted asset URL: / for Cloudflare Pages,
  // /browse/ for GitHub Pages. Must stay ABSOLUTE -- with a relative base the
  // SPA fallback serves index.html at deeper paths (/edit/), where
  // './assets/app.js' resolves to '/edit/assets/app.js', comes back as
  // index.html, and is rejected on MIME type, so the page renders blank.
  base: process.env.VITE_BASE || '/',

  server: {
    port: 8080,
    // Cross-origin isolation, which is what grants SharedArrayBuffer to the
    // pthreads-built vampire.wasm. Mirrors public/_headers.
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    },
  },

  // Serve index.html for unmatched paths: they are client-side routes
  // (see app.js's routeFromLocation), not missing assets.
  appType: 'spa',

  worker: {
    format: 'es',
  },
});
