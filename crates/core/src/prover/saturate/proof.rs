// crates/core/src/saturate/proof.rs
//
// Proof extraction: the derivation DAG rooted at the empty clause →
// ordered [`KifProofStep`]s (the same backend-agnostic shape the TPTP
// pipeline emits, so the CLI's proof printing works unchanged).
//
// Three step sources:
//   * input clauses (axiom / hypothesis / negated_conjecture) — cite
//     their stored root via `source_sid`, so `AxiomSourceIndex` /
//     direct span lookup can print file:line;
//   * derived clauses (resolve / factor / para / hyper / oracle) —
//     premises are their parent steps;
//   * witness facts (oracle discharges) — stored facts surfaced as
//     extra "axiom" premise steps, each citing its own sid.

use std::collections::HashMap;

use crate::parse::ast::AstNode;
use crate::parse::{OpKind, Span};
use crate::prover::proof::KifProofStep;
use crate::types::{Element, Literal, SentenceId};

use super::ProverLayer;
use super::prover::NativeProver;

/// Per-proof skolem display renamer.  The clausifier names skolems
/// `sk_<root-hash>_<n>` (deterministic, so re-clausification is
/// byte-identical) — unreadable in a proof.  This maps each distinct
/// skolem symbol to a clean first-appearance label (`sk0`, `sk1`, …),
/// shared across every step so the same skolem keeps the same name.
#[derive(Default)]
struct SkolemRenamer {
    map: HashMap<u64, String>,
}

impl SkolemRenamer {
    /// Clean label for a skolem symbol, or `None` for ordinary symbols.
    fn label(&mut self, name: &str, id: u64) -> Option<String> {
        if !name.starts_with("sk_") {
            return None;
        }
        let next = self.map.len();
        Some(self.map.entry(id).or_insert_with(|| format!("sk{next}")).clone())
    }
}

/// SIGMA_STATS instrumentation only (Part 1, proof-DAG reach): walk the
/// refutation DAG rooted at `empty_id` (same parent-DFS shape as
/// [`extract_proof`], run separately so this stays a read-only probe with no
/// effect on proof rendering) and count how many clauses in it carry one of
/// the model/oracle discharge rule tags — i.e. whether the *found* proof
/// actually leaned on the model-discharge / rule-join / event-calculus /
/// oracle mechanisms, as opposed to them merely being enabled.  Zero
/// behavior change: called only to fill `ProverStats` counters.
pub(crate) fn count_proof_tags(prover: &NativeProver<'_>, empty_id: u32) -> ProofTagCounts {
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut counts = ProofTagCounts::default();
    fn visit(
        prover: &NativeProver<'_>,
        id: u32,
        seen: &mut std::collections::HashSet<u32>,
        counts: &mut ProofTagCounts,
    ) {
        if !seen.insert(id) { return; }
        let c = &prover.clauses[id as usize];
        for p in &c.parents {
            visit(prover, *p, seen, counts);
        }
        // "rule_join" is the Horn rule-join oracle's tag (SIGMA_RULE_JOIN) —
        // the mechanism the task/field name `proof_tag_join` refers to; there
        // is no bare `"join"` tag in the codebase.
        match c.rule {
            "model" => counts.model += 1,
            "model_join" => counts.model_join += 1,
            "rule_join" => counts.join += 1,
            "event_calculus" => counts.event_calculus += 1,
            "oracle" => counts.oracle += 1,
            _ => {}
        }
    }
    visit(prover, empty_id, &mut seen, &mut counts);
    counts
}

/// Per-rule-tag clause counts over one proof DAG — see [`count_proof_tags`].
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ProofTagCounts {
    pub(crate) model: u64,
    pub(crate) model_join: u64,
    pub(crate) join: u64,
    pub(crate) event_calculus: u64,
    pub(crate) oracle: u64,
}

