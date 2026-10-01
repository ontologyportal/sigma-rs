// Real WASM + worker + LSP regression: edit, recheck, and invalidate additions.
// Run from the repository root after building sigmakee.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import * as sdk from "sigmakee/sdk";
import { WasmLsp } from "sigmakee";

function load(path, imports = {}) {
  const exports = {};
  vm.runInNewContext(
    ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText,
    { exports, require: (name) => imports[name], URL },
  );
  return exports;
}
await sdk.init({
  module_or_path: readFileSync(
    new URL("../../sigmakee/dist/sumo_parser_wasm_bg.wasm", import.meta.url),
  ),
});
const utils = load("../src/utils/auditReplay.ts");
const { handlers: h } = load("../src/worker/handlers.ts", {
  "sigmakee/sdk": sdk,
  sigmakee: { WasmLsp },
  "../utils/auditReplay": utils,
});
await h.boot();
const name = "Replay.kif";
const original =
  "(instance ReplayEntity Human)\n(not (instance ReplayEntity Human))\n";
const corrected =
  "(instance ReplayEntity Human)\n(not (instance ReplayEntity Animal))\n";
const config = {
  backend: "native",
  maxSteps: 500000,
  maxLits: 12,
  timeLimitSecs: 10,
  forwardClose: true,
  wantProof: true,
};
h.ingest({ name, text: original });
h.promoteAll({ names: [name] });
const positions = [
  { seed: 0, step: 0 },
  { seed: 0, step: 1 },
];
const originalTargets = positions.map((position) => {
  const { result } = h.audit({
    config,
    request: { ...position, count: 1, batch: 1, limit: 64 },
  });
  assert.ok(result.contradictions.length);
  return result.batches[0].focus[0].kif;
});
assert.equal(
  h.prepareAuditRecheck({
    files: [{ name, text: original }],
    positions,
    config,
    limit: 64,
  }).available,
  true,
);

const send = (method, params, id) =>
  h.lsp({
    json: JSON.stringify({
      jsonrpc: "2.0",
      method,
      params,
      ...(id ? { id } : {}),
    }),
  });
send(
  "initialize",
  {
    processId: null,
    rootUri: null,
    capabilities: {},
    initializationOptions: { clientManagesFiles: true },
  },
  1,
);
send("initialized", {});
const uri = "kif:/Replay.kif";
send("textDocument/didOpen", {
  textDocument: { uri, languageId: "kif", version: 1, text: original },
});
send("textDocument/didChange", {
  textDocument: { uri, version: 2 },
  contentChanges: [{ text: corrected }],
});
let state = h.auditRecheckStatus();
assert.equal(state.available, true, state.reason);
for (const index of [0, 1]) {
  const result = h.recheckAudit({ index, revision: state.revision }).result;
  assert.equal(result.next_step, 1);
  assert.equal(result.contradictions.length, 0);
  assert.equal(result.batches.length, 1);
  assert.equal(result.batches[0].status, "Consistent");
  assert.ok(
    result.batches[0].focus[0].kif.includes(
      originalTargets[index].includes("not") ? "Animal" : "Human",
    ),
    "each original target must follow its own edited formula",
  );
}

send("textDocument/didChange", {
  textDocument: { uri, version: 3 },
  contentChanges: [{ text: "; added comment, not a formula\n" + corrected }],
});
assert.equal(
  h.auditRecheckStatus().available,
  true,
  "comments do not invalidate tracking",
);
assert.throws(
  () => h.recheckAudit({ index: 0, revision: state.revision }),
  /changed during rechecking/,
);

// Even a duplicate addition must invalidate: engine source sets alone deduplicate it.
send("textDocument/didChange", {
  textDocument: { uri, version: 4 },
  contentChanges: [{ text: corrected + "(instance ReplayEntity Human)\n" }],
});
state = h.auditRecheckStatus();
assert.equal(state.available, false);
assert.match(state.reason, /added or removed/);
assert.throws(
  () => h.recheckAudit({ index: 0, revision: state.revision }),
  /added or removed/,
);
send("textDocument/didChange", {
  textDocument: { uri, version: 5 },
  contentChanges: [{ text: corrected }],
});
assert.equal(
  h.auditRecheckStatus().available,
  false,
  "undo must not silently revive invalid steps",
);
h.newSession();
assert.equal(
  h.auditRecheckStatus().available,
  false,
  "worker reset must discard lineage",
);
console.log(
  "PASS: original contradiction reproduced, both edited targets rechecked clean, duplicate addition invalidated replay, undo/reset cannot revive it.",
);

// One source formula can expand into several normalized targets after an edit.
const single = "(instance A Human)";
h.ingest({ name, text: single });
h.promoteAll({ names: [name] });
h.prepareAuditRecheck({
  files: [{ name, text: single }],
  positions: [{ seed: 0, step: 0 }],
  config,
  limit: 64,
});
send(
  "initialize",
  {
    processId: null,
    rootUri: null,
    capabilities: {},
    initializationOptions: { clientManagesFiles: true },
  },
  2,
);
send("initialized", {});
send("textDocument/didOpen", {
  textDocument: { uri, languageId: "kif", version: 1, text: single },
});
send("textDocument/didChange", {
  textDocument: { uri, version: 2 },
  contentChanges: [{ text: "(<=> (instance A Human) (instance A Animal))" }],
});
state = h.auditRecheckStatus();
assert.throws(
  () => h.recheckAudit({ index: 0, revision: state.revision }),
  /ambiguous|expansion/,
);
assert.equal(
  h.auditRecheckStatus().available,
  false,
  "engine-detected ambiguity must disable further rechecks",
);
console.log(
  "PASS: expanding an edited source into multiple targets disables rechecking.",
);
