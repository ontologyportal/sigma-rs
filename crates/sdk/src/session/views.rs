// crates/sdk/src/session/views.rs
//
// FFI-safe view projections of core results.
//
// Core types carry `SentenceId`/`SymbolId` fields -- u64 content hashes that
// overflow JavaScript's safe-integer range -- and their `Serialize` impls
// double as the bincode snapshot encoding, so they cannot be reshaped for a
// JS consumer.  Each view here is a curated serde struct of boundary-safe
// types (String/usize/bool/f32) built from `(core value, &kb)`, with ids
// resolved to names, rendered KIF text, or source locations.  The wasm crate
// (and any future IDE/RPC facade) serializes these directly.
//
// Field names are part of the wire contract consumed by the web front end --
// do not rename or add `rename_all` attributes.

use sigmakee_rs_core::{Diagnostic, KnowledgeBase, ManKind, ManPage, SearchHit, TopLayer};

#[cfg(any(feature = "ask", feature = "native-prover"))]
use sigmakee_rs_core::{AstKif as _, AxiomSourceIndex, KifProofStep, ProverStatus};

use super::Session;

// -- Diagnostics ---------------------------------------------------------------

/// A boundary-safe diagnostic: severity/kind/code/message plus the source
/// location (`file`, 1-based `line`/`col` and end position) from the
/// diagnostic's span.  The internal sentence-id list is dropped.
#[derive(serde::Serialize)]
pub struct DiagnosticView {
    pub severity: String,
    pub kind: String,
    pub code: String,
    pub message: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl From<&Diagnostic> for DiagnosticView {
    fn from(d: &Diagnostic) -> Self {
        Self {
            severity: d.severity.as_str().to_string(),
            kind: d.kind.to_string(),
            code: d.code.to_string(),
            message: d.message.clone(),
            file: d.range.file.clone(),
            line: d.range.line,
            col: d.range.col,
            end_line: d.range.end_line,
            end_col: d.range.end_col,
        }
    }
}

impl DiagnosticView {
    /// Project a diagnostics slice wholesale.
    pub fn from_slice(diags: &[Diagnostic]) -> Vec<Self> {
        diags.iter().map(Self::from).collect()
    }
}

/// Paired findings from [`Session::validate_scratch`]: the assertions'
/// diagnostics and the query's, in one boundary-safe object.
#[derive(serde::Serialize)]
pub struct ScratchValidationView {
    pub assertions: Vec<DiagnosticView>,
    pub query: Vec<DiagnosticView>,
}

// -- Search --------------------------------------------------------------------

/// A boundary-safe search hit (the internal `sid` is dropped).
#[derive(serde::Serialize)]
pub struct SearchHitView {
    pub symbol: String,
    pub kinds: Vec<String>,
    pub source: String,
    pub language: String,
    pub text: String,
    pub rank: f32,
}

impl From<&SearchHit> for SearchHitView {
    fn from(h: &SearchHit) -> Self {
        Self {
            symbol: h.symbol.clone(),
            kinds: h.kinds.iter().map(|k| k.as_str().to_string()).collect(),
            source: h.source.as_str().to_string(),
            language: h.language.clone(),
            text: h.text.clone(),
            rank: h.rank,
        }
    }
}

impl SearchHitView {
    /// Project a search-result slice wholesale.
    pub fn from_slice(hits: &[SearchHit]) -> Vec<Self> {
        hits.iter().map(Self::from).collect()
    }
}

/// Parse a UI-facing kind filter string (`"class"`, `"relation"`, ...) into a
/// [`ManKind`] for [`SearchOpts`](sigmakee_rs_core::SearchOpts).
pub fn man_kind_from_str(s: &str) -> Option<ManKind> {
    match s.to_ascii_lowercase().as_str() {
        "class" => Some(ManKind::Class),
        "relation" => Some(ManKind::Relation),
        "function" => Some(ManKind::Function),
        "predicate" => Some(ManKind::Predicate),
        "instance" => Some(ManKind::Instance),
        "individual" => Some(ManKind::Individual),
        _ => None,
    }
}

// -- Man page / taxonomy -------------------------------------------------------

/// One documentation / format string in a given language.
#[derive(serde::Serialize)]
pub struct DocView {
    pub language: String,
    pub text: String,
}

/// One taxonomy edge: `relation` names the edge kind (`subclass`,
/// `instance`, ...), `parent` the symbol on the far side (for downward edges
/// that is the *child*, matching [`ManPageDetail::children`]).
#[derive(serde::Serialize)]
pub struct EdgeView {
    pub relation: String,
    pub parent: String,
}

/// A domain/range sort: the class, and whether the argument is the class
/// itself vs. a subclass position (`domainSubclass` / `rangeSubclass`).
#[derive(serde::Serialize)]
pub struct SortView {
    pub class: String,
    pub subclass: bool,
}

/// One argument-position sort signature entry.
#[derive(serde::Serialize)]
pub struct DomainView {
    pub position: usize,
    pub sort: SortView,
}

/// One formula that references the man-paged symbol: its rendered KIF text
/// plus source location (when the sentence has one -- synthetic/CNF sentences
/// don't). `position` is the symbol's 0-based root-level position in the
/// sentence, or `None` when it only occurs nested inside a sub-sentence.
///
/// `kind` / `arg_pos` classify the formula's top-level shape for the reference
/// filter: `kind` is `"fact"` (a relation atom, possibly under `not`), `"=>"`,
/// `"<=>"`, `"and"`, `"or"`, or `"other"`; for a fact, `arg_pos` is the
/// symbol's argument index in the atom (0 = the relation itself), after
/// peeling one top-level `not`, or `None` when it isn't a direct argument.
#[derive(serde::Serialize)]
pub struct ManPageRefView {
    pub position: Option<usize>,
    pub kif: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub kind: String,
    pub arg_pos: Option<usize>,
}

/// The full symbol card, boundary-safe: the human-facing [`ManPage`] fields
/// with the raw `SentenceId`/`SymbolId` reference lists resolved to rendered
/// KIF + source location (see [`ManPageRefView`]) rather than dropped.
///
/// Named `ManPageDetail` (not `*View`) to keep it distinct from
/// [`ManPageView`](crate::ManPageView), the hover/doc-span projection.
#[derive(serde::Serialize)]
pub struct ManPageDetail {
    pub name: String,
    pub kinds: Vec<String>,
    pub documentation: Vec<DocView>,
    pub term_format: Vec<DocView>,
    pub format: Vec<DocView>,
    pub parents: Vec<EdgeView>,
    pub children: Vec<EdgeView>,
    pub arity: Option<i32>,
    pub domains: Vec<DomainView>,
    pub range: Option<SortView>,
    pub appears_in_count: usize,
    pub consequent_count: usize,
    pub references: Vec<ManPageRefView>,
}

/// Direct taxonomy edges of a symbol, without the man page's reference scan.
/// The lightweight peer of [`ManPageDetail`] for lazily-expanded taxonomy
/// trees.
#[derive(serde::Serialize)]
pub struct TaxonomyView {
    pub parents: Vec<EdgeView>,
    pub children: Vec<EdgeView>,
}

/// One `NaturalLanguage` instance with its display label (the English
/// `termFormat`, falling back to the bare symbol name).
#[derive(serde::Serialize)]
pub struct LangView {
    pub symbol: String,
    pub label: String,
}

/// Classify a reference formula's top-level shape for the man-page filter.
/// Returns `(kind, arg_pos)` as documented on [`ManPageRefView`].
fn classify_reference<L: TopLayer>(
    kb: &KnowledgeBase<L>,
    sid: sigmakee_rs_core::SentenceId,
    target: sigmakee_rs_core::SymbolId,
) -> (String, Option<usize>) {
    use sigmakee_rs_core::{Element, OpKind};
    let Some(root) = kb.sentence(sid) else {
        return ("other".into(), None);
    };
    // A leading `not` wraps its operand as a nested sub-sentence; peel one.
    let atom = if matches!(root.op(), Some(OpKind::Not)) {
        match root.elements.get(1) {
            Some(Element::Sub(inner)) => kb.sentence(*inner),
            _ => Some(root.clone()),
        }
    } else {
        Some(root.clone())
    };
    let Some(atom) = atom else {
        return ("other".into(), None);
    };
    match atom.elements.first() {
        Some(Element::Symbol(_)) => {
            let pos = atom
                .elements
                .iter()
                .position(|el| matches!(el, Element::Symbol(s) if s.id() == target));
            ("fact".into(), pos)
        }
        Some(Element::Op(op)) => {
            let k = match op {
                OpKind::Implies => "=>",
                OpKind::Iff => "<=>",
                OpKind::And => "and",
                OpKind::Or => "or",
                _ => "other",
            };
            (k.into(), None)
        }
        _ => ("other".into(), None),
    }
}

impl ManPageDetail {
    /// Project a [`ManPage`] against the KB it came from, resolving reference
    /// sids to rendered KIF text + source locations.
    pub fn project<L: TopLayer>(kb: &KnowledgeBase<L>, p: &ManPage) -> Self {
        let docs = |v: &[sigmakee_rs_core::DocEntry]| -> Vec<DocView> {
            v.iter()
                .map(|d| DocView {
                    language: d.language.clone(),
                    text: d.text.clone(),
                })
                .collect()
        };
        let edges = |v: &[sigmakee_rs_core::ParentEdge]| -> Vec<EdgeView> {
            v.iter()
                .map(|e| EdgeView {
                    relation: e.relation.clone(),
                    parent: e.parent.clone(),
                })
                .collect()
        };
        let sort = |s: &sigmakee_rs_core::SortSig| SortView {
            class: s.class.clone(),
            subclass: s.subclass,
        };
        let target = kb.symbol_id(&p.name);
        let reference =
            |sid: sigmakee_rs_core::SentenceId, position: Option<usize>| -> ManPageRefView {
                let span = sigmakee_rs_core::DiagnosticSource::sentence_location(kb, sid);
                let (kind, arg_pos) = match target {
                    Some(t) => classify_reference(kb, sid, t),
                    None => ("other".to_string(), None),
                };
                ManPageRefView {
                    position,
                    kif: kb.pretty_print_sentence_plain(sid, 0),
                    file: span.as_ref().map(|s| s.file.clone()),
                    line: span.as_ref().map(|s| s.line),
                    kind,
                    arg_pos,
                }
            };
        let mut references: Vec<ManPageRefView> = p
            .ref_args
            .iter()
            .map(|sigmakee_rs_core::SentenceRef(pos, sid)| reference(*sid, Some(*pos)))
            .collect();
        references.extend(p.ref_nested.iter().map(|&sid| reference(sid, None)));
        Self {
            name: p.name.clone(),
            kinds: p.kinds.iter().map(|k| k.as_str().to_string()).collect(),
            documentation: docs(&p.documentation),
            term_format: docs(&p.term_format),
            format: docs(&p.format),
            parents: edges(&p.parents),
            children: edges(&p.children),
            arity: p.arity,
            domains: p
                .domains
                .iter()
                .map(|(pos, s)| DomainView {
                    position: *pos,
                    sort: sort(s),
                })
                .collect(),
            range: p.range.as_ref().map(sort),
            appears_in_count: p.appears_in_count,
            consequent_count: p.consequent_count,
            references,
        }
    }
}

// -- KB stats ------------------------------------------------------------------

/// One language's documentation coverage (see [`KbStatsView`]).
#[derive(serde::Serialize)]
pub struct DocLangView {
    pub language: String,
    pub documented: usize,
}

/// Summary counts describing the loaded KB, for an overview page.  The
/// vocabulary/coverage fields come from `KnowledgeBase::vocab_stats`;
/// `documented`/`labeled` divide by `symbols` for a coverage percentage.
#[derive(serde::Serialize)]
pub struct KbStatsView {
    pub files: usize,
    pub symbols: usize,
    pub axioms: usize,
    pub rules: usize,
    pub classes: usize,
    pub instances: usize,
    pub relations: usize,
    pub predicates: usize,
    pub functions: usize,
    pub documented: usize,
    pub labeled: usize,
    pub doc_languages: Vec<DocLangView>,
    pub term_languages: Vec<DocLangView>,
}

// -- Test cases ----------------------------------------------------------------

/// A parsed `.kif.tq` test file, boundary-safe.  camelCase because the JS
/// test-editor consumes it verbatim.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCaseView {
    pub name: String,
    pub note: String,
    pub timeout: u32,
    pub query_kif: Option<String>,
    pub axiom_kif: String,
    pub expected_proof: Option<bool>,
    pub expected_answer: Option<Vec<String>>,
    pub extra_files: Vec<String>,
}

