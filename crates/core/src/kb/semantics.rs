//! Public re-exports of semantic operations.
use crate::{SentenceId, Diagnostic, ToDiagnostic};
use crate::layer::{TopLayer, Layer};

use super::KnowledgeBase;

impl<L: TopLayer + Layer> KnowledgeBase<L> {
    // -- Semantic queries ------------------------------------------------------

    /// True if `sym` is declared (or inferred) to be an instance.
    pub fn is_instance(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic().is_instance(sym)
    }

    /// True if `sym` is a class.
    pub fn is_class(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic().is_class(sym)
    }

    /// True if `sym` is a relation.
    pub fn is_relation(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic().is_relation(sym)
    }

    /// True if `sym` is a function.
    pub fn is_function(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic().is_function(sym)
    }

    /// True if `sym` is a predicate.
    pub fn is_predicate(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.semantic().is_predicate(sym)
    }

    /// Axiom sentences in which `sym` occurs.
    pub fn sym_refs(&self, sym: crate::types::SymbolId) -> Vec<SentenceId> {
        self.layer.semantic().syntactic.axiom_sentences_of(sym).iter().copied().collect()
    }

    /// True if `sym` has `ancestor` (by name) somewhere in its taxonomy.
    pub fn has_ancestor(&self, sym: crate::types::SymbolId, ancestor: &str) -> bool {
        self.layer.semantic().has_ancestor_by_name(sym, ancestor)
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
        let store   = &self.layer.semantic().syntactic;

        // Canonical declarations with this symbol as arg 1.
        const DECLARATIONS: &[&str] = &[
            "subclass", "instance", "subrelation", "subAttribute",
            "documentation",
        ];
        for &head in DECLARATIONS {
            for sid in store.by_head(head).iter().copied() {
                let Some(sent) = store.sentence(sid) else { continue };
                if matches!(
                    sent.elements.get(1),
                    Some(crate::types::Element::Symbol(sym)) if sym.id() == sym_id
                ) {
                    // Source location comes from the source AST; `None` ⇒ synthetic.
                    if let Some(span) = store.source_span_of(sid) {
                        return Some((sid, span));
                    }
                }
            }
        }

        // Fall back to any root where symbol is the head.
        for sid in store.by_head(symbol).iter().copied() {
            if let Some(span) = store.source_span_of(sid) {
                return Some((sid, span));
            }
        }
        None
    }

    /// Expected domain class for argument `arg_idx` (1-based) of
    /// relation `head`, or `None` when the relation has no explicit
    /// `(domain head arg_idx class)` axiom for this position.
    ///
    /// Returns the declared class name (instance-of / subclass-of flag folded
    /// away).  Callers that need the distinction use the lower-level
    /// `SemanticLayer::domain` path.
    pub fn expected_arg_class(&self, head: &str, arg_idx: usize) -> Option<String> {
        let head_id   = self.symbol_id(head)?;
        let domains   = self.layer.semantic().domain(head_id);
        // `arg_idx` is 1-based (element-index convention); `domains`
        // is 0-based.
        if arg_idx == 0 || arg_idx > domains.len() { return None; }
        let rd = &domains[arg_idx - 1];
        let class_id = rd.id()?;
        self.sym_name(class_id)
    }
    
    // -- Validation ------------------------------------------------------------
    //
    // Every public validation entrypoint returns a flat `Vec<Diagnostic>`:
    // warnings AND hard errors together — tell them apart by
    // `Diagnostic.severity` — each tagged with the implicated sentence(s) in
    // `Diagnostic.sids`.  An EMPTY vector means the target validated cleanly.
    // The entrypoints differ only in *what* is validated (one sentence / the
    // whole KB / a session / a file / several files) and the scope they reason
    // in; all funnel through the private `validate_sids`.

    /// Validate one sentence in the global (`Base`) scope.
    pub fn validate_sentence(&self, sid: SentenceId) -> Vec<Diagnostic> {
        crate::with_guard!(self);
        self.validate_sids(&[sid], crate::semantics::types::Scope::Base)
    }

    /// Validate one sentence in the context of `session` (`Base` ∪ that
    /// session's transient overlay) — the single-sentence analogue of
    /// [`Self::validate_session`].  Use this to re-check just-edited input
    /// against the declarations the session itself introduced (a transient
    /// `domain`/`subclass`/… that the global `Base` view can't see).
    pub fn validate_sentence_in_session(&self, sid: SentenceId, session: &str) -> Vec<Diagnostic> {
        crate::with_guard!(self);
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;
        self.validate_sids(&[sid], Scope::Session(session_id(session)))
    }

