//! TPTP export entrypoints on `KnowledgeBase`: `to_tptp`,
//! `sentence_tptp`, and their helpers.

#[cfg(feature = "external-prover")]
use crate::layer::Layer;
#[cfg(feature = "external-prover")]
use crate::prover::Conjecture;
use crate::semantics::consts::DEFAULT_EXCLUDED_HEADS;
use crate::trans::assemble::{assemble_tptp_indexed, AssemblyOpts};
use crate::types::SentenceId;
#[cfg(feature = "external-prover")]
use crate::{Diagnostic, ExternalOpts, Parser, SourceFile, TestCase};
use crate::{HasTranslation, TptpLang};
use std::collections::HashSet;

use super::KnowledgeBase;

impl<L: HasTranslation> KnowledgeBase<L> {
    /// Generate TPTP for the KB.
    ///
    /// - Axioms = all promoted/loaded sentences (fingerprint session=None).
    /// - Assertions = sentences in `session` (if Some) rendered as `hypothesis`.
    /// - Pass `session=None` to omit assertions.
    ///
    /// Emits SID-based axiom names (`kb_<sid>`), per-axiom KIF comments when
    /// `opts.show_kif_comment` is set, and applies the `excluded` predicate
    /// filter before conversion.
    pub fn to_tptp(&self, opts: &TptpOptions, session: Option<&str>) -> String {
        self.to_tptp_indexed(opts, session, None)
    }

    /// Like [`Self::to_tptp`], but when `axiom_lines` is `Some`, it's filled
    /// with each emitted axiom's starting 0-based line number — see
    /// [`assemble_tptp_indexed`]. Powers a "jump to this axiom" pane without
    /// re-scanning the (potentially large, whole-KB) output text.
    pub fn to_tptp_indexed(
        &self,
        opts: &TptpOptions,
        session: Option<&str>,
        axiom_lines: Option<&mut std::collections::HashMap<SentenceId, u32>>,
    ) -> String {
        crate::with_guard!(self);

        let candidates: Vec<SentenceId> = self
            .axiom_ids_set()
            .into_iter()
            .chain(
                self.layer
                    .semantic()
                    .syntactic
                    .synthetic_origin
                    .keys()
                    .copied(),
            )
            .collect();
        self.export_tptp_from_sids(
            opts,
            candidates,
            std::iter::empty::<SentenceId>(),
            session,
            axiom_lines,
        )
    }

    /// Like [`Self::to_tptp_indexed`], but restricts the emitted axioms to a
    /// SInE-relevant subset seeded from `seed_sids` (typically the query's
    /// own sentence id(s), plus any support/assertion sids also worth
    /// seeding from) — the SAME selection primitive
    /// (`SyntacticLayer::select_relevant`) the native prover and the CLI's
    /// external-prover path both already use, so an external prover fed
    /// this text searches roughly the axiom set the "Native" backend would.
    /// `session`, if given, still folds its sentences in UNFILTERED —
    /// support hypotheses were explicitly asserted for this query, so
    /// selection never gets a chance to exclude them (matches
    /// [`Self::to_tptp_indexed`]'s own session handling).
    ///
    /// Falls back to the SAME axiom set [`Self::to_tptp_indexed`] would
    /// emit (i.e. no selection) when `seed_sids` is empty — nothing to seed
    /// relevance from.
    ///
    /// `budget_pct`, when given, is a percentage (0-100, clamped) of the
    /// KB's current [`Self::sine_axiom_count`] to use as the SInE
    /// auto-tolerance budget, overriding [`crate::SineParams::default`]'s
    /// fixed budget. `None` uses that engine default. There is no
    /// autoscaling feedback loop here (unlike the native prover's `ask`
    /// path) — whatever this resolves to is the FINAL selection, so a
    /// too-low percentage can under-select for a given query.
    #[cfg(any(feature = "external-prover", feature = "native-prover"))]
    pub fn to_tptp_selected(
        &self,
        opts: &TptpOptions,
        seed_sids: &[SentenceId],
        session: Option<&str>,
        axiom_lines: Option<&mut std::collections::HashMap<SentenceId, u32>>,
        budget_pct: Option<f64>,
    ) -> String {
        if seed_sids.is_empty() {
            return self.to_tptp_indexed(opts, session, axiom_lines);
        }

        crate::with_guard!(self);

        let syn = &self.layer.semantic().syntactic;

        let mut seed: HashSet<crate::SymbolId> = HashSet::new();
        for &sid in seed_sids {
            seed.extend(syn.sentence_symbols(sid));
        }

        let sine_params = match budget_pct {
            Some(pct) => crate::SineParams::auto_pct(self.sine_axiom_count(), pct),
            None => crate::SineParams::default(),
        };

        let ctx = self.prove_ctx();
        let (selected, _liu_frontier) = syn.select_relevant(
            &seed,
            sine_params,
            &crate::syntactic::SelectionParams::default(),
            &ctx,
        );

        // Ensure that all taxonomy chains are correctly closed
        let scope = match session {
            Some(name) => crate::semantics::types::Scope::Session(
                crate::syntactic::caches::session::session_id(name),
            ),
            None => crate::semantics::types::Scope::Base,
        };
        let taxonomy_facts = self
            .layer
            .semantic()
            .taxonomy_closure_facts_scoped(&seed, 4000, scope);
        self.export_tptp_from_sids(opts, selected, taxonomy_facts, session, axiom_lines)
    }

