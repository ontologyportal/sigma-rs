// Open the Sigma browser on the symbol under the cursor.
//
// Pure client-side command: no LSP / kernel involvement.  Resolves
// the word at the caret position against the KIF word pattern
// (optionally `?` / `@` prefixed for variables, but variables are
// ignored here -- Sigma browses terms, not variables) and opens
//
//   <sumo.sigma.url>/Browse.jsp?kb=<kb>&term=<symbol>
//
// in the user's external browser via `vscode.env.openExternal`.
//
// `<kb>` is taken from the active KB's name when it is a config KB
// (temporary KBs have no counterpart in Sigma), falling back to
// `SUMO` as a last resort so the command always resolves to
// *something* sensible rather than erroring out.

import { env, Uri, window } from 'vscode';
import { workspace } from 'vscode';

import { ActiveKb } from './kbSession';

export async function browseInSigmaCommand(
    getActive: () => ActiveKb | null,
): Promise<void> {
    const editor = window.activeTextEditor;
    if (!editor) {
        window.showInformationMessage('Open a .kif file and place the cursor on a symbol to browse.');
        return;
    }

    const range = editor.document.getWordRangeAtPosition(
        editor.selection.active,
        // `?foo` and `@foo` are not terms; deliberately exclude the
        // leading prefix so the cursor on `?Animal` still resolves
        // to `Animal`.
        /[A-Za-z_][A-Za-z0-9_\-]*/,
    );
    if (!range) {
        window.showInformationMessage('No SUMO symbol under the cursor.');
        return;
    }
    const symbol = editor.document.getText(range);

    const base = workspace.getConfiguration('sumo').get<string>('sigma.url', '')
        .trim().replace(/\/+$/, '');
    if (base.length === 0) {
        window.showErrorMessage(
            'sumo.sigma.url is not configured.  Set it in your settings to browse in Sigma.');
        return;
    }

    // Config KBs: their name round-trips to Sigma directly.
    // Temporary KBs: no Sigma equivalent, so fall back to the
    // conventional default ("SUMO") -- the user can still follow
    // the link to explore the symbol in the public KB.
    const active = getActive();
    const kb = (active?.source === 'config' && active.configKbName)
        ? active.configKbName
        : 'SUMO';

    const url = `${base}/Browse.jsp?kb=${encodeURIComponent(kb)}&term=${encodeURIComponent(symbol)}`;
    await env.openExternal(Uri.parse(url));
}
