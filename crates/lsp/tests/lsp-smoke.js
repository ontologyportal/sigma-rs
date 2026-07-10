#!/usr/bin/env node
// Smoke-test sumo-lsp over stdio with the exact message sequence the
// ontologyportal/vscode extension sends:
//   initialize {clientManagesFiles:true} -> initialized
//   sumo/setIgnoredDiagnostics -> sumo/setActiveFiles
//   didOpen -> (publishDiagnostics) -> hover -> sumo/taxonomy
//   shutdown -> exit
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const SERVER = process.argv[2];
if (!SERVER) { console.error('usage: lsp-smoke.js <path-to-sumo-lsp>'); process.exit(2); }

// -- fixture KB file ----------------------------------------------------------
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'sumo-lsp-smoke-'));
const kif = path.join(dir, 'smoke.kif');
const text = [
  '(subclass Human Hominid)',
  '(subclass Hominid Primate)',
  '(documentation Human EnglishLanguage "A member of the species &%Hominid.")',
  '(instance Fido Human)',
  '(subclass Human)',          // arity error -> semantic diagnostic
].join('\n');
fs.writeFileSync(kif, text);
const uri = 'file://' + kif;

// -- LSP plumbing ---------------------------------------------------------------
const proc = spawn(SERVER, [], { stdio: ['pipe', 'pipe', 'pipe'] });
proc.stderr.on('data', d => process.stderr.write('[server] ' + d));

let buf = Buffer.alloc(0);
const pendingById = new Map();   // id -> resolve
const notifications = [];        // captured server->client notifications
proc.stdout.on('data', chunk => {
  buf = Buffer.concat([buf, chunk]);
  for (;;) {
    const headerEnd = buf.indexOf('\r\n\r\n');
    if (headerEnd < 0) return;
    const header = buf.slice(0, headerEnd).toString();
    const m = /Content-Length: (\d+)/i.exec(header);
    if (!m) throw new Error('bad header: ' + header);
    const len = parseInt(m[1], 10);
    if (buf.length < headerEnd + 4 + len) return;
    const body = JSON.parse(buf.slice(headerEnd + 4, headerEnd + 4 + len).toString());
    buf = buf.slice(headerEnd + 4 + len);
    if (body.id !== undefined && (body.result !== undefined || body.error !== undefined)) {
      const resolve = pendingById.get(body.id);
      if (resolve) { pendingById.delete(body.id); resolve(body); }
    } else if (body.method) {
      notifications.push(body);
    }
  }
});

let nextId = 1;
function send(msg) {
  const s = JSON.stringify(msg);
  proc.stdin.write(`Content-Length: ${Buffer.byteLength(s)}\r\n\r\n${s}`);
}
function request(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`timeout waiting for ${method}`)), 15000);
    pendingById.set(id, body => { clearTimeout(t); resolve(body); });
    send({ jsonrpc: '2.0', id, method, params });
  });
}
function notify(method, params) { send({ jsonrpc: '2.0', method, params }); }
const sleep = ms => new Promise(r => setTimeout(r, ms));

let failures = 0;
function check(name, cond, detail) {
  if (cond) { console.log(`ok   - ${name}`); }
  else      { console.log(`FAIL - ${name}${detail ? ': ' + detail : ''}`); failures++; }
}

