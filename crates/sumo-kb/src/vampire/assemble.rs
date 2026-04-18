// crates/sumo-kb/src/vampire/assemble.rs
//
// TPTP assembler: walks an `ir::Problem` that came out of `NativeConverter`
// and produces a TPTP string using SUMO-friendly conventions.
//
// The canonical `ir::Problem::to_tptp()` is available for anyone who wants
// the vampire-prover default output.  We use our own assembler here so we
// can:
//
//   * Name axioms `<prefix><sentence-id>` (e.g. `kb_42`) instead of the
//     opaque `axiom_0 / axiom_1 / ...`.  Stable SID-based names are what
//     the proof back-translator in `tptp/kif.rs` needs to resolve proof
//     step premises to the original KIF sentences.
//
//   * Optionally emit each axiom's original KIF as a leading `%` comment
//     (the `--show-kif` flag on `sumo translate`).
//
//   * Customise the conjecture label (the `query_0`, `check_consistency`,
//     ... names the different ask paths use).
//
// Gated: requires the `vampire` feature.

use std::fmt::Write as _;

use vampire_prover::ir::{LogicMode, Problem as IrProblem};

use crate::kif_store::sentence_to_plain_kif;
use crate::semantic::SemanticLayer;
use crate::types::SentenceId;

/// Configuration for [`assemble_tptp`].
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
}

impl<'a> Default for AssemblyOpts<'a> {
    fn default() -> Self {
        Self {
            show_kif: false,
            layer: None,
            axiom_prefix: "kb_",
            axiom_role: "axiom",
            conjecture_name: "conjecture",
        }
    }
}

/// Serialise `problem` to TPTP.
///
/// Axioms are named `<prefix><sid>` using the corresponding entry in
/// `sid_map` (assumed to be parallel to `problem.axioms()`).  If `sid_map`
/// is shorter than the axiom list, the remainder fall back to
/// `<prefix>anon_<index>`.
pub fn assemble_tptp(
    problem: &IrProblem,
    sid_map: &[SentenceId],
    opts:    &AssemblyOpts,
) -> String {
    let kw = match problem.mode() {
        LogicMode::Tff => "tff",
        LogicMode::Fof => "fof",
    };
    let mut out = String::new();

    // Preamble: sort / function / predicate declarations in insertion order.
    for s in problem.sort_decls() {
        if let Some(d) = s.tptp_decl() {
            let _ = writeln!(out, "{}", d);
        }
    }
    for f in problem.fn_decls() {
        if let Some(d) = f.tptp_decl() {
            let _ = writeln!(out, "{}", d);
        }
    }
    for p in problem.pred_decls() {
        if let Some(d) = p.tptp_decl() {
            let _ = writeln!(out, "{}", d);
        }
    }

    // Axioms.
    for (i, ax) in problem.axioms().iter().enumerate() {
        let sid = sid_map.get(i).copied();
        if opts.show_kif {
            if let (Some(s), Some(layer)) = (sid, opts.layer) {
                let kif = sentence_to_plain_kif(s, &layer.store);
                for line in kif.lines() {
                    let _ = writeln!(out, "% {}", line);
                }
            }
        }
        let name = match sid {
            Some(s) => format!("{}{}", opts.axiom_prefix, s),
            None    => format!("{}anon_{}", opts.axiom_prefix, i),
        };
        let _ = writeln!(out, "{}({}, {}, {}).", kw, name, opts.axiom_role, ax.to_tptp());
    }

    // Conjecture.
    if let Some(c) = problem.conjecture_ref() {
        let _ = writeln!(
            out,
            "{}({}, conjecture, {}).",
            kw, opts.conjecture_name, c.to_tptp(),
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vampire_prover::ir::{
        Formula as IrF, Function as IrFn, Predicate as IrPd, Problem as IrProblem,
        Sort as IrSort, Term as IrT,
    };

    #[test]
    fn empty_problem_produces_empty_output() {
        let problem = IrProblem::new();
        let s = assemble_tptp(&problem, &[], &AssemblyOpts::default());
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

        let tptp = assemble_tptp(&pb, &[42, 7], &AssemblyOpts::default());
        assert!(tptp.contains("fof(kb_42, axiom, P(a))."), "{}", tptp);
        assert!(tptp.contains("fof(kb_7, axiom, P(b))."),  "{}", tptp);
    }

    #[test]
    fn missing_sid_falls_back_to_anon() {
        let p = IrPd::new("P", 0);
        let mut pb = IrProblem::new();
        pb.with_axiom(IrF::atom(p, vec![]));

        let tptp = assemble_tptp(&pb, &[], &AssemblyOpts::default());
        assert!(tptp.contains("fof(kb_anon_0, axiom, P)."), "{}", tptp);
    }

    #[test]
    fn custom_role_and_conjecture_name() {
        let p = IrPd::new("P", 0);
        let mut pb = IrProblem::new();
        pb.with_axiom(IrF::atom(p.clone(), vec![]));
        pb.conjecture(IrF::atom(p, vec![]));

        let opts = AssemblyOpts {
            axiom_role:      "hypothesis",
            conjecture_name: "query_0",
            ..AssemblyOpts::default()
        };
        let tptp = assemble_tptp(&pb, &[1], &opts);
        assert!(tptp.contains("fof(kb_1, hypothesis, P)."),    "{}", tptp);
        assert!(tptp.contains("fof(query_0, conjecture, P)."), "{}", tptp);
    }

    #[test]
    fn tff_mode_emits_type_declarations_first() {
        let person = IrSort::new("person");
        let alice  = IrFn::typed("alice",  &[], person.clone());
        let mortal = IrPd::typed("mortal", &[person.clone()]);

        let mut pb = IrProblem::new_tff();
        pb.declare_sort(person);
        pb.declare_function(alice.clone());
        pb.declare_predicate(mortal.clone());
        pb.with_axiom(IrF::atom(mortal, vec![IrT::apply(alice, vec![])]));

        let tptp = assemble_tptp(&pb, &[3], &AssemblyOpts::default());
        // Type decls come before the axiom.
        let person_pos = tptp.find("person_type").unwrap();
        let axiom_pos  = tptp.find("kb_3").unwrap();
        assert!(person_pos < axiom_pos, "type decl must precede axiom; got:\n{}", tptp);
        assert!(tptp.contains("tff(kb_3, axiom, mortal(alice))."), "{}", tptp);
    }
}
