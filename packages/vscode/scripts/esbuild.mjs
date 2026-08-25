// Bundles the extension into a single CommonJS file for packaging.
//
// Everything the extension imports is inlined except `vscode` (supplied by the
// editor at runtime, never resolvable on disk) and Node's built-ins. That
// leaves no `node_modules` for the VSIX to carry, which is what lets
// `vsce package --no-dependencies` be correct rather than a way to silently
// ship a broken extension.
//
// Type checking is NOT done here -- esbuild strips types without checking
// them. `npm run typecheck` (tsc --noEmit) is the gate for that.

import { build, context } from 'esbuild';

const watch = process.argv.includes('--watch');
const production = process.argv.includes('--production');

/** @type {import('esbuild').BuildOptions} */
const options = {
  entryPoints: ['src/extension.ts'],
  outfile: 'dist/extension.js',
  bundle: true,
  platform: 'node',
  format: 'cjs',
  // Matches the `engines.vscode` floor: VSCode 1.85 ships Node 18.
  target: 'node18',
  external: ['vscode'],
  minify: production,
  sourcemap: production ? false : 'linked',
  logLevel: 'info',
};

if (watch) {
  const ctx = await context(options);
  await ctx.watch();
  console.log('esbuild: watching');
} else {
  await build(options);
}
