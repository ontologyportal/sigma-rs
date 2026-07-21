//! Paragraph-style proof rendering: a discourse layer over the
//! step-wise transcript.
//!
//! The surface text comes from the same `format`/`termFormat` machinery
//! as the step view (kb/natural_lang.rs); this module adds document
//! planning (rhetorical grouping), microplanning (premise
//! elision/anchors, connective rotation, variable cleanup), and English
//! glue templates.
//!
//! Pipeline:
//!   1. Document plan — partition steps by rhetorical role: goal
//!      restatement, givens (axioms, with file:line citations),
//!      hypotheses, the assume-for-contradiction move, the derivation
//!      chain, closing.
//!   2. Microplanning — premise references are elided when the premise
//!      is a given or the immediately preceding derived sentence, and
//!      cited by inline anchor "(n)" otherwise; connectives rotate by
//!      position; the negated conjecture's free variable renders as a
//!      negative quantifier ("nothing is a parent of Bill"); remaining
//!      variable tokens lose their `?` sigil; skolem witnesses get a
//!      "(call it skN)" introduction on first occurrence.
//!   3. Surface — a small English glue table. Formula rendering follows
//!      the requested `language`; the glue is English-only.

use std::collections::{BTreeSet, HashMap};

use crate::parse::fingerprint::canonical_sentence_fingerprint;
use crate::{AxiomSource, AxiomSourceIndex, SentenceId};
use crate::parse::ast::AstNode;
use crate::parse::OpKind;
use crate::prover::proof::KifProofStep;
use crate::layer::{TopLayer, Layer};

use super::KnowledgeBase;
use super::natural_lang::RenderReport;

/// Deterministic connective rotation for the derivation chain.
const CONNECTIVES: [&str; 4] = ["Then", "Hence", "Therefore", "So"];

/// Derivation sentences per paragraph before a break is inserted.
const SENTENCES_PER_PARA: usize = 5;

impl<L: TopLayer + Layer> KnowledgeBase<L> {
    /// Render a refutation transcript as connected prose.
    ///
    /// `conjecture`, when available, opens with a goal restatement;
    /// `steps` is the proof transcript. Formula text follows `language`;
    /// discourse glue is English. `RenderReport.missing` aggregates
    /// vocabulary gaps.
    pub fn render_proof_prose(
        &self,
        conjecture: Option<&AstNode>,
        steps:      &[KifProofStep],
        language:   &str,
    ) -> RenderReport {
        let src_idx = self.build_axiom_source_index();
        self.render_proof_prose_with(conjecture, steps, language, &src_idx)
    }

