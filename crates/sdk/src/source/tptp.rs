//! TPTP problem handling: `include(...)` resolution + native-prover solving.
//!
//! This is the orchestration that used to live in `sigmakee_rs_core::kb::tptp`
//! (`solve_tptp` / `load_tptp_axioms` / `resolve_tptp_includes`).  The core is
//! deliberately filesystem-free, so include resolution (a directory walk + the
//! `$TPTP` search path) and the solve orchestration belong here in the SDK.
//!
//! Two layers, gated differently:
//! - [`resolve_includes`] is pure text over an injected byte-reader (no `Source`
//!   or feature coupling) — available on every build.  It returns the problem
//!   plus each resolved include as its own [`SourceFile`]; the caller supplies
//!   the reader (local FS, an HTTP GET, `Source::read`, a test stub), so
//!   includes can be local or remote.
//! - `solve` / `load_axioms` need the native prover (`KnowledgeBase<ProverLayer>`)
//!   and are gated behind the `native-prover` feature.  The role split mirrors
//!   `TestCase::from_tptp`: `axiom`-role formulas are background theory →
//!   promoted as SInE-selectable axioms; `hypothesis`-role formulas are
//!   force-included support; `conjecture` / `negated_conjecture` is the goal.

use std::collections::HashSet;
use std::path::PathBuf;

use sigmakee_rs_core::{FileOrigin, LocalProvenance, Parser, SourceFile};

// -- include resolution (text + an injected reader; no Source/feature coupling) --

/// Resolve a TPTP problem's `include('…')` directives into a flat list of
/// [`SourceFile`]s: element 0 is `text`'s own file (its `include` lines blanked
/// to empty lines, so line numbers are preserved), followed by one `SourceFile`
/// per resolved include — recursively, cycle-safe (`seen` + depth ≤ 8).
///
/// `base` is the location of `text` — a local path **or** a URL.  Each include
/// is resolved relative to it (and `$TPTP`) and fetched through `read`, so the
/// caller decides how bytes are obtained (local FS, an HTTP GET, `Source::read`,
/// a test stub) and includes can be local or remote.  Search order per TPTP
/// convention: `$TPTP/<rel>`, `<base dir>/<rel>`, `<base dir>/../<rel>`; the
/// first candidate `read` returns `Ok` for wins.
///
/// A selective include (`include('f.ax', [a, b]).`) keeps the included file
/// whole but **blanks** every non-selected formula's characters (newlines kept),
/// so the file's line numbers survive for diagnostics.  An include that does not
/// resolve to a TPTP file (`.ax` / `.p` / `.tptp`) is an error.
pub fn resolve_includes(
    text: &str,
    base: &str,
    read: &dyn Fn(&str) -> Result<String, String>,
) -> Result<Vec<SourceFile>, String> {
    let mut seen = HashSet::new();
    resolve_rec(text, base, 0, &mut seen, read)
}

fn resolve_rec(
    text:  &str,
    base:  &str,
    depth: usize,
    seen:  &mut HashSet<String>,
    read:  &dyn Fn(&str) -> Result<String, String>,
) -> Result<Vec<SourceFile>, String> {
    if depth > 8 {
        return Err("include depth exceeds 8 (cycle?)".to_string());
    }
    let dir = dir_of(base);
    let mut self_contents = String::with_capacity(text.len());
    let mut includes: Vec<SourceFile> = Vec::new();

    for line in text.lines() {
        let Some((rel, names)) = parse_include(line.trim()) else {
            self_contents.push_str(line);
            self_contents.push('\n');
            continue;
        };
        // Replace the directive with a blank line so this file's later lines keep
        // their numbers; the included content rides in as its own SourceFile(s).
        self_contents.push('\n');

        let (location, inner) = fetch_include(rel, &dir, read)?;

        // Whole-file repeats are skipped (already pulled in); a selective include
        // is always processed — a different name list picks different formulas.
        if names.is_none() && !seen.insert(location.clone()) {
            continue;
        }

        let mut sub = resolve_rec(&inner, &location, depth + 1, seen, read)?;
        if let Some(wanted) = &names {
            for sf in &mut sub {
                sf.contents = blank_unselected(&sf.contents, wanted);
            }
        }
        includes.append(&mut sub);
    }

    let mut out = Vec::with_capacity(1 + includes.len());
    out.push(make_source(base, self_contents));
    out.append(&mut includes);
    Ok(out)
}

