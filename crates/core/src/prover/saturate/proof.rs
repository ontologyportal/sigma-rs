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
            "model" | "model_refute" | "model_complete" => counts.model += 1,
            "model_join" => counts.model_join += 1,
            "rule_join" => counts.join += 1,
            "event_calculus" => counts.event_calculus += 1,
            "oracle" | "exhaustive" | "fd_congruence" => counts.oracle += 1,
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
        // A conjecture sid is a `build_detached` interning (see
        // `intern_conjecture_native`) — no stored file span, so
        // `source_node_of` always misses and this always falls back to
        // `sentence_ast`, which reconstructs bound variables from their
        // internal hashed slot ids (`V<hex>`, unreadable).  Relabel them
        // A, B, C, … in first-appearance order so the negated conjecture
        // reads like the rest of the proof's axiom citations, which get
        // their variable names verbatim from `source_node_of`'s file text
        // and are left untouched here.
        let formula = match layer.semantic.syntactic.source_node_of(sid) {
            Some(f) => f,
            None => {
                let mut f = sentence_ast(layer, sid, renamer).unwrap_or_else(|| AstNode::Symbol {
                    name: format!("<unresolved {:x}>", sid),
                    span: Span::synthetic(),
                });
                rename_vars_pretty(&mut f, &mut HashMap::new());
                f
            }
        };
        // The original conjecture appears as its own leaf step, with the
        // negated form derived FROM it — verifiers (GDV) check the
        // `negate_conjecture` inference against its parent, so a parentless
        // negated conjecture fails structural verification ("'f1' is not a
        // cth of its parents").  This mirrors Vampire transcripts: a
        // `conjecture` input step, then `inference(negate_conjecture,
        // [status(cth)], [<it>])`.
        steps.push(KifProofStep {
            index: steps.len(),
            rule: "conjecture".to_string(),
            premises: Vec::new(),
            formula: formula.clone(),
            source_sid: None,
        });
        let conj_idx = steps.len() - 1;
        let negated = AstNode::List {
            elements: vec![AstNode::Operator { op: OpKind::Not, span: Span::synthetic() }, formula],
            span: Span::synthetic(),
        };
        steps.push(KifProofStep {
            index: steps.len(),
            rule: "negated_conjecture".to_string(),
            premises: vec![conj_idx],
            formula: negated,
            source_sid: None,
        });
        let i = steps.len() - 1;
        neg_conj_steps.insert(sid, i);
        i
    }

    // Expand an oracle-refuted ground binary goal into explicit hop steps.
    //
    // The empty clause's `fact_parents` hold the taxonomy oracle's witness
    // walk for a goal `(rel x Y)`: one start fact `(rel x c0)` plus edge
    // facts `(_ c0 c1) … (_ cn Y)` (edge order is recovered by endpoint
    // matching, so this is insensitive to `fact_parents` ordering).  Emits
    //
    //   cnf_transformation:  (not (rel x Y))         <- negated conjecture
    //   taxonomy:            (rel x c1)              <- start, edge0
    //   taxonomy:            (rel x c2)              <- prev, edge1  …
    //   resolve:             FALSE                   <- unit, last hop
    //
    // Returns `None` (caller falls back to the flat witness citation) when
    // the goal is not a ground binary atom, any witness is not one, or the
    // witnesses do not form exactly one chain from `x` to `Y`.
    fn expand_oracle_refutation(
        layer:      &ProverLayer,
        src:        SentenceId,
        witnesses:  &[SentenceId],
        root_idx:   usize,
        steps:      &mut Vec<KifProofStep>,
        root_steps: &mut HashMap<SentenceId, usize>,
        renamer:    &mut SkolemRenamer,
    ) -> Option<usize> {
        if witnesses.is_empty() {
            return None;
        }
        let goal = sentence_ast(layer, src, renamer)?;
        let (grel, gx, gy) = ground_binary(&goal)?;

        // Split witnesses into chain edges and "license" facts — property
        // declarations about the goal relation itself, e.g. `(instance
        // sister TransitiveRelation)` from a schema dispatch.  Licenses are
        // not part of the walk; they justify it, so they become premises of
        // every hop step.
        let mut facts:    Vec<(SentenceId, String, String, String)> = Vec::new();
        let mut licenses: Vec<SentenceId> = Vec::new();
        for sid in witnesses {
            let f = layer.semantic.syntactic.source_node_of(*sid)
                .or_else(|| sentence_ast(layer, *sid, renamer))?;
            let (r, a, b) = ground_binary(&f)?;
            if r == "instance" && a == grel {
                licenses.push(*sid);
            } else {
                facts.push((*sid, r, a, b));
            }
        }

        // Chain walk: start at the fact `(grel gx _)`, then repeatedly take
        // the unused edge whose left endpoint is the current class.
        let start = facts.iter().position(|(_, r, a, _)| *r == grel && *a == gx)?;
        let mut used = vec![false; facts.len()];
        used[start] = true;
        let mut chain: Vec<usize> = Vec::new();
        let mut current = facts[start].3.clone();
        while current != gy {
            let next = facts.iter().enumerate()
                .position(|(i, (_, _, a, _))| !used[i] && *a == current)?;
            used[next] = true;
            current = facts[next].3.clone();
            chain.push(next);
        }
        if used.iter().any(|u| !u) {
            return None; // leftover witnesses — not a single clean chain
        }

        let sp = Span::synthetic;
        let nc_unit = steps.len();
        steps.push(KifProofStep {
            index: nc_unit,
            rule: "cnf_transformation".to_string(),
            premises: vec![root_idx],
            formula: AstNode::List {
                elements: vec![
                    AstNode::Operator { op: OpKind::Not, span: sp() },
                    mk_ground_binary(&grel, &gx, &gy),
                ],
                span: sp(),
            },
            source_sid: None,
        });
        let mut prev = root_step(layer, facts[start].0, "axiom", steps, root_steps, renamer);
        let license_steps: Vec<usize> = if chain.is_empty() { Vec::new() } else {
            licenses.iter()
                .map(|sid| root_step(layer, *sid, "axiom", steps, root_steps, renamer))
                .collect()
        };
        // Schema-licensed chains are the relation's own property at work
        // (transitivity), not the taxonomy oracle's built-in semantics.
        let hop_rule = if license_steps.is_empty() { "taxonomy" } else { "transitivity" };
        for &ei in &chain {
            let edge = root_step(layer, facts[ei].0, "axiom", steps, root_steps, renamer);
            let idx = steps.len();
            let mut premises = vec![prev, edge];
            premises.extend(&license_steps);
            steps.push(KifProofStep {
                index: idx,
                rule: hop_rule.to_string(),
                premises,
                formula: mk_ground_binary(&grel, &gx, &facts[ei].3),
                source_sid: None,
            });
            prev = idx;
        }
        let f_idx = steps.len();
        steps.push(KifProofStep {
            index: f_idx,
            rule: "resolve".to_string(),
            premises: vec![nc_unit, prev],
            formula: AstNode::Symbol { name: "FALSE".to_string(), span: sp() },
            source_sid: None,
        });
        Some(f_idx)
    }

    // Expand a disjointness-oracle refutation of a ground binary INPUT fact
    // (`sumo audit` contradictions: an `(instance x C)` axiom/hypothesis
    // refuted because two of x's derived classes are declared disjoint).
    //
    // The witness set is one disjointness declaration (`partition` /
    // `disjoint` / `disjointDecomposition`) plus subclass edges forming a
    // DAG from C — possibly BRANCHING toward the two conflicting classes,
    // so this is a worklist walk, not the single chain of
    // [`expand_oracle_refutation`].  Emits one `taxonomy` hop per edge and
    // a final `disjoint` step:
    //
    //   taxonomy:  (instance x c_i)   <- reached-class step, edge
    //   disjoint:  FALSE              <- (instance x D1), (instance x D2), decl
    //
    // Returns `None` (caller falls back to the flat witness citation) when
    // the shape is anything else: no single declaration, non-symbol
    // witnesses, unused edges, or fewer than two reached declared-disjoint
    // classes.
    fn expand_disjoint_refutation(
        layer:      &ProverLayer,
        src:        SentenceId,
        witnesses:  &[SentenceId],
        input_idx:  usize,
        steps:      &mut Vec<KifProofStep>,
        root_steps: &mut HashMap<SentenceId, usize>,
        renamer:    &mut SkolemRenamer,
    ) -> Option<usize> {
        if witnesses.len() < 2 {
            return None;
        }
        let goal = layer.semantic.syntactic.source_node_of(src)
            .or_else(|| sentence_ast(layer, src, renamer))?;
        let (grel, gx, gc0) = ground_binary(&goal)?;

        let mut edges: Vec<(SentenceId, String, String)> = Vec::new();
        let mut decls: Vec<(SentenceId, Vec<String>)> = Vec::new();
        for sid in witnesses {
            let f = layer.semantic.syntactic.source_node_of(*sid)
                .or_else(|| sentence_ast(layer, *sid, renamer))?;
            let syms = ground_symbols(&f)?;
            if DISJOINT_HEADS.contains(&syms[0].as_str()) {
                decls.push((*sid, syms));
            } else if syms.len() == 3 {
                edges.push((*sid, syms[1].clone(), syms[2].clone()));
            } else {
                return None;
            }
        }
        if decls.len() != 1 {
            return None;
        }
        let (decl_sid, decl_syms) = decls.pop().expect("one declaration");

        // Worklist: classes x provably inhabits, each with the step that
        // establishes it.  Seeded by the refuted input fact itself.
        let mut reached: Vec<(String, usize)> = vec![(gc0, input_idx)];
        let mut used = vec![false; edges.len()];
        loop {
            let mut progress = false;
            for (i, (sid, a, b)) in edges.iter().enumerate() {
                if used[i] {
                    continue;
                }
                // A direct `(instance x K)` witness fact seeds K itself.
                if *a == gx {
                    let st = root_step(layer, *sid, "axiom", steps, root_steps, renamer);
                    if !reached.iter().any(|(c, _)| c == b) {
                        reached.push((b.clone(), st));
                    }
                    used[i] = true;
                    progress = true;
                    continue;
                }
                // Both branches of a fork share their prefix chain, and the
                // oracle cites the shared edges once per branch — an edge
                // whose target class is already established needs no second
                // derivation.
                if reached.iter().any(|(c, _)| c == b) {
                    used[i] = true;
                    progress = true;
                    continue;
                }
                let Some(&(_, prev)) = reached.iter().find(|(c, _)| c == a) else {
                    continue;
                };
                let edge_step = root_step(layer, *sid, "axiom", steps, root_steps, renamer);
                let idx = steps.len();
                steps.push(KifProofStep {
                    index: idx,
                    rule: "taxonomy".to_string(),
                    premises: vec![prev, edge_step],
                    formula: mk_ground_binary(&grel, &gx, b),
                    source_sid: None,
                });
                reached.push((b.clone(), idx));
                used[i] = true;
                progress = true;
            }
            if !progress {
                break;
            }
        }
        if used.iter().any(|u| !u) {
            return None; // leftover witnesses — not the shape we understand
        }

        // The declaration's class arguments: `(disjoint C1 C2)` lists them
        // from position 1; `(partition Parent C1 …)` and
        // `(disjointDecomposition Parent C1 …)` from position 2.
        let classes = if decl_syms[0] == "disjoint" { &decl_syms[1..] } else { &decl_syms[2..] };
        let mut conflict: Vec<usize> = classes.iter()
            .filter_map(|c| reached.iter().find(|(rc, _)| rc == c).map(|&(_, i)| i))
            .collect();
        conflict.sort_unstable();
        conflict.dedup();
        if conflict.len() < 2 {
            return None;
        }
        let decl_step = root_step(layer, decl_sid, "axiom", steps, root_steps, renamer);
        let f_idx = steps.len();
        steps.push(KifProofStep {
            index: f_idx,
            rule: "disjoint".to_string(),
            premises: vec![conflict[0], conflict[1], decl_step],
            formula: AstNode::Symbol { name: "FALSE".to_string(), span: Span::synthetic() },
            source_sid: None,
        });
        Some(f_idx)
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
                // An EMPTY input clause is an oracle-refuted fact (an audit
                // contradiction): expand the disjointness witnesses into
                // the derivation — taxonomy hops up both branches, then
                // the disjoint/partition conflict step to FALSE.
                if c.lits.is_empty() {
                    if let Some(f_idx) = expand_disjoint_refutation(
                        layer, src, &c.fact_parents, idx,
                        &mut steps, &mut root_steps, &mut renamer)
                    {
                        clause_step.insert(cid, f_idx);
                        continue;
                    }
                    // Unrecognized refutation shape: still terminate the
                    // transcript.  The witnesses become premises of an
                    // explicit [oracle] FALSE step instead of floating
                    // detached, and the contradiction actually ENDS in a
                    // contradiction.
                    let mut premises = vec![idx];
                    for w in &c.fact_parents {
                        premises.push(root_step(
                            layer, *w, "axiom", &mut steps, &mut root_steps, &mut renamer));
                    }
                    premises.extend(c.parents.iter().filter_map(|p| clause_step.get(p).copied()));
                    premises.sort_unstable();
                    premises.dedup();
                    steps.push(KifProofStep {
                        index: steps.len(),
                        rule: "oracle".to_string(),
                        premises,
                        formula: AstNode::Symbol {
                            name: "FALSE".to_string(),
                            span: Span::synthetic(),
                        },
                        source_sid: None,
                    });
                    clause_step.insert(cid, steps.len() - 1);
                    continue;
                }
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
                // A goal literal struck to the empty clause by the taxonomy
                // oracle: expand the witness chain into explicit hop steps
                // ((instance Rex Mammal), (instance Rex Vertebrate), …) so
                // the proof reads like the axiomatic derivation instead of
                // one opaque jump from edge facts to FALSE.  Falls through
                // to the flat witness citation for any other shape.
                if c.lits.is_empty() {
                    if let Some(idx) = expand_oracle_refutation(
                        layer, src, &c.fact_parents, root_idx,
                        &mut steps, &mut root_steps, &mut renamer)
                    {
                        clause_step.insert(cid, idx);
                        continue;
                    }
                }
                // Oracle witnesses attached during clausification (e.g. a
                // taxonomy oracle striking a goal literal it knows true,
                // possibly all the way to the empty clause) are premises of
                // this step too — dropping them renders a one-step "proof"
                // that never cites the facts that refuted the conjecture.
                // Likewise clause parents: an equality-oracle strike records
                // its justifying equality/fact units there (`eq_clauses`),
                // not in `fact_parents`.
                let mut premises = vec![root_idx];
                for w in &c.fact_parents {
                    premises.push(root_step(
                        layer, *w, "axiom", &mut steps, &mut root_steps, &mut renamer));
                }
                premises.extend(c.parents.iter().filter_map(|p| clause_step.get(p).copied()));
                premises.sort_unstable();
                premises.dedup();
                // A goal that vanished with NO witnesses and NO clause
                // parents was decided by a theory procedure (arithmetic
                // evaluation, `x = x`); say so instead of the misleading
                // bare `cnf_transformation` — that's the only trace the
                // proof gets, since theory truths have no KB premise.
                let rule = if c.lits.is_empty() && premises.len() == 1 {
                    sentence_ast(layer, src, &mut renamer)
                        .as_ref()
                        .and_then(theory_discharge_rule)
                        .unwrap_or("cnf_transformation")
                } else {
                    "cnf_transformation"
                };
                steps.push(KifProofStep {
                    index: steps.len(),
                    rule: rule.to_string(),
                    premises,
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

/// Rule label for a goal a theory decision procedure discharged with no
/// premises at all: a syntactic self-equality is `reflexivity`; a ground
/// comparison (or equality) over two numeric literals is `arithmetic`.
/// `None` when the goal's shape doesn't explain a premise-free discharge.
fn theory_discharge_rule(goal: &AstNode) -> Option<&'static str> {
    match goal {
        AstNode::Annotated { formula, .. } => theory_discharge_rule(formula),
        AstNode::List { elements, .. } => match elements.as_slice() {
            [AstNode::Operator { op: OpKind::Equal, .. }, a, b] => {
                if let (AstNode::Number { value: x, .. }, AstNode::Number { value: y, .. }) = (a, b) {
                    return (x == y).then_some("arithmetic");
                }
                match (a, b) {
                    (AstNode::Symbol { name: x, .. }, AstNode::Symbol { name: y, .. })
                        if x == y => Some("reflexivity"),
                    _ => None,
                }
            }
            [AstNode::Symbol { name, .. }, AstNode::Number { .. }, AstNode::Number { .. }]
                if matches!(name.as_str(),
                    "greaterThan" | "lessThan"
                    | "greaterThanOrEqualTo" | "lessThanOrEqualTo") =>
                Some("arithmetic"),
            _ => None,
        },
        _ => None,
    }
}

/// A flat all-symbol statement `(head s1 s2 …)` → its symbol names
/// (unwrapping any `Annotated` shell).  `None` when any element is not a
/// bare symbol (variables, nested terms) or there are fewer than three.
fn ground_symbols(node: &AstNode) -> Option<Vec<String>> {
    match node {
        AstNode::Annotated { formula, .. } => ground_symbols(formula),
        AstNode::List { elements, .. } => {
            let mut out = Vec::with_capacity(elements.len());
            for e in elements {
                match e {
                    AstNode::Symbol { name, .. } => out.push(name.clone()),
                    _ => return None,
                }
            }
            (out.len() >= 3).then_some(out)
        }
        _ => None,
    }
}

/// Statement heads that declare class disjointness — the possible "conflict
/// declaration" witness of a disjointness-oracle refutation.  (SUMO-standard
/// spellings; the expansion falls back to the flat citation for dialects
/// that name them differently.)
const DISJOINT_HEADS: &[&str] = &["disjoint", "partition", "disjointDecomposition"];

/// `(rel a b)` ground binary atom → its three symbol names (unwrapping any
/// `Annotated` shell).  `None` for anything else — variables, nested terms,
/// other arities.
fn ground_binary(node: &AstNode) -> Option<(String, String, String)> {
    match node {
        AstNode::Annotated { formula, .. } => ground_binary(formula),
        AstNode::List { elements, .. } => match elements.as_slice() {
            [AstNode::Symbol { name: r, .. },
             AstNode::Symbol { name: a, .. },
             AstNode::Symbol { name: b, .. }] => Some((r.clone(), a.clone(), b.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Build a `(rel a b)` ground atom AST.
fn mk_ground_binary(rel: &str, a: &str, b: &str) -> AstNode {
    let sp = Span::synthetic;
    let sym = |name: &str| AstNode::Symbol { name: name.to_string(), span: sp() };
    AstNode::List { elements: vec![sym(rel), sym(a), sym(b)], span: sp() }
}

/// Rewrite every bound variable name in `node`, in first-appearance order,
/// to a short sequential label (`A`, `B`, …, `Z`, `A1`, `B1`, …) — the
/// [`negated_root_step`] fallback for a conjecture's variables, which have
/// no original file spelling to preserve.
fn rename_vars_pretty(node: &mut AstNode, seen: &mut HashMap<String, String>) {
    match node {
        AstNode::Variable { name, .. } | AstNode::RowVariable { name, .. } => {
            let next = seen.len();
            let label = seen.entry(name.clone())
                .or_insert_with(|| sequential_var_label(next));
            *name = label.clone();
        }
        AstNode::List { elements, .. } => {
            for e in elements.iter_mut() { rename_vars_pretty(e, seen); }
        }
        AstNode::Annotated { formula, .. } => rename_vars_pretty(formula, seen),
        _ => {}
    }
}

/// `0` → `A`, `1` → `B`, … `25` → `Z`, `26` → `A1`, `27` → `B1`, …
fn sequential_var_label(n: usize) -> String {
    let letter = (b'A' + (n % 26) as u8) as char;
    let suffix = n / 26;
    if suffix == 0 { letter.to_string() } else { format!("{letter}{suffix}") }
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