(async () => {
  // 1. initialize — mirrors extension.ts clientOptions.
  const init = await request('initialize', {
    processId: process.pid,
    rootUri: 'file://' + dir,
    capabilities: {},
    initializationOptions: { clientManagesFiles: true },
    workspaceFolders: [{ uri: 'file://' + dir, name: 'smoke' }],
  });
  check('initialize responds', !init.error, JSON.stringify(init.error));
  check('server advertises hover', !!init.result?.capabilities?.hoverProvider);
  check('server advertises rename', !!init.result?.capabilities?.renameProvider);
  notify('initialized', {});

  // 2. settings sync, exactly as the extension pushes on activation.
  notify('sumo/setIgnoredDiagnostics', { codes: ['function-case'] });
  notify('sumo/setActiveFiles', { files: [kif] });
  await sleep(300); // let the server ingest + republish

  // 3. didOpen (client-managed: must NOT double-load, still publishes diags).
  notify('textDocument/didOpen', {
    textDocument: { uri, languageId: 'kif', version: 1, text },
  });
  await sleep(300);

  const diags = notifications.filter(n => n.method === 'textDocument/publishDiagnostics');
  check('publishDiagnostics received', diags.length > 0);
  const last = diags[diags.length - 1];
  // No upper ontology loaded, so `subclass` itself is undeclared — the
  // validator's head-not-relation / no-entity-ancestor findings are the
  // expected semantic diagnostics here.
  const semCount = (last?.params?.diagnostics || []).length;
  check('semantic diagnostics surfaced', semCount > 0);

  // 3b. ignoredCodes round-trip: the extension sends kebab-case names from
  // its package.json enum; the server must match them against d.kind and
  // re-publish immediately.
  notify('sumo/setIgnoredDiagnostics', { codes: ['head-not-relation', 'no-entity-ancestor'] });
  await sleep(300);
  const afterIgnore = notifications.filter(n => n.method === 'textDocument/publishDiagnostics').pop();
  const remaining = (afterIgnore?.params?.diagnostics || []);
  check('ignoredCodes suppress by kebab-case name', remaining.length < semCount,
        `before=${semCount} after=${remaining.length}: ` + JSON.stringify(remaining.map(d => d.message)));
  notify('sumo/setIgnoredDiagnostics', { codes: [] });
  await sleep(200);

  // 4. hover over `Human` (line 0, col 12) — extension relies on markdown manpage.
  const hover = await request('textDocument/hover', {
    textDocument: { uri }, position: { line: 0, character: 12 },
  });
  const md = hover.result?.contents?.value || '';
  check('hover returns markdown manpage', md.includes('### Human'), JSON.stringify(hover).slice(0, 300));
  check('hover shows documentation text', md.includes('member of the species'), md);
  check('hover resolves &% cross-refs', !md.includes('&%'), md);

  // 5. sumo/taxonomy — the Show Taxonomy webview request.
  const tax = await request('sumo/taxonomy', { symbol: 'Human' });
  const t = tax.result || {};
  check('taxonomy: known symbol', t.unknown === false, JSON.stringify(t).slice(0, 200));
  check('taxonomy: edges reach Primate',
        (t.edges || []).some(e => e.from === 'Hominid' && e.to === 'Primate' && e.relation === 'subclass'),
        JSON.stringify(t.edges));
  check('taxonomy: documentation present',
        (t.documentation || []).some(d => d.language === 'EnglishLanguage' && d.text.includes('Hominid')),
        JSON.stringify(t.documentation));

  const taxUnknown = await request('sumo/taxonomy', { symbol: 'NoSuchThing' });
  check('taxonomy: unknown symbol flagged', taxUnknown.result?.unknown === true);

  // 6. definition + documentSymbol + rename, which the extension gets "for free"
  //    through the vscode-languageclient default feature set.
  const def = await request('textDocument/definition', {
    textDocument: { uri }, position: { line: 1, character: 11 },  // Hominid
  });
  check('goto-definition resolves', def.result != null && def.result.uri !== undefined || Array.isArray(def.result) || def.result?.uri, JSON.stringify(def.result));

  const syms = await request('textDocument/documentSymbol', {
    textDocument: { uri },
  });
  check('documentSymbol lists root sentences', Array.isArray(syms.result) && syms.result.length >= 4,
        JSON.stringify(syms.result?.length));

  // 7. shutdown handshake.
  const sd = await request('shutdown', null);
  check('shutdown ok', !sd.error);
  notify('exit', {});

  await sleep(200);
  console.log(failures === 0 ? '\nSMOKE: all checks passed' : `\nSMOKE: ${failures} check(s) FAILED`);
  process.exit(failures === 0 ? 0 : 1);
})().catch(e => { console.error('SMOKE: aborted:', e.message); process.exit(1); });
