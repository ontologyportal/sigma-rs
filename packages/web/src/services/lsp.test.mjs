import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const source = ts.transpileModule(
  readFileSync(new URL("./lsp.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture({ pauseChange = false, failChange = false } = {}) {
  const exports = {};
  const edits = [];
  let pending;
  let current;
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const publish = (doc) =>
    JSON.stringify({
      jsonrpc: "2.0",
      method: "textDocument/publishDiagnostics",
      params: {
        uri: doc.uri,
        version: doc.version,
        diagnostics: doc.text
          ? [
              {
                range: {
                  start: { line: 0, character: 0 },
                  end: { line: 0, character: doc.text.length },
                },
                severity: 1,
                code: "parse/test",
                message: doc.text,
              },
            ]
          : [],
      },
    });
  const call = async (cmd, args) => {
    if (cmd === "lspFlush") {
      const out = pending ? [publish(pending)] : [];
      pending = undefined;
      return { out };
    }
    const msg = JSON.parse(args.json);
    if (msg.method === "textDocument/didOpen") {
      current = msg.params.textDocument;
      return { out: [publish(current)] };
    }
    if (msg.method === "textDocument/didChange") {
      if (failChange) {
        failChange = false;
        throw new Error("transport failed");
      }
      const out = pending ? [publish(pending)] : [];
      current = {
        ...msg.params.textDocument,
        text: msg.params.contentChanges[0].text,
      };
      pending = current;
      edits.push(current.text);
      if (pauseChange) {
        pauseChange = false;
        await gate;
      }
      return { out };
    }
    return { out: [] };
  };
  vm.runInNewContext(source, {
    exports,
    require: () => ({ call }),
    console: { info() {}, error() {} },
  });
  return { ...exports, edits, release };
}

test("validation uses the latest publication when didChange also flushes an older edit", async () => {
  const f = fixture();
  await f.lspSyncDocument("test.kif", "initial");
  await f.lspSyncDocument("test.kif", "(re", { reconcile: false });
  const result = await f.lspSyncDocument("test.kif", "(relation Foo Bar)");
  assert.equal(result[0].message, "(relation Foo Bar)");
  assert.equal(
    (await f.lspSyncDocument("test.kif", "(relation Foo Bar)"))[0].message,
    "(relation Foo Bar)",
  );
});

test("overlapping completion and validation syncs preserve text and diagnostics", async () => {
  const f = fixture({ pauseChange: true });
  await f.lspSyncDocument("test.kif", "initial");
  const completion = f.lspSyncDocument("test.kif", "(re", { reconcile: false });
  while (!f.edits.length) await new Promise((resolve) => setImmediate(resolve));
  const validation = f.lspSyncDocument("test.kif", "(relation Foo Bar)");
  await new Promise((resolve) => setImmediate(resolve));
  f.release();
  await Promise.all([completion, validation]);
  assert.equal(f.lspSyncedText("test.kif"), "(relation Foo Bar)");
  assert.equal(
    (await f.lspSyncDocument("test.kif", "(relation Foo Bar)"))[0].message,
    "(relation Foo Bar)",
  );
});

test("a failed sync does not block subsequent validation", async () => {
  const f = fixture({ failChange: true });
  await f.lspSyncDocument("test.kif", "initial");
  await assert.rejects(
    f.lspSyncDocument("test.kif", "broken"),
    /transport failed/,
  );
  assert.equal(
    (await f.lspSyncDocument("test.kif", "recovered"))[0].message,
    "recovered",
  );
});

test("validation reconciles unchanged text previously synced by autocomplete", async () => {
  const f = fixture();
  await f.lspSyncDocument("test.kif", "initial");
  await f.lspSyncDocument("test.kif", "(relation", { reconcile: false });
  assert.equal(
    (await f.lspSyncDocument("test.kif", "(relation"))[0].message,
    "(relation",
  );
});

test("empty buffers replace diagnostics for the previous text", async () => {
  const f = fixture();
  await f.lspSyncDocument("test.kif", "initial");
  assert.equal((await f.lspSyncDocument("test.kif", "")).length, 0);
  assert.equal(f.lspSyncedText("test.kif"), "");
});