impl From<&sigmakee_rs_core::TestCase> for TestCaseView {
    fn from(tc: &sigmakee_rs_core::TestCase) -> Self {
        Self {
            name: tc.file_name.clone(),
            note: tc.note.clone(),
            timeout: tc.timeout,
            query_kif: tc.query_kif(),
            axiom_kif: tc.axiom_kif(),
            expected_proof: tc.expected_proof,
            expected_answer: tc.expected_answer.clone(),
            extra_files: tc.extra_files.clone(),
        }
    }
}

// -- Prover results ------------------------------------------------------------

/// One step of a cited derivation -- a refutation proof or an audit
/// contradiction; both project to this single shape.
#[cfg(any(feature = "ask", feature = "native-prover"))]
#[derive(serde::Serialize)]
pub struct ProofStepView {
    pub index: usize,
    pub rule: String,
    pub premises: Vec<usize>,
    pub kif: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[cfg(any(feature = "ask", feature = "native-prover"))]
impl ProofStepView {
    /// Project a proof/contradiction transcript, citing each step's source
    /// axiom (via `src_idx`) where it has one.
    pub fn project(steps: &[KifProofStep], src_idx: &AxiomSourceIndex) -> Vec<Self> {
        steps
            .iter()
            .map(|s| {
                let loc = s.source_sid.and_then(|sid| src_idx.lookup_by_sid(sid));
                Self {
                    index: s.index,
                    rule: s.rule.clone(),
                    premises: s.premises.clone(),
                    kif: s.formula.format_plain(0),
                    file: loc.map(|a| a.file.clone()),
                    line: loc.map(|a| a.line),
                }
            })
            .collect()
    }
}

/// Curated prover ask result: SZS-ish status, the cited proof, and three
/// renderings of it (Graphviz DOT, English prose, raw engine trace).
#[cfg(any(feature = "ask", feature = "native-prover"))]
#[derive(serde::Serialize)]
pub struct AskResultView {
    pub status: String,
    pub proved: bool,
    pub given_steps: Option<usize>,
    pub raw_output: String,
    pub proof: Vec<ProofStepView>,
    /// The proof rendered as a Graphviz DOT digraph (one node per step, one
    /// edge per premise) -- always a syntactically valid graph, even when
    /// `proof` is empty.  Safe to hand straight to a DOT renderer.
    pub graphviz: String,
    /// The proof narrated as connected English prose (goal restatement, the
    /// axioms/hypotheses used, then the derivation chain).  Empty when there
    /// is no proof to narrate.
    pub prose: String,
    /// Symbols the prose showed by bare name because they have no
    /// `format`/`termFormat` in the rendering language.  Sorted, de-duplicated.
    pub prose_missing: Vec<String>,
}

#[cfg(any(feature = "ask", feature = "native-prover"))]
impl AskResultView {
    /// Project a prover outcome (from the native engine or a parsed external
    /// transcript) against the KB that ran it.  `query_kif` is reparsed only
    /// for the prose's goal restatement; a parse failure just drops that
    /// opener line, it never fails the projection.
    ///
    /// Building the axiom source index walks and fingerprints every root
    /// sentence, so it happens once and only when there is actually a proof
    /// to cite -- an unproved ask (Timeout/Unknown) doesn't pay a whole-KB
    /// pass to project an empty vec.
    pub fn project<L: TopLayer>(
        kb: &KnowledgeBase<L>,
        status: ProverStatus,
        given_steps: Option<usize>,
        raw_output: String,
        proof_kif: &[KifProofStep],
        query_kif: &str,
    ) -> Self {
        let status_str = format!("{:?}", status);
        let graphviz = sigmakee_rs_core::render_graphviz(proof_kif, "ask", &status_str);
        let (proof, prose, prose_missing) = if proof_kif.is_empty() {
            (Vec::new(), String::new(), Vec::new())
        } else {
            let src_idx = kb.build_axiom_source_index();
            let proof = ProofStepView::project(proof_kif, &src_idx);
            let goal_doc = sigmakee_rs_core::parse_document(
                "__prose_goal__",
                query_kif.to_string(),
                sigmakee_rs_core::Parser::Kif { options: None },
            );
            let goal_ast = goal_doc.ast.iter().find_map(|d| d.as_stmt());
            let report =
                kb.render_proof_prose_with(goal_ast, proof_kif, "EnglishLanguage", &src_idx);
            (proof, report.rendered, report.missing)
        };
        Self {
            status: status_str,
            proved: status == ProverStatus::Proved,
            given_steps,
            raw_output,
            proof,
            graphviz,
            prose,
            prose_missing,
        }
    }
}

/// One distinct contradiction an audit found -- a full derivation to `FALSE`,
/// with the same three renderings as [`AskResultView`].
#[cfg(any(feature = "ask", feature = "native-prover"))]
#[derive(serde::Serialize)]
pub struct ContradictionView {
    pub steps: Vec<ProofStepView>,
    pub graphviz: String,
    pub prose: String,
    pub prose_missing: Vec<String>,
}

/// Curated consistency-audit result.
#[cfg(any(feature = "ask", feature = "native-prover"))]
#[derive(serde::Serialize)]
pub struct AuditResultView {
    pub status: String,
    pub inconsistent: bool,
    pub given_steps: Option<usize>,
    pub raw_output: String,
    pub contradictions: Vec<ContradictionView>,
}

#[cfg(any(feature = "ask", feature = "native-prover"))]
impl AuditResultView {
    /// Project an audit outcome against the KB that ran it.  The axiom source
    /// index is built once and shared across all contradictions -- rendering N
    /// contradictions would otherwise repeat a whole-KB fingerprint pass N
    /// times.
    pub fn project<L: TopLayer>(
        kb: &KnowledgeBase<L>,
        status: ProverStatus,
        given_steps: Option<usize>,
        raw_output: String,
        contradiction_proofs: &[Vec<KifProofStep>],
    ) -> Self {
        let src_idx = if contradiction_proofs.is_empty() {
            None
        } else {
            Some(kb.build_axiom_source_index())
        };
        let contradictions: Vec<ContradictionView> = contradiction_proofs
            .iter()
            .enumerate()
            .map(|(i, steps)| {
                let src_idx = src_idx.as_ref().expect("index built when proofs exist");
                // A contradiction has no conjecture to restate -- it refutes
                // the KB itself -- so the prose opens straight into the
                // derivation.
                let prose_report =
                    kb.render_proof_prose_with(None, steps, "EnglishLanguage", src_idx);
                ContradictionView {
                    graphviz: sigmakee_rs_core::render_graphviz(
                        steps,
                        &format!("contradiction-{}", i + 1),
                        "Inconsistent",
                    ),
                    prose: prose_report.rendered,
                    prose_missing: prose_report.missing,
                    steps: ProofStepView::project(steps, src_idx),
                }
            })
            .collect();
        Self {
            status: format!("{:?}", status),
            inconsistent: status == ProverStatus::Inconsistent,
            given_steps,
            raw_output,
            contradictions,
        }
    }
}

// -- Session view ops ----------------------------------------------------------

impl<L: TopLayer> Session<L> {
    /// The full symbol card for `symbol` (see [`ManPageDetail`]), or `None`
    /// when the symbol is unknown.
    pub fn manpage_detail(&self, symbol: &str) -> Option<ManPageDetail> {
        self.kb
            .manpage(symbol)
            .map(|p| ManPageDetail::project(&self.kb, &p))
    }

