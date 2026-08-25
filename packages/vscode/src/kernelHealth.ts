// src/kernelHealth.ts
//
// Eager health check for the `sigmakee` kernel binary.  Runs at
// activation (and whenever the user changes `sumo.kernel.path`) to
// classify the kernel's state into one of:
//
//   - `ok`        — binary found, has a `serve` subcommand, and
//                   starts successfully.
//   - `missing`   — no binary at the configured / PATH location.
//   - `no-serve`  — binary exists but was compiled without the
//                   `server` feature (no `serve` subcommand).
//   - `crash`     — binary starts but exits before we get a reply,
//                   or stderr surfaces an OS error.
//
// The extension consumes the result to flip a context variable
// (`sumo.kernelAvailable`) that gates every RPC-dependent command
// in the Command Palette, and to show a status-appropriate error
// dialog that points the user at fixes.
//
// The check never throws — every failure path returns a typed
// `KernelHealth` with a human-readable `reason` the dialog renders.

import { spawn } from 'child_process';
import * as fs   from 'fs';
import * as path from 'path';
import { OutputChannel, WorkspaceConfiguration } from 'vscode';

/** URL the "Download" button opens when the binary is missing /
 *  has no `serve` subcommand.  Pointed at the sigma-rs repo whose
 *  release artifacts ship prebuilt `sigmakee` binaries. */
export const RELEASES_URL = 'https://github.com/ontologyportal/sigma-rs/releases';

/** Outcome of a single health-check run. */
export type KernelHealth =
    | {
        status:     'ok';
        /** Resolved absolute path to the binary. */
        binaryPath: string;
      }
    | {
        status:     'missing';
        /** The path we tried (empty string when we fell back to PATH
         *  lookup and didn't find anything). */
        attempted:  string;
        reason:     string;
      }
    | {
        status:     'no-serve';
        binaryPath: string;
        reason:     string;
      }
    | {
        status:     'crash';
        binaryPath: string;
        reason:     string;
        /** Captured stderr or OS error — shown verbatim in the
         *  output channel for diagnosis. */
        detail:     string;
      };

/**
 * Run the four-tier health check.
 *
 *   1. Resolve the binary (config override → PATH lookup of `sigmakee`).
 *   2. Verify it exists on disk.
 *   3. Probe `<binary> --help` and confirm `serve` is in the subcommand list.
 *   4. Spawn `<binary> serve --no-db`, send a noop request, verify
 *      it responds before exit.
 *
 * Returns `ok` only when all four pass.  The first failure short-
 * circuits — the remaining tiers are skipped since later checks
 * presuppose the earlier ones.
 *
 * `output` receives per-step progress lines at info level.  The
 * caller decides whether to surface dialogs; this function never
 * shows UI itself.
 */
