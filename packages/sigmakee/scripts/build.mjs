#!/usr/bin/env node
/**
 * Build the publishable `sigmakee` package into `dist/`.
 *
 * Needs only cargo; wasm-bindgen-cli is installed here, pinned to the version
 * the workspace `Cargo.lock` resolved (the generated glue and the crate's
 * runtime must match, or bindgen errors out).
 *
 *   node scripts/build.mjs [target] [out-dir]
 *
 *     target   wasm-bindgen target: web (default) | bundler | nodejs
 *     out-dir  output directory, relative to this package (default: dist)
 *
 * An alternate target MUST use its own out-dir: `dist/` is what this package's
 * `exports` resolve to, so CommonJS glue there leaves an ESM manifest over a
 * CJS payload.
 *
 *   node scripts/build.mjs nodejs dist-node
 */

import { execFileSync, spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const PKG_DIR = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WORKSPACE_ROOT = resolve(PKG_DIR, '..', '..');
const LIB_NAME = 'sumo_parser_wasm';
const WASM_TRIPLE = 'wasm32-unknown-unknown';
const TARGET = process.argv[2] ?? 'web';
const OUT_DIR = join(PKG_DIR, process.argv[3] ?? 'dist');

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: 'inherit', ...opts });
  if (r.error) throw r.error;
  if (r.status !== 0) {
    throw new Error(`${cmd} ${args.join(' ')} exited with ${r.status}`);
  }
}

function capture(cmd, args) {
  try {
    return execFileSync(cmd, args, { encoding: 'utf8' });
  } catch {
    return null;
  }
}

function has(cmd) {
  return spawnSync(cmd, ['--version'], { stdio: 'ignore' }).status === 0;
}

/** The `wasm-bindgen` version the workspace `Cargo.lock` resolved. */
function lockedBindgenVersion() {
  const lock = readFileSync(join(WORKSPACE_ROOT, 'Cargo.lock'), 'utf8');
  const m = lock.match(/\[\[package\]\]\nname = "wasm-bindgen"\nversion = "([^"]+)"/);
  if (!m) throw new Error('could not find wasm-bindgen in the workspace Cargo.lock');
  return m[1];
}

console.log('==> Checking toolchain');
if (!has('cargo')) throw new Error('cargo not found');

// Installed repo-locally, keyed on version: a global `cargo install` would
// replace a version another project depends on, and probing PATH cannot
// converge when a differently-versioned wasm-bindgen shadows ~/.cargo/bin.
const wanted = lockedBindgenVersion();
const bindgenRoot = join(WORKSPACE_ROOT, 'target', 'wasm-bindgen', wanted);
const localBindgen = join(bindgenRoot, 'bin', 'wasm-bindgen');
let bindgen;
if (existsSync(localBindgen)) {
  bindgen = localBindgen;
  console.log(`    wasm-bindgen ${wanted} (repo-local)`);
} else if (capture('wasm-bindgen', ['--version'])?.trim().split(/\s+/)[1] === wanted) {
  bindgen = 'wasm-bindgen';
  console.log(`    wasm-bindgen ${wanted} (from PATH)`);
} else {
  console.log(`==> Installing wasm-bindgen-cli ${wanted} into ${bindgenRoot}`);
  run('cargo', [
    'install', 'wasm-bindgen-cli',
    '--version', wanted, '--locked',
    '--root', bindgenRoot,
  ]);
  bindgen = localBindgen;
}

if (!capture('rustup', ['target', 'list', '--installed'])?.includes(WASM_TRIPLE)) {
  console.log(`==> Adding ${WASM_TRIPLE} target`);
  run('rustup', ['target', 'add', WASM_TRIPLE]);
}

console.log(`==> Compiling (release, ${WASM_TRIPLE})`);
run('cargo', [
  'build',
  '--manifest-path', join(WORKSPACE_ROOT, 'crates', 'wasm', 'Cargo.toml'),
  '--target', WASM_TRIPLE,
  '--release',
]);

const wasmIn = join(WORKSPACE_ROOT, 'target', WASM_TRIPLE, 'release', `${LIB_NAME}.wasm`);
if (!existsSync(wasmIn)) throw new Error(`expected ${wasmIn}`);

console.log(`==> Generating bindings (${TARGET}) into ${basename(OUT_DIR)}/`);
rmSync(OUT_DIR, { recursive: true, force: true });
mkdirSync(OUT_DIR, { recursive: true });
run(bindgen, ['--target', TARGET, '--out-dir', OUT_DIR, wasmIn]);

// Prefer the pinned `binaryen` devDependency over PATH: this build emits
// reference-types / bulk-memory opcodes, and the older binaryen distros ship
// rejects them ("invalid code after misc prefix: 17"). Never fatal -- an
// unoptimized .wasm is correct, just ~25% bigger.
const wasmFile = join(OUT_DIR, `${LIB_NAME}_bg.wasm`);
const localWasmOpt = join(WORKSPACE_ROOT, 'node_modules', '.bin', 'wasm-opt');
const wasmOpt = existsSync(localWasmOpt) ? localWasmOpt : (has('wasm-opt') ? 'wasm-opt' : null);
if (wasmOpt) {
  const before = statSync(wasmFile).size;
  console.log(`==> Optimizing .wasm with wasm-opt -Oz (${wasmOpt === localWasmOpt ? 'binaryen devDependency' : 'PATH'})`);
  const r = spawnSync(wasmOpt, [
    '-Oz',
    '--enable-bulk-memory', '--enable-reference-types', '--enable-mutable-globals',
    '--enable-nontrapping-float-to-int', '--enable-sign-ext',
    wasmFile, '-o', `${wasmFile}.opt`,
  ], { stdio: 'ignore' });
  if (r.status === 0) {
    copyFileSync(`${wasmFile}.opt`, wasmFile);
    rmSync(`${wasmFile}.opt`, { force: true });
    const after = statSync(wasmFile).size;
    console.log(`    optimized: ${before} -> ${after} bytes (-${(100 * (1 - after / before)).toFixed(1)}%)`);
  } else {
    rmSync(`${wasmFile}.opt`, { force: true });
    console.warn(`    WARNING: wasm-opt failed (exit ${r.status}); keeping the unoptimized .wasm`);
  }
} else {
  console.warn('==> WARNING: no wasm-opt found (run `npm install`); shipping an unoptimized .wasm');
}

// The facade imports the generated bindings as a sibling, so it ships beside
// them rather than from src/. LICENSE goes to the package root, where npm and
// license scanners look; only the workspace-root copy exists, and `npm publish`
// cannot reach outside the package directory.
console.log('==> Staging SDK facade and license');
for (const f of ['sdk.mjs', 'sdk.d.ts']) {
  copyFileSync(join(PKG_DIR, 'src', f), join(OUT_DIR, f));
}
if (existsSync(join(WORKSPACE_ROOT, 'LICENSE'))) {
  copyFileSync(join(WORKSPACE_ROOT, 'LICENSE'), join(PKG_DIR, 'LICENSE'));
}

console.log(`\nDone. Package output is in: ${OUT_DIR}`);
console.log(`  Inspect : npm publish --dry-run --workspace sigmakee`);
