// src/kernelClient.ts
//
// Lazy-spawned JSON-RPC client for the `sumo-kernel` daemon
// (`sumo serve` subcommand of the native binary).
//
// Design notes
//
// Two backends live alongside this extension: `sumo-lsp` (static
// analysis) and `sumo-kernel` (ask/tell/prove).  The LSP is spawned
// eagerly on activation because its features (hover, goto, ...) are
// what the user sees most often.  The kernel is spawned lazily --
// on the first ask/tell -- because it's slower to start and most
// sessions never need it.  This matches the VSCode-Python extension
// pattern where Pylance starts immediately but the interpreter /
// Jupyter kernel only starts when you "Run Selection" or open a
// notebook.
//
// Wire format is newline-delimited JSON (ndjson): one JSON object
// per line on stdin/stdout, UTF-8.  Each request carries an `id`;
// the response echoes the same `id`.  Notifications (no reply
// expected) omit `id`.  `$/cancelRequest`-style cancellation isn't
// in the MVP -- the kernel runs each ask synchronously and we
// surface progress via VSCode's withProgress UX client-side.

import { ChildProcess, spawn } from 'child_process';
import {
    ExtensionContext,
    OutputChannel,
    window,
} from 'vscode';

// -- Wire types (must match `crates/native/src/cli/serve.rs`) ----------------

export interface TellResult {
    ok: boolean;
    errors:   string[];
    warnings: string[];
}

export interface AskResult {
    /** One of `Proved`, `Disproved`, `Consistent`, `Inconsistent`, `Timeout`, `Unknown`. */
    status:    string;
    bindings:  string[];
    /** Plain-text KIF per proof step (no ANSI escapes). */
    proofKif:  string[];
    /** Full Vampire transcript, unparsed -- for the "raw output" tab. */
    raw:       string;
}

export interface ReconcileFileResult {
    path:           string;
    added:          number;
    removed:        number;
    retained:       number;
    revalidated:    number;
    parseErrors:    string[];
    semanticErrors: string[];
    /** True iff the delta was committed to LMDB (false on parse error / --no-db). */
    persisted:      boolean;
}

export interface RemoveFileResult {
    removed:   number;
    persisted: boolean;
}

export interface FlushResult {
    filesRemoved:     number;
    sentencesRemoved: number;
    persisted:        boolean;
}

export interface ListFilesResult {
    files: Array<{ path: string; sentenceCount: number }>;
}

export interface GenerateTptpResult {
    /** UTF-8 TPTP source.  The caller writes this to disk. */
    tptp: string;
    /** Count of top-level formulae emitted. */
    formulaCount: number;
    /** Echo of the resolved dialect.  Present since kernel v1.x. */
    lang?: string;
}

/**
 * One axiom that contributed to a refutation.  Populated only when
 * the kernel returns `status === "Inconsistent"` AND Vampire emitted
 * a traceable proof transcript.
 */
export interface ContradictionEntry {
    sid:  number;
    file: string;
    /** 1-based line number in `file`. */
    line: number;
    kif:  string;
}

/**
 * One step in the proof transcript returned by `debug`.  Same shape
 * as the subprocess `--proof kif` output on the CLI.
 */
export interface DebugProofStepEntry {
    index:       number;
    rule:        string;
    premises:    number[];
    formula:     string;
    sourceSid?:  number;
    sourceFile?: string;
    sourceLine?: number;
}

export interface DebugResult {
    /** Resolved file tag (may differ from the requested `file`
     *  when basename matching resolved to a loaded absolute path). */
    file:           string;
    rootSentences:  number;
    sampled:        number;
    sineExpanded:   number;
    totalChecked:   number;
    tolerance:      number;
    /** Other KB files from which SInE pulled axioms. */
    filesPulled:    string[];
    /** One of `Consistent`, `Inconsistent`, `Timeout`, `Unknown`. */
    status:         string;
    /** Populated only when `status === "Inconsistent"` AND Vampire
     *  emitted a traceable refutation.  Empty otherwise. */
    contradictions: ContradictionEntry[];
    /** Full KIF proof transcript.  Empty when no refutation was
     *  produced. */
    proofKif:       DebugProofStepEntry[];
    /** Raw prover transcript — useful for debugging when the
     *  structured paths above return nothing. */
    raw:            string;
}

interface Pending {
    resolve: (value: unknown) => void;
    reject:  (err: Error) => void;
    /** Loose method name for logs + diagnostics. */
    method:  string;
}

// -- Client -------------------------------------------------------------------

/**
 * Handle on a running `sumo-kernel` child process.
 *
 * Lifecycle: constructed but not spawned; the first `tell` / `ask`
 * calls `ensureRunning()` which spawns the subprocess and keeps it
 * alive until the extension deactivates or `stop()` is called.  If
 * the kernel exits unexpectedly we mark the client as dead and the
 * next call will respawn.
 */
