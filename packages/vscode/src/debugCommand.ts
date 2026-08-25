// src/debugCommand.ts
//
// "SUMO: Consistency-Check File" — runs the kernel's `debug`
// method on the active `.kif` file and highlights contributing
// axioms as errors in the editor.
//
// The kernel's `debug` response groups contradictions by source
// (sid + file + line).  Each entry becomes a VSCode `Diagnostic`
// on the line the axiom lives on — visible in the gutter, the
// Problems panel, and the file's minimap.  Diagnostics span every
// file whose axioms contributed, not just the file the user
// invoked the command on: SInE pulls relevant axioms from across
// the KB and the contradiction can involve any of them.
//
// Status is shown via `window.showInformationMessage` /
// `showErrorMessage` — deliberately not a webview, matching the
// "quick verdict + in-place errors" pattern the user asked for.
//
// Re-running the command clears every diagnostic the extension
// previously published (across all files) before publishing the
// fresh set.  That way a second run doesn't leave stale red
// squiggles on an axiom that's no longer contradictory.

import {
    DiagnosticCollection,
    Diagnostic,
    DiagnosticSeverity,
    ExtensionContext,
    Position,
    ProgressLocation,
    Range,
    Uri,
    window,
} from 'vscode';

import { SumoKernelClient, DebugResult, ContradictionEntry } from './kernelClient';
import { normaliseAbs } from './kbSession';

/**
 * Command handler for `sumo.debug.file`.
 *
 * Gathers the active editor's file path, invokes the kernel's
 * `debug` method, and dispatches the verdict to the right UI
 * surface: diagnostics for the axioms, a dialog for the verdict.
 *
 * No-op when the active editor isn't a `.kif` file or no kernel
 * is reachable.  Errors are surfaced as error toasts + written to
 * the output channel via the kernel client's existing logging.
 */
export async function onDebugFile(
    context:    ExtensionContext,
    kernel:     SumoKernelClient,
    diagnostics: DiagnosticCollection,
): Promise<void> {
    const editor = window.activeTextEditor;
    if (!editor) {
        window.showInformationMessage(
            'Open a .kif file first — consistency-checking runs against the file under the cursor.');
        return;
    }
    const doc = editor.document;
    if (doc.languageId !== 'kif' || doc.uri.scheme !== 'file') {
        window.showInformationMessage(
            'The active editor is not a KIF file on disk.  Open a loaded .kif file and retry.');
        return;
    }
    const file = normaliseAbs(doc.uri.fsPath);

    let result: DebugResult;
    try {
        result = await window.withProgress({
            location:    ProgressLocation.Notification,
            title:       `SUMO: consistency-checking ${pathBasename(file)}…`,
            cancellable: false,
        }, () => kernel.debug(file));
    } catch (err) {
        window.showErrorMessage(
            `Consistency check failed: ${String(err)}`,
        );
        return;
    }

    // Always start from a clean slate — previous runs can have
    // published diagnostics into any number of files, and a fresh
    // verdict should invalidate all of them.
    diagnostics.clear();

    switch (result.status) {
        case 'Consistent':
            window.showInformationMessage(
                `SUMO: ${pathBasename(result.file)} is consistent ` +
                `(${result.totalChecked} axioms checked` +
                (result.filesPulled.length > 0
                    ? `, ${result.filesPulled.length} other file${result.filesPulled.length === 1 ? '' : 's'} pulled in by SInE`
                    : '') +
                `).`,
            );
            return;

        case 'Inconsistent':
            publishContradictionDiagnostics(diagnostics, result.contradictions);
            showInconsistencyStatus(result);
            return;

        case 'Timeout':
            window.showWarningMessage(
                `SUMO: prover timed out after ${result.totalChecked} axiom${result.totalChecked === 1 ? '' : 's'}.  ` +
                `Verdict unknown — retry with a higher --timeout or lower --thoroughness.`,
            );
            return;

        case 'Unknown':
        default:
            window.showWarningMessage(
                `SUMO: consistency check returned ${result.status}.  ` +
                `No contradictions surfaced; see the "SUMO / KIF" output channel for prover details.`,
            );
            return;
    }
}

// -- Diagnostic publishing ---------------------------------------------------

