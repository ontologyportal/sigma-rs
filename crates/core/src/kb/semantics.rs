//! Public re-exports of semantic operations.
use std::collections::HashSet;

use crate::layer::{Layer, TopLayer};
use crate::semantics::consts::{CLASS_SYMBOL, FORMULA_SYMBOL, HIGHER_ORDER_CATEGORIES};
use crate::types::{Element, RelationDomain, RelationRange};
use crate::{Diagnostic, OpKind, SentenceId, SymbolId, ToDiagnostic};

use super::KnowledgeBase;

/// What [`KnowledgeBase::validate`] checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationTarget<'a> {
    /// Every root sentence in the KB.
    All,
    /// One sentence.
    Sentence(SentenceId),
    /// The sentences loaded from one source file, by the tag it was loaded
    /// under (e.g. `/tmp/x.kif`). An unknown tag validates nothing.
    File(&'a str),
    /// The sentences belonging to one session.
    Session(&'a str),
}

/// Aggregate vocabulary + documentation-coverage counts — see
/// [`KnowledgeBase::vocab_stats`].
#[derive(Debug, Clone, Default)]
pub struct VocabStats {
    pub total: usize,
    pub classes: usize,
    pub instances: usize,
    pub relations: usize,
    /// Subset of `relations` classified as predicates / as functions.
    pub predicates: usize,
    pub functions: usize,
    pub documented: usize,
    pub labeled: usize,
    /// Distinct documented symbols per language tag, most-covered first.
    pub doc_languages: Vec<(String, usize)>,
    /// Distinct termFormat-labeled symbols per language tag, most-covered
    /// first.  Many languages ship labels without any documentation strings
    /// (SUMO's German/French/… coverage), so this list is usually longer.
    pub term_languages: Vec<(String, usize)>,
}

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

    /// True if `sid` denotes a truth-valued sentence (an operator
    /// application, a predicate-variable application, or a relation/
    /// predicate-headed atom) rather than a term.
    fn sentence_is_logical(&self, sid: SentenceId) -> bool {
        let Some(sentence) = self.sentence(sid) else {
            return false;
        };
        if sentence.is_operator() {
            return true;
        }
        let head_id = match sentence.elements.first() {
            Some(Element::Symbol(sym)) => sym.id(),
            Some(Element::Variable { .. }) => return true,
            _ => return false,
        };
        !self.is_function(head_id)
    }

    /// True if a formula appears anywhere as an argument to a relation or
    /// function in `sid`'s sentence tree -- i.e. `sid` is a higher-order
    /// sentence rather than a first-order one.
    ///
    /// A formula nested inside a *logical operator*'s own arguments (an
    /// `=>`'s antecedent, an `and`'s conjuncts, a quantifier's body, ...) is
    /// ordinary first-order structure, not higher-order -- operators expect
    /// formula arguments by definition. It's higher-order when a formula
    /// occupies an argument slot of a relation or function instead, or when
    /// a predicate-variable application (`(?REL ?X ?Y)`) occurs anywhere,
    /// including at the root.
    pub fn is_higher_order(&self, sid: SentenceId) -> bool {
        let Some(sentence) = self.sentence(sid) else {
            return false;
        };
        if matches!(sentence.elements.first(), Some(Element::Variable { .. })) {
            return true;
        }
        let is_operator = sentence.is_operator();
        let args_start = if matches!(sentence.op(), Some(OpKind::ForAll) | Some(OpKind::Exists)) {
            2
        } else {
            1
        };
        sentence.elements[args_start..].iter().any(|e| match e {
            Element::Sub(sub_id) => {
                (!is_operator && self.sentence_is_logical(*sub_id)) || self.is_higher_order(*sub_id)
            }
            _ => false,
        })
    }

    /// Names of the [`HIGHER_ORDER_CATEGORIES`] whose predicates occur
    /// anywhere in `sid`'s sentence tree, in declaration order.  A sentence
    /// may fall into several categories, or none.  Does not itself check
    /// [`Self::is_higher_order`].
    pub fn higher_order_categories(&self, sid: SentenceId) -> Vec<&'static str> {
        let syn = &self.layer.semantic().syntactic;
        let syms = syn.sentence_symbols(sid);
        HIGHER_ORDER_CATEGORIES
            .iter()
            .filter(|(_, preds)| {
                preds
                    .split(',')
                    .filter_map(|p| syn.sym_id(p.trim()))
                    .any(|id| syms.contains(&id))
            })
            .map(|(name, _)| *name)
            .collect()
    }

    /// Axiom sentences in which `sym` occurs.
    pub fn sym_refs(&self, sym: crate::types::SymbolId) -> Vec<SentenceId> {
        self.layer
            .semantic()
            .syntactic
            .axiom_sentences_of(sym)
            .iter()
            .copied()
            .collect()
    }

    /// Aggregate vocabulary and documentation-coverage counts for an overview
    /// page.  `classes` / `instances` / `relations` classify every "real"
    /// symbol (KIF variables, scope-qualified interning keys, and skolem
    /// constants excluded — the same notion of vocabulary as `search`); a
    /// symbol may land in several buckets, and `relations` covers predicates
    /// and functions too.  `documented` / `labeled` count distinct symbols
    /// carrying a `documentation` / `termFormat` string; `total` is the
    /// vocabulary size the coverage percentages should divide by.
    pub fn vocab_stats(&self) -> VocabStats {
        let syn = &self.layer.semantic().syntactic;
        let ids = self.real_symbol_ids();

        let mut out = VocabStats {
            total: ids.len(),
            ..VocabStats::default()
        };
        for &id in &ids {
            if self.is_class(id) {
                out.classes += 1;
            }
            if self.is_instance(id) {
                out.instances += 1;
            }
            let pred = self.is_predicate(id);
            let func = self.is_function(id);
            if pred {
                out.predicates += 1;
            }
            if func {
                out.functions += 1;
            }
            if self.is_relation(id) || pred || func {
                out.relations += 1;
            }
        }

        // Distinct documented/labeled subjects, straight off the head indexes.
        // Subject/language slots: (documentation SUBJECT LANG "…"),
        //                         (termFormat LANG SUBJECT "…").
        // Distinct subjects per language tag; the scalar count is the union.
        // Subject/language slots: (documentation SUBJECT LANG "…"),
        //                         (termFormat LANG SUBJECT "…").
        let coverage = |head: &str, subj_slot: usize, lang_slot: usize| {
            let mut union: HashSet<crate::types::SymbolId> = HashSet::new();
            let mut per_lang: std::collections::HashMap<String, HashSet<crate::types::SymbolId>> =
                std::collections::HashMap::new();
            for sid in syn.by_head(head).iter().copied() {
                let Some(sent) = syn.sentence(sid) else {
                    continue;
                };
                let (Some(Element::Symbol(subj)), Some(Element::Symbol(lang))) =
                    (sent.elements.get(subj_slot), sent.elements.get(lang_slot))
                else {
                    continue;
                };
                union.insert(subj.id());
                per_lang
                    .entry(lang.to_string())
                    .or_default()
                    .insert(subj.id());
            }
            let mut langs: Vec<(String, usize)> = per_lang
                .into_iter()
                .map(|(l, set)| (l, set.len()))
                .collect();
            langs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            (union.len(), langs)
        };
        (out.documented, out.doc_languages) = coverage("documentation", 1, 2);
        (out.labeled, out.term_languages) = coverage("termFormat", 2, 1);
        out
    }

    /// Every interned symbol that counts as real KB vocabulary: KIF
    /// variables (`?x`/`@row`), scope-qualified variable interning keys, and
    /// CNF skolem constants are excluded.
    fn real_symbol_ids(&self) -> Vec<SymbolId> {
        let syn = &self.layer.semantic().syntactic;
        let mut ids: Vec<SymbolId> = Vec::new();
        syn.symbols.entries().for_each(|(&sym_id, sym)| {
            let name = sym.name();
            if name.starts_with('?') || name.starts_with('@') {
                return;
            }
            if syn.is_skolem(sym_id) {
                return;
            }
            if crate::syntactic::sentence::is_scoped_variable_name(&name) {
                return;
            }
            ids.push(sym_id);
        });
        ids
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
        let sym_id = self.symbol_id(symbol)?;
        let sem = self.layer.semantic();
        let sid = sem.defining_sentence(sym_id)?;
        Some((sid, sem.syntactic.source_span_of(sid)?))
    }

    /// Expected domain class for argument `arg_idx` (1-based) of
    /// relation `head`, or `None` when `head` isn't interned or `arg_idx`
    /// is out of range for its declared domains.
    ///
    /// Distinguishes instance-of (`RelationDomain::Domain`) from
    /// subclass-of (`RelationDomain::DomainSubclass`) constraints;
    /// `RelationDomain::Unknown` means `head` is interned but declares no
    /// `(domain head arg_idx class)` axiom for this position.
    pub fn domain(&self, head: &str, arg_idx: usize) -> Option<RelationDomain> {
        let head_id = self.symbol_id(head)?;
        let domains = self.layer.semantic().domain(head_id);
        // `arg_idx` is 1-based (element-index convention); `domains`
        // is 0-based.
        if arg_idx == 0 || arg_idx > domains.len() {
            return None;
        }
        let rd = &domains[arg_idx - 1];
        Some(rd.clone())
    }

    /// Expected range class of relation `head`, or `None` when `head` isn't
    /// interned in this KB.
    ///
    /// Distinguishes instance-of (`RelationRange::Range`) from subclass-of
    /// (`RelationRange::RangeSubclass`) constraints -- the latter is SUMO's
    /// convention for a function that itself returns a class-denoting term
    /// rather than an instance; `RelationRange::Unknown` means `head` is
    /// interned but declares no `(range ...)` / `(rangeSubclass ...)` axiom.
    pub fn range(&self, head: &str) -> Option<RelationRange> {
        let head_id = self.symbol_id(head)?;
        Some(self.layer.semantic().range(head_id))
    }

    /// Utility function which checks whether a given ID corresponds to the specific
    /// type which indicates a sub-formula (a formula appearing in a term position)
    pub fn is_formula_type(&self, id: SymbolId) -> bool {
        id == FORMULA_SYMBOL.id()
    }

    /// Utility function which checks whether a given ID corresponds to SUMO's
    /// `Class` symbol -- the implicit superclass of every class-denoting
    /// symbol, whether or not it's explicitly declared `(instance X Class)`
    /// (most classes are declared instances of a narrower subclass of
    /// `Class` instead, e.g. `SetOrClass`). Callers needing "every symbol
    /// that IS a class" for a `Class`-typed domain should use
    /// [`Self::is_class`] over all symbols rather than [`Self::instances_of`],
    /// which would only catch the ones declared instance of `Class` itself.
    pub fn is_class_type(&self, id: SymbolId) -> bool {
        id == CLASS_SYMBOL.id()
    }

    // -- Validation ------------------------------------------------------------

    /// Validate `target`, returning every finding as a [`Diagnostic`] --
    /// warnings and hard errors together (tell them apart by
    /// `Diagnostic.severity`), each tagged with the implicated sentence(s) in
    /// `Diagnostic.sids`. An empty vector means the target validated cleanly.
    ///
    /// `session` picks the scope the checks reason in: `None` is the global
    /// (`Base`) view; `Some(s)` adds session `s`'s transient overlay, so
    /// declarations staged there but not yet promoted are visible (e.g. a
    /// just-typed, correctly-parented class in an editor buffer is not flagged
    /// as underived). A [`ValidationTarget::Session`] reasons in its own
    /// session's scope unless `session` names another.
    pub fn validate(&self, target: ValidationTarget<'_>, session: Option<&str>) -> Vec<Diagnostic> {
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;
        crate::with_guard!(self);
        let syntactic = &self.layer.semantic().syntactic;
        let (sids, session) = match target {
            ValidationTarget::All => (syntactic.root_sids(), session),
            ValidationTarget::Sentence(sid) => (vec![sid], session),
            ValidationTarget::File(tag) => (syntactic.file_root_sids(tag), session),
            ValidationTarget::Session(name) => (self.session_sids(name), session.or(Some(name))),
        };
        let scope = session.map_or(Scope::Base, |s| Scope::Session(session_id(s)));
        self.validate_sids(&sids, scope)
    }

    /// The single implementation behind every public validate entrypoint:
    /// validate each of `sids` in `scope` and flatten the results to
    /// [`Diagnostic`]s.  Every `SemanticError` becomes a `Diagnostic` (via
    /// [`ToDiagnostic`]), tagged with its originating `sid` when the variant
    /// doesn't already carry one — so attribution survives even for symbol-level
    /// findings.  Parallel under `feature = "parallel"`; each worker builds its
    /// own validator (a cheap borrow) so there's no cross-thread sharing.
    fn validate_sids(
        &self,
        sids: &[SentenceId],
        scope: crate::semantics::types::Scope,
    ) -> Vec<Diagnostic> {
        let syntactic = &self.layer.semantic().syntactic;
        let spans = syntactic.source_span_index();
        let one = |sid: SentenceId| -> Vec<Diagnostic> {
            let source_spans = syntactic.source_spans(sid);
            self.layer
                .semantic()
                .validation_scoped(sid, scope)
                .iter()
                .flat_map(|e| {
                    let mut d = e.to_diagnostic();
                    if d.sids.is_empty() {
                        d.sids = vec![sid];
                    }
                    if !d.range.file.is_empty() || source_spans.is_empty() {
                        if d.range.file.is_empty() {
                            if let Some(span) = spans.get(&sid) {
                                d.range = span.clone();
                            }
                        }
                        return vec![d];
                    }
                    source_spans
                        .iter()
                        .cloned()
                        .map(|range| {
                            let mut occurrence = d.clone();
                            occurrence.range = range;
                            occurrence
                        })
                        .collect()
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
    use std::path::PathBuf;

    use crate::{KnowledgeBase, SourceFile};

    #[test]
    fn validation_fans_out_a_finding_to_all_source_occurrences() {
        let mut kb = KnowledgeBase::new();
        let formula = "(UnknownRelation Dog)".to_string();
        assert!(
            kb.load(
                SourceFile::kif(PathBuf::from("one.kif"), formula.clone()),
                "one"
            )
            .ok
        );
        assert!(
            kb.load(SourceFile::kif(PathBuf::from("two.kif"), formula), "two")
                .ok
        );

        let mut findings: Vec<_> = kb
            .validate(crate::ValidationTarget::All, None)
            .into_iter()
            .filter(|d| d.code == "head-not-relation")
            .collect();
        findings.sort_by(|a, b| a.range.file.cmp(&b.range.file));

        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].range.file, "one.kif");
        assert_eq!(findings[1].range.file, "two.kif");
    }

    #[test]
    fn is_class_type_matches_only_the_class_symbol() {
        let mut kb = KnowledgeBase::new();
        let r = kb.tell("(subclass SetOrClass Class)(instance Dog SetOrClass)", "s");
        assert!(r.ok, "ingest failed: {:?}", r.diagnostics);

        let class_id = kb.symbol_id("Class").expect("Class interned");
        let dog_id = kb.symbol_id("Dog").expect("Dog interned");
        assert!(kb.is_class_type(class_id));
        assert!(!kb.is_class_type(dog_id));
    }

    #[test]
    fn is_higher_order_false_for_first_order_rule() {
        let mut kb = KnowledgeBase::new();
        let r = kb.tell("(=> (instance ?X Dog) (instance ?X Mammal))", "s");
        assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
        let sid = r.sids[0];
        assert!(!kb.is_higher_order(sid));
    }

    #[test]
    fn is_higher_order_true_for_predicate_variable_argument() {
        let mut kb = KnowledgeBase::new();
        let r = kb.tell(
            "(instance Relation Class)(=> (instance ?REL Relation) (?REL Fido Fido))",
            "s",
        );
        assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
        let sid = *r
            .sids
            .last()
            .expect("the => rule ingested as the second root");
        assert!(kb.is_higher_order(sid));
    }

    #[test]
    fn is_higher_order_false_for_function_headed_argument() {
        // A function application nested as a term argument is not itself
        // logical -- the rule stays first-order. `is_function` only reasons
        // over promoted (`Base`-scope) axioms, so this needs a file-sourced
        // load plus an explicit promotion, not a transient `tell`.
        let mut kb = KnowledgeBase::new();
        let r = kb.load(
            crate::types::SourceFile::kif(
                std::path::PathBuf::from("t.kif"),
                "(instance AbsoluteValueFn Function)(=> (instance ?X Integer) (greaterThan (AbsoluteValueFn ?X) 0))".to_string(),
            ),
            "t.kif",
        );
        assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("t.kif").expect("promote");
        let sid = *r
            .sids
            .last()
            .expect("the => rule ingested as the second root");
        assert!(!kb.is_higher_order(sid));
    }

    #[test]
    fn higher_order_categories_match_nested_predicates() {
        let mut kb = KnowledgeBase::new();
        let r = kb.tell(
            "(=> (holdsDuring ?T (believes ?A (attribute ?X Happy))) (knows ?A (attribute ?X Happy)))\
             (=> (instance ?REL Relation) (?REL Fido Fido))\
             (=> (instance ?X Dog) (instance ?X Mammal))",
            "s",
        );
        assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
        assert_eq!(
            kb.higher_order_categories(r.sids[0]),
            vec!["temporal", "epistemic"]
        );
        assert!(kb.higher_order_categories(r.sids[1]).is_empty());
        assert!(kb.higher_order_categories(r.sids[2]).is_empty());
    }

    #[test]
    fn higher_order_categories_empty_for_unknown_sentence() {
        let kb = KnowledgeBase::new();
        assert!(kb.higher_order_categories(0).is_empty());
    }

    #[test]
    fn validate_clean_target_yields_empty_vec() {
        // The contract: an empty diagnostic vector means "validated cleanly".
        // A session with no sentences has nothing to flag.
        let kb = KnowledgeBase::new();
        assert!(kb
            .validate(crate::ValidationTarget::Session("nonexistent"), None)
            .is_empty());
        assert!(
            kb.validate(crate::ValidationTarget::All, None).is_empty(),
            "an empty KB validates clean"
        );
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
            "s",
        );
        assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
        let sid = *kb
            .layer
            .semantic
            .syntactic
            .by_head("likes")
            .first()
            .expect("a (likes ...) root");

        let base = kb.validate(crate::ValidationTarget::Sentence(sid), None);
        assert!(
            base.iter().any(|d| d.code == "head-not-relation"),
            "Base never saw the declaration → HeadNotRelation; got {:?}",
            base.iter().map(|d| d.code).collect::<Vec<_>>()
        );

        let scoped = kb.validate(crate::ValidationTarget::Sentence(sid), Some("s"));
        assert!(
            !scoped.iter().any(|d| d.code == "head-not-relation"),
            "session scope sees `likes` as a relation → no HeadNotRelation; got {:?}",
            scoped.iter().map(|d| d.code).collect::<Vec<_>>()
        );
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

        let diags = kb.validate(crate::ValidationTarget::Session("s"), None);
        assert!(
            !diags.is_empty(),
            "an ill-formed sentence must yield diagnostics"
        );
        for d in &diags {
            assert!(
                !d.sids.is_empty(),
                "every diagnostic must carry its sentence id"
            );
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
            &std::path::PathBuf::from("m1.kif"),
            "load",
        );
        assert!(r.ok);
        let syn = kb.store_for_testing();
        let o = syn.sym_id("orientation").unwrap();
        // Unpromoted: the edge is session-scoped, not Base.
        assert!(
            kb.semantic().parents_of(o).is_empty(),
            "transient roots must not populate the Base taxonomy"
        );
        #[cfg(feature = "external-prover")]
        kb.make_session_axiomatic("load").expect("promote");
        #[cfg(not(feature = "external-prover"))]
        kb.make_session_axiomatic("load").expect("promote");
        assert!(
            !kb.semantic().parents_of(o).is_empty(),
            "promotion must surface the instance edge in Base"
        );
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
            &std::path::PathBuf::from("base.kif"),
            "load",
        );
        assert!(r.ok);
        #[cfg(feature = "external-prover")]
        kb.make_session_axiomatic("load").expect("promote");
        #[cfg(not(feature = "external-prover"))]
        kb.make_session_axiomatic("load").expect("promote");

        assert!(kb.tell("(orientation A B Right)", "case").ok);
        let diags = kb.validate(crate::ValidationTarget::Session("case"), None);
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            !messages
                .iter()
                .any(|m| m.contains("not a declared relation")),
            "declared relation must not warn; got {:?}",
            messages
        );
    }
}