/// Parse a single-line `include('rel')` / `include('rel', [n1, n2])` directive.
/// Returns the relative target and the optional selection set, or `None` when
/// the line is not an include.
fn parse_include(trimmed: &str) -> Option<(&str, Option<HashSet<String>>)> {
    let rest = trimmed.strip_prefix("include('")?;
    let (rel, tail) = rest.split_once('\'')?;
    let names: Option<HashSet<String>> = tail
        .find('[')
        .and_then(|a| tail[a..].find(']').map(|b| (a, a + b)))
        .map(|(a, b)| {
            tail[a + 1..b]
                .split(',')
                .map(|n| n.trim().trim_matches('\'').to_string())
                .filter(|n| !n.is_empty())
                .collect()
        });
    Some((rel, names))
}

/// Resolve `rel` against `$TPTP` and the base directory, fetching each candidate
/// through `read`; the first that reads `Ok` wins.  Errors if `rel` is not a
/// TPTP file or no candidate could be read.
fn fetch_include(
    rel:  &str,
    dir:  &str,
    read: &dyn Fn(&str) -> Result<String, String>,
) -> Result<(String, String), String> {
    // The target must be a TPTP file (.ax / .p / .tptp) — the extension is the
    // same across every candidate, so check it once on `rel`.
    if !matches!(Parser::from_filename(rel), Some(Parser::Tptp { .. })) {
        return Err(format!(
            "include('{rel}') does not point at a TPTP file (.ax / .p / .tptp)"));
    }
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(root) = std::env::var("TPTP") {
        candidates.push(join_loc(&root, rel));
    }
    candidates.push(join_loc(dir, rel));
    if let Some(parent) = parent_dir(dir) {
        candidates.push(join_loc(&parent, rel));
    }
    let mut errs = Vec::new();
    for cand in candidates {
        match read(&cand) {
            Ok(contents) => return Ok((cand, contents)),
            Err(e) => errs.push(format!("{cand} ({e})")),
        }
    }
    Err(format!("cannot resolve include('{rel}') — tried {}", errs.join(", ")))
}

/// Build the `SourceFile` for `location` with the given (possibly blanked)
/// contents.  Parser comes from the extension (falling back to TPTP, since this
/// is the TPTP include resolver); origin is `Remote` for a `scheme://` location,
/// else `Local` — tagged [`LocalProvenance::UNKNOWN`] since this resolver is
/// deliberately filesystem-free (bytes come from an injected reader, so there's
/// no mtime/hash to stat here; see the module doc comment).
fn make_source(location: &str, contents: String) -> SourceFile {
    SourceFile {
        parser:   Parser::from_filename(location).unwrap_or(Parser::Tptp { options: None }),
        name:     file_name(location),
        path:     PathBuf::from(location),
        origin:   if is_url(location) { FileOrigin::Remote } else { FileOrigin::Local(LocalProvenance::UNKNOWN) },
        contents,
        prebuilt: None,
    }
}

/// Blank every top-level formula NOT named in `wanted`, replacing its characters
/// with spaces while keeping newlines — so the file's line count and the kept
/// formulas' line numbers are untouched.
fn blank_unselected(contents: &str, wanted: &HashSet<String>) -> String {
    let mut bytes = contents.as_bytes().to_vec();
    for (name, start, end) in tptp_statement_spans(contents) {
        let keep = name.as_deref().is_some_and(|n| wanted.contains(n));
        if !keep {
            for b in &mut bytes[start..end] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
        }
    }
    // Spans are char-aligned at their boundaries and we only swap a byte for a
    // space (or keep newlines), so the result is still valid UTF-8.
    String::from_utf8(bytes).unwrap_or_else(|_| contents.to_string())
}

// -- location string helpers (path- and URL-aware) ----------------------------

fn is_url(s: &str) -> bool {
    s.contains("://")
}

/// The last `/`-separated segment of a location (`…/Foo.ax` → `Foo.ax`).
fn file_name(location: &str) -> String {
    location.rsplit('/').find(|s| !s.is_empty()).unwrap_or(location).to_string()
}

/// The "directory" of a location — everything up to (not including) the last
/// `/`, ignoring a leading `scheme://`.
fn dir_of(loc: &str) -> String {
    let start = loc.find("://").map(|i| i + 3).unwrap_or(0);
    match loc[start..].rfind('/') {
        Some(rel) => loc[..start + rel].to_string(),
        None => loc.to_string(),
    }
}

/// One directory level up from `dir`, or `None` at the root / host.
fn parent_dir(dir: &str) -> Option<String> {
    let start = dir.find("://").map(|i| i + 3).unwrap_or(0);
    dir[start..].rfind('/').map(|rel| dir[..start + rel].to_string())
}

/// Join a relative include onto a directory location with exactly one `/`.
fn join_loc(dir: &str, rel: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), rel)
}