    /// Direct taxonomy edges of `symbol` (see [`TaxonomyView`]).
    pub fn taxonomy_view(&self, symbol: &str) -> TaxonomyView {
        let (parents, children) = self.kb.taxonomy_edges(symbol);
        let edge = |e: sigmakee_rs_core::ParentEdge| EdgeView {
            relation: e.relation,
            parent: e.parent,
        };
        TaxonomyView {
            parents: parents.into_iter().map(edge).collect(),
            children: children.into_iter().map(edge).collect(),
        }
    }

    /// Full-text / symbol search projected to boundary-safe hits.
    pub fn search_view(
        &self,
        query: &str,
        opts: &sigmakee_rs_core::SearchOpts,
    ) -> Vec<SearchHitView> {
        SearchHitView::from_slice(&self.kb.search(query, opts))
    }

    /// Summary counts describing the loaded KB (see [`KbStatsView`]).
    ///
    /// `symbols` counts ontology terms a reader would recognise: KIF
    /// variables (`?x` / `@row`), the store's scope-qualified variable
    /// symbols, and CNF skolem constants are excluded.  Internal scratch
    /// sessions (files starting `"__"`) are not constituents and are excluded
    /// from every count.  `rules` are axioms whose top-level connective is
    /// `=>` or `<=>`.
    pub fn stats_view(&self) -> KbStatsView {
        use sigmakee_rs_core::{Element, OpKind};
        let kb = &self.kb;

        let symbols = kb
            .iter_symbols()
            .filter(|(_, name)| !name.starts_with('?') && !name.starts_with('@'))
            .filter(|(_, name)| !kb.symbol_is_variable(name))
            .filter(|(_, name)| !kb.symbol_is_skolem(name))
            .count();

        let files: Vec<String> = kb
            .iter_files()
            .into_iter()
            .filter(|f| !f.starts_with("__"))
            .collect();

        let mut axioms = 0usize;
        let mut rules = 0usize;
        for f in &files {
            for sid in kb.file_roots(f) {
                axioms += 1;
                if let Some(sent) = kb.sentence(sid) {
                    if matches!(
                        sent.elements.first(),
                        Some(Element::Op(OpKind::Implies | OpKind::Iff))
                    ) {
                        rules += 1;
                    }
                }
            }
        }

        let doc_langs = |v: Vec<(String, usize)>| -> Vec<DocLangView> {
            v.into_iter()
                .map(|(language, documented)| DocLangView {
                    language,
                    documented,
                })
                .collect()
        };

        let v = kb.vocab_stats();
        KbStatsView {
            files: files.len(),
            symbols,
            axioms,
            rules,
            classes: v.classes,
            instances: v.instances,
            relations: v.relations,
            predicates: v.predicates,
            functions: v.functions,
            documented: v.documented,
            labeled: v.labeled,
            doc_languages: doc_langs(v.doc_languages),
            term_languages: doc_langs(v.term_languages),
        }
    }

