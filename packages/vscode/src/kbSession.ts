// The single active knowledge base for this VSCode window.
//
// Per-window-per-KB model: each VSCode window edits at most one
// KB, whose symbol namespace is isolated from any other window's
// by virtue of running its own `sumo-lsp` subprocess.  A file
// belonging to multiple KBs is opened in multiple windows (one
// editor tab each); the "which KB am I in?" question is answered
// trivially by the window.
//
// A session has one of two sources:
//
//   * `config`    -- backed by a `<kb>` entry in config.xml.
//                    `configKbName` points at the backing element;
//                    add / remove mutate the XML.
//   * `temporary` -- ad-hoc, this-window-only.  Holds whatever
//                    standalone `.kif` files the user has opted to
//                    group here.  Not persisted.
//
// When the window has no active KB yet, `get()` returns `null`;
// the first `.kif` didOpen triggers the selection prompt.

import * as path from 'path';
import * as crypto from 'crypto';

import { ParsedConfig } from './config';

export type KbSource = 'config' | 'temporary';

export interface ActiveKb {
    name: string;
    source: KbSource;
    /** For `config` sessions, the value of the `<kb name="…">` attribute. */
    configKbName: string | null;
    /**
     * Opaque per-session identifier for `temporary` sessions.  Used to
     * key the ephemeral LMDB directory (see `kernelDb.ts`) so
     * concurrent temp KBs in different windows don't clobber each
     * other and orphan cleanup can tell live sessions from dead.
     * `null` for `config` sessions (their path is derived from
     * `configKbName` instead).
     */
    sessionId: string | null;
    /** Absolute canonical paths. */
    readonly files: Set<string>;
}

/** Mutable single-slot holder for the window's active KB. */
export class KbState {
    private active: ActiveKb | null = null;

    get(): ActiveKb | null { return this.active; }
    isActive(): boolean { return this.active !== null; }

    /**
     * Load a config-declared KB as the window's active session.
     * Replaces any existing active session.
     */
    setConfigKb(kbName: string, files: string[]): ActiveKb {
        this.active = {
            name:          kbName,
            source:        'config',
            configKbName:  kbName,
            sessionId:     null,
            files:         new Set(files.map(normaliseAbs)),
        };
        return this.active;
    }

    /**
     * Open a fresh temporary session.  If `initialFile` is given,
     * seeds the file set; otherwise starts empty.  A fresh UUID is
     * minted as `sessionId` so the kernel's ephemeral LMDB gets a
     * unique on-disk directory.
     */
    openTemp(initialFile?: string): ActiveKb {
        this.active = {
            name:          'Temporary',
            source:        'temporary',
            configKbName:  null,
            sessionId:     crypto.randomUUID(),
            files:         new Set(initialFile ? [normaliseAbs(initialFile)] : []),
        };
        return this.active;
    }

    /** Drop the active session entirely. */
    clear(): void { this.active = null; }

    /** Add `file` to the active session.  No-op when no session is active. */
    addFile(file: string): boolean {
        if (!this.active) { return false; }
        this.active.files.add(normaliseAbs(file));
        return true;
    }

    /** Remove `file` from the active session.  `false` when absent. */
    removeFile(file: string): boolean {
        if (!this.active) { return false; }
        return this.active.files.delete(normaliseAbs(file));
    }

    /** True when the active session contains `file`. */
    contains(file: string): boolean {
        return this.active?.files.has(normaliseAbs(file)) ?? false;
    }

    /** Size of the active session.  Zero when no session is active. */
    size(): number { return this.active?.files.size ?? 0; }
}

// -- Helpers -----------------------------------------------------------------

/**
 * Canonicalise a file path for set membership.  Mirrors the
 * server-side `uri_to_tag` so client and server agree on file
 * identity even when casing / symlinks / relative forms differ
 * at the edges.
 */
export function normaliseAbs(file: string): string {
    let abs = path.resolve(file);
    if (process.platform === 'win32' && abs.length >= 2 && abs[1] === ':') {
        abs = abs[0].toLowerCase() + abs.slice(1);
    }
    return abs;
}

/**
 * Every config-declared KB whose constituents include `file`.
 * Returns config-order KB names.
 */
export function configKbsContaining(cfg: ParsedConfig, file: string): string[] {
    const norm = normaliseAbs(file);
    return cfg.kbs
        .filter(kb => kb.files.some(f => normaliseAbs(f) === norm))
        .map(kb => kb.name);
}
