// crates/core/src/kb/semantic.rs
//
// Public re-exports of semantic operations
use super::KnowledgeBase;

use crate::{SentenceId};
use crate::semantics::errors::{Findings, SemanticError};

impl KnowledgeBase {
    // -- Semantic queries ------------------------------------------------------

    pub fn is_instance(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic.is_instance(sym)
    }

    pub fn is_class(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic.is_class(sym)
    }

    pub fn is_relation(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic.is_relation(sym)
    }

    pub fn is_function(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic.is_function(sym)
    }

    pub fn is_predicate(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic.is_predicate(sym)
    }

    pub fn sym_refs(&self, sym: crate::types::SymbolId) -> Vec<SentenceId> {
        self.layer.semantic.syntactic.axiom_sentences_of(sym).to_vec()
    }

    pub fn has_ancestor(&self, sym: crate::types::SymbolId, ancestor: &str) -> bool {
        self.layer.semantic.has_ancestor_by_name(sym, ancestor)
    }

    /// Return the most-specific class from `classes` according to the
    /// loaded subclass taxonomy.  See
    /// [`crate::semantic::SemanticLayer::most_specific_class`] for the
    /// detailed contract: returns the candidate whose every co-candidate
    /// is one of its ancestors, or `None` when no such candidate exists.
    pub fn most_specific_class(&self, classes: &[&str]) -> Option<String> {
        self.layer.semantic.most_specific_class(classes)
    }

    /// Defining sentence for `symbol`, by heuristic: the first
    /// `(subclass sym _)`, `(instance sym _)`, `(subrelation sym _)`,
    /// `(subAttribute sym _)`, or `(documentation sym _ _)`
    /// root sentence, in that priority order.  Returns the
    /// `(SentenceId, Span)` of that sentence so the caller can
    /// resolve the source location (e.g. LSP goto-definition).
    ///
    /// Falls back to any root where `symbol` appears as the head,
    /// then to any root where it appears at all.  `None` when the
    /// symbol has no declarations anywhere.
    pub fn defining_sentence(&self, symbol: &str) -> Option<(SentenceId, crate::Span)> {
        let sym_id  = self.symbol_id(symbol)?;
        let store   = &self.layer.semantic.syntactic;

        // Priority 1: canonical declarations -- subclass / instance /
        // subrelation / subAttribute with this symbol as arg 1.
        const DECLARATIONS: &[&str] = &[
            "subclass", "instance", "subrelation", "subAttribute",
            "documentation",
        ];
        for &head in DECLARATIONS {
            for &sid in store.by_head(head) {
                let sent = &store.sentences[store.sent_idx(sid)];
                if matches!(
                    sent.elements.get(1),
                    Some(crate::types::Element::Symbol { id, .. }) if *id == sym_id
                ) {
                    if !sent.span.is_synthetic() {
                        return Some((sid, sent.span.clone()));
                    }
                }
            }
        }

        // Priority 2: any root where symbol is the head.  O(1)
        // id -> &Symbol via `symbol_of`.
        let sym_vec = store.symbol_of(sym_id)?;
        for &sid in &sym_vec.head_sentences {
            let sent = &store.sentences[store.sent_idx(sid)];
            if !sent.span.is_synthetic() {
                return Some((sid, sent.span.clone()));
            }
        }
        None
    }

    /// Expected domain class for argument `arg_idx` (1-based) of
    /// relation `head`, or `None` when the relation has no explicit
    /// `(domain head arg_idx class)` axiom for this position.
    ///
    /// Completion and validation both want this: context-aware
    /// completion filters the candidate list by the expected class;
    /// arity / domain checks use the same data from a different
    /// angle.  The return is the declared class name (instance-of
    /// or subclass-of flag folded away) -- callers that care about
    /// the distinction (e.g. TFF sort derivation) use the lower-level
    /// `SemanticLayer::domain` path.
    pub fn expected_arg_class(&self, head: &str, arg_idx: usize) -> Option<String> {
        let head_id   = self.symbol_id(head)?;
        let domains   = self.layer.semantic.domain(head_id);
        // `arg_idx` is 1-based (element-index convention); `domains`
        // is 0-based.
        if arg_idx == 0 || arg_idx > domains.len() { return None; }
        let rd = &domains[arg_idx - 1];
        let class_id = rd.id();
        // Sentinel `u64::MAX` means "no explicit domain for this arg".
        if class_id == u64::MAX { return None; }
        self.sym_name(class_id)
    }
    