    /// The `(instance ? NaturalLanguage)` symbols -- including instances of
    /// `NaturalLanguage` subclasses -- each with the English label from its
    /// `termFormat` (falling back to the bare symbol name).  Sorted by label,
    /// with `EnglishLanguage` guaranteed present.  Powers a UI language
    /// selector.
    pub fn natural_languages_view(&self) -> Vec<LangView> {
        let mut langs: Vec<LangView> = Vec::new();
        for symbol in self.kb.instances_of("NaturalLanguage") {
            let label = self
                .kb
                .term_format(&symbol, Some("EnglishLanguage"))
                .first()
                .map(|d| d.text.clone())
                .unwrap_or_else(|| symbol.clone());
            langs.push(LangView { symbol, label });
        }
        if !langs.iter().any(|l| l.symbol == "EnglishLanguage") {
            langs.push(LangView {
                symbol: "EnglishLanguage".into(),
                label: "English".into(),
            });
        }
        langs.sort_by_key(|a| a.label.to_lowercase());
        langs
    }

    /// Natural-language paraphrase of a single KIF formula in `language`,
    /// using the KB's format / termFormat templates.  Empty string when the
    /// KIF does not parse to a statement.
    #[cfg(any(feature = "ask", feature = "native-prover"))]
    pub fn render_nl(&self, kif: &str, language: &str) -> String {
        let doc = sigmakee_rs_core::parse_document(
            "__sdk:render_nl__",
            kif.to_string(),
            sigmakee_rs_core::Parser::Kif { options: None },
        );
        match doc.ast.iter().find_map(|d| d.as_stmt()) {
            Some(ast) => self.kb.render_formula(ast, language).rendered,
            None => String::new(),
        }
    }

