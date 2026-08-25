// src/kernelDb.ts
//
// Kernel LMDB path resolution + lifecycle management.
//
// Maps the current `ActiveKb` to a deterministic LMDB directory
// under VSCode's per-user global storage, and tears down ephemeral
// ones automatically.
//
// Layout:
//
//     <globalStorage>/kbs/
//         config/
//             <slug(kbName)>.lmdb/        ← persistent, one per config.xml KB
//         ephemeral/
//             <sessionId>.lmdb/           ← per-run temp KB, deleted on close
//
// Design choices:
//
//   * Global storage (per-user) over workspace storage.  A config
//     KB referenced from two workspaces should share a single
//     LMDB -- builds are expensive, the data is derived, and two
//     separate builds would be a waste.
//
//   * Stable slug for config KBs so the same `<kb name="X">` always
//     resolves to the same directory across extension upgrades.
//
//   * Fresh UUID per ephemeral session so concurrent temp KBs in
//     different windows don't clobber each other.  The UUID lives
//     on `ActiveKb.sessionId` (see kbSession.ts).
//
//   * Orphan cleanup on activation catches crashes / force-kills
//     that skipped `deactivate()`.  Anything under `ephemeral/`
//     older than the session map is prunable.

import * as fs   from 'fs';
import * as path from 'path';
import * as crypto from 'crypto';

import { ExtensionContext, OutputChannel } from 'vscode';

import { ActiveKb } from './kbSession';

// -- Path resolution ---------------------------------------------------------

export type KbKind = 'config' | 'ephemeral';

export interface KernelDbPath {
    /** Absolute filesystem path to the LMDB directory. */
    lmdb: string;
    /** Kind, echoed for lifecycle decisions (e.g. delete-on-close). */
    kind: KbKind;
}

/**
 * Resolve the LMDB directory for `active` under the extension's
 * global storage.  Idempotent -- two calls with the same `active`
 * return the same path.  Does NOT create the directory; that's
 * the kernel's job on `KnowledgeBase::open`.
 *
 * Returns `undefined` when `active` is `null` (no KB open).
 */
export function resolveKernelDbPath(
    context: ExtensionContext,
    active:  ActiveKb | null,
): KernelDbPath | undefined {
    if (!active) { return undefined; }
    const root = globalKbsRoot(context);
    if (active.source === 'config' && active.configKbName) {
        return {
            lmdb: path.join(root, 'config', slugify(active.configKbName) + '.lmdb'),
            kind: 'config',
        };
    }
    // Ephemeral: require a sessionId.  If missing (shouldn't happen
    // -- KbState.openTemp allocates one) bail to undefined so the
    // extension falls back to `--no-db` gracefully.
    if (!active.sessionId) { return undefined; }
    return {
        lmdb: path.join(root, 'ephemeral', active.sessionId + '.lmdb'),
        kind: 'ephemeral',
    };
}

/** Root dir under which all per-KB LMDBs live.  Created on demand. */
function globalKbsRoot(context: ExtensionContext): string {
    const base = context.globalStorageUri.fsPath;
    const root = path.join(base, 'kbs');
    // `mkdirSync` is a no-op if the dir exists; recursive handles
    // the missing `globalStorage` dir on first-ever activation.
    fs.mkdirSync(root, { recursive: true });
    fs.mkdirSync(path.join(root, 'config'),    { recursive: true });
    fs.mkdirSync(path.join(root, 'ephemeral'), { recursive: true });
    return root;
}

/**
 * Turn an arbitrary KB name into a filesystem-safe slug.  Collisions
 * are vanishingly unlikely with a trailing hash so two KBs named
 * `foo/bar` and `foo_bar` still get distinct dirs.
 */
function slugify(name: string): string {
    const cleaned = name.replace(/[^A-Za-z0-9_-]+/g, '_').slice(0, 64);
    const hash    = crypto.createHash('sha256').update(name).digest('hex').slice(0, 8);
    return `${cleaned}-${hash}`;
}

// -- Lifecycle ----------------------------------------------------------------

/**
 * Delete the ephemeral LMDB for `active`, if any.  No-op for
 * config KBs (they persist) and for `active` that doesn't own a
 * session id.  Idempotent.
 */
export function deleteEphemeralDb(
    context: ExtensionContext,
    active:  ActiveKb | null,
    output:  OutputChannel,
): void {
    const resolved = resolveKernelDbPath(context, active);
    if (!resolved || resolved.kind !== 'ephemeral') { return; }
    tryRmDir(resolved.lmdb, output);
}

/**
 * Delete the persistent LMDB for `active`, if any.  Used by the
 * "SUMO: Delete Kernel Database" command -- the caller is
 * responsible for stopping the kernel first (LMDB fd held open).
 */
export function deletePersistentDb(
    context: ExtensionContext,
    active:  ActiveKb | null,
    output:  OutputChannel,
): boolean {
    const resolved = resolveKernelDbPath(context, active);
    if (!resolved || resolved.kind !== 'config') { return false; }
    return tryRmDir(resolved.lmdb, output);
}

/**
 * Scan the ephemeral dir for LMDB directories that don't correspond
 * to any currently-live session, and delete them.  Called once at
 * activation so VSCode crashes / force-kills don't leak state on
 * disk indefinitely.
 *
 * `liveSessionIds` is the set of session IDs that the extension
 * knows about right now.  Typically empty at activation time --
 * but making this a parameter keeps the function testable and
 * lets a future "open N previous ephemerals on restart" feature
 * pass them in.
 */
export function cleanupOrphanEphemerals(
    context:        ExtensionContext,
    liveSessionIds: Set<string>,
    output:         OutputChannel,
): number {
    const dir = path.join(globalKbsRoot(context), 'ephemeral');
    let dropped = 0;
    let entries: fs.Dirent[];
    try {
        entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch (e) {
        // Dir might not exist yet; benign.
        return 0;
    }
    for (const entry of entries) {
        if (!entry.isDirectory()) { continue; }
        const suffix = '.lmdb';
        if (!entry.name.endsWith(suffix)) { continue; }
        const sessionId = entry.name.slice(0, -suffix.length);
        if (liveSessionIds.has(sessionId)) { continue; }
        if (tryRmDir(path.join(dir, entry.name), output)) { dropped++; }
    }
    if (dropped > 0) {
        output.appendLine(
            `[kernel-db] pruned ${dropped} orphan ephemeral database(s)`);
    }
    return dropped;
}

function tryRmDir(p: string, output: OutputChannel): boolean {
    try {
        fs.rmSync(p, { recursive: true, force: true });
        return true;
    } catch (e) {
        output.appendLine(`[kernel-db] failed to delete '${p}': ${e}`);
        return false;
    }
}
