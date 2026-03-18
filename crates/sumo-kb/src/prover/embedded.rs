// crates/sumo-kb/src/prover/embedded.rs
//
// EmbeddedProverRunner — converts KifStore sentences directly to the
// vampire-prover programmatic API, bypassing TPTP string generation.
//
// Gated: requires both `ask` (via parent module) and `integrated-prover`.

#[cfg(all(feature = "integrated-prover", target_arch = "wasm32"))]
compile_error!(
    "The 'integrated-prover' feature is not supported on wasm32 targets. \
     The Vampire C++ library requires threads, fork(), and platform syscalls."
);

use std::collections::{HashMap, HashSet};

use regex::Regex;
use vampire_prover::{Formula, Function, Predicate, Problem, Proof as VampireProof,
                     ProofRes, ProofRule as VampireProofRule, Term, Options};

use crate::kif_store::KifStore;
use super::{Binding, ProverMode, ProverOpts, ProverResult, ProverStatus};
use crate::parse::kif::OpKind;
use crate::types::{Element, Literal, SentenceId};

// Symbol name helpers (mirror TPTP conventions)

const S: &str = "s__";
const M: &str = "__m";

/// Encode a SUMO symbol name as a constant term (s__Name).
fn sym_const(name: &str) -> Term {
    let clean = name.replace('.', "_").replace('-', "_");
    Function::constant(&format!("{}{}", S, clean))
}

/// Encode a SUMO symbol name as a function symbol (s__Name / arity N).
fn sym_func(name: &str, arity: u32) -> Function {
    let clean = name.replace('.', "_").replace('-', "_");
    Function::new(&format!("{}{}", S, clean), arity)
}

/// Encode a SUMO symbol name as a mention constant (s__Name__m).
fn sym_mention(name: &str) -> Term {
    let clean = name.replace('.', "_").replace('-', "_");
    Function::constant(&format!("{}{}{}", S, clean, M))
}

// Variable collection

fn collect_free_vars(sid: SentenceId, store: &KifStore, out: &mut HashSet<u64>, bound: &mut HashSet<u64>) {
    let sent_idx = store.sent_idx(sid);
    // If the current sentence is an existential operation, the variables are bound,
    // so add to a list of bound variables.
    if matches!(&store.sentences[sent_idx].elements.first(),
                Some(Element::Op(OpKind::ForAll)) | Some(Element::Op(OpKind::Exists))) {
        // The first sentence contains all the variables which are bound
        if let Some(Element::Sub(sub_sid)) = &store.sentences[sent_idx].elements.iter().next() {
            for bound_var in &store.sentences[store.sent_idx(*sub_sid)].elements {
                match bound_var {
                    Element::Variable { id, .. } => { bound.insert(*id); },
                    _ => {}
                }
            }
        };
    }
    // For each individual element in the sentence
    for elem in &store.sentences[store.sent_idx(sid)].elements {
        match elem {
            // if its a variable, collect it
            Element::Variable { id, .. } if !bound.contains(id) => { out.insert(*id); }
            // If its a sub-sentence, recurse into it
            Element::Sub(sub) => collect_free_vars(*sub, store, out, bound),
            _ => {}
        }
    }
}

/// Collect all variables that are explicitly bound by forall/exists in the tree.
fn collect_bound_vars(sid: SentenceId, store: &KifStore, out: &mut HashSet<String>) {
    let sentence = &store.sentences[store.sent_idx(sid)];
    if let Some(op) = sentence.op() {
        if matches!(op, OpKind::ForAll | OpKind::Exists) {
            // elements[1] is the variable list sub-sentence
            if let Some(Element::Sub(var_list_sid)) = sentence.elements.get(1) {
                let var_list = &store.sentences[store.sent_idx(*var_list_sid)];
                for e in &var_list.elements {
                    if let Element::Variable { name, .. } = e {
                        out.insert(name.clone());
                    }
                }
            }
        }
    }
    for elem in &sentence.elements {
        if let Element::Sub(sub_sid) = elem {
            collect_bound_vars(*sub_sid, store, out);
        }
    }
}