    /// Filter, merge, translate, and assemble a candidate axiom set.
    ///
    /// The whole-KB and SInE-selected export paths differ only in how they
    /// collect candidates. They share exclusion, session-support, ordering,
    /// language-resolution, and assembly semantics here.
    fn export_tptp_from_sids<I, R>(
        &self,
        opts: &TptpOptions,
        candidates: I,
        required_sids: R,
        session: Option<&str>,
        axiom_lines: Option<&mut std::collections::HashMap<SentenceId, u32>>,
    ) -> String
    where
        I: IntoIterator<Item = SentenceId>,
        R: IntoIterator<Item = SentenceId>,
    {
        let suppressed = self.layer.translation().suppressed.read().unwrap();
        let mut axioms_sorted: Vec<SentenceId> = candidates
            .into_iter()
            .filter(|&sid| {
                !self.sentence_excluded(sid, &opts.excluded) && !suppressed.contains(&sid)
            })
            .collect();
        drop(suppressed);

        // Session assertions are explicit support hypotheses, not SInE
        // candidates, so include them regardless of how `candidates` was made.
        if let Some(name) = session {
            axioms_sorted.extend(
                self.session_sids(name)
                    .into_iter()
                    .filter(|&sid| !self.sentence_excluded(sid, &opts.excluded)),
            );
        }
        // The selected-export path appends its taxonomy chain after filtering:
        // these facts are required to preserve the selected query's ancestry.
        axioms_sorted.extend(required_sids);
        axioms_sorted.sort_unstable();
        axioms_sorted.dedup();

        let syn = &self.layer.semantic().syntactic;
        let mode = syn.resolve_tptp_lang(opts.lang, &axioms_sorted);
        let (axiom_problem, axiom_sid_map) =
            self.layer.translation().build_problem(&axioms_sorted, mode);

        assemble_tptp_indexed(
            &axiom_problem,
            &axiom_sid_map,
            &AssemblyOpts {
                show_kif: opts.show_kif_comment,
                layer: Some(self.layer.semantic()),
                ..AssemblyOpts::default()
            },
            axiom_lines,
        )
    }

    /// Return the head predicate name of a sentence, if it has one.
    /// Returns `None` for operator-rooted sentences (e.g. `(and ...)`) or
    /// for sentences whose first element is not a plain symbol.
    fn sentence_head_name(&self, sid: SentenceId) -> Option<String> {
        use crate::types::Element;
        let store = &self.layer.semantic().syntactic;
        if !store.has_sentence(sid) {
            return None;
        }
        let sentence = store.sentence(sid)?;
        match sentence.elements.first()? {
            Element::Symbol(sym) => Some(sym.to_string()),
            _ => None,
        }
    }

