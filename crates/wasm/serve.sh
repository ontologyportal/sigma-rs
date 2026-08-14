#!/usr/bin/env bash
#
# Serve the demo site over HTTP. The page uses ES modules and fetches the
# .wasm, both of which browsers BLOCK on file:// ("Module source URI is not
# allowed") — so opening index.html directly fails.
#
# This ALWAYS rebuilds pkg/ first (so the served wasm + JS are current — cargo
# is incremental, so it's fast when nothing changed) and serves with
# `Cache-Control: no-store` so the browser never shows a stale app.js/wasm on
# reload. Serves the crate root so both /web/ and /pkg/ resolve.
#
#   ./serve.sh [port]        # default port 8080
#   NO_REBUILD=1 ./serve.sh  # skip the rebuild (serve whatever is in pkg/)
#   SKIP_VAMPIRE=1 ./serve.sh    # skip building the Vampire WASM backend
#   VAMPIRE_RECLONE=1 ./serve.sh # force a clean Vampire rebuild
#
# The Vampire WASM backend (build-vampire.sh) is built here too, UNLESS
# SKIP_VAMPIRE=1: it's a multi-minute Emscripten build (not a `cargo` one),
# so it's cached by pinned ref (see build-vampire.sh) and only actually
# rebuilds on a fresh checkout or a version bump — a plain rerun of this
# script is a fast no-op for it. Needs its own toolchain (Emscripten SDK +
# GNU awk); see build-vampire.sh's header for exactly what and why. Also
# builds fine as its own step: `./build-vampire.sh` directly.
#
set -euo pipefail

PORT="${1:-8080}"
CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ "${NO_REBUILD:-}" != "1" ]; then
  echo "==> Rebuilding pkg/ so the served wasm + JS are current…"
  "$CRATE_DIR/build-npm.sh"
elif [ ! -f "$CRATE_DIR/pkg/sdk.mjs" ]; then
  "$CRATE_DIR/build-npm.sh"
fi

# The demo imports `./pkg/…`, so make pkg/ a sibling of web/index.html by
# mirroring the built package into web/pkg/. This is the same layout the Pages
# deploy publishes at /browse/, so local and deployed behave identically.
rm -rf "$CRATE_DIR/web/pkg"
cp -R "$CRATE_DIR/pkg" "$CRATE_DIR/web/pkg"

# Threaded bundle is optional (needs nightly + rust-src, see build-npm.sh) —
# mirror it only when a prior THREADED=1 build-npm.sh run produced it.
# sigma.worker.js probes for its presence at runtime and falls back to pkg/.
if [ -d "$CRATE_DIR/pkg-threaded" ]; then
  rm -rf "$CRATE_DIR/web/pkg-threaded"
  cp -R "$CRATE_DIR/pkg-threaded" "$CRATE_DIR/web/pkg-threaded"
fi

"$CRATE_DIR/build-vampire.sh" || {
  echo "==> Vampire WASM build failed or skipped — the demo still runs, just without that backend." >&2
}

echo
echo "  Open:  http://localhost:${PORT}/"
echo "  (Ctrl-C to stop)"
echo

# SimpleHTTPRequestHandler + no-store headers, so reloads always fetch fresh
# assets (ES modules are otherwise cached aggressively by the browser).
# Cross-Origin-Opener-Policy/Cross-Origin-Embedder-Policy: vampire.wasm is a
# pthread-enabled Emscripten build, which browsers only grant SharedArrayBuffer
# to on a cross-origin-isolated page — these two headers are what turn that on.
# Harmless when the Vampire backend isn't built (nothing on this page depends
# on cross-origin isolation otherwise).
exec python3 - "$PORT" "$CRATE_DIR" <<'PY'
import sys, functools
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

port, directory = int(sys.argv[1]), sys.argv[2]

class NoCache(SimpleHTTPRequestHandler):
    # SPA fallback, mirroring web/_redirects on Cloudflare Pages: the app does
    # client-side path routing (/edit, /diagnostics, …), so a path with no
    # file extension that isn't an actual file on disk is a route, not a
    # missing asset — serve index.html and let app.js's router take over.
    def send_head(self):
        path = self.translate_path(self.path.split('?', 1)[0])
        import os
        if '.' not in os.path.basename(path) and not os.path.exists(path):
            self.path = '/index.html'
        return super().send_head()
    def end_headers(self):
        self.send_header('Cache-Control', 'no-store, no-cache, must-revalidate')
        self.send_header('Expires', '0')
        self.send_header('Cross-Origin-Opener-Policy', 'same-origin')
        self.send_header('Cross-Origin-Embedder-Policy', 'require-corp')
        super().end_headers()
    def log_message(self, *a):
        pass

handler = functools.partial(NoCache, directory=directory + '/web')
ThreadingHTTPServer(('127.0.0.1', port), handler).serve_forever()
PY
