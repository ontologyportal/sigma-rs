// -- options.rs ----------------------------------------------------------------
//
// Public configuration types for TPTP output.
//
// `TptpOptions` is passed all the way down through every translation call.
// It is constructed once by the CLI and never mutated during translation.

use std::collections::HashSet;

/// Which TPTP dialect to emit.
///
/// - `Fof`  -- First-order Form.  No sort annotations; every term is `$i`.
///           Variables are implicitly universally quantified at the top level.
///           Used for classic Vampire / E invocation.
///
/// - `Tff`  -- Typed First-order Form.  Variables carry explicit sort annotations
///           (`$i`, `$int`, `$rat`, `$real`, `$o`) and every predicate/function
///           symbol must have a `tff(name, type, ...)` declaration in the
///           preamble.  Enables arithmetic reasoning in Vampire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TptpLang {
    #[default]
    Fof,
    Tff,
}

impl TptpLang {
    pub fn as_str(self) -> &'static str {
        match self {
            TptpLang::Fof => "fof",
            TptpLang::Tff => "tff",
        }
    }
}

/// Options controlling TPTP output.
#[derive(Debug, Clone)]
pub struct TptpOptions {
    pub lang:             TptpLang,
    /// Wrap free variables in `?` (existential) instead of `!` (universal).
    /// Used for query/conjecture sentences -- Vampire negates the conjecture
    /// and tries to derive a contradiction, so `?` correctly scopes the
    /// variable bindings we are searching for.
    pub query:            bool,
    /// Replace numeric literals with `n__N` tokens (default false).
    /// Useful for FOF output where numerics have no special meaning and
    /// would otherwise be treated as uninterpreted constants by the prover.
    /// Ignored in TFF mode (numerics are native `$int`/`$real` literals).
    pub hide_numbers:     bool,
    /// Head predicates whose sentences are omitted from KB output entirely.
    /// Defaults include `documentation`, `format`, `domain`, `range`, etc.
    /// These are SUMO bookkeeping predicates that add noise rather than
    /// useful logical content for a theorem prover.
    /// NOTE: `domain`/`range` are excluded as top-level *axioms* but their
    /// TFF *type declarations* are still emitted (see `tff.rs::is_structural_meta`).
    pub excluded:         HashSet<String>,
    /// Emit a `% <original KIF>` comment before each TPTP formula.
    pub show_kif_comment: bool,
}

impl Default for TptpOptions {
    fn default() -> Self {
        let mut excluded = HashSet::new();
        // Exclude the SUMO relations that are more for internal use and will just muck up the
        //  theorem prover
        excluded.insert("documentation".to_string());
        excluded.insert("domain".to_string());
        excluded.insert("format".to_string());
        excluded.insert("termFormat".to_string());
        excluded.insert("externalImage".to_string());
        excluded.insert("relatedExternalConcept".to_string());
        excluded.insert("relatedInternalConcept".to_string());
        excluded.insert("formerName".to_string());
        excluded.insert("abbreviation".to_string());
        excluded.insert("conventionalShortName".to_string());
        excluded.insert("conventionalLongName".to_string());
        TptpOptions { lang: TptpLang::default(), query: false, hide_numbers: false, excluded, show_kif_comment: false }
    }
}

impl TptpOptions {
    pub fn default_with_hide_numbers() -> Self {
        Self { hide_numbers: true, ..Self::default() }
    }
}