    /// `true` if the sentence's head predicate matches an `excluded` entry.
    fn sentence_excluded(&self, sid: SentenceId, excluded: &HashSet<String>) -> bool {
        if excluded.is_empty() {
            return false;
        }
        self.sentence_head_name(sid)
            .map(|n| excluded.contains(&n))
            .unwrap_or(false)
    }

    /// Render one sentence as a TPTP formula body -- no `fof(...)` /
    /// `tff(...)` framing; callers add their own `<kw>(name, role, ...)`.
    ///
    /// Respects `opts.query` (existential wrap of free variables, the
    /// conjecture form; otherwise the universal axiom form), `opts.lang`, and
    /// `opts.hide_numbers` (ignored by typed languages). `None` when the
    /// sentence was suppressed by the rewrite pass -- see
    /// [`Self::synthetic_replacements_of`] for what replaced it -- or cannot
    /// be lowered.
    pub fn sentence_tptp(&self, sid: SentenceId, opts: &TptpOptions) -> Option<String> {
        crate::with_guard!(self);
        let trans = self.layer.translation();
        trans.ensure_rewrite_pass();
        let typed = opts.lang.is_typed();
        let cf = if opts.query {
            trans
                .lower_conjecture(sid, typed, opts.hide_numbers, None)
                .map(|(cf, _qvm)| cf)
        } else if typed {
            trans.formula_tff(sid)
        } else if opts.hide_numbers {
            trans.formula_fof(sid)
        } else {
            trans.lower_axiom(sid, false, false)
        }?;
        Some(cf.formula.to_tptp())
    }

    /// `true` if `sid` was suppressed by the rewrite pass (its synthetic
    /// replacement, not the original, is what the prover sees).
    pub fn is_suppressed(&self, sid: SentenceId) -> bool {
        let trans = self.layer.translation();
        trans.ensure_rewrite_pass();
        trans
            .suppressed
            .read()
            .is_ok_and(|suppressed| suppressed.contains(&sid))
    }

    /// The synthetic sentences that replaced `sid` (transitively), if it
    /// was normalized / guard-augmented by the rewrite pass.  Empty for
    /// sentences that pass through unchanged.
    pub fn synthetic_replacements_of(&self, sid: SentenceId) -> Vec<SentenceId> {
        let trans = self.layer.translation();
        trans.ensure_rewrite_pass();
        trans.synthetic_replacements(&[sid])
    }
}