// ── Converter ─────────────────────────────────────────────────────────────────

/// Converts KifStore sentences to vampire-prover `Formula` values.
///
/// The mapping mirrors the TPTP encoding:
/// - Predicates become `s__holds(s__pred__m, arg1, ..., argN)` (N+1 arity)
/// - Functions/terms become `s__pred(arg1, ..., argN)` (N arity)
/// - Variables become `Term::new_var(i)` with stable indices
/// - `(equal A B)` becomes `Term::eq(Term)`
/// - Logical operators map to native vampire-prover combinators
pub(crate) struct Converter<'a> {
    store: &'a KifStore,
    /// Variable name → stable u32 index (shared across all formulas in a proof).
    vars: &'a HashMap<String, u32>,
}

impl<'a> Converter<'a> {
    pub(crate) fn new(store: &'a KifStore, vars: &'a HashMap<String, u32>) -> Self {
        Self { store, vars }
    }

    fn var_term(&self, name: &str) -> Term {
        let idx = self.vars.get(name)
            .copied()
            .unwrap_or_else(|| {
                log::warn!(target: "sumo_kb::embedded_prover",
                    "unknown variable '{}' — defaulting to index 0", name);
                0
            });
        Term::new_var(idx)
    }

    // ── Formula builders ──────────────────────────────────────────────────────

