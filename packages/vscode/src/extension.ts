// SUMO / KIF VSCode extension.
//
// Per-window-per-KB model.  Each VSCode window edits at most one
// knowledge base; cross-KB work is done by opening new VSCode
// windows.  Each window runs its own `sumo-lsp` subprocess whose
// single-KB scope is naturally the window's KB -- package
// semantics (isolated symbol namespaces between KBs) come for
// free from OS-level process isolation.
//
// The extension handles:
//
//   1. Server-binary resolution (see comment below).
//   2. Parsing SigmaKEE's `config.xml` and exposing declared
//      KBs as loadable units.
//   3. Bootstrapping the active KB on window open:
//        - `sumo.activeKb` workspace setting declares the KB.
//        - Extension looks it up in config.xml, loads it.
//      When unset, the window starts with no active KB until the
//      user opens a `.kif` file or runs a load command.
//   4. Classifying each opened `.kif` against the active KB +
//      declared KBs and presenting a three-way decision:
//        - Fits current KB -> no-op.
//        - No active KB yet -> pick a config KB, or create a
//          temp KB seeded with this file.
//        - File belongs to another KB -> open that KB in a new
//          window, add to the current one, or spawn a temp
//          window.
//   5. Writing add / remove back to config.xml.
//
// Server-binary resolution (in priority order):
//
//   1. User override -- `sumo.server.path` at any config scope.
//   2. Bundled binary in `<extensionPath>/server/` (the
//      platform-specific VSIX ships this).
//   3. `sumo-lsp` on $PATH.

import * as fs   from 'fs';
import * as os   from 'os';
import * as path from 'path';
import {
    window,
    workspace,
    commands,
    languages,
    StatusBarAlignment,
    StatusBarItem,
    ExtensionContext,
    OutputChannel,
    ConfigurationChangeEvent,
    QuickPickItem,
    TextDocument,
    Uri,
} from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    Executable,
    TransportKind,
    Trace,
} from 'vscode-languageclient/node';

import { ParsedConfig, parseConfigXml, relativiseToKbDir, resolveConfigPath,
         writeKbConstituents } from './config';
import { ActiveKb, KbState, normaliseAbs } from './kbSession';
import { KbTreeProvider } from './kbTreeView';
import { SumoKernelClient } from './kernelClient';
import {
    cleanupOrphanEphemerals,
    deleteEphemeralDb,
    deletePersistentDb,
    resolveKernelDbPath,
} from './kernelDb';
import { showAskResult } from './askWebview';
import { promptForKbMembership, PromptOutcome } from './prompts';
import { showTaxonomyCommand } from './taxonomy';
import { browseInSigmaCommand } from './browseInSigma';
import { createKbCommand } from './createKb';
import { formatAxiomCommand } from './formatAxiom';
import { generateTptpCommand } from './generateTptp';
import { openReplCommand } from './repl';
import { onDebugFile } from './debugCommand';
import { checkKernelHealth, KernelHealth, RELEASES_URL } from './kernelHealth';

let client:        LanguageClient | undefined;
let outputChannel: OutputChannel  | undefined;
let state:         KbState;
let treeProvider:  KbTreeProvider;
let statusBarItem: StatusBarItem;
let parsedConfig:  ParsedConfig | null = null;
let kernelClient:  SumoKernelClient | undefined;
/**
 * Dedicated collection for contradictions surfaced by the
 * `sumo.debug.file` command.  Kept separate from `sumo-lsp`'s
 * parse / semantic diagnostics so the two never fight — VSCode
 * overlays them naturally in the Problems panel.  Cleared before
 * each debug run and whenever the active KB changes, so stale red
 * squiggles never linger past the condition that produced them.
 */
let debugDiagnostics: import('vscode').DiagnosticCollection | undefined;

/**
 * Latest result of the kernel health check.  Populated by
 * `refreshKernelHealth()` at activation and on every
 * `sumo.kernel.path` config change.  When `status !== 'ok'` the
 * RPC-dependent commands are hidden from the palette via a
 * context-variable gate (`sumo.kernelAvailable`).
 */
let kernelHealth: KernelHealth | undefined;
/**
 * Captured at `activate()` so `deactivate()` (which VSCode calls
 * without arguments) can still resolve ephemeral LMDB paths for
 * cleanup.  The reference outlives the extension host only by the
 * few ms of tear-down, so we don't need to re-capture it.
 */
let extensionContext: ExtensionContext | undefined;

// -- Activation --------------------------------------------------------------