    /// Parse a captured Vampire run's combined stdout+stderr into the same
    /// shape a native ask projects to -- status (with Vampire's own
    /// Theorem-vs-ContradictoryAxioms mislabelling corrected), proof steps,
    /// Graphviz digraph, and English prose -- so both backends render through
    /// one UI code path.
    #[cfg(any(feature = "ask", feature = "native-prover"))]
    pub fn vampire_ask_view(&self, raw_output: &str, query_kif: &str) -> AskResultView {
        let parsed =
            sigmakee_rs_core::parse_vampire_result(raw_output, sigmakee_rs_core::ProverMode::Prove);
        AskResultView::project(
            &self.kb,
            parsed.status,
            None,
            raw_output.to_string(),
            &parsed.proof,
            query_kif,
        )
    }

    /// Parse a captured Vampire consistency-check run into the same shape a
    /// native audit projects to.  Vampire's one-shot run yields at most a
    /// single contradiction, so `contradictions` has 0 or 1 entries.
    #[cfg(any(feature = "ask", feature = "native-prover"))]
    pub fn vampire_audit_view(&self, raw_output: &str) -> AuditResultView {
        let parsed = sigmakee_rs_core::parse_vampire_result(
            raw_output,
            sigmakee_rs_core::ProverMode::CheckConsistency,
        );
        let proofs: Vec<Vec<KifProofStep>> =
            if parsed.status == ProverStatus::Inconsistent && !parsed.proof.is_empty() {
                vec![parsed.proof]
            } else {
                Vec::new()
            };
        AuditResultView::project(
            &self.kb,
            parsed.status,
            None,
            raw_output.to_string(),
            &proofs,
        )
    }
}

#[cfg(feature = "native-prover")]
impl<S: TopLayer + 'static> Session<sigmakee_rs_core::ProverLayer<S>> {
    /// Prove `query_kif` (a single KIF conjecture) with the native saturation
    /// prover and project the outcome (see [`AskResultView`]).  `session`
    /// names optional in-memory support assertions; `opts` carries the budget
    /// and SInE selection.
    pub fn ask_view(
        &self,
        query_kif: &str,
        session: Option<&str>,
        opts: sigmakee_rs_core::NativeOpts,
    ) -> AskResultView {
        let sine = opts.selection;
        let result = self.kb.ask_query(query_kif, session, sine, opts);
        AskResultView::project(
            &self.kb,
            result.status,
            result.given_steps,
            result.raw_output,
            &result.proof_kif,
            query_kif,
        )
    }