    /// Convert a SentenceId to a formula.  `query=true` flips free-variable
    /// wrapping to existential (for conjecture sentences).
    pub(crate) fn sid_to_formula(&mut self, sid: SentenceId) -> Option<Formula> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        if sentence.is_operator() {
            return self.operator_sid_to_formula(sid);
        }
        self.predicate_sid_to_formula(sid)
    }

    fn predicate_sid_to_formula(&mut self, sid: SentenceId) -> Option<Formula> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let n_args = sentence.elements.len().saturating_sub(1);

        match sentence.elements.first()? {
            Element::Symbol(head_id) => {
                let head_name = self.store.sym_name(*head_id).to_owned();
                let mention = sym_mention(&head_name);
                let mut args: Vec<Term> = vec![mention];
                for elem in &sentence.elements[1..].to_vec() {
                    args.push(self.element_to_term(elem)?);
                }
                let pred = Predicate::new("s__holds", (n_args + 1) as u32);
                Some(pred.with(args.as_slice()))
            }
            Element::Variable { name, .. } => {
                // Higher-order: variable in head position
                let name = name.clone();
                let var_t = self.var_term(&name);
                let mut args: Vec<Term> = vec![var_t];
                for elem in &sentence.elements[1..].to_vec() {
                    args.push(self.element_to_term(elem)?);
                }
                let pred = Predicate::new("s__holds_app", (n_args + 1) as u32);
                Some(pred.with(args.as_slice()))
            }
            _ => None,
        }
    }

    fn operator_sid_to_formula(&mut self, sid: SentenceId) -> Option<Formula> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let op = sentence.op()?.clone();
        let args: Vec<Element> = sentence.elements[1..].to_vec();

        match op {
            OpKind::And => {
                let formulas: Vec<Formula> = args.iter()
                    .filter_map(|e| self.element_to_formula(e))
                    .collect();
                match formulas.len() {
                    0 => None,
                    1 => Some(formulas.into_iter().next().unwrap()),
                    _ => Some(Formula::new_and(&formulas)),
                }
            }
            OpKind::Or => {
                let formulas: Vec<Formula> = args.iter()
                    .filter_map(|e| self.element_to_formula(e))
                    .collect();
                match formulas.len() {
                    0 => None,
                    1 => Some(formulas.into_iter().next().unwrap()),
                    _ => Some(Formula::new_or(&formulas)),
                }
            }
            OpKind::Not => {
                let inner = self.element_to_formula(args.first()?)?;
                Some(Formula::new_not(inner))
            }
            OpKind::Implies => {
                let a = self.element_to_formula(args.get(0)?)?;
                let b = self.element_to_formula(args.get(1)?)?;
                Some(a >> b)
            }
            OpKind::Iff => {
                let a = self.element_to_formula(args.get(0)?)?;
                let b = self.element_to_formula(args.get(1)?)?;
                Some(a.iff(b))
            }
            OpKind::Equal => {
                let a = self.element_to_term(args.get(0)?)?;
                let b = self.element_to_term(args.get(1)?)?;
                Some(a.eq(b))
            }
            OpKind::ForAll => {
                let var_names = self.extract_quantifier_var_names(args.get(0)?);
                let body = self.element_to_formula(args.get(1)?)?;
                let mut formula = body;
                for name in var_names.iter().rev() {
                    if let Some(&idx) = self.vars.get(name) {
                        formula = Formula::new_forall(idx, formula);
                    }
                }
                Some(formula)
            }
            OpKind::Exists => {
                let var_names = self.extract_quantifier_var_names(args.get(0)?);
                let body = self.element_to_formula(args.get(1)?)?;
                let mut formula = body;
                for name in var_names.iter().rev() {
                    if let Some(&idx) = self.vars.get(name) {
                        formula = Formula::new_exists(idx, formula);
                    }
                }
                Some(formula)
            }
        }
    }

    fn extract_quantifier_var_names(&self, elem: &Element) -> Vec<String> {
        match elem {
            Element::Sub(var_list_sid) => {
                self.store.sentences[self.store.sent_idx(*var_list_sid)]
                    .elements
                    .iter()
                    .filter_map(|e| {
                        if let Element::Variable { name, .. } = e {
                            Some(name.clone())
                        } else {
                            None
                        }
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn element_to_formula(&mut self, elem: &Element) -> Option<Formula> {
        match elem {
            Element::Sub(sid) => self.sid_to_formula(*sid),
            Element::Symbol(id) => {
                // Bare symbol as formula: s__holds(s__sym__m)
                let name = self.store.sym_name(*id).to_owned();
                let mention = sym_mention(&name);
                Some(Predicate::new("s__holds", 1).with(mention))
            }
            Element::Variable { name, .. } => {
                // Bare variable as formula: s__holds(var)
                let name = name.clone();
                let var_t = self.var_term(&name);
                Some(Predicate::new("s__holds", 1).with(var_t))
            }
            _ => None,
        }
    }

    // ── Term builders ─────────────────────────────────────────────────────────

    fn element_to_term(&mut self, elem: &Element) -> Option<Term> {
        match elem {
            Element::Symbol(id) => {
                let name = self.store.sym_name(*id).to_owned();
                Some(sym_const(&name))
            }
            Element::Variable { name, .. } => {
                let name = name.clone();
                Some(self.var_term(&name))
            }
            Element::Literal(lit) => Some(literal_to_term(lit)),
            Element::Sub(sid) => self.sid_to_term(*sid),
            Element::Op(op) => Some(sym_const(op.name())),
        }
    }

    fn sid_to_term(&mut self, sid: SentenceId) -> Option<Term> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let n_args = sentence.elements.len().saturating_sub(1);

        if sentence.is_operator() {
            // Operator in term position: encode as a skolem-style function
            let op = sentence.op()?.clone();
            let func = Function::new(&format!("{}{}__op", S, op.name()), n_args as u32);
            let args: Vec<Term> = sentence.elements[1..].iter()
                .filter_map(|e| self.element_to_term(e))
                .collect();
            if args.len() == n_args {
                return Some(func.with(args.as_slice()));
            }
            return None;
        }

        match sentence.elements.first()? {
            Element::Symbol(head_id) => {
                let name = self.store.sym_name(*head_id).to_owned();
                let func = sym_func(&name, n_args as u32);
                let args: Vec<Term> = sentence.elements[1..].iter()
                    .filter_map(|e| self.element_to_term(e))
                    .collect();
                if args.len() == n_args {
                    Some(func.with(args.as_slice()))
                } else {
                    None
                }
            }
            Element::Variable { name, .. } => {
                // Variable application in term position — return just the variable term
                let name = name.clone();
                Some(self.var_term(&name))
            }
            _ => None,
        }
    }
}

fn literal_to_term(lit: &Literal) -> Term {
    match lit {
        Literal::Str(s) => {
            let inner = &s[1..s.len() - 1];
            let safe: String = inner
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .take(48)
                .collect();
            Function::constant(&format!("str__{}", safe))
        }
        Literal::Number(n) => {
            let safe = n.replace('.', "_").replace('-', "neg_");
            Function::constant(&format!("n__{}", safe))
        }
    }
}

// ── Per-formula variable index allocation ─────────────────────────────────────

/// Allocate variable indices for a single sentence and return the mapping.
/// All variables across all sentences in a proof share a global offset,
/// so callers must pass `base` = the next free index.
/// Returns (mapping, new_base).
fn alloc_vars(
    sid: SentenceId,
    store: &KifStore,
    base: u32,
) -> (HashMap<String, u32>, u32) {
    let mut free_vars: HashSet<u64> = HashSet::new();
    let mut bound_vars: HashSet<u64> = HashSet::new();
    collect_free_vars(sid, store, &mut free_vars, &mut bound_vars);
    let mut next = base;
    let vars: HashMap<String, u32> = free_vars
        .into_iter()
        .map(|id| {
            let name = store.sym_name(id);
            let idx = next;
            next += 1;
            (name, idx)
        })
        .collect();
    (vars, next)
}

/// Wrap a formula with top-level quantifiers for free (unbound) variables.
fn wrap_free_vars(
    formula: Formula,
    all_vars: &HashMap<String, u32>,
    bound_vars: &HashSet<String>,
    query: bool,
) -> Formula {
    let mut free: Vec<u32> = all_vars
        .iter()
        .filter(|(name, _)| !bound_vars.contains(*name))
        .map(|(_, &idx)| idx)
        .collect();
    free.sort_unstable(); // deterministic

    let mut result = formula;
    for idx in free.into_iter().rev() {
        result = if query {
            Formula::new_exists(idx, result)
        } else {
            Formula::new_forall(idx, result)
        };
    }
    result
}

// ── QueryVarMap ───────────────────────────────────────────────────────────────

/// Records the variable indices used for the conjecture's free variables,
/// enabling binding extraction from the returned proof.
struct QueryVarMap {
    /// Variable index → KIF variable name (e.g. 0 → "?X", 1 → "?Y").
    idx_to_kif: HashMap<u32, String>,
    /// Free variable indices from the conjecture in sorted order.
    /// These are the ones we need bindings for.
    free_var_indices: Vec<u32>,
}

impl QueryVarMap {
    /// Returns the Vampire formula variable names for the free variables
    /// (e.g. indices [0, 1] → ["X0", "X1"]).
    fn var_names(&self) -> Vec<String> {
        self.free_var_indices.iter().map(|&i| format!("X{}", i)).collect()
    }

    fn kif_name(&self, vampire_var: &str) -> String {
        let idx: u32 = vampire_var.trim_start_matches('X').parse().unwrap_or(0);
        self.idx_to_kif.get(&idx).cloned().unwrap_or_else(|| vampire_var.to_string())
    }
}

/// Convert one root conjecture sentence to a formula AND return the variable map.
fn build_conjecture_formula(
    store: &KifStore,
    sid: SentenceId,
) -> Option<(Formula, QueryVarMap)> {
    let (vars, _) = alloc_vars(sid, store, 0);
    let mut bound = HashSet::new();
    collect_bound_vars(sid, store, &mut bound);

    let mut free_var_indices: Vec<u32> = vars.iter()
        .filter(|(name, _)| !bound.contains(*name))
        .map(|(_, &idx)| idx)
        .collect();
    free_var_indices.sort_unstable();

    let idx_to_kif: HashMap<u32, String> = vars.iter()
        .map(|(name, &idx)| (idx, name.clone()))
        .collect();

    let mut conv = Converter::new(store, &vars);
    let formula = conv.sid_to_formula(sid)?;
    let wrapped = wrap_free_vars(formula, &vars, &bound, true);

    Some((wrapped, QueryVarMap { idx_to_kif, free_var_indices }))
}

// ── EmbeddedProverRunner ──────────────────────────────────────────────────────

/// Converts KB sentences directly to vampire-prover formulas and runs Vampire
/// in-process.  No TPTP string round-trip.
pub struct EmbeddedProverRunner;

impl EmbeddedProverRunner {
    /// Prove `query_sids` (conjectures) against `axiom_sids` + `assertion_sids`.
    pub(crate) fn run(
        &self,
        store: &KifStore,
        axiom_sids: &[SentenceId],
        assertion_sids: &[SentenceId],
        query_sids: &[SentenceId],
        opts: &ProverOpts,
    ) -> ProverResult {
        // Create a new Vampire prover
        let mut problem = {
            let mut vp_opts = Options::new();
            if let ProverMode::Prove = opts.mode {
                if opts.timeout_secs > 0 {
                    vp_opts.timeout(std::time::Duration::from_secs(opts.timeout_secs as u64));
                }
            }
            Problem::new(vp_opts)
        };

        let mut skipped = 0usize;

        // Add axioms
        for &sid in axiom_sids {
            if let Some(f) = convert_sid_top(store, sid, false) {
                problem.with_axiom(f);
            } else {
                skipped += 1;
            }
        }

        // Add session assertions as hypotheses (treated as axioms here since
        // vampire-prover does not distinguish hypothesis from axiom in the API)
        for &sid in assertion_sids {
            if let Some(f) = convert_sid_top(store, sid, false) {
                problem.with_axiom(f);
            } else {
                skipped += 1;
            }
        }

        if skipped > 0 {
            log::debug!(target: "sumo_kb::embedded_prover",
                "skipped {} sentences during conversion", skipped);
        }

        // Add the conjecture, retaining the variable map for binding extraction
        let mut query_var_map: Option<QueryVarMap> = None;
        for &sid in query_sids {
            if let Some((f, qvm)) = build_conjecture_formula(store, sid) {
                problem.conjecture(f);
                query_var_map = Some(qvm);
                break; // vampire-prover supports only one conjecture
            }
        }

        let (res, proof_opt) = problem.solve_and_prove();

        log::debug!(target: "sumo_kb::embedded_prover",
            "embedded prover result: {:?}", res);

        let status = map_proof_res(&res, &opts.mode);

        let bindings = if matches!(status, ProverStatus::Proved) {
            match (proof_opt, query_var_map) {
                (Some(proof), Some(qvm)) => {
                    log::debug!(target: "sumo_kb::embedded_prover",
                        "proof has {} steps; extracting bindings", proof.steps().len());
                    extract_bindings_from_proof(&proof, &qvm)
                }
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };

        ProverResult {
            status,
            raw_output: format!("{:?}", res),
            bindings,
            proof_kif: Vec::new(),
        }
    }
}

/// Convert one root sentence to a top-level formula (with free-var wrapping).
/// Used for axioms (query = false).
fn convert_sid_top(store: &KifStore, sid: SentenceId, query: bool) -> Option<Formula> {
    let (vars, _) = alloc_vars(sid, store, 0);
    let mut bound = HashSet::new();
    collect_bound_vars(sid, store, &mut bound);

    let mut conv = Converter::new(store, &vars);
    let formula = conv.sid_to_formula(sid)?;
    Some(wrap_free_vars(formula, &vars, &bound, query))
}

fn map_proof_res(res: &ProofRes, mode: &ProverMode) -> ProverStatus {
    match mode {
        ProverMode::Prove => match res {
            ProofRes::Proved       => ProverStatus::Proved,
            ProofRes::Unprovable   => ProverStatus::Disproved,
            ProofRes::Unknown(_)   => ProverStatus::Unknown,
        },
        ProverMode::CheckConsistency => match res {
            ProofRes::Proved       => ProverStatus::Inconsistent,
            ProofRes::Unprovable   => ProverStatus::Consistent,
            ProofRes::Unknown(_)   => ProverStatus::Unknown,
        },
    }
}

// ── Binding extraction ────────────────────────────────────────────────────────

/// Strip `s__` prefix and `__m` suffix from a Vampire term name.
fn unmangle_sumo(term: &str) -> String {
    let mut clean = term.to_string();
    if clean.starts_with("s__") { clean = clean[3..].to_string(); }
    if clean.ends_with("__m")   { clean = clean[..clean.len() - 3].to_string(); }
    clean
}

/// Try to unify a negative literal in `variadic` (containing X\d+ vars) with
/// a positive literal in `resolvent` (fully ground).  Returns a substitution
/// map `{ "X0" → "s__Foo", … }` on success.
fn unify_negative_with_positive(
    variadic: &str,
    resolvent: &str,
) -> Option<HashMap<String, String>> {
    let neg_lit_re = Regex::new(r"~s__holds\(([^()]+)\)").unwrap();
    let pos_lit_re = Regex::new(r"(?:^|[^~])s__holds\(([^()]+)\)").unwrap();
    let var_re     = Regex::new(r"^X\d+$").unwrap();

    let res_cap  = pos_lit_re.captures(resolvent)?;
    let res_args: Vec<&str> = res_cap[1].split(',').map(str::trim).collect();

    for cap in neg_lit_re.captures_iter(variadic) {
        let var_args: Vec<&str> = cap[1].split(',').map(str::trim).collect();
        if var_args.len() != res_args.len() { continue; }
        if var_args[0] != res_args[0] { continue; } // predicate head must match

        let mut sub = HashMap::new();
        let mut consistent = true;
        for (va, ra) in var_args.iter().zip(res_args.iter()).skip(1) {
            if var_re.is_match(va) {
                sub.insert(va.to_string(), ra.to_string());
            } else if va != ra {
                consistent = false;
                break;
            }
        }
        if consistent && !sub.is_empty() { return Some(sub); }
    }
    None
}

/// Extract variable bindings from a Vampire proof using the same two strategies
/// as the TPTP subprocess prover:
///
/// - **Strategy 2** (resolution unification): find a clause derived by
///   resolving the negated-conjecture clause (which has free variables) against
///   a ground axiom clause, then read off the substitution.
/// - **Strategy 3** (descendant heuristic): scan descendants of the negated-
///   conjecture for ground constants that appear after a variable disappears.
fn extract_bindings_from_proof(proof: &VampireProof, qvm: &QueryVarMap) -> Vec<Binding> {
    let vars = qvm.var_names(); // ["X0", "X1", …]
    if vars.is_empty() { return Vec::new(); }

    let var_re = Regex::new(r"\bX\d+\b").unwrap();

    // Flatten proof into (formula_string, rule, premises) for easy indexing.
    let steps: Vec<(String, VampireProofRule, Vec<usize>)> = proof.steps()
        .iter()
        .map(|s| (s.conclusion().to_string(), s.rule(), s.premises().to_vec()))
        .collect();

    // Find the NegatedConjecture input step.
    let neg_conj_idx = match steps.iter().position(|(_, rule, _)| {
        *rule == VampireProofRule::NegatedConjecture
    }) {
        Some(i) => i,
        None => {
            log::debug!(target: "sumo_kb::embedded_prover",
                "no NegatedConjecture step found in proof");
            return Vec::new();
        }
    };

    log::debug!(target: "sumo_kb::embedded_prover",
        "NegatedConjecture step {}: {}", neg_conj_idx, steps[neg_conj_idx].0);

    // ── Build variadic set ────────────────────────────────────────────────────
    // All steps derived (transitively) from the negated conjecture that still
    // contain free variables.  These are the steps that need to be resolved
    // against ground axioms to produce bindings.
    let mut variadic_set: HashSet<usize> = HashSet::new();
    variadic_set.insert(neg_conj_idx);
    let mut changed = true;
    while changed {
        changed = false;
        for (i, (formula, _, premises)) in steps.iter().enumerate() {
            if variadic_set.contains(&i) { continue; }
            if premises.iter().any(|&p| variadic_set.contains(&p))
                && var_re.is_match(formula)
            {
                variadic_set.insert(i);
                changed = true;
            }
        }
    }

    // ── Strategy 2: resolution unification ───────────────────────────────────
    for (_formula, _rule, premises) in &steps {
        // Look for a step with NO variables (fully ground) that was derived
        // from at least one variadic parent and at least one ground resolvent.
        let variadic_parent_idx = match premises.iter().find(|&&p| variadic_set.contains(&p)) {
            Some(&p) => p,
            None => continue,
        };
        let variadic_formula = &steps[variadic_parent_idx].0;
        if !var_re.is_match(variadic_formula) { continue; }

        for &resolvent_idx in premises.iter() {
            if variadic_set.contains(&resolvent_idx) { continue; }
            let resolvent_formula = &steps[resolvent_idx].0;
            if var_re.is_match(resolvent_formula) { continue; } // resolvent must be ground

            if let Some(sub) = unify_negative_with_positive(variadic_formula, resolvent_formula) {
                let bindings: Vec<Binding> = vars.iter()
                    .filter_map(|var_name| {
                        sub.get(var_name).map(|val| Binding {
                            variable: qvm.kif_name(var_name),
                            value:    unmangle_sumo(val),
                        })
                    })
                    .collect();
                if bindings.len() == vars.len() {
                    log::debug!(target: "sumo_kb::embedded_prover",
                        "strategy 2 extracted {} bindings", bindings.len());
                    return bindings;
                }
            }
        }
    }

    // ── Strategy 3: descendant heuristic ─────────────────────────────────────
    // Collect all descendants of the variadic set.
    let descendants: Vec<usize> = {
        let mut result  = Vec::new();
        let mut frontier: Vec<usize> = variadic_set.iter().cloned().collect();
        let mut visited  = variadic_set.clone();
        while let Some(idx) = frontier.pop() {
            for (i, (_, _, premises)) in steps.iter().enumerate() {
                if !visited.contains(&i) && premises.contains(&idx) {
                    result.push(i);
                    visited.insert(i);
                    frontier.push(i);
                }
            }
        }
        result
    };

    let const_re = Regex::new(r"\b(s__[A-Za-z0-9_]+)\b").unwrap();
    let bindings: Vec<Binding> = vars.iter()
        .filter_map(|var_name| {
            let value = descendants.iter().find_map(|&i| {
                let formula = &steps[i].0;
                if !formula.contains(var_name.as_str()) {
                    const_re.captures_iter(formula).find_map(|cap| {
                        let candidate = cap[1].to_string();
                        if !candidate.ends_with("__m") { Some(candidate) } else { None }
                    })
                } else {
                    None
                }
            })?;
            Some(Binding {
                variable: qvm.kif_name(var_name),
                value:    unmangle_sumo(&value),
            })
        })
        .collect();

    if !bindings.is_empty() {
        log::debug!(target: "sumo_kb::embedded_prover",
            "strategy 3 extracted {} bindings", bindings.len());
    }
    bindings
}