export async function activate(context: ExtensionContext): Promise<void> {
    extensionContext = context;
    outputChannel = window.createOutputChannel('SUMO / KIF');
    context.subscriptions.push(outputChannel);

    state        = new KbState();
    treeProvider = new KbTreeProvider(state);
    context.subscriptions.push(
        window.registerTreeDataProvider('sumoKnowledgeBases', treeProvider),
    );

    statusBarItem = window.createStatusBarItem(StatusBarAlignment.Left, 100);
    statusBarItem.command = 'sumo.kb.showStatus';
    context.subscriptions.push(statusBarItem);
    refreshStatusBar();

    // Diagnostic collection for `sumo.debug.file` contributions.
    // Separate from the LSP's `sumo-lsp`-sourced diagnostics so the
    // two channels stay independent; cleared before every debug run
    // and on any KB change so stale contradictions don't linger.
    debugDiagnostics = languages.createDiagnosticCollection('sumo-debug');
    context.subscriptions.push(debugDiagnostics);

    reloadConfig();

    // Prune any ephemeral LMDB directories left behind by a
    // previous VSCode crash / force-kill.  At activation the live-
    // session set is empty (no temp KB open yet), so every
    // ephemeral entry on disk is by definition an orphan.
    cleanupOrphanEphemerals(context, new Set(), outputChannel);

    // Commands.
    context.subscriptions.push(
        commands.registerCommand('sumo.restartServer',      onRestartServer(context)),
        commands.registerCommand('sumo.showServerOutput',   () => outputChannel?.show()),
        commands.registerCommand('sumo.kb.load',            onLoadKb),
        commands.registerCommand('sumo.kb.openInNewWindow', onOpenInNewWindow),
        commands.registerCommand('sumo.kb.create',          () => onCreateKb()),
        commands.registerCommand('sumo.kb.addFile',         onAddFile),
        commands.registerCommand('sumo.kb.removeFile',      onRemoveFile),
        commands.registerCommand('sumo.kb.close',           () => onCloseKb(context)),
        commands.registerCommand('sumo.kb.reloadConfig',    onReloadConfig),
        commands.registerCommand('sumo.kb.showStatus',      () => outputChannel?.show()),
        commands.registerCommand('sumo.browseInSigma',      () => browseInSigmaCommand(() => state.get())),
        commands.registerCommand('sumo.formatAxiom',        () => formatAxiomCommand(() => client)),
        commands.registerCommand('sumo.generateTPTP',       () => onGenerateTptp(context)),
        commands.registerCommand('sumo.openRepl',           () => openReplCommand(ensureKernel(context))),
        // `sumo.showTaxonomy` may be invoked from the command palette
        // (no argument -- resolves the word under the cursor) or
        // programmatically with an explicit symbol name.
        commands.registerCommand('sumo.showTaxonomy',       (arg?: unknown) =>
            showTaxonomyCommand(context, () => client, outputChannel!, arg),
        ),
        // Ask/tell kernel commands -- lazy-spawned, see
        // `ensureKernel()`.
        commands.registerCommand('sumo.ask.cursor',         () => onAskCursor(context)),
        commands.registerCommand('sumo.kernel.restart',     () => onRestartKernel(context)),
        commands.registerCommand('sumo.kernel.showOutput',  () => outputChannel?.show()),
        commands.registerCommand('sumo.kernel.rebuildDatabase', () => onRebuildKernelDb(context)),
        commands.registerCommand('sumo.kernel.deleteDatabase',  () => onDeleteKernelDb(context)),
        commands.registerCommand('sumo.kernel.reconcileOpenFiles', () => onReconcileOpenFiles(context)),
        commands.registerCommand('sumo.kernel.showFileStatus',  () => onShowKernelFileStatus(context)),
        commands.registerCommand('sumo.debug.file',             () => onDebugActiveFile(context)),
    );

    // File watcher: keep the kernel's DB in sync with disk.
    // Option A (per the agreed plan): save triggers reconcile,
    // unsaved buffer edits do not.  We use an on-disk watcher so
    // the event fires regardless of how the file changed (editor
    // save, git checkout, external tool).
    const watcher = workspace.createFileSystemWatcher('**/*.{kif,kif.tq}');
    context.subscriptions.push(
        watcher,
        watcher.onDidChange(uri => onFileSaved(context, uri.fsPath)),
        watcher.onDidCreate(uri => onFileSaved(context, uri.fsPath)),
        watcher.onDidDelete(uri => onFileDeleted(context, uri.fsPath)),
    );

    // Restart + reload on relevant config changes.
    context.subscriptions.push(
        workspace.onDidChangeConfiguration(async (e: ConfigurationChangeEvent) => {
            if (
                e.affectsConfiguration('sumo.server.path') ||
                e.affectsConfiguration('sumo.server.args') ||
                e.affectsConfiguration('sumo.server.env')
            ) {
                outputChannel?.appendLine('[extension] server config changed, restarting');
                await stopClient();
                await startClient(context);
            }
            if (e.affectsConfiguration('sumo.configPath')) {
                reloadConfig();
            }
            if (e.affectsConfiguration('sumo.activeKb')) {
                await bootstrapFromWorkspaceSettings();
            }
            if (e.affectsConfiguration('sumo.diagnostics.ignoredCodes')) {
                await pushIgnoredDiagnosticsToServer();
            }
            if (e.affectsConfiguration('sumo.trace.server')) {
                await applyTraceLevel();
            }
            if (e.affectsConfiguration('sumo.kernel.path')) {
                // User pointed us at a new binary — re-classify its
                // health so the palette gate reflects reality.  A
                // previously-missing binary becoming available
                // re-enables every RPC command on the spot; no
                // window reload required.
                await refreshKernelHealth();
            }
        }),
    );

    // didOpen classification.
    context.subscriptions.push(
        workspace.onDidOpenTextDocument(async (doc) => {
            if (doc.languageId !== 'kif') { return; }
            await classifyOpenedKif(doc);
        }),
    );

    await startClient(context);
    // Bootstrap *after* the client is running so setActiveFiles lands.
    await bootstrapFromWorkspaceSettings();
    // Push the current ignored-diagnostics list so the server
    // starts out agreeing with the UI.  No-op when the list is
    // empty; the server's default is an empty set anyway.
    await pushIgnoredDiagnosticsToServer();

    // Eagerly probe the `sigmakee` kernel binary.  Surface any
    // problem (missing, no-serve, crash) as a dialog and flip the
    // `sumo.kernelAvailable` context variable so the Command
    // Palette hides every kernel-dependent command until the user
    // fixes the install.
    await refreshKernelHealth();

    // Any editors that were already open at activation get
    // classified after the bootstrap so they see the active KB.
    for (const doc of workspace.textDocuments) {
        if (doc.languageId === 'kif') {
            void classifyOpenedKif(doc);
        }
    }
}

export async function deactivate(): Promise<void> {
    // Snapshot the active KB BEFORE we tear down so we can tell
    // whether an ephemeral LMDB needs deleting.
    const active = state?.get() ?? null;

    await Promise.allSettled([
        stopClient(),
        kernelClient?.stop() ?? Promise.resolve(),
    ]);
    kernelClient = undefined;

    // Kernel is stopped now; safe to `rm -rf` the ephemeral dir.
    if (active && active.source === 'temporary'
        && extensionContext && outputChannel) {
        deleteEphemeralDb(extensionContext, active, outputChannel);
    }
    extensionContext = undefined;
}

// -- Workspace-settings bootstrap -------------------------------------------

/**
 * Look up `sumo.activeKb` in the workspace settings; if present,
 * auto-load that KB so the window is pre-configured when opened
 * via the `openInNewWindow` command (or by a user who committed
 * a `.code-workspace` file into their project).
 */
async function bootstrapFromWorkspaceSettings(): Promise<void> {
    const wanted = workspace.getConfiguration('sumo').get<string>('activeKb', '').trim();
    if (!wanted) { return; }

    if (!parsedConfig) {
        outputChannel?.appendLine(
            `[extension] sumo.activeKb="${wanted}" but no config.xml is loaded`);
        return;
    }

    const kb = parsedConfig.kbs.find(k => k.name === wanted);
    if (!kb) {
        outputChannel?.appendLine(
            `[extension] sumo.activeKb="${wanted}" is not declared in config.xml`);
        return;
    }

    const active = state.get();
    if (active && active.source === 'config' && active.configKbName === wanted) {
        // Already bootstrapped; no-op.
        return;
    }

    state.setConfigKb(kb.name, kb.files);
    outputChannel?.appendLine(
        `[extension] bootstrapped KB "${kb.name}" (${kb.files.length} files) from workspace settings`);
    treeProvider.refresh();
    refreshStatusBar();
    await pushActiveFilesToServer();
}

// -- LSP client lifecycle ----------------------------------------------------