/**
 * Map the kernel's `ContradictionEntry[]` into VSCode diagnostics,
 * grouped by file (one `DiagnosticCollection` update per file).
 *
 * Each entry becomes a single diagnostic on its source line.  We
 * don't have a column range from the kernel — the line number is
 * 1-based and the axiom often spans multiple lines in the source —
 * so we highlight the entire line (`0..Number.MAX_SAFE_INTEGER`;
 * VSCode caps at line length automatically).  That's precise
 * enough for the Problems panel and the gutter; exact span would
 * require reading each file and walking the parser, which is
 * overkill for a verdict-level check.
 *
 * Duplicates (same sid) are already deduped server-side, so we
 * don't re-dedupe here.  Multiple axioms on the same line — rare
 * but legal — each get their own diagnostic.
 */
function publishContradictionDiagnostics(
    collection:     DiagnosticCollection,
    contradictions: ContradictionEntry[],
): void {
    // Group by URI so we can update each file once.  Using a plain
    // object keyed by the string form of the URI avoids Map vs
    // Uri-instance-equality gotchas.
    const byFile: Record<string, { uri: Uri; diags: Diagnostic[] }> = {};
    for (const c of contradictions) {
        const uri = Uri.file(c.file);
        const key = uri.toString();
        if (!byFile[key]) {
            byFile[key] = { uri, diags: [] };
        }
        // Convert 1-based line to 0-based, clamp to non-negative.
        const line = Math.max(0, c.line - 1);
        const range = new Range(
            new Position(line, 0),
            new Position(line, Number.MAX_SAFE_INTEGER),
        );
        const diag = new Diagnostic(
            range,
            `Contributes to KB inconsistency: ${c.kif}`,
            DiagnosticSeverity.Error,
        );
        diag.source = 'sumo-debug';
        // Stable code so users can filter / mute via the Problems
        // panel's "filter by code" UI.
        diag.code = 'sumo-debug/contradictory-axiom';
        byFile[key].diags.push(diag);
    }

    // Publish in one pass.  `set(uri, diags)` replaces the
    // per-file entry wholesale, which is exactly what we want
    // after a `clear()` above.
    for (const { uri, diags } of Object.values(byFile)) {
        collection.set(uri, diags);
    }
}

// -- Status dialog -----------------------------------------------------------

/**
 * Surface the `Inconsistent` verdict as an error toast.  Shows
 * the count of contributing axioms and the number of files they
 * span — enough signal for the user to know where to look — with
 * a "Show in Problems" action that reveals the Problems panel.
 */
function showInconsistencyStatus(result: DebugResult): void {
    const files = new Set(result.contradictions.map(c => c.file));
    const axiomCount = result.contradictions.length;
    const fileCount  = files.size;

    const headline = axiomCount === 0
        ? `SUMO: ${pathBasename(result.file)} is inconsistent, but no traceable contributing axioms were produced.`
        : `SUMO: ${pathBasename(result.file)} is inconsistent — ` +
          `${axiomCount} contributing axiom${axiomCount === 1 ? '' : 's'} ` +
          `across ${fileCount} file${fileCount === 1 ? '' : 's'}.`;

    // "Show in Problems" drives the user to the panel where every
    // highlighted axiom is clickable.  Keep the action available
    // even when the list is empty so they can see the "no trace"
    // hint there and decide what to do next.
    void window.showErrorMessage(headline, 'Show in Problems').then(choice => {
        if (choice === 'Show in Problems') {
            // `workbench.actions.view.problems` is VSCode's
            // built-in command; any extension can ping it.  No
            // args — it focuses the panel.
            void (async () => {
                try {
                    const { commands } = await import('vscode');
                    await commands.executeCommand('workbench.actions.view.problems');
                } catch {
                    // Falling back silently — failure here is
                    // purely cosmetic.
                }
            })();
        }
    });
}

// -- Small helpers -----------------------------------------------------------

/**
 * Last path segment for display in toasts.  Deliberately doesn't
 * normalise separators — the kernel gives us filesystem-native
 * paths, and we want the status dialog to show the user-visible
 * form.
 */
function pathBasename(p: string): string {
    const i = Math.max(p.lastIndexOf('/'), p.lastIndexOf('\\'));
    return i >= 0 ? p.slice(i + 1) : p;
}
