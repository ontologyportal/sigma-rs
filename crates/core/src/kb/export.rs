//! TPTP export entrypoints on `KnowledgeBase`: `to_tptp`,
//! `format_sentence_tptp`, and their helpers.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cache::events::Event;
use crate::syntactic::SelectionParams;
use crate::types::SentenceId;
use crate::{Diagnostic, ExternalOpts, HasTranslation, Parser, ProveCtx, SourceFile, TestCase, TptpLang};
use crate::prover::Conjecture;
use super::assemble::{assemble_tptp, AssemblyOpts};

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
    pub fn to_tptp(&mut self, opts: &TptpOptions, session: Option<&str>) -> String {

        crate::with_guard!(self);

        let mode = opts.lang;
        let syn  = &self.layer.semantic().syntactic;

        let mut axioms_sorted: Vec<SentenceId> = self
            .axiom_ids_set()
            .into_iter()
            .chain(syn.synthetic_origin.keys().copied())
            .filter(|&sid| {
                !self.sentence_excluded(sid, &opts.excluded)
                    && !self.layer.translation().suppressed.read().unwrap().contains(&sid)
            })
            .collect();
        axioms_sorted.sort_unstable();
        axioms_sorted.dedup();

        // Session assertions (hypotheses) fold into the axiom list so one
        // `build_problem` pass assembles everything.
        if let Some(name) = session {
            if let Some(sids) = self.sessions.get(name) {
                for &sid in sids {
                    if self.sentence_excluded(sid, &opts.excluded) { continue; }
                    axioms_sorted.push(sid);
                }
                axioms_sorted.sort_unstable();
                axioms_sorted.dedup();
            }
        }

        let (axiom_problem, axiom_sid_map) = self.layer.translation().build_problem(&axioms_sorted, mode);

        assemble_tptp(&axiom_problem, &axiom_sid_map, &AssemblyOpts {
            show_kif: opts.show_kif_comment,
            layer:    Some(&self.layer.semantic()),
            ..AssemblyOpts::default()
        })
    }

    /// Render a testcase to TPTP.
    ///
    /// Like [`KnowledgeBase::to_tptp`], but accepts optional [`ExternalOpts`]
    /// controlling axiom selection.
    ///
    /// # Errors
    ///
    /// Returns the ingestion diagnostics if interning the testcase's
    /// hypotheses or conjecture fails.
    pub fn tc_to_tptp(
        &self,
        tc: TestCase,
        translation_opts: &TptpOptions,
        session: Option<&str>,
        prover_opts: Option<ExternalOpts>
    ) -> Result<String, Vec<Diagnostic>> {
        let syn = &self.layer.semantic().syntactic;
        let uuid = format!("{:x}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis());
        let session = session.unwrap_or(&uuid);
        let mode = translation_opts.lang;
        let prover_opts = prover_opts.unwrap_or_default();

        // Intern the hypotheses into the session as force-included support axioms.
        let support = self.ingest_source(SourceFile {
            parser:   Parser::Kif,
            name:     tc.file_name.clone(),
            path:     tc.file_name.clone().into(),
            origin:   crate::FileOrigin::Local,
            contents: String::new(),
            prebuilt: Some(tc.axioms),
        }, session, true);
        if support.errors.iter().any(|d| d.is_err()) {
            return Err(support.errors);
        }
        let problem_sids: Vec<SentenceId> = support.emitted.into_iter()
            .filter_map(|e| match e { Event::RootAdded { sid } => Some(sid), _ => None })
            .collect();

        // Intern the conjecture (if any), collecting its SInE seed symbols and sid.
        let (seed_syms, query_sid): (HashSet<_>, Option<SentenceId>) = match tc.query.clone() {
            Some(query) => {
                let normalized = Conjecture::normalize(vec![query]);
                let syms       = Conjecture::seed(&normalized);
                let res = self.ingest_source(SourceFile {
                    parser:   Parser::Kif,
                    name:     tc.file_name.clone(),
                    path:     tc.file_name.clone().into(),
                    origin:   crate::FileOrigin::Local,
                    contents: String::new(),
                    prebuilt: Some(normalized),
                }, session, true);
                if res.errors.iter().any(|d| d.is_err()) {
                    return Err(res.errors);
                }
                let sid = res.emitted.into_iter()
                    .find_map(|e| match e { Event::RootAdded { sid } => Some(sid), _ => None });
                (syms, sid)
            }
            None => (HashSet::new(), None),
        };

        // SInE-select the relevant KB axioms from the conjecture's seed.
        let (mut selected, frontier) = syn.select_relevant(
            &seed_syms,
            prover_opts.selection,
            &SelectionParams::default(),
            &ProveCtx::default(),
        );
        selected.extend(frontier);

        // Drop the explicitly-added hypotheses and conjecture from the SInE set
        // to avoid duplicating them, or re-asserting the un-negated conjecture
        // as an axiom.
        let mut explicit: HashSet<SentenceId> = problem_sids.iter().copied().collect();
        explicit.extend(query_sid);
        let axiom_sids: Vec<SentenceId> = selected.difference(&explicit).copied().collect();

        let mut all_axioms = axiom_sids;
        for &sid in &problem_sids {
            if Some(sid) == query_sid { continue; } // never add the conjecture as support
            if self.sentence_excluded(sid, &translation_opts.excluded) { continue; }
            all_axioms.push(sid);
        }
        let conjecture: Vec<SentenceId> = query_sid.into_iter().collect();
        let query_scope = crate::semantics::types::Scope::Session(
            crate::syntactic::caches::session::session_id(session),
        );
        let (problem, sid_map, _qvm) = self.layer.translation().assemble_problem(
            &all_axioms,
            &problem_sids,
            &conjecture,
            mode,
            Some(query_scope),
        );
        Ok(assemble_tptp(&problem, &sid_map, &AssemblyOpts {
            show_kif: translation_opts.show_kif_comment,
            layer:    Some(&self.layer.semantic()),
            ..AssemblyOpts::default()
        }))
    }
    

    /// Return the head predicate name of a sentence, if it has one.
    /// Returns `None` for operator-rooted sentences (e.g. `(and ...)`) or
    /// for sentences whose first element is not a plain symbol.
    fn sentence_head_name(&self, sid: SentenceId) -> Option<String> {
        use crate::types::Element;
        let store = &self.layer.semantic().syntactic;
        if !store.has_sentence(sid) { return None; }
        let sentence = store.sentence(sid)?;
        match sentence.elements.first()? {
            Element::Symbol(sym) => Some(sym.to_string()),
            _ => None,
        }
    }

    /// `true` if the sentence's head predicate matches an `excluded` entry.
    fn sentence_excluded(&self, sid: SentenceId, excluded: &HashSet<String>) -> bool {
        if excluded.is_empty() { return false; }
        self.sentence_head_name(sid)
            .map(|n| excluded.contains(&n))
            .unwrap_or(false)
    }

    /// Render a single sentence as TPTP.
    ///
    /// Returns the formula body only (no `tff(...)` / `fof(...)` wrapper);
    /// callers add their own `<kw>(name, role, ...)` framing.  Respects
    /// `opts.query` (existential wrap for conjectures vs universal wrap
    /// for axioms), `opts.lang`, and `opts.hide_numbers`.
    pub fn format_sentence_tptp(&mut self, sid: SentenceId, opts: &TptpOptions) -> String {
        crate::with_guard!(self);
        self.layer.translation().ensure_rewrite_pass();

        let mode  = opts.lang;
        let trans = self.layer.translation();

        if opts.query {
            return trans
                .lower_conjecture(sid, mode.is_typed(), opts.hide_numbers, None)
                .map(|(cf, _qvm)| cf.formula.to_tptp())
                .unwrap_or_default();
        }
        trans
            .lower_axiom(sid, mode.is_typed(), opts.hide_numbers)
            .map(|cf| cf.formula.to_tptp())
            .unwrap_or_default()
    }
}