async function startClient(context: ExtensionContext): Promise<void> {
    if (client) { return; }

    const config = workspace.getConfiguration('sumo');
    const serverArgs = config.get<string[]>('server.args', []);
    const extraEnv   = config.get<Record<string, string>>('server.env', {});

    const resolvedPath = resolveServerBinary(config, context);

    const executable: Executable = {
        command: resolvedPath,
        args:    serverArgs,
        transport: TransportKind.stdio,
        options: { env: { ...process.env, ...extraEnv } },
    };
    const serverOptions: ServerOptions = { run: executable, debug: executable };

    const clientOptions: LanguageClientOptions = {
        documentSelector: [
            { scheme: 'file',     language: 'kif' },
            { scheme: 'untitled', language: 'kif' },
        ],
        outputChannel,
        traceOutputChannel: outputChannel,
        synchronize: {
            fileEvents: workspace.createFileSystemWatcher('**/*.{kif,kif.tq}'),
        },
        // `clientManagesFiles` tells the server not to run its
        // initial workspace sweep.  Without this, `sumo-lsp` loads
        // every .kif under the workspace roots and the subsequent
        // `sumo/setActiveFiles` call has to un-load the ones that
        // don't belong to the active KB — each `remove_file` is
        // O(total occurrences), so larger workspaces starve the
        // event loop for minutes before any request goes through.
        initializationOptions: {
            clientManagesFiles: true,
        },
    };

    client = new LanguageClient('sumo-lsp', 'SUMO / KIF', serverOptions, clientOptions);
    outputChannel?.appendLine(
        `[extension] starting sumo-lsp: ${resolvedPath} ${serverArgs.join(' ')}`);

    try {
        await client.start();
        outputChannel?.appendLine('[extension] sumo-lsp ready');
        // Apply the user's trace level.  `vscode-languageclient`
        // derives its auto-bound trace config key from the client
        // ID (`sumo-lsp`), not from our user-facing `sumo.*`
        // namespace, so the setting is wired programmatically here.
        await applyTraceLevel();
        await pushActiveFilesToServer();
        // Re-push the ignore-list after every (re)start so a
        // freshly-spawned server immediately honours the user's
        // `sumo.diagnostics.ignoredCodes` setting.
        await pushIgnoredDiagnosticsToServer();
    } catch (err) {
        outputChannel?.appendLine(`[extension] failed to start sumo-lsp: ${err}`);
        window.showErrorMessage(
            `sumo-lsp failed to start.  Set "sumo.server.path" or install the binary on PATH; ` +
            `see the "SUMO / KIF" output channel for details.`);
        client = undefined;
    }
}

async function stopClient(): Promise<void> {
    if (!client) { return; }
    const c = client;
    client = undefined;
    try { await c.stop(); } catch (err) {
        outputChannel?.appendLine(`[extension] error stopping sumo-lsp: ${err}`);
    }
}

function onRestartServer(context: ExtensionContext): () => Promise<void> {
    return async () => {
        outputChannel?.appendLine('[extension] restart requested');
        await stopClient();
        await startClient(context);
    };
}

// -- Server-binary resolution ------------------------------------------------

function resolveServerBinary(
    config:  ReturnType<typeof workspace.getConfiguration>,
    context: ExtensionContext,
): string {
    const inspection   = config.inspect<string>('server.path');
    const userOverride =
        inspection?.workspaceFolderValue ??
        inspection?.workspaceValue       ??
        inspection?.globalValue;

    if (userOverride && userOverride.length > 0) {
        outputChannel?.appendLine(`[extension] using user-configured server path: ${userOverride}`);
        return resolvePathString(userOverride, context);
    }

    const bundled = bundledServerPath(context);
    if (bundled && fs.existsSync(bundled)) {
        outputChannel?.appendLine(`[extension] using bundled server binary: ${bundled}`);
        return bundled;
    }

    outputChannel?.appendLine('[extension] no bundled server; falling back to `sumo-lsp` on PATH');
    return 'sumo-lsp';
}

function bundledServerPath(context: ExtensionContext): string | undefined {
    if (!context.extensionPath) { return undefined; }
    const name = process.platform === 'win32' ? 'sumo-lsp.exe' : 'sumo-lsp';
    return path.join(context.extensionPath, 'server', name);
}

function resolvePathString(serverPath: string, context: ExtensionContext): string {
    if (path.isAbsolute(serverPath)) { return serverPath; }
    if (serverPath.startsWith('~/') || serverPath === '~') {
        const home = process.env.HOME ?? process.env.USERPROFILE ?? '';
        return path.join(home, serverPath.slice(2));
    }
    if (!serverPath.includes(path.sep) && !serverPath.includes('/')) {
        return serverPath;
    }
    const folders = workspace.workspaceFolders;
    if (folders && folders.length > 0) {
        return path.resolve(folders[0].uri.fsPath, serverPath);
    }
    return path.resolve(context.extensionPath, serverPath);
}

// -- Config.xml --------------------------------------------------------------

function reloadConfig(): void {
    const setting  = workspace.getConfiguration('sumo').get<string>('configPath');
    const resolved = resolveConfigPath(setting && setting.length > 0 ? setting : undefined);
    if (!resolved) {
        outputChannel?.appendLine(
            '[extension] no config.xml found (set `sumo.configPath` or `$SIGMA_HOME`)');
        parsedConfig = null;
        return;
    }
    try {
        parsedConfig = parseConfigXml(resolved);
        outputChannel?.appendLine(
            `[extension] loaded config.xml from ${resolved} (${parsedConfig.kbs.length} KBs declared)`);
    } catch (err) {
        outputChannel?.appendLine(`[extension] failed to parse config.xml ${resolved}: ${err}`);
        parsedConfig = null;
    }
}

async function onReloadConfig(): Promise<void> {
    reloadConfig();
    window.showInformationMessage(
        parsedConfig
            ? `Re-read config.xml (${parsedConfig.kbs.length} KBs declared)`
            : `config.xml not found`,
    );
}

// -- Server-side state sync --------------------------------------------------

async function pushActiveFilesToServer(): Promise<void> {
    if (!client) { return; }
    const active = state.get();
    const files  = active ? Array.from(active.files).sort() : [];
    try {
        await client.sendNotification('sumo/setActiveFiles', { files });
    } catch (err) {
        outputChannel?.appendLine(`[extension] setActiveFiles failed: ${err}`);
    }
    // A KB mutation invalidates any prior debug run — contradictions
    // discovered against the old file set can become phantom
    // against the new one.  Clearing here keeps the Problems panel
    // honest without requiring the user to re-run debug.
    debugDiagnostics?.clear();
}

// -- Kernel (ask/tell) lifecycle ---------------------------------------------

/**
 * Lazy-construct the kernel client.  Spawning is deferred until
 * `ensureRunning()` is called inside the first `tell` / `ask`;
 * constructing the client up-front costs nothing (no IPC, no
 * subprocess).
 */
function ensureKernel(context: ExtensionContext): SumoKernelClient {
    if (kernelClient) { return kernelClient; }
    kernelClient = new SumoKernelClient(
        context,
        outputChannel!,
        () => resolveKernelSpawn(context),
    );
    return kernelClient;
}