    // -- Validation ------------------------------------------------------------

    pub fn validate_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        self.layer.semantic.validate_sentence(sid)
    }

    /// Run semantic validation on `sid` and return every finding
    /// (warnings + hard errors).
    ///
    /// Unlike [`Self::validate_sentence`], this does not honour the
    /// CLI's `-Wall` / `--warning=<code>` promotion flags -- it
    /// always returns the raw set of checks the validator
    /// performed.  The caller decides how to surface them (the
    /// LSP maps each to an LSP diagnostic using `is_warn()` to
    /// pick a severity).
    pub fn validate_sentence_all(&self, sid: SentenceId) -> Vec<SemanticError> {
        self.layer.semantic.validate_sentence_collect(sid)
    }

    pub fn validate_all(&self) -> Vec<(SentenceId, SemanticError)> {
        self.layer.semantic.validate_all()
    }

    /// Validate only the sentences belonging to `session`.
    ///
    /// Use this after `load_kif` to perform end-of-load validation without
    /// re-validating the entire base KB.
    pub fn validate_session(&self, session: &str) -> Vec<(SentenceId, SemanticError)> {
        let sids = self.sessions.get(session).cloned().unwrap_or_default();
        sids.iter()
            .filter_map(|&sid| self.layer.semantic.validate_sentence(sid).err().map(|e| (sid, e)))
            .collect()
    }

    // -- Classified-findings entry points ------------------------------------
    //
    // Following Option B of the warning-print extraction: every
    // semantic finding (warning or hard error) is captured via
    // `with_collector`, classified by `SemanticError::is_warn`, and
    // returned to the caller.  `sigmakee-rs-core` no longer prints; consumers
    // (CLI, SDK, LSP) decide how to render.  See `crate::Findings`.

    /// Validate one sentence and return EVERY finding (warnings +
    /// hard errors), pre-classified.
    ///
    /// Same coverage as [`Self::validate_sentence_all`] but in the
    /// classified [`Findings`] shape so callers don't have to
    /// partition by [`SemanticError::is_warn`] themselves.
    pub fn validate_sentence_findings(&self, sid: SentenceId) -> Findings {
        let mut f = Findings::default();
        let (_, errs) = super::error::with_collector(|| self.layer.semantic.validate_sentence(sid));
        for e in errs {
            f.push(sid, e);
        }
        f
    }

    /// Validate every root sentence in the KB and return classified
    /// [`Findings`].
    ///
    /// Equivalent to looping [`Self::validate_sentence_findings`]
    /// over `kb.iter_files()`'s roots.  Wraps each per-sentence
    /// validation in its own `with_collector` so attribution is
    /// preserved sentence-by-sentence; if you want a flat list, use
    /// [`Self::validate_all`] (errors only) or accumulate from this
    /// `Findings`.
    pub fn validate_all_findings(&self) -> Findings {
        let mut f = Findings::default();
        for &sid in self.layer.semantic.syntactic.roots.iter() {
            let (_, errs) = super::error::with_collector(|| self.layer.semantic.validate_sentence(sid));
            for e in errs {
                f.push(sid, e);
            }
        }
        f
    }

    /// Validate only the sentences belonging to `session`, returning
    /// classified [`Findings`].
    ///
    /// Counterpart of [`Self::validate_session`] for the everything-
    /// classified flow.  Use this from CLI handlers that want to
    /// render warnings via `semantic_warning!` and abort on the
    /// `errors` list.
    pub fn validate_session_findings(&self, session: &str) -> Findings {
        let sids = self.sessions.get(session).cloned().unwrap_or_default();
        let mut f = Findings::default();
        for sid in sids {
            let (_, errs) = super::error::with_collector(|| self.layer.semantic.validate_sentence(sid));
            for e in errs {
                f.push(sid, e);
            }
        }
        f
    }
}