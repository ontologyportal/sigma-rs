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

echo
echo "  Open:  http://localhost:${PORT}/"
echo "  (Ctrl-C to stop)"
echo

# SimpleHTTPRequestHandler + no-store headers, so reloads always fetch fresh
# assets (ES modules are otherwise cached aggressively by the browser).
exec python3 - "$PORT" "$CRATE_DIR" <<'PY'
import sys, functools
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

port, directory = int(sys.argv[1]), sys.argv[2]

class NoCache(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header('Cache-Control', 'no-store, no-cache, must-revalidate')
        self.send_header('Expires', '0')
        super().end_headers()
    def log_message(self, *a):
        pass

handler = functools.partial(NoCache, directory=directory + '/web')
ThreadingHTTPServer(('127.0.0.1', port), handler).serve_forever()
PY