#[cfg(feature = "external-prover")]
impl<L: HasTranslation + Layer> KnowledgeBase<L> {
    /// Render a test case as the TPTP problem an external prover is handed
    /// for it: the hypotheses (plus `prover_opts.session`'s assertions) as
    /// hypotheses, the conjecture, and the axioms selected at
    /// `prover_opts.selection`.  Hypotheses and conjecture are staged as
    /// [`ask`](KnowledgeBase::ask) stages them and rolled back before
    /// returning, so the KB is left unchanged.
    /// `translation_opts` picks the TPTP dialect and whether KIF comments are
    /// emitted.
    ///
    /// # Errors
    ///
    /// Returns the diagnostics of a hypothesis or conjecture that failed to
    /// stage.
    pub fn tc_to_tptp(
        &self,
        tc: TestCase,
        translation_opts: &TptpOptions,
        prover_opts: &ExternalOpts,
    ) -> Result<String, Vec<Diagnostic>> {
        crate::with_guard!(self);
        let trans = self.layer.translation();
        trans.ensure_rewrite_pass();
        let staged =
            self.stage_hypotheses(&tc.file_name, tc.axioms, prover_opts.session.as_deref());
        let mut errors: Vec<Diagnostic> = staged
            .errors
            .iter()
            .filter(|d| d.is_err())
            .cloned()
            .collect();

        let query_key = trans.semantic.syntactic.unique_source_key("query");
        let mut query_sids = Vec::new();
        if let Some(query) = tc.query {
            let (mut normalized, _) = Conjecture::normalize(vec![query]);
            for ast in &mut normalized {
                ast.attribute_to(&query_key);
            }
            let outcome = self.ingest_source(
                SourceFile {
                    parser: Parser::Kif { options: None },
                    name: query_key.clone(),
                    path: std::path::PathBuf::new(),
                    origin: crate::FileOrigin::Inline,
                    contents: String::new(),
                    prebuilt: Some(normalized),
                },
                &query_key,
                false,
            );
            errors.extend(outcome.errors.into_iter().filter(|d| d.is_err()));
            query_sids = trans.semantic.syntactic.file_root_sids(&query_key);
        }

        let tptp = errors.is_empty().then(|| {
            let built = trans.query_problem(
                &query_sids,
                prover_opts.selection,
                staged.session.as_deref(),
                translation_opts.lang,
                &self.prove_ctx(),
            );
            assemble_tptp_indexed(
                &built.problem,
                &built.sid_map,
                &AssemblyOpts {
                    show_kif: translation_opts.show_kif_comment,
                    layer: Some(&trans.semantic),
                    ..AssemblyOpts::default()
                },
                None,
            )
        });

        let _ = self.ingest_source(
            SourceFile::truncate(std::path::PathBuf::from(&query_key)),
            &query_key,
            false,
        );
        self.unstage_hypotheses(staged);
        tptp.ok_or(errors)
    }
}

/// Options controlling TPTP output.
#[derive(Debug, Clone)]
pub struct TptpOptions {
    /// The TPTP language to emit.
    pub lang: TptpLang,
    /// Wrap free variables in `?` (existential) instead of `!` (universal).
    /// Used for query/conjecture sentences.
    pub query: bool,
    /// Replace numeric literals with `n__N` tokens (default false).
    /// Ignored in TFF mode, where numerics are native `$int`/`$real` literals.
    pub hide_numbers: bool,
    /// Head predicates whose sentences are omitted from KB output entirely.
    /// `domain`/`range` are excluded as top-level *axioms* but their TFF
    /// *type declarations* are still emitted.
    pub excluded: HashSet<String>,
    /// Emit a `% <original KIF>` comment before each TPTP formula.
    pub show_kif_comment: bool,
}

impl Default for TptpOptions {
    fn default() -> Self {
        let excluded: HashSet<String> = DEFAULT_EXCLUDED_HEADS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        TptpOptions {
            lang: TptpLang::default(),
            query: false,
            hide_numbers: false,
            excluded,
            show_kif_comment: false,
        }
    }
}

impl TptpOptions {
    /// [`TptpOptions::default`] with `hide_numbers` enabled.
    pub fn default_with_hide_numbers() -> Self {
        Self {
            hide_numbers: true,
            ..Self::default()
        }
    }
}

#[cfg(all(test, any(feature = "external-prover", feature = "native-prover")))]
mod session_support_tests {
    use super::KnowledgeBase;
    use crate::TptpOptions;

    fn kb_with(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("t.kif"), "t.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("t.kif").expect("promote");
        kb
    }

    /// Stage assertions + query in two tags and select from both, as an
    /// external ask does; returns the selected problem text.
    fn selected(kb: &mut KnowledgeBase, assertions: &str, query: &str, pct: Option<f64>) -> String {
        assert!(kb.tell(assertions, "s").ok);
        assert!(kb.tell(query, "q").ok);
        let mut seed = kb.session_sids("s");
        seed.extend(kb.session_sids("q"));
        let opts = TptpOptions {
            hide_numbers: true,
            ..TptpOptions::default()
        };
        let out = kb.to_tptp_selected(&opts, &seed, Some("s"), None, pct);
        kb.flush_session("s");
        kb.flush_session("q");
        out
    }