export class SumoKernelClient {
    private child:   ChildProcess | undefined;
    private readBuf: string = '';
    private nextId:  number = 1;
    private pending: Map<number, Pending> = new Map();
    private startupPromise: Promise<void> | undefined;

    constructor(
        private readonly context:       ExtensionContext,
        private readonly output:        OutputChannel,
        /**
         * Factory that yields the resolved kernel binary path
         * and any extra CLI args (e.g. `-f <kif-file>` per
         * active-KB file) at spawn time.  Called once per spawn
         * so a KB change or config update is picked up on
         * restart.
         */
        private readonly resolveSpawn:  () => { command: string; args: string[] },
    ) {}

    /** True iff the kernel process is currently running. */
    isRunning(): boolean {
        return this.child !== undefined && this.child.exitCode === null;
    }

    /**
     * Spawn the kernel if it isn't already running.  Concurrent
     * callers during startup all wait on the same promise, so we
     * never double-spawn on a fast burst of commands.
     */
    async ensureRunning(): Promise<void> {
        if (this.isRunning()) { return; }
        if (this.startupPromise) { return this.startupPromise; }

        this.startupPromise = (async () => {
            const { command, args } = this.resolveSpawn();
            this.output.appendLine(`[kernel] starting: ${command} ${args.join(' ')}`);

            const child = spawn(command, args, {
                stdio: ['pipe', 'pipe', 'pipe'],
                env:   process.env,
            });

            child.on('error', (err) => {
                // Spawn-level failure (ENOENT, EACCES, ...).  We
                // can't recover without user intervention; reject
                // every in-flight request and leave the client dead.
                this.output.appendLine(`[kernel] spawn error: ${err.message}`);
                this.rejectAll(new Error(`kernel failed to start: ${err.message}`));
                this.child = undefined;
            });

            child.on('exit', (code, signal) => {
                this.output.appendLine(
                    `[kernel] exited (code=${code ?? 'null'}, signal=${signal ?? 'null'})`);
                this.rejectAll(new Error(
                    `kernel exited unexpectedly (code=${code}, signal=${signal})`));
                this.child = undefined;
            });

            child.stdout!.setEncoding('utf8');
            child.stdout!.on('data', (chunk: string) => this.onStdout(chunk));

            // Kernel logs (info / warn / error) land on stderr.  The
            // user can inspect them via the "SUMO / KIF" output
            // channel; we only prefix with a marker so they don't
            // blend in with LSP lines.
            child.stderr!.setEncoding('utf8');
            child.stderr!.on('data', (chunk: string) => {
                for (const line of chunk.split('\n')) {
                    if (line.trim().length > 0) {
                        this.output.appendLine(`[kernel:stderr] ${line}`);
                    }
                }
            });

            this.child = child;
        })();

        try {
            await this.startupPromise;
        } finally {
            this.startupPromise = undefined;
        }
    }

    /**
     * Send a `tell` request.  Resolves once the kernel replies.
     */
    async tell(kif: string, session = 'default'): Promise<TellResult> {
        return this.sendRequest<TellResult>('tell', { session, kif });
    }

    /**
     * Send an `ask` request.  Resolves once Vampire completes (or
     * times out on the *kernel's* side -- the default is 30 s).
     * The caller is responsible for wrapping long-running calls in
     * `withProgress` so the UI stays responsive.
     */
    async ask(query: string, opts: { session?: string; timeoutSecs?: number } = {}): Promise<AskResult> {
        return this.sendRequest<AskResult>('ask', {
            session:     opts.session     ?? 'default',
            query,
            timeoutSecs: opts.timeoutSecs ?? 30,
        });
    }

    // -- KB maintenance (LMDB-mutating) --------------------------------------

    /**
     * Sync `path` into the kernel's KB, committing the delta to
     * LMDB when running in `--db` mode.  When `text` is omitted
     * the kernel reads from disk -- this is the save-triggered
     * flow the extension uses (Option A from the plan).
     */
    async reconcileFile(filePath: string, opts: { text?: string } = {}): Promise<ReconcileFileResult> {
        const params: Record<string, unknown> = { path: filePath };
        if (opts.text !== undefined) { params.text = opts.text; }
        return this.sendRequest<ReconcileFileResult>('kb.reconcileFile', params);
    }

    /**
     * Drop `path` from the kernel's KB and (in `--db` mode)
     * delete its sentences from LMDB.
     */
    async removeFile(filePath: string): Promise<RemoveFileResult> {
        return this.sendRequest<RemoveFileResult>('kb.removeFile', { path: filePath });
    }

    /**
     * Wipe every loaded file from the kernel's KB.  In `--db` mode
     * this deletes everything from LMDB too.  Use the
     * "SUMO: Rebuild Kernel Database" command -- it flushes then
     * re-syncs via reconcileFile, producing a clean state.
     */
    async flush(): Promise<FlushResult> {
        return this.sendRequest<FlushResult>('kb.flush', {});
    }

    /** Introspection: which files the kernel currently has loaded. */
    async listFiles(): Promise<ListFilesResult> {
        return this.sendRequest<ListFilesResult>('kb.listFiles', {});
    }

