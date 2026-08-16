//! Public re-exports of semantic operations.
use std::collections::{HashMap, HashSet};

use crate::layer::{Layer, TopLayer};
use crate::types::Element;
use crate::{Diagnostic, SemanticError, SentenceId, SymbolId, ToDiagnostic};

use super::KnowledgeBase;

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
    /// CNF skolem constants are excluded. Shared by [`Self::vocab_stats`] and
    /// [`Self::completeness_findings`] so both agree on what a "symbol" is.
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
            if crate::kb::search::is_scoped_variable_name(&name) {
                return;
            }
            ids.push(sym_id);
        });
        ids
    }

    /// The set of symbols bound to `subject_slot` across every sentence
    /// matching `pattern_kif` (head-indexed via `head`, same O(1)
    /// pre-filter [`Self::vocab_stats`]'s hand-written scan uses). Empty when
    /// `pattern_kif`'s head relation isn't interned in this KB at all — never
    /// panics (unlike the public [`Self::lookup`], where an unknown symbol in
    /// a developer-typed pattern is a genuine usage error worth aborting on).
    fn symbols_matching(
        &self,
        pattern_kif: &str,
        head: &str,
        subject_slot: usize,
    ) -> HashSet<SymbolId> {
        let syn = &self.layer.semantic().syntactic;
        let Ok(pat) = syn.patterns().pattern_from_kif(pattern_kif) else {
            return HashSet::new();
        };
        syn.patterns()
            .find_by_pattern(&pat, Some(head), None)
            .into_iter()
            .filter_map(|(_, b)| match b.elements.get(&subject_slot) {
                Some(Element::Symbol(s)) => Some(s.id()),
                _ => None,
            })
            .collect()
    }

    /// Per-`(subject, language)` occurrence counts for `(documentation
    /// SUBJECT LANGUAGE "...")` sentences — the finer key
    /// [`Self::symbols_matching`] can't give, needed to tell "documented in
    /// three languages" (fine) apart from "documented three times in
    /// English" (a real duplicate — [`SemanticError::MultipleDocumentation`]
    /// fires per-language, never across languages).
    fn documentation_occurrences(&self) -> HashMap<(SymbolId, String), usize> {
        let syn = &self.layer.semantic().syntactic;
        let Ok(pat) = syn
            .patterns()
            .pattern_from_kif("(documentation ?Subj ?Lang ?Text)")
        else {
            return HashMap::new();
        };
        let mut counts: HashMap<(SymbolId, String), usize> = HashMap::new();
        for (_, b) in syn
            .patterns()
            .find_by_pattern(&pat, Some("documentation"), None)
        {
            let (Some(Element::Symbol(subj)), Some(Element::Symbol(lang))) =
                (b.elements.get(&0), b.elements.get(&1))
            else {
                continue;
            };
            *counts.entry((subj.id(), lang.to_string())).or_insert(0) += 1;
        }
        counts
    }

    /// The whole-KB documentation-completeness pass folded into
    /// [`Self::validate_all`] as a second pass alongside the per-sentence
    /// structural checks: every real symbol ([`Self::real_symbol_ids`])
    /// missing a `documentation` or `termFormat` entry, every relation
    /// symbol missing a `format` string, and every `(symbol, language)` pair
    /// documented more than once. All findings are [`crate::Severity::Hint`]
    /// (advisory, additive — never a substitute for the structural checks).
    ///
    /// Uses the syntactic layer's pattern matcher
    /// ([`crate::syntactic::pattern`], reached the same way
    /// [`Self::lookup`] does) rather than hand-indexed element positions, so
    /// each check states the formula shape it looks for directly instead of
    /// through bare argument-index numbers.
    fn completeness_findings(&self) -> Vec<Diagnostic> {
        let syn = &self.layer.semantic().syntactic;
        let doc_occurrences = self.documentation_occurrences();
        let documented: HashSet<SymbolId> = doc_occurrences.keys().map(|(id, _)| *id).collect();
        let has_term_format =
            self.symbols_matching("(termFormat ?Lang ?Subj ?Text)", "termFormat", 1);
        let has_format = self.symbols_matching("(format ?Lang ?Rel ?Text)", "format", 1);

        // Bulk anchor map replacing per-diagnostic `defining_sentence` calls:
        // each of those scanned EVERY declaration sentence (plus a full
        // forward-map span scan per candidate), and at thousands of
        // completeness findings that quadratic pass dominated validate_all.
        // One walk over the declaration head lists in the same priority order,
        // first hit per symbol wins, span-less (synthetic) candidates skipped —
        // per-symbol results identical to `defining_sentence`'s declaration
        // pass.  Its by-head fallback stays per-symbol below (an O(1) list
        // lookup, not a scan).
        let spans = syn.source_span_index();
        let mut defining: std::collections::HashMap<SymbolId, (SentenceId, crate::Span)> =
            std::collections::HashMap::new();
        const DECLARATIONS: &[&str] = &[
            "subclass",
            "instance",
            "subrelation",
            "subAttribute",
            "documentation",
        ];
        for &head in DECLARATIONS {
            for sid in syn.by_head(head).iter().copied() {
                let Some(sent) = syn.sentence(sid) else {
                    continue;
                };
                let Some(crate::types::Element::Symbol(sym)) = sent.elements.get(1) else {
                    continue;
                };
                let Some(span) = spans.get(&sid) else {
                    continue;
                };
                defining
                    .entry(sym.id())
                    .or_insert_with(|| (sid, span.clone()));
            }
        }
        let anchor = |err: SemanticError, id: SymbolId, name: &str| -> Diagnostic {
            let mut d = err.to_diagnostic();
            let found = defining.get(&id).cloned().or_else(|| {
                // `defining_sentence`'s fallback: any root headed by the symbol.
                syn.by_head(name)
                    .iter()
                    .copied()
                    .find_map(|sid| spans.get(&sid).map(|sp| (sid, sp.clone())))
            });
            if let Some((sid, span)) = found {
                d.sids = vec![sid];
                d.range = span;
            }
            d
        };

        let mut out = Vec::new();
        for id in self.real_symbol_ids() {
            let Some(name) = syn.sym_name(id).map(|s| s.name().to_string()) else {
                continue;
            };

            if !documented.contains(&id) {
                out.push(anchor(
                    SemanticError::MissingDocumentation { sym: name.clone() },
                    id,
                    &name,
                ));
            }
            if !has_term_format.contains(&id) {
                out.push(anchor(
                    SemanticError::MissingTermFormat { sym: name.clone() },
                    id,
                    &name,
                ));
            }
            if (self.is_relation(id) || self.is_predicate(id) || self.is_function(id))
                && !has_format.contains(&id)
            {
                out.push(anchor(
                    SemanticError::MissingFormatString { sym: name.clone() },
                    id,
                    &name,
                ));
            }
        }

        for ((id, language), count) in &doc_occurrences {
            if *count <= 1 {
                continue;
            }
            if let Some(name) = syn.sym_name(*id).map(|s| s.name().to_string()) {
                out.push(anchor(
                    SemanticError::MultipleDocumentation {
                        sym: name.clone(),
                        language: language.clone(),
                        count: *count,
                    },
                    *id,
                    &name,
                ));
            }
        }
        out
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
        let store = &self.layer.semantic().syntactic;

        // Canonical declarations with this symbol as arg 1.
        const DECLARATIONS: &[&str] = &[
            "subclass",
            "instance",
            "subrelation",
            "subAttribute",
            "documentation",
        ];
        for &head in DECLARATIONS {
            for sid in store.by_head(head).iter().copied() {
                let Some(sent) = store.sentence(sid) else {
                    continue;
                };
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
        let head_id = self.symbol_id(head)?;
        let domains = self.layer.semantic().domain(head_id);
        // `arg_idx` is 1-based (element-index convention); `domains`
        // is 0-based.
        if arg_idx == 0 || arg_idx > domains.len() {
            return None;
        }
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

    /// Validate every root sentence in the KB, reasoning globally (`Base`),
    /// PLUS a second, whole-KB pass of documentation-completeness hints
    /// ([`Self::completeness_findings`]) — a distinct axis from the
    /// per-sentence structural checks above (a symbol's documentation
    /// coverage isn't a property of any one sentence), folded into this same
    /// entry point rather than exposed separately so `validate_all` remains
    /// the one "check everything" call.
    pub fn validate_all(&self) -> Vec<Diagnostic> {
        crate::with_guard!(self);
        let roots: Vec<SentenceId> = self.layer.semantic().syntactic.root_sids();
        let mut diags = self.validate_sids(&roots, crate::semantics::types::Scope::Base);
        diags.extend(self.completeness_findings());
        diags
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

    /// [`Self::validate_file`] in the context of `session` (`Base` ∪ that
    /// session's transient overlay).  For live editor buffers staged into a
    /// file's own session but not yet promoted: their declarations are
    /// session-scoped, so Base-scope validation would falsely flag symbols
    /// they connect (e.g. E001 on a just-typed, correctly-parented class).
    pub fn validate_file_in_session(&self, file_tag: &str, session: &str) -> Vec<Diagnostic> {
        crate::with_guard!(self);
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;
        let sids = self.layer.semantic().syntactic.file_root_sids(file_tag);
        self.validate_sids(&sids, Scope::Session(session_id(session)))
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
    fn validate_sids(
        &self,
        sids: &[SentenceId],
        scope: crate::semantics::types::Scope,
    ) -> Vec<Diagnostic> {
        // Span anchoring uses the bulk index, not per-sid `source_span`:
        // that call is a full forward-map scan, and at thousands of
        // diagnostics the per-call scans dominated validate_all (and their
        // shard locking serialised the parallel fan-out below).
        let spans = self.layer.semantic().syntactic.source_span_index();
        let one = |sid: SentenceId| -> Vec<Diagnostic> {
            self.layer
                .semantic()
                .validation_scoped(sid, scope)
                .iter()
                .map(|e| {
                    let mut d = e.to_diagnostic();
                    if d.sids.is_empty() {
                        d.sids = vec![sid];
                    }
                    // Anchor the diagnostic at the *root* formula's source span,
                    // so findings on nested sub-sentences (which carry no span of
                    // their own) still report the enclosing formula's file:line.
                    if d.range.file.is_empty() {
                        if let Some(span) = spans.get(&sid) {
                            d.range = span.clone();
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

        let base = kb.validate_sentence(sid);
        assert!(
            base.iter().any(|d| d.code == "head-not-relation"),
            "Base never saw the declaration → HeadNotRelation; got {:?}",
            base.iter().map(|d| d.code).collect::<Vec<_>>()
        );

        let scoped = kb.validate_sentence_in_session(sid, "s");
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

        let diags = kb.validate_session("s");
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
        #[cfg(feature = "ask")]
        kb.make_session_axiomatic("load").expect("promote");
        #[cfg(not(feature = "ask"))]
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
        #[cfg(feature = "ask")]
        kb.make_session_axiomatic("load").expect("promote");
        #[cfg(not(feature = "ask"))]
        kb.make_session_axiomatic("load").expect("promote");

        assert!(kb.tell("(orientation A B Right)", "case").ok);
        let diags = kb.validate_session("case");
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
        let diags = kb.validate_all();
        let d = find(&diags, "missing-documentation", "Foo")
            .expect("Foo has no documentation axiom — should be flagged");
        assert_eq!(d.severity, Severity::Hint);
    }

    #[test]
    fn missing_documentation_hint_absent_when_documented() {
        let kb = promoted("(subclass Foo Entity)\n(documentation Foo EnglishLanguage \"A foo.\")");
        let diags = kb.validate_all();
        assert!(
            find(&diags, "missing-documentation", "Foo").is_none(),
            "Foo is documented — must not be flagged"
        );
    }

    #[test]
    fn missing_term_format_hint_fires_and_absent_when_present() {
        let undocumented = promoted("(subclass Foo Entity)");
        assert!(find(&undocumented.validate_all(), "missing-term-format", "Foo").is_some());

        let labeled = promoted("(subclass Foo Entity)\n(termFormat EnglishLanguage Foo \"foo\")");
        assert!(find(&labeled.validate_all(), "missing-term-format", "Foo").is_none());
    }

    #[test]
    fn missing_format_string_only_applies_to_relations() {
        let kb = promoted(
            "(subclass BinaryRelation Relation)\n\
             (instance likes BinaryRelation)\n\
             (subclass Foo Entity)",
        );
        let diags = kb.validate_all();
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
        assert!(find(&kb.validate_all(), "missing-format-string", "likes").is_none());
    }

    #[test]
    fn multiple_documentation_same_language_fires_with_count() {
        let kb = promoted(
            "(subclass Foo Entity)\n\
             (documentation Foo EnglishLanguage \"A foo.\")\n\
             (documentation Foo EnglishLanguage \"Another foo description.\")",
        );
        let diags = kb.validate_all();
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
        assert!(find(&kb.validate_all(), "multiple-documentation", "Foo").is_none());
    }

    #[test]
    fn empty_kb_has_no_completeness_findings() {
        let kb = KnowledgeBase::new();
        assert!(
            kb.validate_all().is_empty(),
            "an empty KB has no symbols to flag"
        );
    }
}
