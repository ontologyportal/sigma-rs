//! Dynamic relations inside the given-clause loop.
//!
//! A literal of a registered dynamic relation whose firing condition holds
//! (Partial: some argument ground; Moded: every declared input ground) is a
//! *demand*.  `make` queues demands as clauses are built ([`note_demands`]),
//! and the loop-top drain ([`drain_dynamic`]) calls the relation's function
//! and adds each solution as a ground positive unit clause, rule `dynamic`,
//! citing the relation's `(instance R DynamicRelation)` declaration.
//!
//! Enabled only after the background snapshot is frozen (snapshots are
//! cached across asks), so [`enable_dynamic`] first scans every clause
//! already loaded.  Clauses the `dynamic` rule itself produced never raise
//! demands.
//!
//! [`note_demands`]: NativeProver::note_demands
//! [`drain_dynamic`]: NativeProver::drain_dynamic
//! [`enable_dynamic`]: NativeProver::enable_dynamic

use std::collections::HashSet;
use std::sync::Arc;

use crate::layer::TopLayer;
use crate::prover::dynamic::{DynOutcome, DynValue, DynamicOracle};
use crate::semantics::caches::dynamic_relations::{DynamicRegistry, Trigger};
use crate::types::{Literal, Symbol};

use super::super::clause::Term;
use super::{NativeProver, SUPPORT};

/// The rule label of clauses the dynamic oracle supplies.
pub(crate) const DYNAMIC_RULE: &str = "dynamic";

/// One queued call: the relation symbol and its argument pattern.
struct Demand {
    rel: Symbol,
    args: Vec<Option<DynValue>>,
}

/// Per-run dynamic-relation state.
pub(crate) struct DynamicState {
    oracle: Arc<dyn DynamicOracle>,
    registry: Arc<DynamicRegistry>,
    /// Demand keys already queued this run.
    seen: HashSet<String>,
    pending: Vec<Demand>,
}