    /**
     * Consistency-check `filePath` against the rest of the loaded
     * KB via SInE + Vampire.  Same flow as `sumo debug` from the CLI.
     *
     * The kernel resolves `file` against loaded KB tags using a
     * three-tier strategy: exact string → canonicalised absolute
     * path → basename suffix.  Passing an absolute path is the
     * recommended form (what the extension uses elsewhere).
     *
     * `thoroughness` ∈ (0.0, 1.0] controls random subsampling;
     * `scope` is the SInE tolerance (default ≈ 2.0).  `timeoutSecs`
     * defaults to 60.
     *
     * Throws on transport failure or on a server-side `-32602`
     * (e.g. file not loaded in the KB).  Successful calls return
     * a `DebugResult` whose `status` field carries the verdict —
     * `"Timeout"` and `"Unknown"` are non-error outcomes the caller
     * surfaces as status dialogs, not error toasts.
     */
    async debug(filePath: string, opts: {
        thoroughness?: number;
        scope?:        number;
        timeoutSecs?:  number;
    } = {}): Promise<DebugResult> {
        const params: Record<string, unknown> = { file: filePath };
        if (opts.thoroughness !== undefined) { params.thoroughness = opts.thoroughness; }
        if (opts.scope        !== undefined) { params.scope        = opts.scope; }
        if (opts.timeoutSecs  !== undefined) { params.timeoutSecs  = opts.timeoutSecs; }
        return this.sendRequest<DebugResult>('debug', params);
    }

    /**
     * Compile the active KB to TPTP.  `lang` selects the dialect
     * (`fof` | `tff`).
     *
     * The kernel resolves this via the same converter used for
     * `ask`, so the emitted TPTP is exactly what the prover sees
     * when the same KB is queried.
     */
    async generateTptp(opts: { lang?: string } = {}): Promise<GenerateTptpResult> {
        const params: Record<string, unknown> = {};
        if (opts.lang) { params.lang = opts.lang; }
        return this.sendRequest<GenerateTptpResult>('kb.generateTptp', params);
    }

    /** Stop the kernel gracefully.  Idempotent. */
    async stop(): Promise<void> {
        if (!this.isRunning()) { return; }
        try {
            await this.sendRequest<null>('shutdown', {});
        } catch {
            // Ignore -- we're going to kill the process anyway.
        }
        if (this.child && this.child.exitCode === null) {
            this.child.kill();
        }
        this.child = undefined;
        this.rejectAll(new Error('kernel stopped'));
    }

    // -- Internals -----------------------------------------------------------

    private sendRequest<T>(method: string, params: Record<string, unknown>): Promise<T> {
        return new Promise<T>((resolve, reject) => {
            this.ensureRunning()
                .then(() => {
                    if (!this.child || !this.child.stdin || this.child.stdin.destroyed) {
                        reject(new Error('kernel stdin not writable'));
                        return;
                    }
                    const id = this.nextId++;
                    this.pending.set(id, {
                        resolve: resolve as (v: unknown) => void,
                        reject,
                        method,
                    });
                    const line = JSON.stringify({ id, method, params }) + '\n';
                    this.child.stdin.write(line, (err) => {
                        if (err) {
                            this.pending.delete(id);
                            reject(err);
                        }
                    });
                })
                .catch(reject);
        });
    }

    /**
     * Consume newline-delimited responses from the kernel's stdout.
     * Partial reads are buffered across chunk boundaries.
     */
    private onStdout(chunk: string): void {
        this.readBuf += chunk;
        let newlineIdx: number;
        while ((newlineIdx = this.readBuf.indexOf('\n')) !== -1) {
            const line = this.readBuf.slice(0, newlineIdx).trim();
            this.readBuf = this.readBuf.slice(newlineIdx + 1);
            if (line.length === 0) { continue; }
            this.dispatchLine(line);
        }
    }

    private dispatchLine(line: string): void {
        let msg: { id?: number; result?: unknown; error?: { code: number; message: string } };
        try {
            msg = JSON.parse(line);
        } catch (e) {
            this.output.appendLine(`[kernel] malformed response: ${line}`);
            return;
        }

        if (typeof msg.id !== 'number') {
            // Unsolicited message; the kernel shouldn't send any in
            // the MVP (no notifications from server → client yet).
            this.output.appendLine(`[kernel] unexpected notification: ${line}`);
            return;
        }
        const pending = this.pending.get(msg.id);
        if (!pending) {
            this.output.appendLine(`[kernel] reply to unknown id=${msg.id}`);
            return;
        }
        this.pending.delete(msg.id);

        if (msg.error) {
            pending.reject(new Error(
                `kernel ${pending.method} error (${msg.error.code}): ${msg.error.message}`));
        } else {
            pending.resolve(msg.result);
        }
    }

    private rejectAll(err: Error): void {
        for (const [, p] of this.pending) {
            p.reject(err);
        }
        this.pending.clear();
        this.readBuf = '';
    }
}