    /// A starved selection budget must not drop a link of the subclass chain
    /// between an asserted class and the queried class: the chain facts are
    /// injected regardless of SInE's ranking.
    #[test]
    fn a_starved_budget_keeps_the_taxonomy_chain() {
        let mut kb = kb_with(
            "(subclass Mammal WarmBloodedVertebrate)\n\
             (subclass WarmBloodedVertebrate Vertebrate)\n\
             (subclass Vertebrate Animal)\n\
             (subclass Animal Organism)\n\
             (=> (and (subclass ?X ?Y) (instance ?Z ?X)) (instance ?Z ?Y))\n\
             (subclass Rock Object)\n",
        );
        let tptp = selected(
            &mut kb,
            "(instance Rex Dog)\n(subclass Dog Mammal)",
            "(instance Rex Animal)",
            Some(0.0001),
        );
        for link in [
            "s__subclass(s__Dog,s__Mammal)",
            "s__subclass(s__Mammal,s__WarmBloodedVertebrate)",
            "s__subclass(s__WarmBloodedVertebrate,s__Vertebrate)",
            "s__subclass(s__Vertebrate,s__Animal)",
        ] {
            assert!(tptp.contains(link), "missing chain link {link}:\n{tptp}");
        }
    }

    /// Session assertions the query needs must reach the problem as support,
    /// both with SInE selection and without.
    #[test]
    fn session_assertions_are_emitted_as_support() {
        let mut kb = kb_with("(subclass Mammal Animal)");
        for pct in [None, Some(100.0)] {
            let tptp = selected(
                &mut kb,
                "(instance Rex Dog)\n(subclass Dog Mammal)",
                "(instance Rex Animal)",
                pct,
            );
            assert!(
                tptp.lines().any(|l| l.contains("s__Rex")),
                "pct={pct:?}: the asserted (instance Rex Dog) never reached the problem:\n{tptp}"
            );
            assert!(
                tptp.lines()
                    .any(|l| l.contains("s__Dog") && l.contains("s__Mammal")),
                "pct={pct:?}: the asserted (subclass Dog Mammal) is missing:\n{tptp}"
            );
        }
    }

    #[test]
    fn selected_taxonomy_facts_bypass_the_axiom_exclusion_filter() {
        let mut kb = kb_with("(subclass Dog Mammal)\n(subclass Mammal Animal)");
        assert!(kb.tell("(instance Rex Dog)", "s").ok);
        assert!(kb.tell("(instance Rex Animal)", "q").ok);
        let mut seed = kb.session_sids("s");
        seed.extend(kb.session_sids("q"));

        let mut opts = TptpOptions::default();
        opts.excluded.insert("subclass".to_string());
        let tptp = kb.to_tptp_selected(&opts, &seed, Some("s"), None, Some(0.0001));

        assert!(
            tptp.contains("s__subclass(s__Dog,s__Mammal)"),
            "the required taxonomy fact must survive explicit exclusion:\n{tptp}"
        );

        kb.flush_session("s");
        kb.flush_session("q");
    }
}

#[cfg(test)]
mod tests {
    use super::TptpOptions;
    use crate::{KnowledgeBase, TptpLang};

    fn kb_with(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("t.kif"), "t.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("t.kif").expect("promote");
        kb
    }

    #[test]
    fn sentence_tptp_wraps_free_variables_by_role() {
        let kb = kb_with("(=> (instance ?X Dog) (instance ?X Animal))");
        let sid = kb.syntactic().root_sids()[0];
        let axiom = kb.sentence_tptp(sid, &TptpOptions::default()).unwrap();
        let query = kb
            .sentence_tptp(
                sid,
                &TptpOptions {
                    query: true,
                    ..TptpOptions::default()
                },
            )
            .unwrap();
        assert!(axiom.starts_with('!'), "axiom form is universal: {axiom}");
        assert!(
            query.starts_with('?'),
            "conjecture form is existential: {query}"
        );
    }