/**
 * Decide what binary to run, what database to open, and what KIF
 * files to pre-load.
 *
 * Binary: `sumo.kernel.path` override → `sigmakee` on PATH.  The
 * extension doesn't bundle the kernel binary — users install it
 * separately via a prebuilt release or a source build.  The
 * activation-time health check (`refreshKernelHealth`) catches a
 * missing / wrong-feature-set binary before commands that would
 * hit this resolver are invokable.
 *
 * Database: unless `sumo.kernel.disableCache` is on, the LMDB path
 * is auto-derived from the active KB (see `kernelDb.ts`).  Config
 * KBs get a stable per-user LMDB under global storage that persists
 * across sessions; temporary KBs get a per-session LMDB that's
 * deleted on close.  An explicit `sumo.kernel.dbPath` still
 * overrides -- user intent wins.
 *
 * Pre-load: every `-f <path>` for the active KB's file set.  The
 * kernel reconciles each against the DB on boot, so repeat spawns
 * on an unchanged KB are near-instant (no-op reconciles detect
 * unchanged content).
 */
function resolveKernelSpawn(context: ExtensionContext): { command: string; args: string[] } {
    const config = workspace.getConfiguration('sumo');
    const override = config.get<string>('kernel.path', '').trim();

    let command: string;
    if (override.length > 0) {
        command = resolvePathString(override, context);
    } else {
        // No override → rely on PATH.  `refreshKernelHealth` has
        // already verified the binary is findable (or blocked the
        // command that got us here).  If this resolver is somehow
        // reached with no binary available, spawn will fail and
        // the client surfaces the error.
        command = 'sigmakee';
    }

    const args: string[] = ['serve'];

    // -- Database argument -----------------------------------------------
    const disableCache = config.get<boolean>('kernel.disableCache', false);
    const userDbPath   = config.get<string>('kernel.dbPath', '').trim();

    if (disableCache) {
        args.push('--no-db');
    } else if (userDbPath.length > 0) {
        // User override wins over auto-derivation.
        args.push('--db', userDbPath);
    } else {
        const resolved = resolveKernelDbPath(context, state.get());
        if (resolved) {
            args.push('--db', resolved.lmdb);
            outputChannel?.appendLine(
                `[extension] kernel DB (${resolved.kind}): ${resolved.lmdb}`);
        } else {
            // No active KB → no derivable path.  Fall back to
            // in-memory so the kernel at least starts.
            args.push('--no-db');
            outputChannel?.appendLine(
                '[extension] no active KB; kernel starting in --no-db mode');
        }
    }

    // Optional Vampire override.
    const vampire = config.get<string>('kernel.vampirePath', '').trim();
    if (vampire.length > 0) {
        args.push('--vampire', vampire);
    }
    // Pre-load the active KB's files.  The kernel reconciles each
    // against the DB at boot, so a first spawn populates and later
    // spawns are near-instant no-ops.  The `-f` flag is repeatable.
    const active = state.get();
    if (active) {
        for (const file of Array.from(active.files).sort()) {
            args.push('-f', file);
        }
    }
    return { command, args };
}

// -- Kernel health check -----------------------------------------------------

/**
 * Run the kernel health probe and update both the cached
 * `kernelHealth` struct and the `sumo.kernelAvailable` context
 * variable.  On any non-`ok` result, surface a status-appropriate
 * error dialog pointing the user at the fix.
 *
 * Safe to call repeatedly — each invocation replaces the previous
 * result.  Throttling isn't needed because the check is bounded
 * (5 s timeout + `--help` probe); a user who changes
 * `sumo.kernel.path` in quick succession can hit the probe a few
 * times per second and it won't pile up.
 */
async function refreshKernelHealth(): Promise<void> {
    const config = workspace.getConfiguration('sumo');
    outputChannel?.appendLine('[extension] running kernel health check');
    const health = await checkKernelHealth(config, outputChannel!);
    kernelHealth = health;

    // Flip the palette gate.  VSCode reads context variables
    // synchronously when evaluating `when` clauses, so the command
    // palette picks up the new value on the next menu open.
    await commands.executeCommand(
        'setContext',
        'sumo.kernelAvailable',
        health.status === 'ok',
    );

    if (health.status === 'ok') {
        outputChannel?.appendLine(`[extension] kernel available at ${health.binaryPath}`);
        return;
    }

    // Dispatch by failure tier so each dialog offers the right
    // action.  The dialog is fire-and-forget — we don't await the
    // user's choice (other than the button callbacks); activation
    // continues regardless.
    presentKernelHealthDialog(health);
}

/**
 * Show a status-appropriate error dialog for a non-`ok` health
 * result.  Each dialog offers one primary action + a passive
 * "Show Output" option that reveals the output channel where the
 * full diagnostic lives.
 */
function presentKernelHealthDialog(health: KernelHealth): void {
    if (health.status === 'ok') { return; }   // shouldn't happen; defensive

    switch (health.status) {
        case 'missing': {
            void window.showErrorMessage(
                `SUMO kernel binary not found.  ${health.reason}`,
                'Download…',
                'Open Settings',
                'Show Output',
            ).then(choice => {
                if (choice === 'Download…') {
                    void commands.executeCommand('vscode.open', Uri.parse(RELEASES_URL));
                } else if (choice === 'Open Settings') {
                    void commands.executeCommand('workbench.action.openSettings', 'sumo.kernel.path');
                } else if (choice === 'Show Output') {
                    outputChannel?.show(true);
                }
            });
            return;
        }

        case 'no-serve': {
            void window.showErrorMessage(
                `SUMO kernel is missing the RPC server feature.  ${health.reason}`,
                'Download…',
                'Show Output',
            ).then(choice => {
                if (choice === 'Download…') {
                    void commands.executeCommand('vscode.open', Uri.parse(RELEASES_URL));
                } else if (choice === 'Show Output') {
                    outputChannel?.show(true);
                }
            });
            return;
        }

        case 'crash': {
            // Dump the full stderr/OS-error into the output channel
            // so the user sees it even if the modal's truncated
            // summary buries the real cause.
            outputChannel?.appendLine(`[extension] kernel start failed: ${health.detail}`);
            void window.showErrorMessage(
                `SUMO kernel failed to start at ${health.binaryPath}: ${health.reason}\n\n${health.detail}`,
                { modal: false },
                'Show Output',
            ).then(choice => {
                if (choice === 'Show Output') {
                    outputChannel?.show(true);
                }
            });
            return;
        }
    }
}

