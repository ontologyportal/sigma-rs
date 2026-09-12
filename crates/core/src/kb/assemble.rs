//! TPTP assembler: walks an `ir::Problem` and produces a TPTP string using
//! SUMO-friendly conventions — SID-based axiom names (`kb_<sid>`), optional
//! leading KIF comments, and a customisable conjecture label.

use std::collections::HashSet;
use std::fmt::Write as _;

#[cfg(feature = "external-prover")]
use crate::trans::ir::HoProblem;
use crate::trans::ir::{LogicMode, Problem as IrProblem};

use crate::semantics::SemanticLayer;
use crate::syntactic::sentence_to_plain_kif;
use crate::types::SentenceId;

/// What the assembler needs from a problem representation: the language
/// keyword, the declaration preamble, and each axiom / the conjecture as
/// formula text.  The first-order [`IrProblem`] and the higher-order
/// [`HoProblem`] both implement it, so `kb_<sid>` naming, line indexing,
/// filtering, and KIF comments are one implementation for every dialect.
pub trait TptpProblem {
    /// `fof` / `tff` / `thf`.
    fn keyword(&self) -> &'static str;
    /// Declaration lines emitted ahead of the axioms, in order.
    fn preamble_lines(&self) -> Vec<String>;
    /// Each axiom's formula text, in `sid_map` order.
    fn axiom_texts(&self) -> Box<dyn Iterator<Item = String> + '_>;
    /// The conjecture's formula text, if the problem has one.
    fn conjecture_text(&self) -> Option<String>;
}

impl TptpProblem for IrProblem {
    fn keyword(&self) -> &'static str {
        match self.mode() {
            LogicMode::Tff => "tff",
            LogicMode::Fof => "fof",
        }
    }

    fn preamble_lines(&self) -> Vec<String> {
        // Sort / function / predicate declarations in insertion order.
        self.sort_decls()
            .iter()
            .filter_map(|s| s.tptp_decl())
            .chain(self.fn_decls().iter().filter_map(|f| f.tptp_decl()))
            .chain(self.pred_decls().iter().filter_map(|p| p.tptp_decl()))
            .collect()
    }

    fn axiom_texts(&self) -> Box<dyn Iterator<Item = String> + '_> {
        Box::new(self.axioms().iter().map(|ax| ax.to_tptp()))
    }

    fn conjecture_text(&self) -> Option<String> {
        self.conjecture_ref().map(|c| c.to_tptp())
    }
}

#[cfg(feature = "external-prover")]
impl TptpProblem for HoProblem {
    fn keyword(&self) -> &'static str {
        "thf"
    }

    fn preamble_lines(&self) -> Vec<String> {
        self.decls()
            .iter()
            .map(|d| format!("thf({}_tp, type, {}: {}).", d.name, d.name, d.sort.thf()))
            .collect()
    }

    fn axiom_texts(&self) -> Box<dyn Iterator<Item = String> + '_> {
        Box::new(self.axioms().iter().map(|ax| ax.thf()))
    }

    fn conjecture_text(&self) -> Option<String> {
        self.conjecture_ref().map(|c| c.thf())
    }
}

/// Configuration for [`assemble_tptp_indexed`].
pub struct AssemblyOpts<'a> {
    /// Emit `% <original KIF>` before each axiom whose SentenceId appears in
    /// the `sid_map`.  Requires `layer` to be `Some` to render the KIF
    /// string; silently ignored otherwise.
    pub show_kif: bool,

    /// Semantic layer used for rendering KIF comments.  Only consulted when
    /// `show_kif` is `true`.
    pub layer: Option<&'a SemanticLayer>,

    /// Prefix for axiom identifiers.  An axiom whose `sid_map` entry is
    /// SentenceId `N` becomes `<prefix>N`.  Default: `"kb_"`.
    pub axiom_prefix: &'a str,

    /// Role for axioms.  Default: `"axiom"`.  Set to `"hypothesis"` when
    /// emitting session assertions, or `"negated_conjecture"` for
    /// consistency checks.
    pub axiom_role: &'a str,

    /// Identifier used for the problem's conjecture.  Default:
    /// `"conjecture"`.
    pub conjecture_name: &'a str,

    /// Optional allow-list of axiom SentenceIds: when `Some`, only
    /// axioms whose parallel [`sid_map`] entry is a member of the
    /// set are emitted.  `None` emits every axiom in
    /// `problem.axioms()` (the default).  The conjecture is always
    /// emitted when present; filtering affects axioms only.
    ///
    /// Axioms whose `sid_map` entry is missing (index beyond
    /// `sid_map.len()`, emitted as `<prefix>anon_<i>`) are always
    /// emitted regardless of the filter — the filter can't decide
    /// relevance without a sid.
    ///
    /// [`sid_map`]: assemble_tptp_indexed
    pub axiom_filter: Option<&'a HashSet<SentenceId>>,
}

