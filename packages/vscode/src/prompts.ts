// didOpen decision logic -- per-window-per-KB model.
//
// When the user opens a .kif file, the extension classifies it
// against (a) the active KB in this window and (b) the
// config.xml-declared KBs.  Three-way decision tree:
//
//   * File is a constituent of the active KB        -> no-op.
//   * No active KB yet                              -> offer:
//       - Load KB X (if the file belongs to one)
//       - Create a new temporary KB seeded with this file
//   * File is *not* in the active KB and the active
//     KB is not a superset                          -> offer:
//       - Open KB X in a new window (if file in another KB)
//       - Add file to the active KB (writes config.xml if permanent)
//       - Open in new window as a temporary KB
//       - Just open the file (no KB change; floats)
//
// "Open in new window" commands are executed by the caller via
// VSCode's `vscode.openFolder` with `forceNewWindow: true`; this
// module only returns the *outcome* indicating what to do.

import { QuickPickItem, window } from 'vscode';

import { ParsedConfig } from './config';
import { ActiveKb, configKbsContaining, KbState, normaliseAbs } from './kbSession';

/**
 * Like `window.showQuickPick`, but the user is not allowed to
 * dismiss.  Escape / focus-loss re-shows the picker; the Promise
 * only resolves once an item is selected.
 *
 * Used for the KB-membership decision, where a `.kif` that isn't
 * classified leaks into an inconsistent state (diagnostics point
 * at symbols the LSP doesn't know exist, the tree view stays
 * empty for no obvious reason).  Forcing the choice keeps the
 * window's KB-and-editor relationship an invariant.
 */
async function pickOrRetry<T extends QuickPickItem>(
    items:       T[],
    title:       string,
    placeHolder: string,
): Promise<T> {
    // Single-item picks degrade to "confirm" without a real
    // decision.  We still show the picker so the user sees the
    // options, but there is only one right answer.
    return new Promise<T>((resolve) => {
        const qp = window.createQuickPick<T>();
        qp.title              = title;
        qp.placeholder        = placeHolder;
        qp.items              = items;
        qp.ignoreFocusOut     = true;   // clicking away doesn't dismiss
        qp.matchOnDescription = true;
        qp.canSelectMany      = false;

        let accepted = false;
        qp.onDidAccept(() => {
            const [chosen] = qp.selectedItems;
            if (!chosen) { return; }     // nothing highlighted -- ignore
            accepted = true;
            resolve(chosen);
            qp.hide();
        });
        qp.onDidHide(() => {
            if (accepted) {
                qp.dispose();
                return;
            }
            // Escape / dismissal.  Re-show asynchronously so we
            // don't re-enter the hide-event callback.  A short
            // delay lets the disappear animation complete, which
            // avoids a visual glitch and makes the re-appearance
            // feel deliberate rather than buggy.
            setTimeout(() => qp.show(), 50);
        });

        qp.show();
    });
}

/** Outcome produced by `promptForKbMembership`.  Callers implement the action. */
export type PromptOutcome =
    | { kind: 'noop' }
    /** Install config KB `configKbName` as the window's active KB. */
    | { kind: 'load-config-here'; configKbName: string }
    /** Open config KB `configKbName` in a fresh VSCode window. */
    | { kind: 'open-config-in-new-window'; configKbName: string }
    /** Replace the active KB with a fresh temporary session seeded with this file. */
    | { kind: 'new-temp-here'; file: string }
    /** Add this file to the currently-active KB. */
    | { kind: 'add-to-active'; file: string }
    /** Open a fresh temporary session with this file in a new window. */
    | { kind: 'temp-in-new-window'; file: string };

/**
 * Classify a newly-opened `.kif` file and prompt for an action
 * when the user has a decision to make.
 */
export async function promptForKbMembership(
    state:    KbState,
    cfg:      ParsedConfig | null,
    file:     string,
): Promise<PromptOutcome> {
    const norm   = normaliseAbs(file);
    const active = state.get();

    // (a) Already a member of the active session -- no decision.
    if (active && active.files.has(norm)) {
        return { kind: 'noop' };
    }

    const memberOfConfigKbs = cfg ? configKbsContaining(cfg, norm) : [];

    // (b) No active KB yet: pick one.
    if (!active) {
        return await promptNoActiveKb(norm, memberOfConfigKbs);
    }

    // (c) Active KB exists but doesn't include this file.
    return await promptActiveKbMismatch(active, norm, memberOfConfigKbs);
}

// -- Prompt variants ---------------------------------------------------------

async function promptNoActiveKb(
    file:               string,
    memberOfConfigKbs:  string[],
): Promise<PromptOutcome> {
    interface Pick extends QuickPickItem { outcome: PromptOutcome }
    const picks: Pick[] = [];

    for (const kbName of memberOfConfigKbs) {
        picks.push({
            label:       `$(library) Load "${kbName}" (from config.xml)`,
            description: 'Installs this window as the editor for that KB',
            outcome:     { kind: 'load-config-here', configKbName: kbName },
        });
    }

    picks.push({
        label:       '$(file-submodule) New temporary KB with this file',
        description: 'Ad-hoc, not persisted to config.xml',
        outcome:     { kind: 'new-temp-here', file },
    });

    const header = memberOfConfigKbs.length > 0
        ? `This file belongs to ${memberOfConfigKbs.length > 1
            ? `${memberOfConfigKbs.length} declared KBs`
            : `KB "${memberOfConfigKbs[0]}"`}.  Choose how to handle it.`
        : 'This file is not part of any declared KB.  Choose how to handle it.';

    const chosen = await pickOrRetry(picks, 'SUMO Knowledge Base', header);
    return chosen.outcome;
}

async function promptActiveKbMismatch(
    active:             ActiveKb,
    file:               string,
    memberOfConfigKbs:  string[],
): Promise<PromptOutcome> {
    interface Pick extends QuickPickItem { outcome: PromptOutcome }
    const picks: Pick[] = [];

    // Config KBs this file belongs to, excluding the active one
    // (if the active one is config-backed).
    for (const kbName of memberOfConfigKbs) {
        if (active.source === 'config' && active.configKbName === kbName) { continue; }
        picks.push({
            label:       `$(multiple-windows) Open "${kbName}" in a new window`,
            description: 'Each VSCode window edits exactly one KB',
            outcome:     { kind: 'open-config-in-new-window', configKbName: kbName },
        });
    }

    // Add to the currently-active KB.
    picks.push({
        label:       `$(add) Add to "${active.name}"`,
        description: active.source === 'config'
            ? 'Writes config.xml'
            : 'This window only (temporary KB)',
        outcome:     { kind: 'add-to-active', file },
    });

    // Open in a fresh temporary window.
    picks.push({
        label:       '$(multiple-windows) Open in a new temporary window',
        description: 'Fresh VSCode window; this file becomes its temp KB',
        outcome:     { kind: 'temp-in-new-window', file },
    });

    const header = memberOfConfigKbs.length > 0
        ? `This file belongs to ${memberOfConfigKbs.length > 1
            ? `${memberOfConfigKbs.length} declared KBs`
            : `KB "${memberOfConfigKbs[0]}"`} — but this window is editing "${active.name}".  Choose how to handle it.`
        : `This file is not part of "${active.name}".  Choose how to handle it.`;

    const chosen = await pickOrRetry(picks, 'SUMO Knowledge Base', header);
    return chosen.outcome;
}