async function onAskCursor(context: ExtensionContext): Promise<void> {
    const editor = window.activeTextEditor;
    if (!editor) {
        window.showInformationMessage(
            'Open a .kif file and select (or place the cursor inside) a conjecture first.');
        return;
    }

    // Prefer an explicit selection; fall back to the KIF
    // top-level form that encloses the cursor.
    const query = editor.selection.isEmpty
        ? enclosingTopLevelForm(editor.document.getText(), editor.document.offsetAt(editor.selection.active))
        : editor.document.getText(editor.selection);

    if (!query || !query.trim()) {
        window.showInformationMessage(
            'Could not find a KIF sentence under the cursor.  Select a conjecture and retry.');
        return;
    }

    const kernel = ensureKernel(context);
    try {
        const result = await window.withProgress(
            {
                location:    { viewId: 'sumoKnowledgeBases' },
                title:       'SUMO: asking…',
                cancellable: false,
            },
            () => kernel.ask(query),
        );
        showAskResult(query, result);
    } catch (err) {
        outputChannel?.appendLine(`[extension] ask failed: ${err}`);
        window.showErrorMessage(`Ask failed: ${String(err)}`);
    }
}

/**
 * Wrapper for the `sumo.debug.file` command.  Ensures the kernel
 * is spawned + the shared `debugDiagnostics` collection is
 * present, then delegates to `onDebugFile`.  Degrades gracefully
 * when activation didn't create the collection (shouldn't happen
 * in practice — `activate()` always does — but keeps the handler
 * resilient if the order ever changes).
 */
async function onDebugActiveFile(context: ExtensionContext): Promise<void> {
    if (!debugDiagnostics) {
        window.showErrorMessage(
            'SUMO: debug diagnostics not initialised.  Try reloading the window.');
        return;
    }
    const kernel = ensureKernel(context);
    await onDebugFile(context, kernel, debugDiagnostics);
}

async function onRestartKernel(context: ExtensionContext): Promise<void> {
    if (kernelClient) {
        try { await kernelClient.stop(); } catch { /* ignore */ }
    }
    kernelClient = undefined;
    // Eagerly respawn so the user sees activity in the output
    // channel immediately (otherwise the next ask triggers spawn).
    const kernel = ensureKernel(context);
    try {
        await kernel.ensureRunning();
        window.showInformationMessage('SUMO kernel restarted.');
    } catch (err) {
        window.showErrorMessage(`Kernel restart failed: ${String(err)}`);
    }
}

/**
 * "SUMO: Rebuild Kernel Database" -- wipe every persisted file
 * from the LMDB and re-reconcile the active KB's file set from
 * scratch.  Useful after external changes (git history rewrite,
 * large refactor, suspected cache corruption) where the user
 * wants a guaranteed clean state.
 */
async function onRebuildKernelDb(context: ExtensionContext): Promise<void> {
    const active = state.get();
    if (!active) {
        window.showInformationMessage('No active KB to rebuild.');
        return;
    }
    const kernel = ensureKernel(context);
    try {
        await window.withProgress(
            {
                location:    { viewId: 'sumoKnowledgeBases' },
                title:       'SUMO: rebuilding kernel database…',
                cancellable: false,
            },
            async () => {
                await kernel.ensureRunning();
                await kernel.flush();
                // Re-populate by reconciling every active-KB file.
                const files = Array.from(active.files).sort();
                for (const file of files) {
                    try {
                        await kernel.reconcileFile(file);
                    } catch (err) {
                        outputChannel?.appendLine(
                            `[kernel] reconcile '${file}' failed during rebuild: ${err}`);
                    }
                }
            },
        );
        window.showInformationMessage(
            `SUMO kernel database rebuilt (${active.files.size} file(s) reconciled).`);
    } catch (err) {
        outputChannel?.appendLine(`[extension] rebuild failed: ${err}`);
        window.showErrorMessage(`Rebuild failed: ${String(err)}`);
    }
}

/**
 * "SUMO: Delete Kernel Database" -- stop the kernel, delete the
 * LMDB directory for the active KB, and leave the kernel stopped.
 * Next ask/tell respawns against an empty DB.  For ephemeral KBs
 * this is effectively the same as closing the KB; for config KBs
 * it's destructive (wipes the shared persistent cache).
 */
async function onDeleteKernelDb(context: ExtensionContext): Promise<void> {
    const active = state.get();
    if (!active) {
        window.showInformationMessage('No active KB.');
        return;
    }
    const confirm = await window.showWarningMessage(
        active.source === 'config'
            ? `Delete the persistent kernel database for "${active.name}"?  The next ask will rebuild it from source files.`
            : `Delete the temporary kernel database for "${active.name}"?`,
        { modal: true },
        'Delete',
    );
    if (confirm !== 'Delete') { return; }

    // Stop the kernel so the LMDB fd is closed before we `rmSync`.
    if (kernelClient) {
        try { await kernelClient.stop(); } catch { /* ignore */ }
    }
    kernelClient = undefined;

    let deleted: boolean;
    if (active.source === 'config') {
        deleted = deletePersistentDb(context, active, outputChannel!);
    } else {
        deleteEphemeralDb(context, active, outputChannel!);
        deleted = true;
    }
    if (deleted) {
        window.showInformationMessage('SUMO kernel database deleted.');
    } else {
        window.showWarningMessage('No kernel database was found to delete.');
    }
}

/**
 * "SUMO: Reconcile Open Files" -- reconcile every open `.kif`
 * editor in the workspace into the kernel.  Useful after external
 * changes (git pull, rebase) where the file watcher missed the
 * update (e.g. because VSCode wasn't running at the time).
 */
async function onReconcileOpenFiles(context: ExtensionContext): Promise<void> {
    const openFiles = workspace.textDocuments
        .filter(d => d.languageId === 'kif' && d.uri.scheme === 'file')
        .map(d => d.uri.fsPath);
    if (openFiles.length === 0) {
        window.showInformationMessage('No open .kif files to reconcile.');
        return;
    }
    const kernel = ensureKernel(context);
    try {
        await window.withProgress(
            {
                location:    { viewId: 'sumoKnowledgeBases' },
                title:       `SUMO: reconciling ${openFiles.length} file(s)…`,
                cancellable: false,
            },
            async () => {
                await kernel.ensureRunning();
                for (const file of openFiles) {
                    try {
                        await kernel.reconcileFile(file);
                    } catch (err) {
                        outputChannel?.appendLine(
                            `[kernel] reconcile '${file}' failed: ${err}`);
                    }
                }
            },
        );
        window.showInformationMessage(`Reconciled ${openFiles.length} file(s).`);
    } catch (err) {
        window.showErrorMessage(`Reconcile failed: ${String(err)}`);
    }
}

/**
 * "SUMO: Show Kernel File Status" -- dump `kb.listFiles` into the
 * output channel so the user can see what the kernel believes it
 * has loaded.  Debugging aid when the extension's view of the KB
 * seems to have drifted from the kernel's.
 */
