// Delegate selection / axiom formatting to sumo-lsp via the
// standard `textDocument/rangeFormatting` request.  The LSP owns
// all S-expression formatting logic; this command only expands
// the caller's selection to a full top-level form (so a cursor
// on one line of an axiom still reformats the whole axiom) and
// issues the LSP request.

import {
    FormattingOptions,
    Position,
    Range,
    TextEdit,
    window,
    workspace,
    WorkspaceEdit,
} from 'vscode';
import { LanguageClient, RequestType } from 'vscode-languageclient/node';
import {
    DocumentRangeFormattingParams,
    DocumentRangeFormattingRequest,
} from 'vscode-languageclient';

export async function formatAxiomCommand(
    getClient: () => LanguageClient | undefined,
): Promise<void> {
    const client = getClient();
    const editor = window.activeTextEditor;
    if (!editor || editor.document.languageId !== 'kif') {
        window.showInformationMessage('Open a .kif file to format an axiom.');
        return;
    }
    if (!client) {
        window.showErrorMessage('sumo-lsp is not running; cannot format.');
        return;
    }

    const doc = editor.document;
    const range = editor.selection.isEmpty
        ? enclosingFormRange(doc.getText(), doc.offsetAt(editor.selection.active), doc)
        : new Range(editor.selection.start, editor.selection.end);

    if (!range) {
        window.showInformationMessage('No S-expression found around the cursor to format.');
        return;
    }

    const options: FormattingOptions = {
        tabSize:      Number(editor.options.tabSize ?? 2),
        insertSpaces: editor.options.insertSpaces !== false,
    };

    const params: DocumentRangeFormattingParams = {
        textDocument: { uri: doc.uri.toString() },
        range: {
            start: { line: range.start.line, character: range.start.character },
            end:   { line: range.end.line,   character: range.end.character   },
        },
        options,
    };

    const edits = await client.sendRequest(
        DocumentRangeFormattingRequest.type as RequestType<DocumentRangeFormattingParams, TextEdit[] | null, void>,
        params,
    );
    if (!edits || edits.length === 0) { return; }

    const we = new WorkspaceEdit();
    for (const e of edits) {
        we.replace(
            doc.uri,
            new Range(
                new Position(e.range.start.line, e.range.start.character),
                new Position(e.range.end.line,   e.range.end.character),
            ),
            e.newText,
        );
    }
    await workspace.applyEdit(we);
}

function enclosingFormRange(
    text:   string,
    offset: number,
    doc:    import('vscode').TextDocument,
): Range | undefined {
    let depth = 0;
    let start = -1;
    let inStr = false;
    let inCmt = false;
    for (let i = 0; i < text.length; i++) {
        const c = text[i];
        if (inCmt) {
            if (c === '\n') { inCmt = false; }
            continue;
        }
        if (inStr) {
            if (c === '"' && text[i - 1] !== '\\') { inStr = false; }
            continue;
        }
        if (c === ';') { inCmt = true; continue; }
        if (c === '"') { inStr = true; continue; }
        if (c === '(') {
            if (depth === 0) { start = i; }
            depth++;
        } else if (c === ')') {
            depth--;
            if (depth === 0) {
                const end = i + 1;
                if (offset >= start && offset <= end) {
                    return new Range(doc.positionAt(start), doc.positionAt(end));
                }
                start = -1;
            }
        }
    }
    return undefined;
}
