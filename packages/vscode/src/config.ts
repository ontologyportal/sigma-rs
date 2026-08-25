// SigmaKEE config.xml parser + write-back.
//
// Shape (mirrors sumo-parser's own `crates/native/src/config.rs`):
//
//   <configuration>
//     <preference name="kbDir" value="/home/user/sumo" />
//     ...
//     <kb name="SUMO">
//       <constituent filename="Merge.kif" />
//       <constituent filename="Mid-level-ontology.kif" />
//     </kb>
//     <kb name="other-kb"> ... </kb>
//   </configuration>
//
// Constituent filenames are relative to the `kbDir` preference
// (absolute paths pass through).  The parser resolves all
// constituent paths to absolute at load time so downstream code
// never has to think about relativity.
//
// Write-back uses `fast-xml-parser` in preserveOrder mode so
// comments, attribute order, and whitespace round-trip cleanly
// for the common edit (adding or removing one <constituent>
// inside an existing <kb>).

import * as fs   from 'fs';
import * as os   from 'os';
import * as path from 'path';

import { XMLBuilder, XMLParser } from 'fast-xml-parser';

/** One knowledge base declared in config.xml. */
export interface KbDecl {
    /** Value of the `name` attribute. */
    name: string;
    /** Absolute paths of every constituent, in declaration order. */
    files: string[];
}

/** Parsed SigmaKEE config.xml. */
export interface ParsedConfig {
    /** Absolute filesystem path of the source file. */
    configPath: string;
    /** `kbDir` preference (absolute).  Falls back to dirname(configPath). */
    kbDir: string;
    /** Optional `sumokbname` preference -- the "default" KB. */
    defaultKbName: string | null;
    /** All declared KBs. */
    kbs: KbDecl[];
}

const PARSER_OPTS = {
    ignoreAttributes:    false,
    attributeNamePrefix: '@_',
    preserveOrder:       true,
    trimValues:          false,
    parseAttributeValue: false,
};

const BUILDER_OPTS = {
    ignoreAttributes:    false,
    attributeNamePrefix: '@_',
    preserveOrder:       true,
    format:              true,
    indentBy:            '  ',
    suppressEmptyNode:   true,
};

// -- Location resolution -----------------------------------------------------

/**
 * Decide which config.xml to read.  Priority:
 *
 *   1. `sumo.configPath` VSCode setting (file path or directory).
 *   2. `$SIGMA_HOME/KBs/config.xml`.
 *   3. `~/.sigmakee/KBs/config.xml`.
 *
 * Returns an absolute path or `null` if nothing resolves.
 */
export function resolveConfigPath(explicit: string | undefined): string | null {
    if (explicit && explicit.length > 0) {
        const expanded = expandTilde(explicit);
        if (isDir(expanded)) {
            return path.join(expanded, 'config.xml');
        }
        return expanded;
    }

    const sigmaHome = process.env.SIGMA_HOME;
    if (sigmaHome) {
        const p = path.join(sigmaHome, 'config.xml');
        if (fs.existsSync(p)) { return p; }
    }

    const fallback = path.join(os.homedir(), '.sigmakee', 'config.xml');
    if (fs.existsSync(fallback)) { return fallback; }

    return null;
}

function expandTilde(p: string): string {
    if (p === '~' || p.startsWith('~/')) {
        return path.join(os.homedir(), p.slice(2));
    }
    return p;
}

function isDir(p: string): boolean {
    try { return fs.statSync(p).isDirectory(); } catch { return false; }
}

// -- Parse -------------------------------------------------------------------

/** Read + parse config.xml at `configPath`. */
export function parseConfigXml(configPath: string): ParsedConfig {
    const text    = fs.readFileSync(configPath, 'utf8');
    const parser  = new XMLParser(PARSER_OPTS);
    const parsed  = parser.parse(text);

    // preserveOrder yields an array-of-nodes shape.  Walk to find
    // the top-level <configuration> element, then its children.
    const configNode = findNamed(parsed, 'configuration');
    const children   = configNode?.configuration ?? [];

    // Extract preferences + kb declarations.
    const preferences: Map<string, string> = new Map();
    const kbs: KbDecl[] = [];

    for (const child of children) {
        const kind = Object.keys(child).find(k => !k.startsWith(':@')) ?? '';
        if (kind === 'preference') {
            const attrs = child[':@'] ?? {};
            const name  = attrs['@_name'];
            const value = attrs['@_value'];
            if (name && value !== undefined) { preferences.set(name, value); }
        } else if (kind === 'kb') {
            kbs.push(parseKbNode(child));
        }
    }

    const rawKbDir = preferences.get('kbDir') ?? path.dirname(configPath);
    const kbDir    = expandTilde(rawKbDir);

    // Resolve every constituent to an absolute path.
    for (const kb of kbs) {
        kb.files = kb.files.map(f => {
            const expanded = expandTilde(f);
            return path.isAbsolute(expanded) ? expanded : path.resolve(kbDir, expanded);
        });
    }

    return {
        configPath,
        kbDir,
        defaultKbName: preferences.get('sumokbname') ?? null,
        kbs,
    };
}