/// Convert the refutation DAG ending at `empty_id` into proof steps.
pub(crate) fn extract_proof(prover: &NativeProver<'_>, empty_id: u32) -> Vec<KifProofStep> {
    // Topological order via DFS over clause parents; witness facts are
    // emitted (once) before the step that uses them.
    let mut order: Vec<u32> = Vec::new();
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    fn visit(
        prover: &NativeProver<'_>,
        id: u32,
        seen: &mut std::collections::HashSet<u32>,
        order: &mut Vec<u32>,
    ) {
        if !seen.insert(id) { return; }
        for p in &prover.clauses[id as usize].parents {
            visit(prover, *p, seen, order);
        }
        order.push(id);
    }
    visit(prover, empty_id, &mut seen, &mut order);

    if std::env::var("SIGMA_PROOF_TRACE").is_ok() {
        for cid in &order {
            let c = &prover.clauses[*cid as usize];
            eprintln!("PROOF-CLAUSE id={} rule={} tier={} parents={:?} fact_parents={:?} notes={:?}",
                c.id, c.rule, c.tier, c.parents, c.fact_parents, c.notes);
        }
    }

    let layer = prover.layer();
    let mut steps: Vec<KifProofStep> = Vec::new();
    let mut clause_step: HashMap<u32, usize> = HashMap::new();
    let mut root_steps: HashMap<SentenceId, usize> = HashMap::new();
    // Separate from `root_steps`: one shared "negated conjecture" step per
    // source sid, rendered as the NEGATION of the original (keyed apart from
    // `root_steps` so a conjecture sid can never collide with an axiom
    // step — see `negated_root_step`).
    let mut neg_conj_steps: HashMap<SentenceId, usize> = HashMap::new();
    // One renamer for the whole proof: skolem labels are stable across steps.
    let mut renamer = SkolemRenamer::default();

    // One displayed step per stored root, rendered from the ORIGINAL
    // source formula (file text shape, original variable names) when
    // provenance exists, falling back to the stored sentence.  Shared
    // between witness-fact premises and input clauses, so an axiom that
    // contributed several clausal fragments (or doubles as a witness)
    // shows once — the transcript cites axioms as written, not the
    // clausified internals.
    fn root_step(
        layer:      &ProverLayer,
        sid:        SentenceId,
        rule:       &str,
        steps:      &mut Vec<KifProofStep>,
        root_steps: &mut HashMap<SentenceId, usize>,
        renamer:    &mut SkolemRenamer,
    ) -> usize {
        if let Some(&i) = root_steps.get(&sid) { return i; }
        let formula = layer.semantic.syntactic.source_node_of(sid)
            .or_else(|| sentence_ast(layer, sid, renamer))
            .unwrap_or_else(|| AstNode::Symbol {
                name: format!("<unresolved {:x}>", sid),
                span: Span::synthetic(),
            });
        steps.push(KifProofStep {
            index: steps.len(),
            rule: rule.to_string(),
            premises: Vec::new(),
            formula,
            source_sid: Some(sid),
        });
        let i = steps.len() - 1;
        root_steps.insert(sid, i);
        i
    }

    // One displayed "negated conjecture" step per source sid, shared by
    // every unit clause the clausifier split it into — mirrors TPTP/Vampire
    // transcripts (one `negated_conjecture` step, with the clausified units
    // citing it as their parent) instead of each unit appearing as its own
    // unrelated, parentless fact.  Renders `(not <original conjecture>)`;
    // when normalization split a conjunctive conjecture into several roots,
    // `sid` is only the first — a known simplification shared with
    // `clausify_negated_conjunction_lossy`, whose own `root` is likewise
    // just the first conjunct's sid.
    //
    // `source_sid` stays `None` on the pushed step: unlike an axiom/hypothesis
    // root, a conjecture sid is a `build_detached` interning (see
    // `intern_conjecture_native`), not a stored KB root with file:line
    // provenance — `AxiomSourceIndex` lookups (and this module's own
    // "cited sid resolves in the store" invariant) assume `source_sid` only
    // ever names a genuine loaded root. `sid` is still used to render the
    // formula (`sentence_ast`/`atom_ast` resolve it fine via the atom table)
    // and to key the dedup map.
    fn negated_root_step(
        layer:          &ProverLayer,
        sid:            SentenceId,
        steps:          &mut Vec<KifProofStep>,
        neg_conj_steps: &mut HashMap<SentenceId, usize>,
        renamer:        &mut SkolemRenamer,
    ) -> usize {
        if let Some(&i) = neg_conj_steps.get(&sid) { return i; }
        let formula = layer.semantic.syntactic.source_node_of(sid)
            .or_else(|| sentence_ast(layer, sid, renamer))
            .unwrap_or_else(|| AstNode::Symbol {
                name: format!("<unresolved {:x}>", sid),
                span: Span::synthetic(),
            });
        let negated = AstNode::List {
            elements: vec![AstNode::Operator { op: OpKind::Not, span: Span::synthetic() }, formula],
            span: Span::synthetic(),
        };
        steps.push(KifProofStep {
            index: steps.len(),
            rule: "negated_conjecture".to_string(),
            premises: Vec::new(),
            formula: negated,
            source_sid: None,
        });
        let i = steps.len() - 1;
        neg_conj_steps.insert(sid, i);
        i
    }

    for cid in order {
        let c = &prover.clauses[cid as usize];

        // Input clauses cite their source root directly: the step IS
        // the axiom/hypothesis as written.  (`subrel_schema` and other
        // synthesized clauses keep their clause form: no file formula
        // matches them.)
        if matches!(c.rule, "axiom" | "hypothesis") {
            if let Some(src) = c.source {
                let idx = root_step(layer, src, c.rule, &mut steps, &mut root_steps, &mut renamer);
                clause_step.insert(cid, idx);
                // Also surface any oracle witnesses attached to this input
                // clause — e.g. the disjointness/partition axioms that
                // refute `(instance Length MeasurementAttribute)`.  Without
                // this a single-axiom (oracle-refuted) contradiction shows
                // only the trigger, not the axioms it conflicts with.
                for w in &c.fact_parents {
                    root_step(layer, *w, "axiom", &mut steps, &mut root_steps, &mut renamer);
                }
                continue;
            }
        }

        // The negated conjecture's clausified unit clauses: cite the ONE
        // shared "negated conjecture" step (the source-formula negation) as
        // their sole parent, instead of appearing as unrelated, parentless
        // facts (`add_conjecture_clauses` populates `c.source` for exactly
        // this).  Falls through to the generic branch below when `source` is
        // unset (defensive — every live call site sets it).
        if c.rule == "negated_conjecture" {
            if let Some(src) = c.source {
                let root_idx = negated_root_step(layer, src, &mut steps, &mut neg_conj_steps, &mut renamer);
                steps.push(KifProofStep {
                    index: steps.len(),
                    rule: "cnf_transformation".to_string(),
                    premises: vec![root_idx],
                    formula: clause_ast(layer, c, &mut renamer),
                    source_sid: None,
                });
                clause_step.insert(cid, steps.len() - 1);
                continue;
            }
        }

        // Witness facts first, deduped across the whole proof.
        let mut fact_premises: Vec<usize> = Vec::new();
        for sid in &c.fact_parents {
            fact_premises.push(root_step(
                layer, *sid, "axiom", &mut steps, &mut root_steps, &mut renamer));
        }

        let mut premises: Vec<usize> = c
            .parents
            .iter()
            .filter_map(|p| clause_step.get(p).copied())
            .collect();
        premises.extend(fact_premises);
        premises.sort_unstable();
        premises.dedup();

        steps.push(KifProofStep {
            index: steps.len(),
            rule: c.rule.to_string(),
            premises,
            formula: clause_ast(layer, c, &mut renamer),
            source_sid: c.source,
        });
        clause_step.insert(cid, steps.len() - 1);
    }
    steps
}

