// Scaffold a new knowledge base entry in config.xml.
//
// Prompts for:
//   1. KB name (must be unique in config.xml, must not be empty)
//   2. Optional seed file -- either an existing .kif in the
//      workspace, an absolute path, or none.
//
// Writes the new `<kb name="…">` entry to config.xml, reloads the
// parsed config so the tree view picks it up, then activates the
// new KB in the current window.

import * as fs   from 'fs';
import * as path from 'path';
import { window, workspace } from 'vscode';

import { createKbEntry, ParsedConfig, relativiseToKbDir } from './config';
import { KbState } from './kbSession';

export async function createKbCommand(
    state:         KbState,
    getConfig:     () => ParsedConfig | null,
    reloadConfig:  () => void,
    onActivated:   () => Promise<void>,
): Promise<void> {
    const cfg = getConfig();
    if (!cfg) {
        window.showErrorMessage(
            'No config.xml is loaded.  Set `sumo.configPath` or create one under $SIGMA_HOME.');
        return;
    }

    const existing = new Set(cfg.kbs.map(kb => kb.name));

    const name = (await window.showInputBox({
        title:       'Create Knowledge Base',
        prompt:      'Name of the new knowledge base (must be unique in config.xml)',
        placeHolder: 'MyKB',
        ignoreFocusOut: true,
        validateInput(value) {
            const trimmed = value.trim();
            if (!trimmed) { return 'Name must not be empty.'; }
            if (existing.has(trimmed)) { return `"${trimmed}" already exists.`; }
            if (!/^[A-Za-z][A-Za-z0-9_\-]*$/.test(trimmed)) {
                return 'Use letters, digits, underscore, and hyphen only; start with a letter.';
            }
            return null;
        },
    }))?.trim();
    if (!name) { return; }

    const seed = await window.showInputBox({
        title:       `Create Knowledge Base "${name}"`,
        prompt:      'Optional seed .kif file (absolute path or path relative to kbDir).  Leave blank for an empty KB.',
        placeHolder: '',
        ignoreFocusOut: true,
        validateInput(value) {
            const trimmed = value.trim();
            if (!trimmed) { return null; }
            const abs = path.isAbsolute(trimmed) ? trimmed : path.join(cfg.kbDir, trimmed);
            if (!fs.existsSync(abs)) { return `File not found: ${abs}`; }
            if (!abs.endsWith('.kif') && !abs.endsWith('.kif.tq')) {
                return 'Seed file must have a .kif or .kif.tq extension.';
            }
            return null;
        },
    });
    const seedTrim = seed?.trim() ?? '';
    const seedFiles: string[] = [];
    if (seedTrim.length > 0) {
        const abs = path.isAbsolute(seedTrim) ? seedTrim : path.join(cfg.kbDir, seedTrim);
        seedFiles.push(relativiseToKbDir(abs, cfg.kbDir));
    }

    try {
        createKbEntry(cfg.configPath, name, seedFiles);
    } catch (err) {
        window.showErrorMessage(`Failed to create KB "${name}": ${err}`);
        return;
    }

    reloadConfig();

    // Activate the new KB in this window so the tree view + status
    // bar immediately reflect it.  File paths are re-resolved to
    // absolute by `parseConfigXml`, so read back through the
    // reloaded config rather than trusting the seed input.
    const fresh = getConfig();
    const kb = fresh?.kbs.find(k => k.name === name);
    if (kb) {
        state.setConfigKb(kb.name, kb.files);
        await onActivated();
    }

    window.showInformationMessage(
        `Created KB "${name}"${seedFiles.length ? ` with ${seedFiles.length} seed file` : ''}.`);
}