/// Byte spans (`start..end`, with `end` just past the terminating `.`) of each
/// top-level TPTP statement, tagged with the annotated formula's NAME when it
/// has one.  Comments and quoted strings are skipped lexically — exactly the
/// fidelity the selective-include blanking needs.
fn tptp_statement_spans(text: &str) -> Vec<(Option<String>, usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                while i < bytes.len() && bytes[i] != b'\n' { i += 1; }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
            b'\'' | b'"' => {
                if start.is_none() { start = Some(i); }
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
                    if bytes[i] == quote { i += 1; break; }
                    i += 1;
                }
                continue;
            }
            b'(' | b'[' => { if start.is_none() { start = Some(i); } depth += 1; }
            b')' | b']' => { depth -= 1; }
            b'.' if depth == 0 => {
                let s = start.unwrap_or(i);
                let end = i + 1;
                let slice = text[s..end].trim();
                if !slice.is_empty() {
                    out.push((statement_name(slice), s, end));
                }
                start = None;
            }
            b if !b.is_ascii_whitespace() => {
                if start.is_none() { start = Some(i); }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// `fof(NAME, role, …)` → NAME (quotes stripped).
fn statement_name(stmt: &str) -> Option<String> {
    let open = stmt.find('(')?;
    let kw = stmt[..open].trim();
    if !matches!(kw, "fof" | "cnf" | "tff" | "thf" | "tcf") {
        return None;
    }
    let rest = &stmt[open + 1..];
    let comma = rest.find(',')?;
    Some(rest[..comma].trim().trim_matches('\'').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AX: &str = "% a comment\n\
fof(keep_a, axiom, p(a)).\n\
fof(drop_b, axiom,\n    q(b)).\n\
fof(keep_c, axiom, r(c)).\n";

    #[test]
    fn selective_include_blanks_unselected_preserving_lines() {
        let read = |loc: &str| -> Result<String, String> {
            if loc == "/probs/Axioms/T001.ax" { Ok(AX.to_string()) }
            else { Err(format!("not found: {loc}")) }
        };
        let top = "include('Axioms/T001.ax', [keep_a, keep_c]).\n\
fof(goal, conjecture, p(a)).\n";
        let out = resolve_includes(top, "/probs/prob.p", &read).unwrap();

        assert_eq!(out.len(), 2, "problem + one resolved include");
        // Problem file: the include line is blanked, the conjecture survives.
        assert!(!out[0].contents.contains("include("), "include blanked: {:?}", out[0].contents);
        assert!(out[0].contents.contains("fof(goal"));
        // Included file: selected formulas kept, the rest blanked, lines stable.
        let inc = &out[1].contents;
        assert!(inc.contains("keep_a") && inc.contains("keep_c"), "{inc:?}");
        assert!(!inc.contains("drop_b") && !inc.contains("q(b)"), "unselected blanked: {inc:?}");
        assert_eq!(inc.lines().count(), AX.lines().count(), "line numbers preserved");
        assert!(matches!(out[1].origin, FileOrigin::Local(_)));
    }

    #[test]
    fn whole_file_include_keeps_everything() {
        let read = |loc: &str| -> Result<String, String> {
            if loc == "/probs/Axioms/T001.ax" { Ok(AX.to_string()) }
            else { Err(format!("not found: {loc}")) }
        };
        let out = resolve_includes("include('Axioms/T001.ax').\n", "/probs/prob.p", &read).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[1].contents.contains("drop_b"), "whole-file keeps all: {:?}", out[1].contents);
    }

    #[test]
    fn include_resolves_against_remote_base() {
        // base is a URL; the include resolves via `<base dir>/../` = problems root.
        let read = |loc: &str| -> Result<String, String> {
            if loc == "https://example.com/problems/Axioms/test.ax" {
                Ok("fof(a, axiom, p).\n".to_string())
            } else {
                Err(format!("404 {loc}"))
            }
        };
        let top = "include('Axioms/test.ax').\nfof(g, conjecture, p).\n";
        let out = resolve_includes(top, "https://example.com/problems/TPTP/test.p", &read).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[1].contents.contains("fof(a"));
        assert!(matches!(out[1].origin, FileOrigin::Remote), "remote origin from scheme");
    }

    #[test]
    fn non_tptp_include_is_an_error() {
        let read = |_: &str| -> Result<String, String> { Ok(String::new()) };
        let err = resolve_includes("include('foo.kif').\n", "/p/prob.p", &read).unwrap_err();
        assert!(err.contains("TPTP file"), "{err}");
    }
}