    #[test]
    fn sentence_tptp_hides_numbers_only_when_asked() {
        let kb = kb_with("(lessThan 1 2)");
        let sid = kb.syntactic().root_sids()[0];
        let fof = |hide_numbers| TptpOptions {
            lang: TptpLang::Fof,
            hide_numbers,
            ..TptpOptions::default()
        };
        let hidden = kb.sentence_tptp(sid, &fof(true)).unwrap();
        let shown = kb.sentence_tptp(sid, &fof(false)).unwrap();
        assert_ne!(hidden, shown, "hidden: {hidden} / shown: {shown}");
    }

    #[test]
    fn sentence_tptp_of_unknown_sid_is_none() {
        let kb = kb_with("(instance Rex Dog)");
        assert!(kb.sentence_tptp(12345, &TptpOptions::default()).is_none());
    }
}

#[cfg(all(test, feature = "external-prover"))]
mod test_case_tests {
    use super::KnowledgeBase;
    use crate::{parse_document, ExternalOpts, Parser, TestCase, TptpOptions};

    fn kb() -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(
            "(subclass Dog Mammal)\n(subclass Mammal Animal)\n(instance Rex Dog)\n",
            &std::path::PathBuf::from("t.kif"),
            "t.kif",
        );
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("t.kif").expect("promote");
        kb
    }

    fn kif(text: &str) -> Vec<crate::AstNode> {
        parse_document(
            "case.kif.tq",
            text.to_string(),
            Parser::Kif { options: None },
        )
        .ast
        .iter()
        .filter_map(|d| d.as_stmt().cloned())
        .collect()
    }

    fn case(hypotheses: &str, query: &str) -> TestCase {
        TestCase {
            axioms: kif(hypotheses),
            ..TestCase::conjecture("case.kif.tq", kif(query).remove(0))
        }
    }

    fn user_opts() -> ExternalOpts {
        ExternalOpts {
            session: Some("user".into()),
            ..ExternalOpts::default()
        }
    }

    #[test]
    fn a_test_case_renders_hypotheses_session_support_and_the_conjecture() {
        let mut kb = kb();
        assert!(kb.tell("(instance Max Dog)", "user").ok);
        let tptp = kb
            .tc_to_tptp(
                case("(instance Fido Dog)", "(instance Fido Animal)"),
                &TptpOptions::default(),
                &user_opts(),
            )
            .expect("translates");
        let conjecture = tptp
            .lines()
            .find(|l| l.contains(", conjecture,"))
            .expect("a conjecture");
        assert!(conjecture.contains("s__Fido"), "{tptp}");
        assert!(tptp.contains("s__Max"), "session support missing:\n{tptp}");
        assert!(
            tptp.contains("s__Mammal"),
            "selected axioms missing:\n{tptp}"
        );
    }

    #[test]
    fn translating_a_test_case_leaves_the_kb_unchanged() {
        let mut kb = kb();
        assert!(kb.tell("(instance Max Dog)", "user").ok);
        let before = kb.session_sids("user");
        kb.tc_to_tptp(
            case("(instance Fido Dog)", "(instance Fido Animal)"),
            &TptpOptions::default(),
            &user_opts(),
        )
        .expect("translates");
        assert_eq!(
            kb.session_sids("user"),
            before,
            "no hypothesis or conjecture left behind"
        );

        let next = kb
            .tc_to_tptp(
                case("(instance Tom Dog)", "(instance Rex Animal)"),
                &TptpOptions::default(),
                &user_opts(),
            )
            .expect("translates");
        assert!(!next.contains("s__Fido"), "an earlier case leaked:\n{next}");
    }

    #[test]
    fn a_test_case_without_a_conjecture_renders_its_hypotheses() {
        let kb = kb();
        let tc = TestCase {
            query: None,
            ..case("(instance Fido Dog)", "(instance Fido Animal)")
        };
        let tptp = kb
            .tc_to_tptp(tc, &TptpOptions::default(), &ExternalOpts::default())
            .expect("translates");
        assert!(!tptp.contains(", conjecture,"), "{tptp}");
        assert!(tptp.contains("s__Fido"), "{tptp}");
    }
}