#[cfg(test)]
mod completeness_tests {
    use crate::{KnowledgeBase, Severity};

    fn promoted(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("t.kif"), "load");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("load").expect("promote");
        kb
    }

    fn find<'a>(
        diags: &'a [crate::Diagnostic],
        code: &str,
        needle: &str,
    ) -> Option<&'a crate::Diagnostic> {
        diags
            .iter()
            .find(|d| d.code == code && d.message.contains(needle))
    }

    #[test]
    fn missing_documentation_hint_fires_and_is_a_hint() {
        let kb = promoted("(subclass Foo Entity)");
        let diags = kb.validate(crate::ValidationTarget::All, None);
        let d = find(&diags, "missing-documentation", "Foo")
            .expect("Foo has no documentation axiom — should be flagged");
        assert_eq!(d.severity, Severity::Hint);
    }

    #[test]
    fn missing_documentation_hint_absent_when_documented() {
        let kb = promoted("(subclass Foo Entity)\n(documentation Foo EnglishLanguage \"A foo.\")");
        let diags = kb.validate(crate::ValidationTarget::All, None);
        assert!(
            find(&diags, "missing-documentation", "Foo").is_none(),
            "Foo is documented — must not be flagged"
        );
    }

    #[test]
    fn missing_term_format_hint_fires_and_absent_when_present() {
        let undocumented = promoted("(subclass Foo Entity)");
        assert!(find(
            &undocumented.validate(crate::ValidationTarget::All, None),
            "missing-term-format",
            "Foo"
        )
        .is_some());

        let labeled = promoted("(subclass Foo Entity)\n(termFormat EnglishLanguage Foo \"foo\")");
        assert!(find(
            &labeled.validate(crate::ValidationTarget::All, None),
            "missing-term-format",
            "Foo"
        )
        .is_none());
    }

    #[test]
    fn missing_format_string_only_applies_to_relations() {
        let kb = promoted(
            "(subclass BinaryRelation Relation)\n\
             (instance likes BinaryRelation)\n\
             (subclass Foo Entity)",
        );
        let diags = kb.validate(crate::ValidationTarget::All, None);
        assert!(
            find(&diags, "missing-format-string", "likes").is_some(),
            "a relation with no format string should be flagged"
        );
        assert!(
            find(&diags, "missing-format-string", "Foo").is_none(),
            "a non-relation class must never be flagged for a missing format string"
        );
    }

    #[test]
    fn missing_format_string_absent_when_present() {
        let kb = promoted(
            "(subclass BinaryRelation Relation)\n\
             (instance likes BinaryRelation)\n\
             (format EnglishLanguage likes \"%1 likes %2\")",
        );
        assert!(find(
            &kb.validate(crate::ValidationTarget::All, None),
            "missing-format-string",
            "likes"
        )
        .is_none());
    }

    #[test]
    fn multiple_documentation_same_language_fires_with_count() {
        let kb = promoted(
            "(subclass Foo Entity)\n\
             (documentation Foo EnglishLanguage \"A foo.\")\n\
             (documentation Foo EnglishLanguage \"Another foo description.\")",
        );
        let diags = kb.validate(crate::ValidationTarget::All, None);
        let d = find(&diags, "multiple-documentation", "Foo")
            .expect("two English documentation axioms for Foo should be flagged");
        assert_eq!(d.severity, Severity::Hint);
        assert!(
            d.message.contains('2'),
            "message should mention the count: {}",
            d.message
        );
    }

    #[test]
    fn multiple_documentation_different_languages_does_not_fire() {
        // Documented in two DISTINCT languages is normal multilingual
        // coverage, not a duplicate — must not be flagged.
        let kb = promoted(
            "(subclass Foo Entity)\n\
             (documentation Foo EnglishLanguage \"A foo.\")\n\
             (documentation Foo FrenchLanguage \"Un foo.\")",
        );
        assert!(find(
            &kb.validate(crate::ValidationTarget::All, None),
            "multiple-documentation",
            "Foo"
        )
        .is_none());
    }

    #[test]
    fn empty_kb_has_no_completeness_findings() {
        let kb = KnowledgeBase::new();
        assert!(
            kb.validate(crate::ValidationTarget::All, None).is_empty(),
            "an empty KB has no symbols to flag"
        );
    }

    #[test]
    fn file_and_session_validation_report_their_own_gaps() {
        let mut kb = KnowledgeBase::new();
        for (file, kif) in [
            ("a.kif", "(subclass Foo Entity)"),
            ("b.kif", "(subclass Bar Entity)"),
        ] {
            assert!(kb.reload_kif(kif, &std::path::PathBuf::from(file), file).ok);
        }
        assert!(kb.make_session_axiomatic("a.kif").is_ok());
        let a = kb.validate(crate::ValidationTarget::File("a.kif"), None);
        assert!(find(&a, "missing-documentation", "Foo").is_some());
        assert!(find(&a, "missing-documentation", "Bar").is_none());
        // `b.kif` is still a transient session: its own scope reports it.
        let b = kb.validate(crate::ValidationTarget::Session("b.kif"), None);
        assert!(find(&b, "missing-documentation", "Bar").is_some());
        assert!(find(&b, "missing-documentation", "Foo").is_none());
    }
}