    /// Validate every root sentence in the KB, reasoning globally (`Base`).
    pub fn validate_all(&self) -> Vec<Diagnostic> {
        crate::with_guard!(self);
        let roots: Vec<SentenceId> = self.layer.semantic().syntactic.root_sids();
        self.validate_sids(&roots, crate::semantics::types::Scope::Base)
    }

    /// Validate only the sentences belonging to `session`, reasoning in that
    /// session's [`Scope`](crate::semantics::types::Scope) (`Base` ∪ the
    /// session's transient overlay) so its own taxonomy/type declarations are
    /// visible — unlike [`Self::validate_all`], which is global.
    ///
    /// Use this after `load_kif` to validate just the new input.  Reads session
    /// membership from the session cache (the live source of truth).
    pub fn validate_session(&self, session: &str) -> Vec<Diagnostic> {
        crate::with_guard!(self);
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;
        let sids = self.session_sids(session);
        self.validate_sids(&sids, Scope::Session(session_id(session)))
    }

    /// Validate only the sentences whose source file tag is `file_tag` (global
    /// scope).  Surfaces diagnostics about *that* input rather than re-emitting
    /// every pre-existing warning in the wider KB.  Unknown / unloaded tags
    /// yield an empty vector.  Tags match `SyntacticLayer::file_roots` keys
    /// exactly (the path a file was loaded under, e.g. `/tmp/x.kif`).
    pub fn validate_file(&self, file_tag: &str) -> Vec<Diagnostic> {
        crate::with_guard!(self);
        let sids = self.layer.semantic().syntactic.file_root_sids(file_tag);
        self.validate_sids(&sids, crate::semantics::types::Scope::Base)
    }

    /// Validate every sentence whose file tag is in `file_tags` (global scope),
    /// merged and deduped.  Convenience for CLI handlers passed several `-f`/`-d`.
    pub fn validate_files<I, S>(&self, file_tags: I) -> Vec<Diagnostic>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        crate::with_guard!(self);
        let mut sids: Vec<SentenceId> = Vec::new();
        for tag in file_tags {
            sids.extend(self.layer.semantic().syntactic.file_root_sids(tag.as_ref()));
        }
        sids.sort_unstable();
        sids.dedup();
        self.validate_sids(&sids, crate::semantics::types::Scope::Base)
    }

    /// The single implementation behind every public validate entrypoint:
    /// validate each of `sids` in `scope` and flatten the results to
    /// [`Diagnostic`]s.  Every `SemanticError` becomes a `Diagnostic` (via
    /// [`ToDiagnostic`]), tagged with its originating `sid` when the variant
    /// doesn't already carry one — so attribution survives even for symbol-level
    /// findings.  Parallel under `feature = "parallel"`; each worker builds its
    /// own validator (a cheap borrow) so there's no cross-thread sharing.
    fn validate_sids(&self, sids: &[SentenceId], scope: crate::semantics::types::Scope) -> Vec<Diagnostic> {
        let one = |sid: SentenceId| -> Vec<Diagnostic> {
            self.layer.semantic().validation_scoped(sid, scope)
                .iter()
                .map(move |e| {
                    let mut d = e.to_diagnostic();
                    if d.sids.is_empty() { d.sids = vec![sid]; }
                    // Anchor the diagnostic at the *root* formula's source span,
                    // so findings on nested sub-sentences (which carry no span of
                    // their own) still report the enclosing formula's file:line.
                    if d.range.file.is_empty() {
                        if let Some(span) = self.layer.semantic().syntactic.source_span(sid) {
                            d.range = span;
                        }
                    }
                    d
                })
                .collect()
        };
        #[cfg(feature = "parallel")]
        {
            use rayon::prelude::*;
            sids.par_iter().flat_map_iter(|&sid| one(sid)).collect()
        }
        #[cfg(not(feature = "parallel"))]
        {
            sids.iter().flat_map(|&sid| one(sid)).collect()
        }
    }

}

#[cfg(test)]
mod tests {
    use crate::KnowledgeBase;