/// A clause as a KIF AST: the empty clause renders as the symbol
/// `FALSE`, a unit as its (possibly negated) atom, a multi-literal
/// clause as `(or …)`.
fn clause_ast(layer: &ProverLayer, c: &super::prover::ClauseRec, renamer: &mut SkolemRenamer) -> AstNode {
    let sp = Span::synthetic;
    let mut lits: Vec<AstNode> = Vec::with_capacity(c.lits.len());
    for l in &c.lits {
        let atom = atom_ast(layer, l.atom, renamer).unwrap_or_else(|| AstNode::Symbol {
            name: format!("<unresolved {:x}>", l.atom),
            span: sp(),
        });
        lits.push(if l.pos {
            atom
        } else {
            AstNode::List {
                elements: vec![
                    AstNode::Operator { op: OpKind::Not, span: sp() },
                    atom,
                ],
                span: sp(),
            }
        });
    }
    match lits.len() {
        0 => AstNode::Symbol { name: "FALSE".to_string(), span: sp() },
        1 => lits.pop().unwrap(),
        _ => {
            let mut elements = vec![AstNode::Operator { op: OpKind::Or, span: sp() }];
            elements.extend(lits);
            AstNode::List { elements, span: sp() }
        }
    }
}

/// A stored root sentence as a KIF AST (witness facts, input sources).
fn sentence_ast(layer: &ProverLayer, sid: SentenceId, renamer: &mut SkolemRenamer) -> Option<AstNode> {
    atom_ast(layer, sid, renamer)
}

/// An atom/sentence (AtomTable or store) as a KIF AST.
fn atom_ast(layer: &ProverLayer, id: SentenceId, renamer: &mut SkolemRenamer) -> Option<AstNode> {
    let syn = &layer.semantic.syntactic;
    let s = layer.atoms.resolve(id, syn)?;
    let sp = Span::synthetic;
    let mut elements: Vec<AstNode> = Vec::with_capacity(s.elements.len());
    for el in s.elements.iter() {
        elements.push(match el {
            Element::Symbol(sym) => {
                // Skolems get a clean per-proof label; other symbols pass through.
                let raw = sym.name();
                let name = renamer.label(&raw, sym.id()).unwrap_or_else(|| raw.to_string());
                AstNode::Symbol { name, span: sp() }
            }
            // Canonical atoms carry the variable's hashed slot id, not a
            // readable name.  Recover the dense slot (V0, V1, …) and hand
            // `flat()`/`Display` a BARE label — they add the leading `?`.
            Element::Variable { id, .. } => {
                let label = super::canon::canonical_slot(*id)
                    .map(|k| format!("V{k}"))
                    .unwrap_or_else(|| format!("V{id:x}"));
                AstNode::Variable { name: label, span: sp() }
            }
            Element::Literal(Literal::Str(v))    => AstNode::Str { value: v.clone(), span: sp() },
            Element::Literal(Literal::Number(v)) => AstNode::Number { value: v.clone(), span: sp() },
            Element::Op(op) => AstNode::Operator { op: op.clone(), span: sp() },
            Element::Sub(sub) => atom_ast(layer, *sub, renamer)?,
        });
    }
    Some(AstNode::List { elements, span: sp() })
}