async function onShowKernelFileStatus(context: ExtensionContext): Promise<void> {
    const kernel = ensureKernel(context);
    try {
        await kernel.ensureRunning();
        const { files } = await kernel.listFiles();
        outputChannel?.show(true);
        outputChannel?.appendLine(`[kernel] listFiles: ${files.length} file(s) loaded`);
        for (const f of files) {
            outputChannel?.appendLine(`[kernel]   ${f.sentenceCount.toString().padStart(6)} sentences  ${f.path}`);
        }
    } catch (err) {
        window.showErrorMessage(`listFiles failed: ${String(err)}`);
    }
}

/**
 * Stop the kernel and delete the ephemeral LMDB if the active KB
 * is a temporary one.  No-op for config KBs (they persist) or
 * when no KB is active.  Safe to call repeatedly.
 *
 * `reason` is logged so the output channel shows what triggered
 * the cleanup (KB close vs. switch vs. deactivate).
 */
async function disposeActiveIfTemp(reason: string): Promise<void> {
    const active = state?.get() ?? null;
    if (!active || active.source !== 'temporary') { return; }
    if (!extensionContext || !outputChannel) { return; }
    outputChannel.appendLine(
        `[extension] disposing ephemeral KB (${reason})`);
    if (kernelClient) {
        try { await kernelClient.stop(); } catch { /* ignore */ }
    }
    kernelClient = undefined;
    deleteEphemeralDb(extensionContext, active, outputChannel);
}

// -- File watcher handlers ---------------------------------------------------

/**
 * Fired when a `.kif` file is saved or created under the workspace.
 * Only reconciles files that belong to the active KB -- out-of-KB
 * edits are noise.  Fire-and-forget: errors land in the output
 * channel, never as toasts (too spammy on a long editing session).
 */
function onFileSaved(context: ExtensionContext, filePath: string): void {
    if (!kernelClient || !kernelClient.isRunning()) { return; }
    const active = state.get();
    const abs    = normaliseAbs(filePath);
    if (!active || !active.files.has(abs)) { return; }

    kernelClient.reconcileFile(abs).then(
        r => {
            if (r.added > 0 || r.removed > 0) {
                outputChannel?.appendLine(
                    `[kernel] reconciled ${abs}: +${r.added} -${r.removed}`);
            }
            if (r.parseErrors.length > 0) {
                outputChannel?.appendLine(
                    `[kernel] ${abs} parse error(s); delta not persisted`);
            }
        },
        err => {
            outputChannel?.appendLine(`[kernel] reconcile ${abs} failed: ${err}`);
        },
    );
}

/**
 * Fired when a `.kif` file under the workspace is deleted.  If it
 * was part of the active KB, drop it from the kernel too.
 */
function onFileDeleted(context: ExtensionContext, filePath: string): void {
    if (!kernelClient || !kernelClient.isRunning()) { return; }
    const abs = normaliseAbs(filePath);
    kernelClient.removeFile(abs).then(
        r => {
            if (r.removed > 0) {
                outputChannel?.appendLine(
                    `[kernel] removed ${abs} (${r.removed} sentences)`);
            }
        },
        err => {
            outputChannel?.appendLine(`[kernel] removeFile ${abs} failed: ${err}`);
        },
    );
}

/**
 * Return the top-level KIF form whose byte range contains `offset`.
 *
 * KIF's grammar is S-expressions, so "top-level" means the
 * outermost parenthesised form.  We do a dumb paren-matching scan
 * from `offset` backwards to find the opening `(`, then forwards
 * to its matching `)`.  Strings (`"..."`) and line comments (`;`
 * to newline) are respected so a paren inside a string doesn't
 * break the match.
 *
 * Returns `undefined` when the cursor isn't inside any balanced
 * form (e.g. at top-level between sentences).
 */
function enclosingTopLevelForm(text: string, offset: number): string | undefined {
    // Scan *forward* from the start of the file, tracking paren
    // depth.  Every time depth goes 0 → 1, record the position.
    // When depth goes 1 → 0, check whether `offset` was inside the
    // range we just closed; if so, that's our form.
    let depth   = 0;
    let start   = -1;
    let inStr   = false;
    let inCmt   = false;
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
                    return text.slice(start, end);
                }
                start = -1;
            }
        }
    }
    return undefined;
}

// -- Server-side state sync (LSP) --------------------------------------------

/**
 * Send the current `sumo.diagnostics.ignoredCodes` list to the
 * server via the `sumo/setIgnoredDiagnostics` notification.  The
 * server replaces its ignore-set and re-publishes diagnostics
 * for every open document, so the change is reflected in the
 * Problems panel immediately.
 *
 * Called on activation, after every restart, and whenever the
 * setting changes.  Safe to call when the server is down --
 * we just skip the send.
 */
async function pushIgnoredDiagnosticsToServer(): Promise<void> {
    if (!client) { return; }
    const codes = workspace.getConfiguration('sumo')
        .get<string[]>('diagnostics.ignoredCodes', []);
    try {
        await client.sendNotification('sumo/setIgnoredDiagnostics', { codes });
        outputChannel?.appendLine(
            `[extension] setIgnoredDiagnostics: ${codes.length} code(s) ignored`);
    } catch (err) {
        outputChannel?.appendLine(`[extension] setIgnoredDiagnostics failed: ${err}`);
    }
}

/**
 * Apply the user's `sumo.trace.server` setting to the running
 * client.  `vscode-languageclient` auto-wires tracing from a key
 * derived from the client ID (`sumo-lsp.trace.server`), which
 * mismatches our user-facing `sumo.*` namespace -- so we read the
 * config ourselves and call `client.setTrace` to keep the key in
 * our own namespace.
 */
async function applyTraceLevel(): Promise<void> {
    if (!client) { return; }
    const level = workspace.getConfiguration('sumo')
        .get<string>('trace.server', 'off').toLowerCase();
    const trace = level === 'verbose'  ? Trace.Verbose
                : level === 'messages' ? Trace.Messages
                : level === 'compact'  ? Trace.Compact
                :                        Trace.Off;
    try {
        await client.setTrace(trace);
        outputChannel?.appendLine(`[extension] trace level: ${level}`);
    } catch (err) {
        outputChannel?.appendLine(`[extension] setTrace failed: ${err}`);
    }
}

// -- didOpen classification --------------------------------------------------

const inflightPrompts = new Map<string, Promise<void>>();

async function classifyOpenedKif(doc: TextDocument): Promise<void> {
    if (doc.uri.scheme !== 'file') { return; }
    const file = doc.uri.fsPath;
    // `.kif.tq` files are test-query sidecars, not KB constituents;
    // skip the membership prompt so opening one doesn't force a
    // KB decision on the user.  They'll still be syntax-highlighted
    // and linted (parse diagnostics are KB-independent) -- only the
    // auto-classification flow is suppressed.
    if (file.endsWith('.kif.tq')) { return; }
    const norm = normaliseAbs(file);
    if (inflightPrompts.has(norm)) { return; }
    const p = classifyOpenedKifInner(norm);
    inflightPrompts.set(norm, p);
    try { await p; } finally { inflightPrompts.delete(norm); }
}