export async function checkKernelHealth(
    config: WorkspaceConfiguration,
    output: OutputChannel,
): Promise<KernelHealth> {
    const binaryName = process.platform === 'win32' ? 'sigmakee.exe' : 'sigmakee';

    // -- Tier 1 + 2: resolve + existence check -------------------------
    const configured = config.get<string>('kernel.path', '').trim();
    let binaryPath: string | undefined;

    if (configured.length > 0) {
        // Honour absolute, relative, and `~` forms.  Relative resolves
        // against the first workspace folder when available, else the
        // user's home directory — matches the convention used by the
        // LSP-path resolver.
        const resolved = expandPath(configured);
        if (fs.existsSync(resolved)) {
            binaryPath = resolved;
            output.appendLine(`[health] kernel binary (configured): ${binaryPath}`);
        } else {
            output.appendLine(`[health] kernel binary missing at configured path: ${resolved}`);
            return {
                status:    'missing',
                attempted: resolved,
                reason:
                    `The path "${resolved}" (from the \`sumo.kernel.path\` setting) does not exist.  ` +
                    `Clear the setting to fall back to PATH lookup, or set it to a valid binary.`,
            };
        }
    } else {
        const found = findOnPath(binaryName);
        if (!found) {
            output.appendLine(`[health] \`${binaryName}\` not found on PATH`);
            return {
                status:    'missing',
                attempted: '',
                reason:
                    `Cannot find the \`${binaryName}\` binary on your PATH.  ` +
                    `Download a prebuilt release or build from source, then either ` +
                    `install it on PATH or set \`sumo.kernel.path\` to its absolute location.`,
            };
        }
        binaryPath = found;
        output.appendLine(`[health] kernel binary (PATH): ${binaryPath}`);
    }

    // -- Tier 3: probe for the `serve` subcommand ----------------------
    //
    // `<binary> --help` lists subcommands.  We don't run `serve`
    // itself at this stage because that would try to open LMDB and
    // we'd have to pick an LMDB path for a probe we only want to
    // throw away.  `--help` is side-effect-free.
    const helpProbe = await runCommand(binaryPath, ['--help'], /* stdin */ undefined, 5000);
    if (helpProbe.exitCode !== 0) {
        output.appendLine(
            `[health] \`${binaryPath} --help\` exited ${helpProbe.exitCode}; ` +
            `stderr: ${helpProbe.stderr.trim()}`,
        );
        return {
            status:     'crash',
            binaryPath,
            reason:     `The binary at "${binaryPath}" refuses to run.`,
            detail:     helpProbe.stderr.trim() || `exit code ${helpProbe.exitCode}`,
        };
    }
    // Match `serve` as a subcommand entry.  Clap formats these as
    // two-space-indented lines in a block under `Commands:`; a word-
    // boundary match on `serve` in the output is reliable enough
    // that it won't collide with anything else clap prints.
    if (!/^\s+serve\b/m.test(helpProbe.stdout)) {
        output.appendLine(`[health] binary lacks the \`serve\` subcommand`);
        return {
            status:     'no-serve',
            binaryPath,
            reason:
                `The \`${path.basename(binaryPath)}\` binary at "${binaryPath}" was built without the ` +
                `RPC server.  Download a prebuilt release, or rebuild from source with ` +
                `\`cargo build --release --features server\`.`,
        };
    }

    // -- Tier 4: actually start the server -----------------------------
    //
    // Spawns `<binary> serve --no-db` and sends two requests:
    // `kb.listFiles` (any no-op works; this one returns fast) and
    // `shutdown`.  The kernel should respond with two JSON lines
    // and exit cleanly.  If it crashes or times out we capture
    // whatever landed on stderr.
    const smoke = await smokeTestServe(binaryPath, 5000);
    if (!smoke.ok) {
        output.appendLine(`[health] smoke test failed: ${smoke.reason}`);
        return {
            status:     'crash',
            binaryPath,
            reason:     `The kernel binary at "${binaryPath}" failed to start.`,
            detail:     smoke.detail,
        };
    }

    output.appendLine(`[health] kernel OK at ${binaryPath}`);
    return { status: 'ok', binaryPath };
}

// -- Binary-resolution helpers -----------------------------------------------

/**
 * Search `$PATH` for an executable named `binary` (plus each
 * `$PATHEXT` variant on Windows).  Returns the first match or
 * `undefined`.  Handles the `PATH` / `Path` case-difference on
 * Windows by using whichever env var is populated.
 *
 * We roll this ourselves instead of pulling in the `which` npm
 * package so the extension keeps its dependency surface small.
 */
function findOnPath(binary: string): string | undefined {
    const isWin   = process.platform === 'win32';
    const pathEnv = process.env.PATH ?? process.env.Path ?? '';
    const pathSep = isWin ? ';' : ':';
    // On Windows, PATHEXT holds the list of extensions treated as
    // executable.  We iterate each so `sigmakee` matches
    // `sigmakee.exe` / `sigmakee.cmd` / `sigmakee.bat` without the
    // user having to pin the suffix.  On non-Windows we just try
    // the bare name (callers pass `sigmakee` sans `.exe` there).
    const exts: string[] = isWin
        ? (process.env.PATHEXT ?? '.EXE;.BAT;.CMD;.COM').split(';').map(e => e.toLowerCase())
        : [''];

    for (const dir of pathEnv.split(pathSep)) {
        if (!dir) { continue; }
        for (const ext of exts) {
            // If the caller already included an extension (`sigmakee.exe`),
            // avoid doubling it on PATHEXT iterations.
            const name = isWin && ext !== '' && binary.toLowerCase().endsWith(ext)
                ? binary
                : binary + ext;
            const candidate = path.join(dir, name);
            try {
                fs.accessSync(candidate, fs.constants.X_OK);
                return candidate;
            } catch { /* not here; try next */ }
        }
    }
    return undefined;
}

/**
 * Expand `~` / `~/`-prefixed paths and resolve relative paths
 * against an anchor.  Absolute paths pass through.
 *
 * This is a small duplicate of the resolver in `extension.ts`;
 * kept local so the health check doesn't have to import from
 * the main file (which would introduce a cycle via the
 * `resolveServerBinary` helper).
 */
