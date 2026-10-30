/** Line-level edits to the WordNet-SUMO mapping files
 *  (`WordNetMappings30-*.txt`). Each synset record is one line ending in its
 *  SUMO anchor run (`| gloss &%Term= &%Other+`). Only that run is ever
 *  rewritten, so a mapping change is a one-line diff and every line keeps its
 *  number. Parsing the records themselves is the engine's job (see
 *  `Session.wordnetSenses`); this module only locates and replaces the run. */

/** One `&%Term<suffix>` anchor. */
export interface Anchor {
  term: string;
  suffix: string;
}

/** The mapping kinds the editor offers, in the files' own notation. */
export const MAPPING_KINDS = [
  { suffix: "=", label: "equivalent" },
  { suffix: "+", label: "subsuming" },
  { suffix: "@", label: "instance" },
] as const;

/** A name the engine's anchor parser accepts (see `parse_anchors` in
 *  crates/core/src/lexicon/parse.rs). */
export function isValidTerm(term: string): boolean {
  return /^[A-Za-z0-9_-]+$/.test(term);
}

/** Where `line`'s anchor run begins, and where its content ends (before any
 *  `\r`). With no anchors, both are the content end. Null for a line that is
 *  not a synset record. */
function anchorBounds(line: string): { start: number; end: number } | null {
  if (!/^\d/.test(line)) return null;
  const bar = line.indexOf(" | ");
  if (bar < 0) return null;
  const end = line.endsWith("\r") ? line.length - 1 : line.length;
  const at = line.indexOf("&%", bar + 3);
  return { start: at < 0 || at > end ? end : at, end };
}

/** `line` with its anchor run replaced by `anchors` (none removes it).
 *  Throws for a line that is not a synset record or an invalid term. */
export function rewriteAnchors(line: string, anchors: Anchor[]): string {
  const bounds = anchorBounds(line);
  if (!bounds) throw new Error("Not a WordNet synset record");
  for (const a of anchors)
    if (!isValidTerm(a.term)) throw new Error(`Invalid SUMO term: ${a.term}`);
  const head = line.slice(0, bounds.start).trimEnd();
  const run = anchors.map((a) => `&%${a.term}${a.suffix}`).join(" ");
  return (run ? `${head} ${run}` : head) + line.slice(bounds.end);
}

/** The anchor run as written (`&%Dog= &%Pet+`), or "" when it has none. */
export function anchorText(line: string): string {
  const bounds = anchorBounds(line);
  return bounds ? line.slice(bounds.start, bounds.end).trim() : "";
}

/** The anchors in `text` -- a record line or just its anchor run -- with the
 *  same acceptance rule as the engine's parser: an unknown suffix drops the
 *  anchor. */
export function parseAnchors(text: string): Anchor[] {
  const run = anchorBounds(text) ? anchorText(text) : text;
  return [...run.matchAll(/&%([A-Za-z0-9_-]+)([=+@:[\]])/g)].map((m) => ({
    term: m[1],
    suffix: m[2],
  }));
}

/** `"<offset>-<pos>"` for a synset record (satellites fold into `a`, as in
 *  `WordNetSense.id`), else null. */
export function synsetKey(line: string): string | null {
  const m = /^(\d+) \S+ ([nvasr]) /.exec(line);
  if (!m) return null;
  return `${m[1]}-${m[2] === "s" ? "a" : m[2]}`;
}

/** A synset whose anchor run differs between two versions of a file. */
export interface MappingChange {
  key: string;
  /** 1-based line number in `current`. */
  line: number;
  /** The record's first word (underscores intact), for display. */
  word: string;
  before: string;
  after: string;
}

/** Every synset whose anchors differ between `base` and `current`, keyed by
 *  synset identity rather than line number so an upstream that has gained or
 *  lost lines still lines up. Records only in `current` count as changed
 *  from "". */
export function diffMappings(base: string, current: string): MappingChange[] {
  const before = new Map<string, string>();
  for (const line of base.split("\n")) {
    const key = synsetKey(line);
    if (key) before.set(key, anchorText(line));
  }
  const out: MappingChange[] = [];
  current.split("\n").forEach((line, i) => {
    const key = synsetKey(line);
    if (!key) return;
    const after = anchorText(line);
    const was = before.get(key) ?? "";
    if (was !== after)
      out.push({
        key,
        line: i + 1,
        word: line.split(" ")[4] ?? key,
        before: was,
        after,
      });
  });
  return out;
}