    /// [`render_proof_prose`](Self::render_proof_prose) against an index the
    /// caller already has.
    ///
    /// Building the index walks every root sentence and fingerprints its whole
    /// AST, so it is the most expensive pass either prose call makes. A caller
    /// narrating several transcripts from one KB — an audit renders one per
    /// contradiction — should build it once and reuse it here.
    pub fn render_proof_prose_with(
        &self,
        conjecture: Option<&AstNode>,
        steps:      &[KifProofStep],
        language:   &str,
        src_idx:    &AxiomSourceIndex,
    ) -> RenderReport {
        let mut missing: BTreeSet<String> = BTreeSet::new();
        let mut render = |f: &AstNode| -> String {
            let r = self.render_formula(f, language);
            missing.extend(r.missing);
            r.rendered
        };
        let cite = |step: &KifProofStep| -> String {
            step.source_sid
                .and_then(|sid| src_idx.lookup_by_sid(sid))
                .map(|a| {
                    let file = a.file.rsplit('/').next().unwrap_or(&a.file);
                    format!(" ({}:{})", file, a.line)
                })
                .unwrap_or_default()
        };

        // ---- 1. document plan: partition by rhetorical role ----------
        let mut axioms:  Vec<usize> = Vec::new();
        let mut hyps:    Vec<usize> = Vec::new();
        let mut negconj: Vec<usize> = Vec::new();
        let mut derived: Vec<usize> = Vec::new();
        for (i, s) in steps.iter().enumerate() {
            match s.rule.as_str() {
                "axiom" => axioms.push(i),
                "hypothesis" => hyps.push(i),
                "negated_conjecture" => negconj.push(i),
                _ => derived.push(i),
            }
        }

        let mut paras: Vec<String> = Vec::new();
        // Anchor numbering is unified across every emitted statement so
        // inference sentences can reference any earlier statement by "(n)".
        let mut next_anchor = 0usize;
        let mut anchor_of: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();

        if let Some(c) = conjecture {
            paras.push(format!("We want to show that {}.", render(c)));
        }

        for &i in &axioms {
            next_anchor += 1;
            let n = next_anchor;
            anchor_of.insert(i, n);
            paras.push(format!(
                "It is given that {}{}. ({n})",
                render(&steps[i].formula), cite(&steps[i])));
        }
        for &i in &hyps {
            next_anchor += 1;
            let n = next_anchor;
            anchor_of.insert(i, n);
            paras.push(format!("Suppose that {}. ({n})", render(&steps[i].formula)));
        }

        // A negated conjecture that is already the empty clause means
        // the goal was discharged on contact with the givens; say so
        // instead of assuming an empty formula.
        let mut immediate = false;
        for &i in &negconj {
            if is_false(&steps[i].formula) {
                immediate = true;
                continue;
            }
            next_anchor += 1;
            let n = next_anchor;
            anchor_of.insert(i, n);
            paras.push(format!(
                "Assume, for the sake of contradiction, that {}. ({n})",
                render_assumption(&steps[i].formula, &mut render)));
        }

        // ---- 2. derivation chain ------------------------------------
        let mut chain: Vec<String> = Vec::new();
        let mut closing: Option<String> = None;
        let mut prev_emitted: Option<usize> = None;

        for &i in &derived {
            let s = &steps[i];
            if is_false(&s.formula) {
                let assumption_ref = s
                    .premises
                    .iter()
                    .find(|p| negconj.contains(p))
                    .and_then(|p| anchor_of.get(p));
                closing = Some(match assumption_ref {
                    Some(a) => format!(
                        "But this contradicts our assumption ({a}); hence the conjecture holds."),
                    None if s.premises.iter().any(|p| negconj.contains(p)) =>
                        "But this contradicts the assumption; hence the conjecture holds.".to_string(),
                    None =>
                        "This is a contradiction; hence the conjecture holds."
                            .to_string(),
                });
                continue;
            }

            // Order premises by role: suppositions/assumptions first,
            // earlier inferences next, applied rules (axioms) last.
            let role_rank = |p: &usize| -> u8 {
                if hyps.contains(p) || negconj.contains(p) { 0 }
                else if axioms.contains(p) { 2 }
                else { 1 }
            };
            let mut prems: Vec<usize> = s.premises.clone();
            prems.sort_by_key(|p| (role_rank(p), *p));

            let mut since: Vec<String> = Vec::new();
            for p in &prems {
                let a = anchor_of.get(p).copied();
                let aref = a.map(|a| format!("({a})")).unwrap_or_default();
                if hyps.contains(p) {
                    since.push(format!("we supposed {aref}"));
                } else if negconj.contains(p) {
                    since.push(format!("we assumed {aref}"));
                } else if axioms.contains(p) {
                    since.push(format!(
                        "we know {aref} that {}", render(&steps[*p].formula)));
                } else if Some(*p) == prev_emitted {
                    since.push("we just inferred this".to_string());
                } else if a.is_some() {
                    since.push(format!("we inferred {aref}"));
                }
            }

            next_anchor += 1;
            let n = next_anchor;
            anchor_of.insert(i, n);
            let body = render(&s.formula);
            let sentence = if since.is_empty() {
                format!("{} {}. ({n})", CONNECTIVES[(n - 1) % CONNECTIVES.len()], body)
            } else {
                format!(
                    "Since {}, we can infer that {}. ({n})",
                    join_with_and(&since), body)
            };
            chain.push(sentence);
            prev_emitted = Some(i);
        }

        if !chain.is_empty() {
            for para in chain.chunks(SENTENCES_PER_PARA) {
                paras.push(para.join(" "));
            }
        }

        match closing {
            Some(c) => paras.push(c),
            None if immediate => paras.push(
                "Assuming the contrary already contradicts the givens; hence the \
                 conjecture holds."
                    .to_string()),
            None => {}
        }

        // ---- 3. token cleanup over the whole text --------------------
        let text = cleanup_tokens(&paras.join("\n\n"));
        RenderReport {
            rendered: text,
            missing: missing.into_iter().collect(),
        }
    }