    /// Audit the whole KB for logical consistency with the native saturation
    /// prover, enumerating up to `limit` distinct contradictions, and project
    /// the outcome (see [`AuditResultView`]).
    pub fn audit_view(&self, opts: sigmakee_rs_core::NativeOpts, limit: usize) -> AuditResultView {
        let result = self.kb.audit_consistency(&[], opts, limit);
        AuditResultView::project(
            &self.kb,
            result.status,
            result.given_steps,
            result.raw_output,
            &result.contradiction_proofs,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;
    use sigmakee_rs_core::TranslationLayer;

    fn session_with(kif: &str) -> Session<TranslationLayer> {
        let mut s = Session::<TranslationLayer>::new("views-test".into());
        s.ingest(
            Source::Reader {
                name: "t.kif".into(),
                reader: Box::new(std::io::Cursor::new(Vec::from(kif))),
            },
            true,
        );
        s
    }

    #[test]
    fn stats_view_counts_files_axioms_and_rules() {
        let s =
            session_with("(subclass Dog Mammal)\n(=> (instance ?X Dog) (instance ?X Mammal))\n");
        let v = s.stats_view();
        assert_eq!(v.files, 1);
        assert_eq!(v.axioms, 2);
        assert_eq!(v.rules, 1);
        assert!(v.symbols >= 2, "Dog and Mammal at least; got {}", v.symbols);
    }

    #[test]
    fn manpage_detail_projects_references_with_kif_text() {
        // Taxonomy heads (`instance`/`subclass`) are excluded from the
        // reference list by design (they render as edges instead), so the
        // reference fixture is a rule mentioning Dog.
        let s =
            session_with("(subclass Dog Mammal)\n(=> (instance ?X Dog) (instance ?X Mammal))\n");
        let d = s.manpage_detail("Dog").expect("Dog has a man page");
        assert_eq!(d.name, "Dog");
        assert!(
            d.references.iter().any(|r| r.kif.contains("Dog")),
            "references should carry rendered KIF; got {:?}",
            d.references.iter().map(|r| &r.kif).collect::<Vec<_>>()
        );
        assert!(s.manpage_detail("NoSuchSymbol__").is_none());
    }

    #[test]
    fn taxonomy_view_carries_edges_both_ways() {
        let s = session_with("(subclass Dog Mammal)\n");
        let up = s.taxonomy_view("Dog");
        assert!(up.parents.iter().any(|e| e.parent == "Mammal"));
        let down = s.taxonomy_view("Mammal");
        assert!(down.children.iter().any(|e| e.parent == "Dog"));
    }

    #[test]
    fn natural_languages_view_always_includes_english() {
        let s = session_with("(subclass Dog Mammal)\n");
        let langs = s.natural_languages_view();
        assert!(langs.iter().any(|l| l.symbol == "EnglishLanguage"));
    }

    #[test]
    fn diagnostic_view_preserves_span_fields() {
        let mut s = session_with("(subclass Dog Mammal)\n");
        let diags = s.validate_formula("(subclass Dog)").unwrap();
        let views = DiagnosticView::from_slice(&diags);
        assert_eq!(views.len(), diags.len());
        if let (Some(v), Some(d)) = (views.first(), diags.first()) {
            assert_eq!(v.line, d.range.line);
            assert_eq!(v.message, d.message);
        }
    }
}
