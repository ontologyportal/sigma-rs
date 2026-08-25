// Export the active KB to TPTP.
//
// Delegates to the kernel's `kb.generateTptp` RPC -- the kernel
// reuses its already-loaded KB and invokes the same converter
// `sumo ask` would use for conjecture compilation, so the output
// is byte-identical to what the prover sees internally.  Keeping
// this server-side means the extension never has to ship or
// maintain a translator of its own.
//
// The TPTP dialect (`fof` | `tff`) is read from the
// `sumo.tptp.lang` setting.

import * as path from 'path';
import { Uri, window, workspace } from 'vscode';

import { SumoKernelClient } from './kernelClient';
import { ActiveKb } from './kbSession';

export async function generateTptpCommand(
    kernel:    SumoKernelClient,
    getActive: () => ActiveKb | null,
): Promise<void> {
    const active = getActive();
    if (!active) {
        window.showInformationMessage('No active knowledge base.  Load one first.');
        return;
    }

    const lang = workspace.getConfiguration('sumo')
        .get<string>('tptp.lang', 'fof');

    let result: { tptp: string; formulaCount: number };
    try {
        result = await window.withProgress(
            { location: { viewId: 'sumoKnowledgeBases' }, title: 'SUMO: generating TPTP…', cancellable: false },
            () => kernel.generateTptp({ lang }),
        );
    } catch (err) {
        window.showErrorMessage(`TPTP generation failed: ${String(err)}`);
        return;
    }

    const target = await window.showSaveDialog({
        defaultUri: defaultTargetUri(active),
        filters:    { TPTP: ['p', 'tptp'] },
        title:      'Save generated TPTP',
    });
    if (!target) { return; }

    await workspace.fs.writeFile(target, Buffer.from(result.tptp, 'utf8'));
    const doc = await workspace.openTextDocument(target);
    await window.showTextDocument(doc, { preview: false });
    window.setStatusBarMessage(
        `TPTP saved: ${result.formulaCount} formula${result.formulaCount === 1 ? '' : 's'}`,
        5000,
    );
}

function defaultTargetUri(active: ActiveKb): Uri | undefined {
    const first = Array.from(active.files).sort()[0];
    const dir = first ? path.dirname(first) : undefined;
    if (!dir) { return undefined; }
    return Uri.file(path.join(dir, `${active.name}.p`));
}