/// Canonical SUMO bookkeeping predicates that are noise for a theorem
/// prover and are filtered out of any TPTP shipped to Vampire.
///
/// Both [`TptpOptions::default`] and the KB-layer excluded-head filter
/// source their exclusion set from this list. Per-call overrides for
/// non-default exclusion sets stay on `TptpOptions::excluded`.
pub fn default_excluded_heads() -> &'static [&'static str] {
    &[
        // Signature metadata — redundant once translation has emitted a
        // `tff(..., type, ...)` declaration for the head.
        "domain",
        "domainSubclass",
        "range",
        "rangeSubclass",
        // Documentation / surface-form metadata.
        "documentation",
        "format",
        "termFormat",
        "externalImage",
        "relatedExternalConcept",
        "relatedInternalConcept",
        "formerName",
        "abbreviation",
        "conventionalShortName",
        "conventionalLongName",
    ]
}

/// Lazy `HashSet<&'static str>` view of [`default_excluded_heads`] for
/// fast lookup.  Initialised on first use; lives for the process.
pub(crate) fn excluded_heads_set() -> &'static HashSet<&'static str> {
    use std::sync::OnceLock;
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| default_excluded_heads().iter().copied().collect())
}

/// Options controlling TPTP output.
#[derive(Debug, Clone)]
pub struct TptpOptions {
    /// The TPTP language to emit.
    pub lang:             TptpLang,
    /// Wrap free variables in `?` (existential) instead of `!` (universal).
    /// Used for query/conjecture sentences.
    pub query:            bool,
    /// Replace numeric literals with `n__N` tokens (default false).
    /// Ignored in TFF mode, where numerics are native `$int`/`$real` literals.
    pub hide_numbers:     bool,
    /// Head predicates whose sentences are omitted from KB output entirely.
    /// `domain`/`range` are excluded as top-level *axioms* but their TFF
    /// *type declarations* are still emitted.
    pub excluded:         HashSet<String>,
    /// Emit a `% <original KIF>` comment before each TPTP formula.
    pub show_kif_comment: bool,
}

impl Default for TptpOptions {
    fn default() -> Self {
        let excluded: HashSet<String> = default_excluded_heads()
            .iter().map(|s| (*s).to_string()).collect();
        TptpOptions {
            lang:             TptpLang::default(),
            query:            false,
            hide_numbers:     false,
            excluded,
            show_kif_comment: false,
        }
    }
}

impl TptpOptions {
    /// [`TptpOptions::default`] with `hide_numbers` enabled.
    pub fn default_with_hide_numbers() -> Self {
        Self { hide_numbers: true, ..Self::default() }
    }
}