impl<'a> Default for AssemblyOpts<'a> {
    fn default() -> Self {
        Self {
            show_kif: false,
            layer: None,
            axiom_prefix: "kb_",
            axiom_role: "axiom",
            conjecture_name: "conjecture",
            axiom_filter: None,
        }
    }
}

/// Serialise `problem` to TPTP. Axioms are named `<prefix><sid>` using the
/// corresponding entry in `sid_map` (assumed to be parallel to
/// `problem.axioms()`). If `sid_map` is shorter than the axiom list, the
/// remainder fall back to `<prefix>anon_<index>`.
///
/// When `axiom_lines` is `Some`, it's filled with each emitted axiom's
/// starting 0-based line number in the output — e.g. for a "jump to this
/// axiom" pane that needs to know where a given [`SentenceId`] landed in the
/// assembled text, without re-scanning it afterward. An axiom repeated via
/// `_v<n>` naming (a sid pairing with several axioms) keeps only its FIRST
/// line. Tracking is an O(1)-per-axiom running counter, not a re-scan, so
/// passing `None` costs nothing extra.
pub fn assemble_tptp_indexed<P: TptpProblem + ?Sized>(
    problem: &P,
    sid_map: &[SentenceId],
    opts: &AssemblyOpts,
    mut axiom_lines: Option<&mut std::collections::HashMap<SentenceId, u32>>,
) -> String {
    let kw = problem.keyword();
    let mut out = String::new();

    for d in problem.preamble_lines() {
        let _ = writeln!(out, "{}", d);
    }

    // Axioms.  Anonymous axioms (no sid) bypass `axiom_filter`.
    let mut seen_sids: std::collections::HashMap<SentenceId, u32> =
        std::collections::HashMap::new();
    let mut line_no: u32 = out.matches('\n').count() as u32;
    for (i, ax) in problem.axiom_texts().enumerate() {
        let sid = sid_map.get(i).copied();
        if let (Some(s), Some(filter)) = (sid, opts.axiom_filter) {
            if !filter.contains(&s) {
                continue;
            }
        }
        if let (Some(s), Some(lines)) = (sid, axiom_lines.as_deref_mut()) {
            lines.entry(s).or_insert(line_no);
        }
        if opts.show_kif {
            if let (Some(s), Some(layer)) = (sid, opts.layer) {
                let kif = sentence_to_plain_kif(s, &layer.syntactic);
                for line in kif.lines() {
                    let _ = writeln!(out, "% {}", line);
                    line_no += 1;
                }
            }
        }
        let name = match sid {
            // A sid can pair with several axioms; suffix repeats `_v<n>` so
            // TPTP names stay unique.
            Some(s) => {
                let n = *seen_sids.entry(s).and_modify(|n| *n += 1).or_insert(0u32);
                if n == 0 {
                    format!("{}{}", opts.axiom_prefix, s)
                } else {
                    format!("{}{}_v{}", opts.axiom_prefix, s, n)
                }
            }
            None => format!("{}anon_{}", opts.axiom_prefix, i),
        };
        let _ = writeln!(out, "{}({}, {}, {}).", kw, name, opts.axiom_role, ax);
        line_no += 1;
    }

    if let Some(c) = problem.conjecture_text() {
        let _ = writeln!(out, "{}({}, conjecture, {}).", kw, opts.conjecture_name, c);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "external-prover")]
    use crate::trans::ir::Sort as IrSort;
    use crate::trans::ir::{
        Formula as IrF, Function as IrFn, Predicate as IrPd, Problem as IrProblem, Term as IrT,
    };

    #[test]
    fn empty_problem_produces_empty_output() {
        let problem = IrProblem::new();
        let s = assemble_tptp_indexed(&problem, &[], &AssemblyOpts::default(), None);
        assert_eq!(s, "");
    }

    #[test]
    fn axioms_get_sid_based_names() {
        let p = IrPd::new("P", 1);
        let a = IrT::constant(IrFn::new("a", 0));
        let b = IrT::constant(IrFn::new("b", 0));

        let mut pb = IrProblem::new();
        pb.with_axiom(IrF::atom(p.clone(), vec![a]));
        pb.with_axiom(IrF::atom(p, vec![b]));

        let tptp = assemble_tptp_indexed(&pb, &[42, 7], &AssemblyOpts::default(), None);
        assert!(tptp.contains("fof(kb_42, axiom, P(a))."), "{}", tptp);
        assert!(tptp.contains("fof(kb_7, axiom, P(b))."), "{}", tptp);
    }

    #[test]
    fn missing_sid_falls_back_to_anon() {
        let p = IrPd::new("P", 0);
        let mut pb = IrProblem::new();
        pb.with_axiom(IrF::atom(p, vec![]));

        let tptp = assemble_tptp_indexed(&pb, &[], &AssemblyOpts::default(), None);
        assert!(tptp.contains("fof(kb_anon_0, axiom, P)."), "{}", tptp);
    }

    #[test]
    #[cfg(feature = "external-prover")]
    fn custom_role_and_conjecture_name() {
        let p = IrPd::new("P", 0);
        let mut pb = IrProblem::new();
        pb.with_axiom(IrF::atom(p.clone(), vec![]));
        pb.conjecture(IrF::atom(p, vec![]));

        let opts = AssemblyOpts {
            axiom_role: "hypothesis",
            conjecture_name: "query_0",
            ..AssemblyOpts::default()
        };
        let tptp = assemble_tptp_indexed(&pb, &[1], &opts, None);
        assert!(tptp.contains("fof(kb_1, hypothesis, P)."), "{}", tptp);
        assert!(tptp.contains("fof(query_0, conjecture, P)."), "{}", tptp);
    }

    #[test]
    fn axiom_filter_keeps_only_allow_listed_sids() {
        // Build a problem with three axioms (sids 10, 20, 30).
        // Filter to {10, 30} — only those two should appear in the
        // emitted TPTP, sid 20 is dropped.
        let p = IrPd::new("P", 1);
        let a = IrT::constant(IrFn::new("a", 0));
        let b = IrT::constant(IrFn::new("b", 0));
        let c = IrT::constant(IrFn::new("c", 0));

        let mut pb = IrProblem::new();
        pb.with_axiom(IrF::atom(p.clone(), vec![a]));
        pb.with_axiom(IrF::atom(p.clone(), vec![b]));
        pb.with_axiom(IrF::atom(p, vec![c]));

        let allow: HashSet<SentenceId> = [10, 30].into_iter().collect();
        let opts = AssemblyOpts {
            axiom_filter: Some(&allow),
            ..AssemblyOpts::default()
        };
        let tptp = assemble_tptp_indexed(&pb, &[10, 20, 30], &opts, None);
        assert!(tptp.contains("fof(kb_10"), "must keep sid 10: {}", tptp);
        assert!(!tptp.contains("fof(kb_20"), "must drop sid 20: {}", tptp);
        assert!(tptp.contains("fof(kb_30"), "must keep sid 30: {}", tptp);
    }

    #[test]
    fn axiom_filter_none_emits_every_axiom() {
        // Default `axiom_filter: None` is the historical all-emit
        // behaviour.  Regression guard so the new field doesn't
        // accidentally flip that default.
        let p = IrPd::new("P", 0);
        let mut pb = IrProblem::new();
        pb.with_axiom(IrF::atom(p.clone(), vec![]));
        pb.with_axiom(IrF::atom(p, vec![]));

        let tptp = assemble_tptp_indexed(&pb, &[1, 2], &AssemblyOpts::default(), None);
        assert!(tptp.contains("fof(kb_1"), "{}", tptp);
        assert!(tptp.contains("fof(kb_2"), "{}", tptp);
    }

    #[test]
    #[cfg(feature = "external-prover")]
    fn axiom_filter_preserves_conjecture() {
        // Filtering applies to axioms; the conjecture is emitted
        // unconditionally when `problem.conjecture_ref()` is `Some`.
        let p = IrPd::new("P", 0);
        let mut pb = IrProblem::new();
        pb.with_axiom(IrF::atom(p.clone(), vec![]));
        pb.conjecture(IrF::atom(p, vec![]));

        // Filter excludes every axiom — conjecture still rendered.
        let empty: HashSet<SentenceId> = HashSet::new();
        let opts = AssemblyOpts {
            axiom_filter: Some(&empty),
            conjecture_name: "query_0",
            ..AssemblyOpts::default()
        };
        let tptp = assemble_tptp_indexed(&pb, &[1], &opts, None);
        assert!(
            !tptp.contains("fof(kb_1"),
            "axiom must be dropped: {}",
            tptp
        );
        assert!(
            tptp.contains("fof(query_0, conjecture, P)."),
            "conjecture must survive: {}",
            tptp
        );
    }

    #[test]
    fn axiom_filter_keeps_anonymous_axioms() {
        // Axioms with no sid_map entry can't be classified by the
        // filter, so the assembler keeps them rather than silently
        // dropping them — they fall through to the `kb_anon_<i>`
        // name.  Regression guard for this escape hatch.
        let p = IrPd::new("P", 0);
        let mut pb = IrProblem::new();
        pb.with_axiom(IrF::atom(p.clone(), vec![]));
        pb.with_axiom(IrF::atom(p, vec![]));

        // sid_map is shorter than axioms() — the second axiom is
        // anonymous.  Filter excludes the first (sid 7).
        let empty: HashSet<SentenceId> = HashSet::new();
        let opts = AssemblyOpts {
            axiom_filter: Some(&empty),
            ..AssemblyOpts::default()
        };
        let tptp = assemble_tptp_indexed(&pb, &[7], &opts, None);
        assert!(
            !tptp.contains("fof(kb_7"),
            "sid 7 must be dropped: {}",
            tptp
        );
        assert!(
            tptp.contains("fof(kb_anon_1"),
            "anon axiom must survive: {}",
            tptp
        );
    }

    #[test]
    #[cfg(feature = "external-prover")]
    fn tff_mode_emits_type_declarations_first() {
        let person = IrSort::new("person");
        let alice = IrFn::typed("alice", &[], person.clone());
        let mortal = IrPd::typed("mortal", std::slice::from_ref(&person));

        let mut pb = IrProblem::new_tff();
        pb.declare_sort(person);
        pb.declare_function(alice.clone());
        pb.declare_predicate(mortal.clone());
        pb.with_axiom(IrF::atom(mortal, vec![IrT::apply(alice, vec![])]));

        let tptp = assemble_tptp_indexed(&pb, &[3], &AssemblyOpts::default(), None);
        // Type decls come before the axiom.
        let person_pos = tptp.find("person_type").unwrap();
        let axiom_pos = tptp.find("kb_3").unwrap();
        assert!(
            person_pos < axiom_pos,
            "type decl must precede axiom; got:\n{}",
            tptp
        );
        assert!(
            tptp.contains("tff(kb_3, axiom, mortal(alice))."),
            "{}",
            tptp
        );
    }
}

