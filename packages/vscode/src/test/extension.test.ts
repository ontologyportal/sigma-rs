// Integration tests: the real extension + the real bundled `server/sumo-lsp`
// binary, running inside a real VSCode extension host (see .vscode-test.mjs
// for the fixture workspace and how the server binary is staged).
//
// Every LSP-backed feature is exercised through VSCode's own provider
// commands (`vscode.execute*Provider`), i.e. the same route a user's
// hover/goto/rename takes — client wiring, transport, and server all real.

import * as assert from 'assert';
import * as path from 'path';
import * as vscode from 'vscode';

const EXTENSION_ID = 'ontologyportal.sumo';

function fixtureFile(name: string): vscode.Uri {
    const ws = vscode.workspace.workspaceFolders?.[0];
    assert.ok(ws, 'fixture workspace folder is open');
    return vscode.Uri.file(path.join(ws.uri.fsPath, 'kbs', name));
}

/** Poll `probe` until it returns a truthy value or `timeoutMs` elapses. */
async function waitFor<T>(
    what:      string,
    probe:     () => Thenable<T | undefined | null | false>,
    timeoutMs: number = 30000,
): Promise<T> {
    const deadline = Date.now() + timeoutMs;
    let last: unknown;
    for (;;) {
        try {
            const value = await probe();
            if (value) { return value; }
            last = value;
        } catch (err) {
            last = err;
        }
        if (Date.now() > deadline) {
            assert.fail(`timed out waiting for ${what} (last: ${String(last)})`);
        }
        await new Promise(r => setTimeout(r, 250));
    }
}

/** Position of the first occurrence of `needle` in `doc` (offset by `within`). */
function posOf(doc: vscode.TextDocument, needle: string, within = 0): vscode.Position {
    const idx = doc.getText().indexOf(needle, within);
    assert.ok(idx >= 0, `'${needle}' present in ${doc.uri.fsPath}`);
    return doc.positionAt(idx + 1); // one char in, safely inside the token
}