async function classifyOpenedKifInner(file: string): Promise<void> {
    const outcome = await promptForKbMembership(state, parsedConfig, file);
    await applyPromptOutcome(outcome);
}

async function applyPromptOutcome(outcome: PromptOutcome): Promise<void> {
    switch (outcome.kind) {
        case 'noop':
            return;

        case 'load-config-here': {
            if (!parsedConfig) { return; }
            const kb = parsedConfig.kbs.find(k => k.name === outcome.configKbName);
            if (!kb) { return; }
            // If we're abandoning a temporary KB, the next kernel
            // spawn will target a different LMDB -- free the old
            // one now.
            await disposeActiveIfTemp('switching to config KB');
            state.setConfigKb(kb.name, kb.files);
            outputChannel?.appendLine(`[extension] loaded "${kb.name}" (${kb.files.length} files) into this window`);
            break;
        }

        case 'new-temp-here': {
            // If the outgoing active KB is also temporary, its
            // LMDB becomes garbage the moment we overwrite the
            // session id.  Delete first, then `openTemp`.
            await disposeActiveIfTemp('replacing with new temp KB');
            state.openTemp(outcome.file);
            outputChannel?.appendLine(`[extension] started temporary KB with ${outcome.file}`);
            break;
        }

        case 'add-to-active': {
            if (!state.addFile(outcome.file)) { return; }
            const active = state.get();
            outputChannel?.appendLine(`[extension] added ${outcome.file} to "${active?.name ?? '?'}"`);
            if (active?.source === 'config' && active.configKbName && parsedConfig) {
                await writeConfigKb(parsedConfig, active.configKbName, Array.from(active.files));
            }
            break;
        }

        case 'open-config-in-new-window': {
            await openKbInNewWindow(outcome.configKbName);
            // The current window stays on its current KB.
            return;
        }

        case 'temp-in-new-window': {
            await openTempInNewWindow(outcome.file);
            return;
        }
    }

    treeProvider.refresh();
    refreshStatusBar();
    await pushActiveFilesToServer();
}

// -- New-window spawning -----------------------------------------------------

/**
 * Launch a new VSCode window pre-configured for `kbName`.  We
 * synthesise a `.code-workspace` file under the extension's
 * global-storage dir: its `folders` root is the KB's `kbDir`
 * (so the Explorer shows the constituents), and its `settings`
 * pre-pin `sumo.activeKb = <kbName>` so the child window's
 * extension bootstrap installs that KB automatically.
 */
async function openKbInNewWindow(kbName: string): Promise<void> {
    if (!parsedConfig) {
        window.showErrorMessage('config.xml not loaded; cannot open KB in new window.');
        return;
    }
    const kb = parsedConfig.kbs.find(k => k.name === kbName);
    if (!kb) {
        window.showErrorMessage(`KB "${kbName}" not found in config.xml`);
        return;
    }

    // Propagate the resolved config path into the child workspace.
    // Without this, a child window that inherits only user-global
    // settings may fail to locate config.xml -- the KB bootstrap
    // then silently aborts and the tree view stays empty.  We use
    // the resolved absolute path so the child doesn't have to
    // re-run the $SIGMA_HOME / fallback chain.
    const wsPath = ensureWorkspaceFile(kb.name, [parsedConfig.kbDir], {
        'sumo.activeKb':   kb.name,
        'sumo.configPath': parsedConfig.configPath,
    });

    outputChannel?.appendLine(`[extension] opening "${kb.name}" in new window via ${wsPath}`);
    await commands.executeCommand(
        'vscode.openFolder',
        Uri.file(wsPath),
        { forceNewWindow: true },
    );
}

/**
 * Launch a new VSCode window with a temporary KB seeded by a
 * single file.  The new window's extension bootstrap will
 * classify the file normally; the workspace settings don't
 * pre-pin a config KB, so the user lands on the "no active KB"
 * prompt flow scoped to that single file.
 */
async function openTempInNewWindow(file: string): Promise<void> {
    // Use the file's containing dir as the workspace root so VSCode
    // has something to display in the Explorer.
    const root = path.dirname(file);
    const wsPath = ensureWorkspaceFile(`temp-${path.basename(file)}`, [root], {});

    outputChannel?.appendLine(`[extension] opening ${file} in new temp window via ${wsPath}`);
    await commands.executeCommand(
        'vscode.openFolder',
        Uri.file(wsPath),
        { forceNewWindow: true },
    );
}

/**
 * Write a `.code-workspace` file describing the given folders
 * and settings overrides, returning the absolute path.  Files
 * are placed under `$HOME/.sumo-vscode/workspaces/` keyed by a
 * hash of the (folders, settings) pair so repeat invocations
 * reuse the same on-disk file (and VSCode won't spawn a fresh
 * window if one is already open).
 */
function ensureWorkspaceFile(
    tag:      string,
    folders:  string[],
    settings: Record<string, unknown>,
): string {
    const dir = path.join(os.homedir(), '.sumo-vscode', 'workspaces');
    fs.mkdirSync(dir, { recursive: true });
    const safeTag = tag.replace(/[^A-Za-z0-9_-]+/g, '_').slice(0, 80);
    const file    = path.join(dir, `${safeTag}.code-workspace`);
    const body    = {
        folders:  folders.map(f => ({ path: f })),
        settings,
    };
    fs.writeFileSync(file, JSON.stringify(body, null, 2));
    return file;
}

// -- Commands ---------------------------------------------------------------

interface KbPick extends QuickPickItem { kbName?: string; temp?: boolean; newWindow?: boolean }

