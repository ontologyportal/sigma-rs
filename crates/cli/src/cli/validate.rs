use log;
use sigmakee_rs_sdk::manager::KBManager;
use sigmakee_rs_sdk::{Diagnostic, SdkError, Session};
use sigmakee_rs_sdk::{KnowledgeBase, ProvingLayer};

use crate::cli::util::read_stdin;

/// Entry point for `sumo validate`.
///
/// Opens the DB (if present) and layers any `-f`/`-d` files as in-memory
/// axioms.  Never writes to the database.
///
/// `formula`: only validate the given formula in the context of the full KB.
/// `parse_only`: skip all semantic checks; only verify the KIF is syntactically valid.
/// `suppress`: `-W`/`--warning` arguments (`"all"` or a specific code/name) —
/// applied as a post-hoc severity flip via [`apply_severity_overrides`], since
/// `SemanticError` severity is no longer global mutable state.
pub fn run_validate<L>(
    session: Session<L>,
    _manager: KBManager,
    formula: Option<String>,
    parse_only: bool,
    suppress: &[String],
) -> bool
where
    L: ProvingLayer,
{
    log::debug!(
        "run_validate: formula={:?}, parse_only={}",
        formula.is_some(),
        parse_only,
    );

    let formula = formula.or_else(read_stdin);

    match formula {
        Some(text) => validate_single_formula(session, &text, parse_only, suppress),
        None => {
            let mut diagnostics = if !parse_only {
                session.validate()
            } else {
                vec![]
            };
            apply_severity_overrides(&mut diagnostics, suppress);

            let scope = session.kb().iter_files().len();
            let (errs, warns) = split_severity(diagnostics);
            let (n_err, n_warn) = print_diags(session.kb(), &errs, &warns);

            println!(
                "Validated {} constituents: {}, {}",
                scope,
                count_phrase(n_err, "error"),
                count_phrase(n_warn, "warning")
            );
            n_err > 0
        }
    }
}

/// `-W`/`--warning` severity-elevation policy, applied as a stateless
/// transform over an already-produced `Vec<Diagnostic>` — never by mutating
/// how core classifies findings (there is no more global promotion state to
/// mutate: each `SemanticError` carries its own intrinsic severity). `-W all`
/// promotes every `Warning` to `Error`; `-W <code-or-name>` promotes only
/// diagnostics matching that specific code (`"E002"`) or kebab-case name
/// (`"head-not-relation"`). `Info`/`Hint` findings are never touched — they're
/// advisory by design, and `-Wall` promoting a documentation hint to a
/// build-breaking error would defeat the point of giving it that severity.
pub fn apply_severity_overrides(diags: &mut [Diagnostic], suppress: &[String]) {
    if suppress.is_empty() {
        return;
    }
    let promote_all = suppress.iter().any(|s| s == "all");
    for d in diags.iter_mut() {
        if d.severity == sigmakee_rs_sdk::Severity::Warning
            && (promote_all || suppress.iter().any(|s| s == d.code))
        {
            d.severity = sigmakee_rs_sdk::Severity::Error;
        }
    }
}

// -- Validate a single inline formula -----------------------------------------

/// Validate one inline KIF formula against the KB.
///
/// Asserts `text` into a session, runs semantic validation (unless
/// `parse_only`), and prints the findings.  Returns `true` if any hard error
/// was found.
pub fn validate_single_formula<L>(
    mut session: Session<L>,
    text: &str,
    parse_only: bool,
    suppress: &[String],
) -> bool
where
    L: ProvingLayer,
{
    log::debug!(
        "validate_single_formula: text={}, parse_only={}",
        text,
        parse_only,
    );

    let mut open_session = match session.tell(text) {
        Err(errs) => {
            for e in errs {
                match e {
                    SdkError::Kb(e) => session.kb().pretty_print_error(&e, log::Level::Error),
                    _ => log::error!("{}", e),
                }
            }
            return false;
        }
        Ok(s) => s,
    };

    if parse_only {
        return true;
    }

    let mut diags: Vec<Diagnostic> = open_session
        .validate()
        .into_iter()
        .filter_map(|e| match e {
            SdkError::Kb(e) => Some(e),
            _ => None,
        })
        .collect();
    apply_severity_overrides(&mut diags, suppress);

    let (errs, warns) = split_severity(diags);
    let (n_err, n_warn) = print_diags(session.kb(), &errs, &warns);

    println!(
        "Validated formula: {}: {}, {}",
        text,
        count_phrase(n_err, "error"),
        count_phrase(n_warn, "warning")
    );
    n_err > 0
}

/// Pluralised `"N thing(s)"` for summary lines (e.g. `1 error`, `3 warnings`).
fn count_phrase(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {}", noun)
    } else {
        format!("{} {}s", n, noun)
    }
}

/// Split diagnostics into (hard errors, advisories) by severity.
fn split_severity(
    diags: Vec<sigmakee_rs_sdk::Diagnostic>,
) -> (
    Vec<sigmakee_rs_sdk::Diagnostic>,
    Vec<sigmakee_rs_sdk::Diagnostic>,
) {
    diags
        .into_iter()
        .partition(|d| matches!(d.severity, sigmakee_rs_sdk::Severity::Error))
}

/// Render errors then warnings via the KB's pretty-printer, collapsing
/// exact-duplicate renderings.
///
/// Returns the deduplicated `(errors, warnings)` counts — how many distinct
/// findings were surfaced.  Warnings are counted even when suppressed.
fn print_diags<L>(
    kb: &KnowledgeBase<L>,
    errors: &[sigmakee_rs_sdk::Diagnostic],
    warnings: &[sigmakee_rs_sdk::Diagnostic],
) -> (usize, usize)
where
    L: ProvingLayer,
{
    let mut seen = std::collections::HashSet::new();
    let mut n_err = 0;
    for d in errors {
        if seen.insert(kb.render_diagnostic(d)) {
            kb.pretty_print_error(d, log::Level::Error);
            eprintln!();
            n_err += 1;
        }
    }
    let mut n_warn = 0;
    for d in warnings {
        if seen.insert(kb.render_diagnostic(d)) {
            n_warn += 1;
            kb.pretty_print_error(d, log::Level::Warn);
            eprintln!();
        }
    }
    (n_err, n_warn)
}