function expandPath(p: string): string {
    if (p.startsWith('~/') || p === '~') {
        const home = process.env.HOME ?? process.env.USERPROFILE ?? '';
        return path.join(home, p.slice(2));
    }
    return p;
}

// -- Process helpers ---------------------------------------------------------

interface CommandResult {
    exitCode: number | null;
    stdout:   string;
    stderr:   string;
    /** True iff the run timed out and was force-killed. */
    timedOut: boolean;
    /** OS-level spawn error (ENOENT, EACCES, …), if any. */
    error?:   string;
}

/**
 * Run `binary args` to completion (or until `timeoutMs` elapses)
 * and capture stdout + stderr.  Never throws — transport errors
 * land in `result.error`, kernel-level errors land in the exit
 * code + stderr text.
 */
function runCommand(
    binary:    string,
    args:      string[],
    stdin:     string | undefined,
    timeoutMs: number,
): Promise<CommandResult> {
    return new Promise((resolve) => {
        let stdout    = '';
        let stderr    = '';
        let settled   = false;
        let timedOut  = false;

        const child = spawn(binary, args, {
            stdio: ['pipe', 'pipe', 'pipe'],
            env:   process.env,
        });

        const finish = (exitCode: number | null, error?: string) => {
            if (settled) { return; }
            settled = true;
            clearTimeout(timer);
            resolve({ exitCode, stdout, stderr, timedOut, error });
        };

        const timer = setTimeout(() => {
            timedOut = true;
            try { child.kill('SIGKILL'); } catch { /* ignore */ }
            finish(null, `timed out after ${timeoutMs}ms`);
        }, timeoutMs);

        child.stdout!.on('data', (buf: Buffer) => { stdout += buf.toString(); });
        child.stderr!.on('data', (buf: Buffer) => { stderr += buf.toString(); });
        child.on('error', (err: NodeJS.ErrnoException) => {
            finish(null, err.message);
        });
        child.on('exit', (code) => finish(code));

        if (stdin !== undefined) {
            try {
                child.stdin!.write(stdin);
                child.stdin!.end();
            } catch { /* ignore; child.exit will clean up */ }
        } else {
            child.stdin!.end();
        }
    });
}

/**
 * Spawn `binary serve --no-db`, ping with two JSON-RPC requests,
 * and verify the kernel responds and exits cleanly.  Returns `ok`
 * on success, or `reason` + `detail` (stderr / exit info) on
 * failure.
 *
 * `--no-db` keeps the probe side-effect-free: nothing lands on
 * disk even if the user runs the check from an unwritable
 * directory.
 */
async function smokeTestServe(
    binary:    string,
    timeoutMs: number,
): Promise<{ ok: true } | { ok: false; reason: string; detail: string }> {
    // The two requests are sent in one stdin burst: `kb.listFiles`
    // exercises the dispatch path (read-only, fast) and `shutdown`
    // terminates the kernel.  Both IDs are echoed back on stdout,
    // so two newline-separated JSON responses = healthy.
    const stdin =
        '{"id":1,"method":"kb.listFiles"}\n' +
        '{"id":2,"method":"shutdown"}\n';

    const result = await runCommand(binary, ['serve', '--no-db'], stdin, timeoutMs);

    if (result.error) {
        return {
            ok: false,
            reason: 'OS-level spawn failure',
            detail: result.error,
        };
    }
    if (result.timedOut) {
        return {
            ok: false,
            reason: `kernel did not respond within ${timeoutMs}ms`,
            detail: result.stderr.trim() || '(no stderr output)',
        };
    }
    if (result.exitCode !== 0) {
        return {
            ok: false,
            reason: `kernel exited with code ${result.exitCode}`,
            detail: result.stderr.trim() || '(no stderr output)',
        };
    }
    // Count distinct response lines.  Anything ≥ 1 means the kernel
    // wrote to stdout, which requires the dispatch loop to be
    // running.  `shutdown` may or may not write its own response
    // before exiting (the current server does, but we don't rely
    // on it).
    const responseLines = result.stdout
        .split('\n')
        .map(l => l.trim())
        .filter(l => l.length > 0 && l.startsWith('{'))
        .length;
    if (responseLines === 0) {
        return {
            ok: false,
            reason: 'kernel exited 0 but wrote nothing to stdout',
            detail: result.stderr.trim() || '(no stderr output)',
        };
    }
    return { ok: true };
}