async function onLoadKb(): Promise<void> {
    if (!parsedConfig || parsedConfig.kbs.length === 0) {
        window.showInformationMessage(
            'No KBs declared in config.xml.  Set `sumo.configPath` or populate $SIGMA_HOME.',
        );
        return;
    }
    const picks: KbPick[] = parsedConfig.kbs.flatMap(kb => [
        {
            label:       `$(library) ${kb.name}`,
            description: `${kb.files.length} files · load into this window`,
            kbName:      kb.name,
        },
        {
            label:       `$(multiple-windows) ${kb.name} (new window)`,
            description: `${kb.files.length} files · open in a fresh VSCode window`,
            kbName:      kb.name,
            newWindow:   true,
        },
    ]);
    picks.push({
        label:       '$(file-submodule) New temporary KB (empty)',
        description: 'This window, no config.xml backing',
        temp:        true,
    });

    const chosen = await window.showQuickPick(picks, {
        title: 'Load SUMO Knowledge Base',
    });
    if (!chosen) { return; }

    if (chosen.newWindow && chosen.kbName) {
        await openKbInNewWindow(chosen.kbName);
        return;
    }
    // The user is replacing the active KB; if the outgoing one
    // is temporary, its ephemeral LMDB becomes garbage.
    await disposeActiveIfTemp('switching KB via quick pick');
    if (chosen.temp) {
        state.openTemp();
        outputChannel?.appendLine('[extension] opened empty temporary KB');
    } else if (chosen.kbName) {
        const kb = parsedConfig.kbs.find(k => k.name === chosen.kbName);
        if (kb) {
            state.setConfigKb(kb.name, kb.files);
            outputChannel?.appendLine(`[extension] loaded "${kb.name}" (${kb.files.length} files)`);
        }
    }

    treeProvider.refresh();
    refreshStatusBar();
    await pushActiveFilesToServer();
}

async function onOpenInNewWindow(): Promise<void> {
    if (!parsedConfig || parsedConfig.kbs.length === 0) {
        window.showInformationMessage(
            'No KBs declared in config.xml; nothing to open in a new window.');
        return;
    }
    const picks: KbPick[] = parsedConfig.kbs.map(kb => ({
        label:       `$(library) ${kb.name}`,
        description: `${kb.files.length} files`,
        kbName:      kb.name,
    }));
    const chosen = await window.showQuickPick(picks, {
        title: 'Open Knowledge Base in New Window',
    });
    if (!chosen?.kbName) { return; }
    await openKbInNewWindow(chosen.kbName);
}

async function onAddFile(): Promise<void> {
    const active = state.get();
    if (!active) {
        window.showInformationMessage('No KB is active in this window.  Run "SUMO: Load KB…" first.');
        return;
    }
    const picked = await window.showOpenDialog({
        canSelectFiles:   true,
        canSelectFolders: false,
        canSelectMany:    false,
        filters: { 'KIF files': ['kif', 'kif.tq'] },
    });
    if (!picked || picked.length === 0) { return; }
    const file = picked[0].fsPath;

    state.addFile(file);
    outputChannel?.appendLine(`[extension] added ${file} to "${active.name}"`);

    if (active.source === 'config' && active.configKbName && parsedConfig) {
        await writeConfigKb(parsedConfig, active.configKbName, Array.from(active.files));
    }

    treeProvider.refresh();
    refreshStatusBar();
    await pushActiveFilesToServer();
}

async function onRemoveFile(...args: unknown[]): Promise<void> {
    const active = state.get();
    if (!active) { return; }

    // Context-menu right-click on a tree file node hands us the node.
    let file = extractFileFromArgs(args);
    if (!file) {
        const files = Array.from(active.files).sort();
        if (files.length === 0) {
            window.showInformationMessage(`"${active.name}" has no files.`);
            return;
        }
        const picked = await window.showQuickPick(files, {
            title: `Remove file from "${active.name}"`,
        });
        if (!picked) { return; }
        file = picked;
    }

    state.removeFile(file);
    outputChannel?.appendLine(`[extension] removed ${file} from "${active.name}"`);

    if (active.source === 'config' && active.configKbName && parsedConfig) {
        await writeConfigKb(parsedConfig, active.configKbName, Array.from(active.files));
    }

    treeProvider.refresh();
    refreshStatusBar();
    await pushActiveFilesToServer();
}

async function onCloseKb(context: ExtensionContext): Promise<void> {
    const active = state.get();
    if (!active) { return; }
    // Stop the kernel before deleting the ephemeral LMDB so its
    // fd is closed (LMDB on macOS / Linux survives fd-open delete
    // but it's brittle and wastes space until process exit).
    if (kernelClient) {
        try { await kernelClient.stop(); } catch { /* ignore */ }
    }
    kernelClient = undefined;
    // Ephemeral KBs own their own LMDB; clean it up immediately.
    // Config KBs persist -- their LMDB is reused on next open.
    if (active.source === 'temporary' && outputChannel) {
        deleteEphemeralDb(context, active, outputChannel);
    }
    state.clear();
    outputChannel?.appendLine(`[extension] closed "${active.name}"`);
    treeProvider.refresh();
    refreshStatusBar();
    await pushActiveFilesToServer();
}

function extractFileFromArgs(args: unknown[]): string | null {
    if (args.length === 0) { return null; }
    const first = args[0] as any;
    if (first && first.kind === 'file' && typeof first.file === 'string') {
        return first.file as string;
    }
    return null;
}

// -- Config-writer plumbing --------------------------------------------------

async function writeConfigKb(
    cfg:      ParsedConfig,
    kbName:   string,
    files:    string[],
): Promise<void> {
    const rels = files.map(f => relativiseToKbDir(f, cfg.kbDir));
    try {
        writeKbConstituents(cfg.configPath, kbName, rels);
        outputChannel?.appendLine(
            `[extension] wrote ${rels.length} constituents of "${kbName}" to ${cfg.configPath}`);
        reloadConfig();
    } catch (err) {
        outputChannel?.appendLine(`[extension] config.xml write failed: ${err}`);
        window.showErrorMessage(`Failed to write config.xml: ${err}`);
    }
}

// -- Status bar --------------------------------------------------------------

function refreshStatusBar(): void {
    const active = state.get();
    if (!active) {
        statusBarItem.text    = '$(library) SUMO: no KB';
        statusBarItem.tooltip = 'No knowledge base is active in this window.  ' +
                                'Open a .kif file or run "SUMO: Load KB…".';
    } else {
        const n = active.files.size;
        statusBarItem.text    = `$(library) ${active.name} · ${n}f`;
        statusBarItem.tooltip = `${active.source === 'config' ? 'Config' : 'Temporary'} KB · ` +
                                `${n} file${n === 1 ? '' : 's'}\n` +
                                `This window edits exactly one KB.  Use ` +
                                `"SUMO: Open KB in New Window" for another.`;
    }
    statusBarItem.show();
}

// -- Create KB + Generate TPTP wrappers -------------------------------------

async function onCreateKb(): Promise<void> {
    await createKbCommand(
        state,
        () => parsedConfig,
        () => reloadConfig(),
        async () => {
            treeProvider.refresh();
            refreshStatusBar();
            await pushActiveFilesToServer();
        },
    );
}

async function onGenerateTptp(context: ExtensionContext): Promise<void> {
    const active = state.get();
    if (!active) {
        window.showInformationMessage('No active knowledge base.  Load one first.');
        return;
    }
    const kernel = ensureKernel(context);
    await generateTptpCommand(kernel, () => state.get());
}

// Unused import silencer (ActiveKb is re-exported for the tests
// suite but not referenced inline from extension.ts yet).
void (null as unknown as ActiveKb);
