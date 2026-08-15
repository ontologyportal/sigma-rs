#!/usr/bin/env node
/**
 * Build the demo's inputs before Vite runs.
 *
 * Only Vampire is copied into public/: `sigmakee` is a workspace dependency
 * that Vite resolves and bundles itself, whereas the Vampire runner is fetched
 * at runtime as a static asset (see sigma.worker.js).
 *
 *   NO_REBUILD=1       skip the wasm rebuild (use whatever dist/ holds)
 *   SKIP_VAMPIRE=1     skip the Vampire build entirely
 *   VAMPIRE_RECLONE=1  force a clean Vampire rebuild (passed through)
 */

import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const WEB_DIR = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPO_ROOT = resolve(WEB_DIR, '..', '..');
const SIGMAKEE_DIST = join(REPO_ROOT, 'packages', 'sigmakee', 'dist');
const VAMPIRE_DIST = join(REPO_ROOT, 'packages', 'vampire', 'dist');
const VAMPIRE_PUBLIC = join(WEB_DIR, 'public', 'vampire');

function build(workspace) {
  // shell on Windows: `npm` is npm.cmd, which Node will not spawn directly.
  // A spawn failure leaves status null and prints nothing, so report it.
  const r = spawnSync('npm', ['run', 'build', '--workspace', workspace], {
    cwd: REPO_ROOT,
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });
  if (r.error) {
    console.error(`==> failed to run npm for ${workspace}: ${r.error.message}`);
    return false;
  }
  return r.status === 0;
}

if (process.env.NO_REBUILD === '1' && existsSync(join(SIGMAKEE_DIST, 'sdk.mjs'))) {
  console.log('==> NO_REBUILD=1: serving the existing sigmakee build');
} else if (!build('sigmakee')) {
  process.exit(1);
}

// Best-effort: a multi-minute Emscripten build most contributors cannot run.
// Its absence costs one optional prover backend, nothing else.
let vampireOk = true;
if (process.env.SKIP_VAMPIRE === '1') {
  console.log('==> SKIP_VAMPIRE=1: not building the Vampire WASM backend');
} else if (!build('@sigma/vampire')) {
  vampireOk = false;
  console.warn('==> Vampire WASM build failed -- the demo still runs, just without that backend.');
}

// Cleared unconditionally, repopulated only from complete output: Vite copies
// public/ verbatim, so a stale mirror left here would ship and the demo would
// advertise a working backend while running the previous binary.
const VAMPIRE_FILES = ['vampire.js', 'vampire.wasm', 'vampire-runner.js'];
const vampireComplete = VAMPIRE_FILES.every((f) => existsSync(join(VAMPIRE_DIST, f)));
rmSync(VAMPIRE_PUBLIC, { recursive: true, force: true });
if (vampireOk && vampireComplete) {
  cpSync(VAMPIRE_DIST, VAMPIRE_PUBLIC, { recursive: true });
  console.log('==> Mirrored @sigma/vampire into public/vampire/');
} else if (existsSync(VAMPIRE_DIST)) {
  console.warn('==> @sigma/vampire output is incomplete or stale; not mirroring it');
}