    /// Build a fresh [`AxiomSourceIndex`] by canonically hashing every
    /// root sentence in the KB.
    ///
    /// The index is a snapshot, not a live view; re-run it if the KB
    /// mutates. Includes sentences from every loaded file, including
    /// ephemeral ones like `__query__` / `__sine_query__`. Callers that
    /// want only "real" source files typically filter by
    /// [`AxiomSource::file`] starting with `/` or by excluding the
    /// `__` prefix.
    pub fn build_axiom_source_index(&self) -> AxiomSourceIndex {
        let store = self.syntactic();
        let mut by_hash: HashMap<u64, Vec<AxiomSource>> = HashMap::new();
        let mut by_sid:  HashMap<SentenceId, AxiomSource> = HashMap::new();
        // Resolved in one pass over the fingerprint->roots map: per-root
        // `source_node_of` lookups scan that same map and would make this
        // builder quadratic in KB size.
        for (sid, node) in store.root_source_nodes() {
            let h    = canonical_sentence_fingerprint(&node);
            let span = node.span();
            let entry = AxiomSource {
                sid,
                file: span.file.clone(),
                line: span.line,
            };
            by_hash.entry(h).or_default().push(entry.clone());
            // A sid is unique in the KB, so a blind `insert` is correct.
            by_sid.insert(sid, entry);
        }
        AxiomSourceIndex { by_hash, by_sid }
    }
}

/// `true` for the native prover's empty-clause marker.
fn is_false(f: &AstNode) -> bool {
    matches!(f, AstNode::Symbol { name, .. } if name == "FALSE")
}

/// Render the assume-for-contradiction formula.
///
/// The clausal negation of an existential goal is `(not (p … ?V …))`
/// with a free variable; renders the positive literal and substitutes
/// the variable token with "nothing" to yield a negative quantifier.
/// Everything else renders as-is.
fn render_assumption(f: &AstNode, render: &mut impl FnMut(&AstNode) -> String) -> String {
    if let AstNode::List { elements, .. } = f {
        if elements.len() == 2 {
            if matches!(&elements[0], AstNode::Operator { op: OpKind::Not, .. }) {
                if let AstNode::List { elements: inner, .. } = &elements[1] {
                    let vars: Vec<&str> = inner[1..]
                        .iter()
                        .filter_map(|a| match a {
                            AstNode::Variable { name, .. } => Some(name.as_str()),
                            _ => None,
                        })
                        .collect();
                    if vars.len() == 1 {
                        let positive = render(&elements[1]);
                        return positive.replace(&format!("?{}", vars[0]), "nothing");
                    }
                }
            }
        }
    }
    render(f)
}

/// Join "since" clauses: "a", "a and b", "a, and b, and c".
fn join_with_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a}, and since {b}"),
        many => {
            let mut out = many[0].clone();
            for p in &many[1..] {
                out.push_str(", and since ");
                out.push_str(p);
            }
            out
        }
    }
}