function findNamed(parsed: unknown[], name: string): any {
    if (!Array.isArray(parsed)) { return null; }
    for (const node of parsed) {
        if (node && typeof node === 'object' && name in (node as object)) { return node; }
    }
    return null;
}

function parseKbNode(kbNode: any): KbDecl {
    const attrs    = kbNode[':@'] ?? {};
    const name     = attrs['@_name'] ?? '';
    const children = kbNode.kb ?? [];
    const files: string[] = [];
    for (const child of children) {
        const kind = Object.keys(child).find(k => !k.startsWith(':@'));
        if (kind === 'constituent') {
            const fname = child[':@']?.['@_filename'];
            if (fname) { files.push(fname); }
        }
    }
    return { name, files };
}

// -- Write-back --------------------------------------------------------------

/**
 * Rewrite `configPath` so the named KB's `<constituent>` list is
 * exactly the provided relative (or absolute) filenames.  The
 * caller is responsible for converting absolute paths back to
 * relative form if desired; this function writes whatever strings
 * it's given.
 *
 * Preserves preference order, attribute order, and other KBs.
 * No-op changes produce byte-identical output when the input was
 * already canonical.
 */
export function writeKbConstituents(
    configPath: string,
    kbName:     string,
    filenames:  string[],
): void {
    const text    = fs.readFileSync(configPath, 'utf8');
    const parser  = new XMLParser(PARSER_OPTS);
    const tree    = parser.parse(text);
    const cfgNode = findNamed(tree, 'configuration');
    if (!cfgNode) {
        throw new Error(`config.xml at ${configPath} missing <configuration> root`);
    }

    const children: any[] = cfgNode.configuration;
    const kbIdx = children.findIndex(
        c => 'kb' in c && c[':@']?.['@_name'] === kbName,
    );
    if (kbIdx < 0) {
        throw new Error(`KB "${kbName}" not found in ${configPath}`);
    }

    // Build the replacement constituent children, preserving the
    // wrapping <kb> element's own attributes.
    const kbAttrs       = children[kbIdx][':@'];
    const newChildren   = filenames.map(f => ({
        constituent: [],
        ':@': { '@_filename': f },
    }));
    children[kbIdx] = {
        kb:  newChildren,
        ':@': kbAttrs,
    };

    const builder = new XMLBuilder(BUILDER_OPTS);
    const out     = builder.build(tree);
    fs.writeFileSync(configPath, out);
}

/**
 * Relativise `filePath` against `kbDir` when possible, so adding
 * a new constituent writes the short relative form used for
 * existing ones.  Absolute paths outside `kbDir` stay absolute.
 */
export function relativiseToKbDir(filePath: string, kbDir: string): string {
    const rel = path.relative(kbDir, filePath);
    if (rel.startsWith('..') || path.isAbsolute(rel)) { return filePath; }
    return rel;
}

/**
 * Append a brand-new `<kb name="…">` entry (optionally with an
 * initial `<constituent>` list) to `configPath`.  Fails if a KB
 * with the same name already exists -- the caller should confirm
 * with the user before overwriting.
 *
 * Preserves the order and attributes of every existing node so
 * the write is a minimal diff.
 */
export function createKbEntry(
    configPath: string,
    kbName:     string,
    filenames:  string[],
): void {
    const text    = fs.readFileSync(configPath, 'utf8');
    const parser  = new XMLParser(PARSER_OPTS);
    const tree    = parser.parse(text);
    const cfgNode = findNamed(tree, 'configuration');
    if (!cfgNode) {
        throw new Error(`config.xml at ${configPath} missing <configuration> root`);
    }

    const children: any[] = cfgNode.configuration;
    const existing = children.findIndex(
        c => 'kb' in c && c[':@']?.['@_name'] === kbName,
    );
    if (existing >= 0) {
        throw new Error(`KB "${kbName}" already exists in ${configPath}`);
    }

    const constituentChildren = filenames.map(f => ({
        constituent: [],
        ':@': { '@_filename': f },
    }));
    children.push({
        kb:  constituentChildren,
        ':@': { '@_name': kbName },
    });

    const builder = new XMLBuilder(BUILDER_OPTS);
    const out     = builder.build(tree);
    fs.writeFileSync(configPath, out);
}