impl std::fmt::Debug for DynamicState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicState")
            .field("seen", &self.seen.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

/// A ground leaf as a dynamic value; `None` for anything else.
fn leaf_value(t: &Term) -> Option<DynValue> {
    match t {
        Term::Sym(s) => Some(DynValue::Sym(s.name().to_string())),
        Term::Lit(Literal::Str(s)) => Some(DynValue::Str(
            s.strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(s)
                .to_string(),
        )),
        Term::Lit(Literal::Number(n)) => crate::numeric::parse_num(n).map(DynValue::Num),
        _ => None,
    }
}

fn value_term(v: &DynValue) -> Term {
    match v {
        DynValue::Sym(s) => Term::Sym(Symbol::from(s.as_str())),
        // KIF strings have no escape for `"`.
        DynValue::Str(s) => Term::Lit(Literal::Str(format!("\"{}\"", s.replace('"', "'")))),
        DynValue::Num(n) => Term::Lit(Literal::Number(crate::numeric::format_num(*n))),
    }
}

impl<S: TopLayer + 'static> NativeProver<'_, S> {
    /// Turn on dynamic relations for this run and queue the demands of
    /// every clause already loaded.
    pub(crate) fn enable_dynamic(
        &mut self,
        oracle: Arc<dyn DynamicOracle>,
        registry: Arc<DynamicRegistry>,
    ) {
        self.dynamic = Some(DynamicState {
            oracle,
            registry,
            seen: HashSet::new(),
            pending: Vec::new(),
        });
        let terms: Vec<Term> = self
            .clauses
            .iter()
            .filter(|c| c.rule != DYNAMIC_RULE)
            .flat_map(|c| c.terms.iter().map(|(_, t)| t.clone()))
            .collect();
        self.note_demands(terms.iter(), "");
    }

    /// Queue the demands raised by the literals `terms` of a clause built by
    /// `rule`.
    pub(crate) fn note_demands<'t>(&mut self, terms: impl Iterator<Item = &'t Term>, rule: &str) {
        let Some(state) = self.dynamic.as_mut() else {
            return;
        };
        if rule == DYNAMIC_RULE {
            return;
        }
        for t in terms {
            let Term::App(elems) = t else { continue };
            let Some(Term::Sym(rel)) = elems.first() else {
                continue;
            };
            let Some(entry) = state.registry.relations.get(&rel.id()) else {
                continue;
            };
            let mut args = Vec::with_capacity(elems.len() - 1);
            let mut supported = true;
            for a in &elems[1..] {
                match a {
                    Term::Var(_) => args.push(None),
                    _ => match leaf_value(a) {
                        Some(v) => args.push(Some(v)),
                        None => {
                            supported = false;
                            break;
                        }
                    },
                }
            }
            let fires = supported
                && match &entry.trigger {
                    Trigger::Partial => args.iter().any(Option::is_some),
                    Trigger::Moded(inputs) => inputs
                        .iter()
                        .all(|&i| args.get(i).is_some_and(Option::is_some)),
                };
            if !fires {
                continue;
            }
            let key = std::iter::once(entry.name.clone())
                .chain(
                    args.iter()
                        .map(|a| a.as_ref().map_or_else(|| "_".to_string(), DynValue::key)),
                )
                .collect::<Vec<_>>()
                .join("\u{1f}");
            if state.seen.insert(key) {
                state.pending.push(Demand {
                    rel: rel.clone(),
                    args,
                });
            }
        }
    }

    /// Resolve queued demands into activated ground unit clauses.  Stops
    /// early when the run is out of time; whatever is left marks the run
    /// incomplete.
    pub(crate) fn drain_dynamic(&mut self) {
        let Some(state) = self.dynamic.as_mut() else {
            return;
        };
        let demands = std::mem::take(&mut state.pending);
        let oracle = Arc::clone(&state.oracle);
        let registry = Arc::clone(&state.registry);
        let mut rest = demands.into_iter();
        for demand in rest.by_ref() {
            if self.out_of_time() {
                break;
            }
            let Some(entry) = registry.relations.get(&demand.rel.id()) else {
                continue;
            };
            let DynOutcome::Answers(solutions) = oracle.call(&entry.name, &demand.args) else {
                continue;
            };
            for solution in solutions {
                let Some(values) = solution
                    .iter()
                    .zip(&demand.args)
                    .map(|(out, input)| out.as_ref().or(input.as_ref()).map(value_term))
                    .collect::<Option<Vec<Term>>>()
                else {
                    continue;
                };
                let call = format!(
                    "-- {}({})",
                    entry.name,
                    demand
                        .args
                        .iter()
                        .map(|a| a.as_ref().map_or("_".to_string(), DynValue::key))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let atom = Term::App(
                    std::iter::once(Term::Sym(demand.rel.clone()))
                        .chain(values)
                        .collect(),
                );
                let Some(id) = self.make(
                    vec![(true, atom)],
                    Vec::new(),
                    DYNAMIC_RULE,
                    SUPPORT,
                    None,
                    true,
                ) else {
                    continue;
                };
                let rec = &mut self.clauses[id as usize];
                rec.fact_parents.extend(entry.decl);
                if self.want_notes() {
                    self.clauses[id as usize].notes.push(call);
                }
                let key = self.clauses[id as usize].key;
                if self.seen_insert(key, id) {
                    self.activate(id);
                    self.push(Some(id));
                }
            }
        }
        let left: Vec<Demand> = rest.collect();
        if let Some(state) = self.dynamic.as_mut() {
            state.pending.extend(left);
        }
    }

    /// Whether demands are queued but not yet resolved.
    pub(crate) fn has_pending_demands(&self) -> bool {
        self.dynamic.as_ref().is_some_and(|s| !s.pending.is_empty())
    }

    /// Whether the dynamic facts this run saw may be incomplete: a call
    /// failed, timed out, or hit the budget, or demands were left
    /// unresolved.
    pub(crate) fn dynamic_incomplete(&self) -> bool {
        self.dynamic
            .as_ref()
            .is_some_and(|s| !s.pending.is_empty() || s.oracle.incomplete())
    }
}

#[cfg(test)]
mod tests {
    use crate::prover::{ProverLayer, ProverStatus};
    use crate::{KnowledgeBase, NativeOpts, Parser};

    const KB: &str = "
        (subclass DynamicRelation Relation)
        (subclass PartialDynamicRelation DynamicRelation)
        (subclass ModedDynamicRelation DynamicRelation)
        (instance weather ModedDynamicRelation)
        (dynamicInputs weather 2 3)
        (=> (weather Rain ?place ?day) (wet ?place ?day))
    ";

    fn kb(script: &str) -> KnowledgeBase<ProverLayer> {
        let mut kb = KnowledgeBase::new_native();
        let text = format!("{KB}\n#|@js\n{script}\n|#\n");
        let r = kb.reload_kif(&text, &std::path::PathBuf::from("dyn.kif"), "dyn.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("dyn.kif").expect("promote");
        kb
    }

    fn ask(
        kb: &KnowledgeBase<ProverLayer>,
        goal: &str,
        opts: &NativeOpts,
    ) -> crate::prover::ProverResult {
        kb.ask_query_dialect(goal, opts, Parser::Kif { options: None })
    }

    fn opts() -> NativeOpts {
        NativeOpts {
            time_limit_secs: 10,
            want_proof: true,
            ..Default::default()
        }
    }

    const RAINY_BOSTON: &str = r#"
        export function weather(_c, place, day) {
          return place.__kif_symbol === "Boston" ? [[kif.Rain, null, null]] : [[kif.Sun, null, null]];
        }"#;

    #[test]
    fn a_dynamic_fact_proves_a_ground_goal() {
        let kb = kb(RAINY_BOSTON);
        let r = ask(&kb, "(weather Rain Boston Today)", &opts());
        assert_eq!(r.status, ProverStatus::Proved, "{}", r.raw_output);
    }

    #[test]
    fn a_demand_raised_mid_search_is_resolved() {
        let kb = kb(RAINY_BOSTON);
        let r = ask(&kb, "(wet Boston Today)", &opts());
        assert_eq!(r.status, ProverStatus::Proved, "{}", r.raw_output);
        let rules: Vec<&str> = r.proof_kif.iter().map(|s| s.rule.as_str()).collect();
        assert!(rules.contains(&"dynamic"), "{rules:?}");
    }

    #[test]
    fn a_contrary_answer_does_not_prove_the_goal() {
        let kb = kb(RAINY_BOSTON);
        let r = ask(&kb, "(wet Paris Today)", &opts());
        assert_ne!(r.status, ProverStatus::Proved, "{}", r.raw_output);
    }

    #[test]
    fn a_failing_function_makes_saturation_unknown() {
        let kb = kb("export function weather(a, b, c) { throw new Error('sensor offline'); }");
        let r = ask(&kb, "(wet Paris Today)", &opts());
        assert_eq!(r.status, ProverStatus::Unknown, "{}", r.raw_output);
    }

    #[test]
    fn unmet_inputs_never_call() {
        // Only position 2 is ground in `(weather ?c Boston ?d)`; the Moded
        // relation needs 2 and 3, so no call is made and the throw is never
        // reached.
        let kb = kb("export function weather(a, b, c) { throw new Error('called'); }");
        let r = ask(&kb, "(exists (?c ?d) (weather ?c Boston ?d))", &opts());
        assert_ne!(r.status, ProverStatus::Unknown, "{}", r.raw_output);
    }

    #[test]
    fn disabled_dynamic_opts_skip_the_oracle() {
        let kb = kb(RAINY_BOSTON);
        let off = NativeOpts {
            dynamic: None,
            ..opts()
        };
        let r = ask(&kb, "(weather Rain Boston Today)", &off);
        assert_ne!(r.status, ProverStatus::Proved, "{}", r.raw_output);
    }
}