#[cfg(all(test, feature = "external-prover"))]
mod thf_tests {
    use super::*;
    use crate::trans::ir::{HoSort, ThfConst, ThfExpr};
    use std::collections::HashMap;

    fn problem() -> HoProblem {
        let mut p = HoProblem::new();
        p.declare(ThfConst {
            name: "s__p".into(),
            sort: HoSort::O,
        });
        p.with_axiom(ThfExpr::Const("s__p".into()));
        p.with_axiom(ThfExpr::Not(Box::new(ThfExpr::False)));
        p.conjecture(ThfExpr::Const("s__p".into()));
        p
    }

    #[test]
    fn thf_goes_through_the_indexed_assembler_with_line_numbers() {
        let mut lines: HashMap<SentenceId, u32> = HashMap::new();
        let text = assemble_tptp_indexed(
            &problem(),
            &[SentenceId::from(3u64), SentenceId::from(4u64)],
            &AssemblyOpts::default(),
            Some(&mut lines),
        );
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0], "thf(s__p_tp, type, s__p: $o).");
        assert_eq!(rows[1], "thf(kb_3, axiom, s__p).");
        assert_eq!(rows[2], "thf(kb_4, axiom, (~ $false)).");
        assert_eq!(rows[3], "thf(conjecture, conjecture, s__p).");
        assert_eq!(lines[&SentenceId::from(3u64)], 1);
        assert_eq!(lines[&SentenceId::from(4u64)], 2);
    }

    #[test]
    fn thf_honours_the_axiom_filter_and_role() {
        let keep: HashSet<SentenceId> = [SentenceId::from(4u64)].into_iter().collect();
        let opts = AssemblyOpts {
            axiom_role: "hypothesis",
            axiom_filter: Some(&keep),
            ..Default::default()
        };
        let text = assemble_tptp_indexed(
            &problem(),
            &[SentenceId::from(3u64), SentenceId::from(4u64)],
            &opts,
            None,
        );
        assert!(!text.contains("kb_3"), "{text}");
        assert!(
            text.contains("thf(kb_4, hypothesis, (~ $false))."),
            "{text}"
        );
    }
}
