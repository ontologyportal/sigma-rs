// @vscode/test-cli configuration.
//
// `npm test` (→ `vscode-test`) downloads a matching VSCode build, opens the
// fixture workspace generated below, loads this extension from source
// (extensionDevelopmentPath = repo root), and runs the mocha suites in
// `out/test/**` inside the extension host.
//
// The language server is NOT mocked: the extension resolves its bundled
// binary at `server/sumo-lsp`, so stage a build first —
//
//     npm run server:install:local     # build from ../sigma-rs (committed state)
//     npm run server:install           # build from GitHub tag sumo-lsp-latest
//
// The fixture is a tiny config.xml-declared KB.  Workspace settings pin
// `sumo.configPath` + `sumo.activeKb`, so activation bootstraps the KB and
// pushes its files to the server without any user prompts — every file the
// tests open is already a KB member, which keeps the membership QuickPick
// from ever appearing.

import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';
import { defineConfig } from '@vscode/test-cli';

const root    = path.dirname(fileURLToPath(import.meta.url));
const fixture = path.join(root, '.vscode-test', 'fixture');

const server = path.join(root, 'server', process.platform === 'win32' ? 'sumo-lsp.exe' : 'sumo-lsp');
if (!fs.existsSync(server)) {
    throw new Error(
        `bundled language server not found at ${server}\n` +
        `stage one first: npm run server:install:local (or server:install)`,
    );
}

// -- Fixture workspace --------------------------------------------------------

fs.rmSync(fixture, { recursive: true, force: true });
fs.mkdirSync(path.join(fixture, 'kbs'), { recursive: true });
fs.mkdirSync(path.join(fixture, '.vscode'), { recursive: true });

// A minimal self-contained ontology: enough declarations that hover /
// definition / rename have real content to work with.
fs.writeFileSync(path.join(fixture, 'kbs', 'base.kif'), [
    '(subclass Human Hominid)',
    '(subclass Hominid Animal)',
    '(documentation Human EnglishLanguage "A member of the species &%Hominid.")',
    '(instance Fido Human)',
    '(=>',
    '  (instance ?X Human)',
    '  (instance ?X Animal))',
    '',
].join('\n'));

// A second constituent so cross-file definition/references are exercised.
fs.writeFileSync(path.join(fixture, 'kbs', 'facts.kif'), [
    '(instance Rex Human)',
    '(documentation Rex EnglishLanguage "A test individual.")',
    '',
].join('\n'));

fs.writeFileSync(path.join(fixture, 'config.xml'), [
    '<configuration>',
    `  <preference name="kbDir" value="${path.join(fixture, 'kbs')}" />`,
    '  <preference name="sumokbname" value="TEST" />',
    '  <kb name="TEST">',
    '    <constituent filename="base.kif" />',
    '    <constituent filename="facts.kif" />',
    '  </kb>',
    '</configuration>',
    '',
].join('\n'));

fs.writeFileSync(path.join(fixture, '.vscode', 'settings.json'), JSON.stringify({
    'sumo.configPath': path.join(fixture, 'config.xml'),
    'sumo.activeKb':   'TEST',
}, null, 2));

// -- Runner config --------------------------------------------------------------

export default defineConfig({
    files: 'out/test/**/*.test.js',
    workspaceFolder: fixture,
    mocha: {
        ui: 'tdd',
        timeout: 120000,
    },
});