    #[test]
    fn validate_clean_target_yields_empty_vec() {
        // The contract: an empty diagnostic vector means "validated cleanly".
        // A session with no sentences has nothing to flag.
        let kb = KnowledgeBase::new();
        assert!(kb.validate_session("nonexistent").is_empty());
        assert!(kb.validate_all().is_empty(), "an empty KB validates clean");
    }

    #[test]
    fn validate_sentence_in_session_uses_session_scope() {
        // Session `s` transiently declares `likes` a relation and uses it in
        // `(likes Foo Bar)` — none of it promoted.  Validated globally (`Base`),
        // `likes` is an undeclared head → HeadNotRelation (E002); validated in
        // the session's scope, the transient `(instance likes BinaryRelation)`
        // makes `likes` a relation, so that finding disappears.
        let mut kb = KnowledgeBase::new();
        let r = kb.tell(
            "(subclass BinaryRelation Relation)(instance likes BinaryRelation)(likes Foo Bar)",
            "s");
        assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
        let sid = *kb.layer.semantic.syntactic.by_head("likes").iter().next()
            .expect("a (likes ...) root");

        let base = kb.validate_sentence(sid);
        assert!(base.iter().any(|d| d.code == "head-not-relation"),
            "Base never saw the declaration → HeadNotRelation; got {:?}",
            base.iter().map(|d| d.code).collect::<Vec<_>>());

        let scoped = kb.validate_sentence_in_session(sid, "s");
        assert!(!scoped.iter().any(|d| d.code == "head-not-relation"),
            "session scope sees `likes` as a relation → no HeadNotRelation; got {:?}",
            scoped.iter().map(|d| d.code).collect::<Vec<_>>());
    }

    #[test]
    fn validate_session_returns_diagnostics_carrying_sids() {
        // `(Foo Bar Baz)` is headed by an undeclared relation and mentions
        // symbols with no `Entity` ancestry → the validator raises diagnostics.
        // The API returns them as a flat Vec<Diagnostic>, each tagged with the
        // originating sentence (even symbol-level findings, via the attribution
        // fallback in `validate_sids`).
        let mut kb = KnowledgeBase::new();
        let r = kb.tell("(Foo Bar Baz)", "s");
        assert!(r.ok, "ingest failed: {:?}", r.diagnostics);

        let diags = kb.validate_session("s");
        assert!(!diags.is_empty(), "an ill-formed sentence must yield diagnostics");
        for d in &diags {
            assert!(!d.sids.is_empty(), "every diagnostic must carry its sentence id");
            assert_eq!(d.kind, "semantic");
        }
    }
}
#[cfg(test)]
mod session_validate_probe {
    use crate::KnowledgeBase;

    /// After file ingest + promotion, the taxonomy is live in Base.
    #[test]
    fn promoted_file_load_populates_base_taxonomy() {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(
            "(instance orientation TernaryPredicate)",
            &std::path::PathBuf::from("m1.kif"), "load");
        assert!(r.ok);
        let syn = kb.store_for_testing();
        let o = syn.sym_id("orientation").unwrap();
        // Unpromoted: the edge is session-scoped, not Base.
        assert!(kb.semantic().parents_of(o).is_empty(),
            "transient roots must not populate the Base taxonomy");
        #[cfg(feature = "ask")]
        kb.make_session_axiomatic("load").expect("promote");
        #[cfg(not(feature = "ask"))]
        kb.make_session_axiomatic("load").expect("promote");
        assert!(!kb.semantic().parents_of(o).is_empty(),
            "promotion must surface the instance edge in Base");
    }

    /// Session-scoped validation sees Base declarations: a session
    /// fact whose relation is declared in promoted base axioms must
    /// not warn "not a declared relation".
    #[test]
    fn session_validation_sees_base_declarations() {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(
            "(subclass Relation Entity)\n\
             (subclass TernaryPredicate Relation)\n\
             (instance orientation TernaryPredicate)\n\
             (subclass Object Entity)\n\
             (instance Right Entity)",
            &std::path::PathBuf::from("base.kif"), "load");
        assert!(r.ok);
        #[cfg(feature = "ask")]
        kb.make_session_axiomatic("load").expect("promote");
        #[cfg(not(feature = "ask"))]
        kb.make_session_axiomatic("load").expect("promote");

        assert!(kb.tell("(orientation A B Right)", "case").ok);
        let diags = kb.validate_session("case");
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            !messages.iter().any(|m| m.contains("not a declared relation")),
            "declared relation must not warn; got {:?}", messages);
    }
}
