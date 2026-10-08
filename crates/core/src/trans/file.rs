//! crates/core/src/trans/file.rs
//!
//! Trait which abstracts the TPTP problem file format for a given
//! `IrProblem` type

#[cfg(feature = "external-prover")]
use super::ir::{HoProblem, ProblemIr};
use super::ir::{LogicMode, Problem as IrProblem};

/// What the assembler needs from a problem representation: the language
/// keyword, the declaration preamble, and each axiom / the conjecture as
/// formula text.  The first-order [`IrProblem`] and the higher-order
/// [`HoProblem`] both implement it, so `kb_<sid>` naming, line indexing,
/// filtering, and KIF comments are one implementation for every dialect.
pub trait TptpProblemFile {
    /// `fof` / `tff` / `thf`.
    fn keyword(&self) -> &'static str;
    /// Declaration lines emitted ahead of the axioms, in order.
    fn preamble_lines(&self) -> Vec<String>;
    /// Each axiom's formula text, in `sid_map` order.
    fn axiom_texts(&self) -> Box<dyn Iterator<Item = String> + '_>;
    /// The conjecture's formula text, if the problem has one.
    fn conjecture_text(&self) -> Option<String>;
}

impl TptpProblemFile for IrProblem {
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
impl TptpProblemFile for HoProblem {
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

#[cfg(feature = "external-prover")]
impl TptpProblemFile for ProblemIr {
    fn keyword(&self) -> &'static str {
        match self {
            ProblemIr::Fo(p) => p.keyword(),
            ProblemIr::Ho(p) => p.keyword(),
        }
    }

    fn preamble_lines(&self) -> Vec<String> {
        match self {
            ProblemIr::Fo(p) => p.preamble_lines(),
            ProblemIr::Ho(p) => p.preamble_lines(),
        }
    }

    fn axiom_texts(&self) -> Box<dyn Iterator<Item = String> + '_> {
        match self {
            ProblemIr::Fo(p) => p.axiom_texts(),
            ProblemIr::Ho(p) => p.axiom_texts(),
        }
    }

    fn conjecture_text(&self) -> Option<String> {
        match self {
            ProblemIr::Fo(p) => p.conjecture_text(),
            ProblemIr::Ho(p) => p.conjecture_text(),
        }
    }
}
