// crates/sumo-kb/src/vampire/mod.rs
//
// Vampire-specific cache and native TFF Problem builder for sumo-kb.
//
// The KnowledgeBase holds one `Option<VampireAxiomCache>` field.  On the
// first `ask()` or `ask_embedded()` call after a KB mutation that changes the
// axiom set, the cache is rebuilt lazily.  At query time:
//
//   Subprocess path (always): cached TFF TPTP string → prepend to query TPTP.
//   Embedded path (integrated-prover): cached native Problem → clone + append conjecture.
//
// Gated: requires the `vampire` feature.

use std::collections::HashSet;

use crate::semantic::SemanticLayer;
use crate::tptp::{TptpOptions, TptpLang, kb_to_tptp};
use crate::types::SentenceId;

#[cfg(feature = "integrated-prover")]
pub(crate) mod convert;

#[cfg(feature = "integrated-prover")]
use vampire_prover::{Formula, Options as VOptions, Problem};

#[cfg(feature = "integrated-prover")]
use convert::{TffConverter, alloc_vars_tff, collect_bound_var_names, wrap_free_vars_tff};

/// Pre-generated axiom data for the Vampire theorem prover.
pub(crate) struct VampireAxiomCache {
    /// Full TFF TPTP string: type-declaration preamble + all axiom formulas.
    /// Used by the subprocess runner (passed directly without re-parsing).
    pub axiom_tptp: String,

    /// Native vampire-prover Problem pre-loaded with all axiom formulas (TFF-typed).
    /// Used by the embedded runner: clone this, append the conjecture, then solve.
    /// The clone is cheap — it copies formula indices, not the C++ term algebra.
    #[cfg(feature = "integrated-prover")]
    pub axiom_problem: Problem,
}

impl VampireAxiomCache {
    /// Build the cache from the current axiom set.
    ///
    /// Serialises `axiom_ids` to TFF TPTP (for subprocess) and, when the
    /// `integrated-prover` feature is enabled, also builds a native TFF Problem.
    pub fn build(layer: &SemanticLayer, axiom_ids: &HashSet<SentenceId>) -> Self {
        // -- TFF TPTP string (used by subprocess runner) -----------------------
        let opts = TptpOptions {
            lang:         TptpLang::Tff,
            hide_numbers: true,
            ..TptpOptions::default()
        };
        let axiom_tptp = kb_to_tptp(layer, "kb", &opts, axiom_ids, &HashSet::new());
        log::debug!(target: "sumo_kb::vampire",
            "axiom cache built: {} axiom(s), {} bytes of TFF TPTP",
            axiom_ids.len(), axiom_tptp.len());

        // -- Native TFF Problem (used by embedded runner) ----------------------
        #[cfg(feature = "integrated-prover")]
        let axiom_problem = {
            build_tff_problem(layer, axiom_ids)
        };

        VampireAxiomCache {
            axiom_tptp,
            #[cfg(feature = "integrated-prover")]
            axiom_problem,
        }
    }
}

// -- Native TFF Problem builder ------------------------------------------------

/// Build a vampire-prover `Problem` containing all axiom sentences in TFF form.
///
/// Iterates every sentence in `axiom_ids`, converts it to a typed `Formula`,
/// and adds it as an axiom.  Sentences that cannot be converted (e.g. complex
/// row-variable expansions) are silently skipped.
#[cfg(feature = "integrated-prover")]
pub(crate) fn build_tff_problem(
    layer:     &SemanticLayer,
    axiom_ids: &HashSet<SentenceId>,
) -> Problem {
    let mut problem = Problem::new(VOptions::new());
    let mut skipped = 0usize;

    let mut n_added = 0usize;
    for &sid in axiom_ids {
        if let Some(f) = convert_sid_tff_top(layer, sid, false) {
            problem.with_axiom(f);
            n_added += 1;
        } else {
            skipped += 1;
        }
    }

    log::debug!(target: "sumo_kb::vampire",
        "TFF problem builder: added={} skipped={}", n_added, skipped);
    problem
}

/// Convert one sentence to a top-level TFF formula (with free-variable wrapping).
///
/// Returns `None` if the sentence cannot be converted (e.g. unsupported structure).
#[cfg(feature = "integrated-prover")]
pub(crate) fn convert_sid_tff_top(
    layer:       &SemanticLayer,
    sid:         SentenceId,
    existential: bool,   // true for conjecture (free vars wrapped in ∃)
) -> Option<Formula> {
    let store = &layer.store;
    let (vars, var_ids, _) = alloc_vars_tff(sid, store, 0);
    let mut bound: HashSet<String> = HashSet::new();
    collect_bound_var_names(sid, store, &mut bound);

    let mut conv = TffConverter::new(store, layer, &vars, &var_ids);
    let formula = conv.sid_to_formula(sid)?;
    Some(wrap_free_vars_tff(formula, &vars, &var_ids, &bound, layer, existential))
}
