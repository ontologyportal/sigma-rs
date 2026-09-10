/**
 * Thin LSP client over the worker's `lsp` RPC command.
 *
 * `WasmLsp.handleMessage` is the transport-free `sumo-lsp` dispatch core: one
 * JSON-RPC body in, every server->client message that input produced out —
 * synchronously. So this client needs no framing, no socket, and no real
 * async server: a request's response (and any `publishDiagnostics` the
 * handling emitted) come back in the same batch as the call that caused
 * them.
 *
 * Document identity: the LSP speaks URIs, the KB speaks bare file tags
 * ("Merge.kif"). The server's `kif:` scheme maps between them —
 * `kif:/Merge.kif` derives exactly the tag the page's `loadKif` used, so
 * diagnostics and per-file operations line up (see sumo-lsp's `conv.rs`).
 *
 * The server runs inside the worker against the SAME KB as every other
 * command; `initialize` passes `clientManagesFiles: true` so `didOpen` never
 * double-loads a constituent the page already ingested.
 */

import { call } from '../rpc.ts';

let nextId = 1;
let initPromise: Promise<void> | null = null;
// The tag of the document currently open on the server, if any. The edit tab
// drives one live document at a time; a session reset clears it via
// `lspReset` so the next sync re-opens.
let openTag: string | null = null;
let openVersion = 0;
// The text last synced to `openTag` and the diagnostics that sync returned,
// so a call with unchanged text (e.g. completion firing right before the
// validate debounce re-syncs the same buffer) can skip the didOpen/didChange
// round-trip -- and the parse/tokenize it triggers server-side -- entirely.
let lastSyncedText: string | null = null;
let lastSyncedDiags: any[] = [];

/** KB file tag -> `kif:` scheme URI (the server derives the tag back). */
export function tagToUri(tag: string): string {
  return 'kif:/' + tag.split('/').map(encodeURIComponent).join('/');
}

async function send(msg: object): Promise<any[]> {
  try {
    const { out } = await call<{ out: string[] }>('lsp', { json: JSON.stringify(msg) });
    return out.map((s) => JSON.parse(s));
  } catch (e) {
    // Every consumer of the LSP lane degrades quietly (empty suggestions,
    // formatKif fallback, no semantic markers) — so a transport-level
    // failure must be loud, or the lane just looks "disconnected".  The
    // classic cause is a stale served bundle (worker without the `lsp`
    // command, or an old sigmakee dist without `WasmLsp`).
    console.error('[lsp] request failed:', (msg as any).method, e);
    throw e;
  }
}

async function ensureInitialized(): Promise<void> {
  if (!initPromise) {
    initPromise = (async () => {
      await send({
        jsonrpc: '2.0', id: nextId++, method: 'initialize',
        params: {
          processId: null, rootUri: null, capabilities: {},
          initializationOptions: { clientManagesFiles: true },
        },
      });
      await send({ jsonrpc: '2.0', method: 'initialized', params: {} });
      console.info('[lsp] language server initialized (shared-KB lane up)');
    })().catch((e) => {
      // Do not cache a failed handshake: the next call retries it.
      initPromise = null;
      throw e;
    });
  }
  return initPromise;
}

/** Drop client-side server state after the worker's session was replaced
 *  (`newSession` also drops the WasmLsp instance). The next sync re-runs the
 *  handshake and re-opens the document. */
export function lspReset() {
  initPromise = null;
  openTag = null;
  openVersion = 0;
  lastSyncedText = null;
  lastSyncedDiags = [];
}

/**
 * Send one request; resolve with its result. Notifications that ride in the
 * same batch are handed to `onNotification` when given.
 */
export async function lspRequest<T = any>(
  method: string,
  params: unknown,
  onNotification?: (method: string, params: any) => void,
): Promise<T | null> {
  await ensureInitialized();
  const id = nextId++;
  const msgs = await send({ jsonrpc: '2.0', id, method, params });
  let result: T | null = null;
  for (const m of msgs) {
    if (m.id === id) {
      if (m.error) throw new Error(`${method}: ${m.error.message}`);
      result = m.result ?? null;
    } else if (m.method && onNotification) {
      onNotification(m.method, m.params);
    }
  }
  return result;
}

/**
 * Sync the server's copy of `tag` to `text` (didOpen on first sight or after
 * a reset, didChange after) and return the document's diagnostics, converted
 * to the legacy `{ line, col, end_line, end_col, severity, kind, code,
 * message }` shape the edit tab's markers/renderers already consume.
 *
 * The server's `didChange` reconciles the buffer into the live KB (the same
 * diff-and-commit the old `validateBuffer` lane did), so this call IS the
 * "KB tracks the editor" step, not just a query.
 */
export async function lspSyncDocument(tag: string, text: string): Promise<any[]> {
  await ensureInitialized();
  // Same doc, same text as last sync (e.g. a completion-triggered sync
  // followed moments later by the validate debounce's sync of the still-
  // unchanged buffer): nothing changed server-side, so return the diagnostics
  // from that sync instead of round-tripping another didChange.
  if (openTag === tag && lastSyncedText === text) return lastSyncedDiags;
  const uri = tagToUri(tag);
  let msgs: any[];
  if (openTag !== tag) {
    // One live document: close the previous one so the server's doc table
    // doesn't accumulate buffers the page stopped tracking.
    if (openTag !== null) {
      await send({
        jsonrpc: '2.0', method: 'textDocument/didClose',
        params: { textDocument: { uri: tagToUri(openTag) } },
      });
    }
    openTag = tag;
    openVersion = 1;
    msgs = await send({
      jsonrpc: '2.0', method: 'textDocument/didOpen',
      params: { textDocument: { uri, languageId: 'kif', version: openVersion, text } },
    });
  } else {
    openVersion += 1;
    msgs = await send({
      jsonrpc: '2.0', method: 'textDocument/didChange',
      params: {
        textDocument: { uri, version: openVersion },
        contentChanges: [{ text }],
      },
    });
  }
  const diags = msgs.find(
    (m) => m.method === 'textDocument/publishDiagnostics' && m.params?.uri === uri,
  );
  lastSyncedText = text;
  lastSyncedDiags = (diags?.params?.diagnostics ?? []).map(lspDiagToLegacy);
  return lastSyncedDiags;
}

/** The tag of the server-side open document, if any. */
export function lspOpenTag(): string | null {
  return openTag;
}

const LSP_SEVERITY = { 1: 'error', 2: 'warning', 3: 'info', 4: 'hint' };

/** LSP `Diagnostic` -> the legacy diagnostic shape (1-based lines/cols;
 *  `code` arrives as the server's "kind/code" string, split back apart). */
function lspDiagToLegacy(d: any) {
  const codeStr = typeof d.code === 'string' ? d.code : String(d.code ?? '');
  const slash = codeStr.indexOf('/');
  return {
    line: (d.range?.start?.line ?? 0) + 1,
    col: (d.range?.start?.character ?? 0) + 1,
    end_line: (d.range?.end?.line ?? 0) + 1,
    // Both are exclusive end columns; LSP's is 0-based, legacy is 1-based.
    end_col: (d.range?.end?.character ?? 0) + 1,
    severity: LSP_SEVERITY[d.severity] ?? 'info',
    kind: slash > 0 ? codeStr.slice(0, slash) : codeStr,
    code: slash > 0 ? codeStr.slice(slash + 1) : '',
    message: d.message ?? '',
  };
}