suite('sumo-lsp integration', () => {
    let baseDoc: vscode.TextDocument;

    suiteSetup(async function () {
        this.timeout(60000);
        const ext = vscode.extensions.getExtension(EXTENSION_ID);
        assert.ok(ext, `extension ${EXTENSION_ID} is installed in the test host`);
        await ext.activate();

        baseDoc = await vscode.workspace.openTextDocument(fixtureFile('base.kif'));
        await vscode.window.showTextDocument(baseDoc);

        // The KB bootstraps from workspace settings and the server ingests it
        // asynchronously; hover going live is the readiness signal.
        await waitFor('server to serve hover', async () => {
            const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
                'vscode.executeHoverProvider', baseDoc.uri, posOf(baseDoc, 'Human'));
            return hovers && hovers.length > 0;
        });
    });

    test('hover renders the man-page markdown', async () => {
        const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
            'vscode.executeHoverProvider', baseDoc.uri, posOf(baseDoc, 'Human'));
        const md = (hovers[0].contents[0] as vscode.MarkdownString).value;
        assert.ok(md.includes('### Human'), `hover heading present, got: ${md}`);
        assert.ok(md.includes('member of the species'), `documentation present, got: ${md}`);
        assert.ok(!md.includes('&%'), `cross-ref markers resolved, got: ${md}`);
    });

    test('goto-definition resolves across constituents', async () => {
        // `Human` in facts.kif is defined (first declared) in base.kif.
        const factsDoc = await vscode.workspace.openTextDocument(fixtureFile('facts.kif'));
        const locations = await waitFor('definition of Human from facts.kif', () =>
            vscode.commands.executeCommand<vscode.Location[]>(
                'vscode.executeDefinitionProvider', factsDoc.uri, posOf(factsDoc, 'Human')));
        assert.ok(locations.length > 0, 'definition found');
        assert.strictEqual(path.basename(locations[0].uri.fsPath), 'base.kif');
    });

    test('document symbols list each root sentence', async () => {
        const symbols = await waitFor('document symbols', () =>
            vscode.commands.executeCommand<vscode.DocumentSymbol[]>(
                'vscode.executeDocumentSymbolProvider', baseDoc.uri));
        // base.kif has 5 root sentences.
        assert.strictEqual(symbols.length, 5, JSON.stringify(symbols.map(s => s.name)));
        assert.ok(symbols.some(s => s.name === 'subclass'));
        assert.ok(symbols.some(s => s.name === 'documentation'));
    });

    test('workspace symbols find KB symbols by substring', async () => {
        const symbols = await waitFor('workspace symbols for "Homin"', () =>
            vscode.commands.executeCommand<vscode.SymbolInformation[]>(
                'vscode.executeWorkspaceSymbolProvider', 'Homin'));
        assert.ok(symbols.some(s => s.name === 'Hominid'),
            `Hominid in ${JSON.stringify(symbols.map(s => s.name))}`);
    });

    test('references span every constituent', async () => {
        const refs = await waitFor('references to Human', () =>
            vscode.commands.executeCommand<vscode.Location[]>(
                'vscode.executeReferenceProvider', baseDoc.uri, posOf(baseDoc, 'Human')));
        const files = new Set(refs.map(l => path.basename(l.uri.fsPath)));
        assert.ok(files.has('base.kif') && files.has('facts.kif'),
            `references in both constituents, got: ${[...files]}`);
    });

    test('symbol rename edits every occurrence in every file', async () => {
        const edit = await vscode.commands.executeCommand<vscode.WorkspaceEdit>(
            'vscode.executeDocumentRenameProvider',
            baseDoc.uri, posOf(baseDoc, 'Human'), 'Person');
        assert.ok(edit, 'rename produced a WorkspaceEdit');
        const entries = edit.entries();
        const files = new Set(entries.map(([uri]) => path.basename(uri.fsPath)));
        assert.ok(files.has('base.kif') && files.has('facts.kif'),
            `rename touches both constituents, got: ${[...files]}`);
        for (const [, edits] of entries) {
            for (const e of edits) { assert.strictEqual(e.newText, 'Person'); }
        }
    });

    test('variable rename stays inside its form and keeps the sigil', async () => {
        // `?X` appears twice inside the one implication in base.kif.
        const edit = await vscode.commands.executeCommand<vscode.WorkspaceEdit>(
            'vscode.executeDocumentRenameProvider',
            baseDoc.uri, posOf(baseDoc, '?X'), 'Y');
        assert.ok(edit, 'variable rename produced a WorkspaceEdit');
        const entries = edit.entries();
        assert.strictEqual(entries.length, 1, 'edits confined to one file');
        const edits = entries[0][1];
        assert.strictEqual(edits.length, 2, `both ?X occurrences, got ${edits.length}`);
        for (const e of edits) { assert.strictEqual(e.newText, '?Y'); }
    });

    test('semantic diagnostics are published for KB files', async () => {
        // Introduce an arity error in the live buffer; the server reconciles
        // on didChange and republishes.
        const editor = await vscode.window.showTextDocument(baseDoc);
        await editor.edit(b =>
            b.insert(new vscode.Position(baseDoc.lineCount, 0), '\n(subclass Widget)\n'));
        try {
            await waitFor('diagnostics on base.kif', async () => {
                const diags = vscode.languages.getDiagnostics(baseDoc.uri);
                return diags.length > 0;
            });
        } finally {
            // Revert so later suites (if any) see the pristine fixture.
            await vscode.commands.executeCommand('workbench.action.files.revert');
        }
    });

    test('formatting pretty-prints the document', async () => {
        // VSCode minimizes the server's single whole-document edit into diff
        // hunks, so assert on the APPLIED result, not the raw edit list.
        const edits = await waitFor('formatting edits', () =>
            vscode.commands.executeCommand<vscode.TextEdit[]>(
                'vscode.executeFormatDocumentProvider', baseDoc.uri, { tabSize: 2, insertSpaces: true }));
        assert.ok(edits.length > 0, 'formatter returned edits');
        const wsEdit = new vscode.WorkspaceEdit();
        wsEdit.set(baseDoc.uri, edits);
        assert.ok(await vscode.workspace.applyEdit(wsEdit), 'edits applied');
        try {
            const text = baseDoc.getText();
            assert.ok(text.startsWith('(subclass Human Hominid)'), `content preserved, got: ${text.slice(0, 80)}`);
            assert.ok(text.includes('(=> (instance ?X Human) (instance ?X Animal))'),
                `implication reflowed onto one line, got: ${text}`);
        } finally {
            await vscode.commands.executeCommand('workbench.action.files.revert');
        }
    });
});