/// Whole-text token pass: strip the `?` sigil from variable tokens
/// ("?B is a mother of ?A" → "B is a mother of A") and introduce each
/// skolem witness on first occurrence ("sk0" → "something (call it
/// sk0)").
fn cleanup_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut seen_skolems: BTreeSet<String> = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        // `?Var` → `Var`.
        if c == '?'
            && i + 1 < bytes.len()
            && (bytes[i + 1] as char).is_ascii_alphanumeric()
        {
            i += 1; // drop the sigil, keep the name
            continue;
        }
        // Skolem token boundary: `sk<digits>` not preceded by an
        // identifier character.
        if c == 's'
            && (i == 0 || !(bytes[i - 1] as char).is_ascii_alphanumeric())
            && bytes.get(i + 1) == Some(&b'k')
        {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j] as char).is_ascii_digit() {
                j += 1;
            }
            let is_token_end =
                j > i + 2 && bytes.get(j).is_none_or(|b| !(*b as char).is_ascii_alphanumeric());
            if is_token_end {
                let tok = &text[i..j];
                if seen_skolems.insert(tok.to_string()) {
                    out.push_str("something (call it ");
                    out.push_str(tok);
                    out.push(')');
                } else {
                    out.push_str(tok);
                }
                i = j;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover::proof::KifProofStep;
    use crate::{KnowledgeBase, Parser};

    fn kb() -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let kif = r#"
            (format EnglishLanguage mother "%1 is %n a mother of %2")
            (format EnglishLanguage parent "%1 is %n a parent of %2")
            (termFormat EnglishLanguage mother "mother")
            (termFormat EnglishLanguage parent "parent")
            (termFormat EnglishLanguage Jane "Jane")
            (termFormat EnglishLanguage Bill "Bill")
        "#;
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("tests.kif"), "tests.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("tests.kif").expect("promote");
        kb
    }

    fn formula(kif: &str) -> AstNode {
        crate::parse_document("t", kif, Parser::Kif).ast.into_iter().next().expect("ast").as_stmt().cloned().expect("doc stmt")
    }

    fn step(index: usize, rule: &str, kif: &str, premises: Vec<usize>) -> KifProofStep {
        KifProofStep {
            index,
            rule: rule.to_string(),
            premises,
            formula: formula(kif),
            source_sid: None,
        }
    }

    #[test]
    fn prose_renders_roles_and_quantifier() {
        let kb = kb();
        let steps = vec![
            step(0, "negated_conjecture", "(not (parent Bill ?V0))", vec![]),
            step(1, "axiom", "(=> (mother ?B ?A) (parent ?B ?A))", vec![]),
            step(2, "hypothesis", "(mother Bill Jane)", vec![]),
            step(3, "hyper", "(parent Bill Jane)", vec![1, 2]),
            step(4, "resolve", "FALSE", vec![0, 3]),
        ];
        let goal = formula("(exists (?P) (parent Bill ?P))");
        let report = kb.render_proof_prose(Some(&goal), &steps, "EnglishLanguage");
        let text = &report.rendered;

        assert!(text.starts_with("We want to show that"), "{text}");
        assert!(text.contains("It is given that if B is a mother of A"), "{text}");
        assert!(text.contains("Suppose that Bill is a mother of Jane. (2)"), "{text}");
        assert!(
            text.contains("Assume, for the sake of contradiction, that Bill is a parent of nothing. (3)"),
            "{text}");
        assert!(
            text.contains(
                "Since we supposed (2), and since we know (1) that if B is a mother \
                 of A then B is a parent of A, we can infer that Bill is a parent \
                 of Jane. (4)"),
            "{text}");
        assert!(
            text.contains("But this contradicts our assumption (3); hence the conjecture holds."),
            "{text}");
        // No raw sigils survive.
        assert!(!text.contains('?'), "{text}");
    }

    #[test]
    fn skolems_get_witness_introductions() {
        let kb = kb();
        let steps = vec![
            step(0, "hypothesis", "(parent Bill sk0)", vec![]),
            step(1, "hyper", "(mother Bill sk0)", vec![0]),
        ];
        let report = kb.render_proof_prose(None, &steps, "EnglishLanguage");
        let text = &report.rendered;
        let intro = "something (call it sk0)";
        let first = text.find(intro).expect("introduction");
        // Exactly one introduction; later mentions are bare.
        assert!(text[first + intro.len()..].find("call it sk0").is_none(), "{text}");
        assert!(text.matches("sk0").count() >= 2, "{text}");
    }
}
