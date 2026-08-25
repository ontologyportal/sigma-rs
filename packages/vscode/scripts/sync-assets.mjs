// Stages the files this extension needs at runtime but does not author.
//
// Two kinds, both copied rather than referenced:
//
//   * Language assets from @sigma/language. VSCode resolves
//     `contributes.languages[].configuration` and `contributes.grammars[].path`
//     as plain file paths inside the installed extension -- it will not follow
//     a node dependency -- so they must physically live here by packaging time.
//
//   * Webview vendor bundles (mermaid, svg-pan-zoom). These are pre-built
//     browser scripts loaded over a webview URI, never imported by our code,
//     so esbuild cannot inline them; the webview needs real files on disk.
//
// Everything written here is gitignored. Edit the sources, never these copies.
//
// Source directories are located via node resolution rather than relative
// paths, so this keeps working whether npm hoists a package to the workspace
// root or installs it locally.

import { cp, mkdir, rm } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const extRoot = join(dirname(fileURLToPath(import.meta.url)), '..');

/** Absolute path to an installed package's root directory. */
function packageRoot(spec) {
  return dirname(fileURLToPath(import.meta.resolve(`${spec}/package.json`)));
}

const languageRoot = packageRoot('@sigma/language');
const languageAssets = [
  ['configs/kif-language-configuration.json', 'kif-language-configuration.json'],
  ['configs/tptp-language-configuration.json', 'tptp-language-configuration.json'],
  ['syntaxes/kif.tmLanguage.json', 'syntaxes/kif.tmLanguage.json'],
  ['syntaxes/tptp.tmLanguage.json', 'syntaxes/tptp.tmLanguage.json'],
];

await mkdir(join(extRoot, 'syntaxes'), { recursive: true });
for (const [from, to] of languageAssets) {
  await cp(join(languageRoot, from), join(extRoot, to));
  console.log(`synced ${to}`);
}

// Just the UMD bundles the webview <script>-tags, not the whole `dist/`
// directory: mermaid's dist is 28 MB of specs, typings and mocks, and
// mermaid.min.js is self-contained (no dynamic imports of its siblings).
// Mirrored under vendor/<name>/ -- see src/taxonomy.ts.
const vendorBundles = [
  ['mermaid', 'mermaid.min.js'],
  ['svg-pan-zoom', 'svg-pan-zoom.min.js'],
];

await rm(join(extRoot, 'vendor'), { recursive: true, force: true });
for (const [pkg, file] of vendorBundles) {
  const to = join('vendor', pkg, file);
  await mkdir(join(extRoot, 'vendor', pkg), { recursive: true });
  await cp(join(packageRoot(pkg), 'dist', file), join(extRoot, to));
  console.log(`synced ${to}`);
}
