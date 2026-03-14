/// Human-readable KIF-style display for stored CNF types.
///
/// Unlike the TPTP emitter, these functions produce output that looks like
/// the original KIF — `(pred arg1 arg2)`, `(not ...)`, `(or ...)`, `?X` for
/// variables, and `(sk_N ...)` for Skolem functions.
///
/// All rendering requires a `sym_names: &HashMap<u64, String>` map built from
/// the LMDB symbol table (or from `KifStore::symbol_data` in tests).
///
/// # Example
///
/// ```ignore
/// let names: HashMap<u64, String> = env.all_symbols(&txn)?
///     .into_iter().map(|s| (s.id, s.name)).collect();
///
/// for formula in env.all_formulas(&txn)? {
///     println!("{}", clauses_to_kif(&formula.clauses, &names));
/// }
/// ```

use std::collections::HashMap;
use crate::schema::{Clause, CnfLiteral, CnfTerm};

/// Render a slice of CNF clauses as KIF, one clause per line.
pub fn clauses_to_kif(clauses: &[Clause], sym_names: &HashMap<u64, String>) -> String {
    clauses.iter()
        .map(|c| clause_to_kif(c, sym_names))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a single CNF clause as KIF.
///
/// - Empty clause  → `"false"` (unsatisfiable)
/// - Unit clause   → the single literal (e.g. `"(subclass Human Animal)"`)
/// - Multi-literal → `"(or lit1 lit2 ...)"`
pub fn clause_to_kif(clause: &Clause, sym_names: &HashMap<u64, String>) -> String {
    match clause.literals.len() {
        0 => "false".to_owned(),
        1 => literal_to_kif(&clause.literals[0], sym_names),
        _ => {
            let parts: Vec<String> = clause.literals.iter()
                .map(|l| literal_to_kif(l, sym_names))
                .collect();
            format!("(or {})", parts.join(" "))
        }
    }
}

fn literal_to_kif(lit: &CnfLiteral, sym_names: &HashMap<u64, String>) -> String {
    let atom = atom_to_kif(&lit.pred, &lit.args, sym_names);
    if lit.positive { atom } else { format!("(not {})", atom) }
}

fn atom_to_kif(pred: &CnfTerm, args: &[CnfTerm], sym_names: &HashMap<u64, String>) -> String {
    // Equality is stored as Const(u64::MAX) with two args
    if matches!(pred, CnfTerm::Const(u64::MAX)) && args.len() == 2 {
        return format!(
            "(equal {} {})",
            term_to_kif(&args[0], sym_names),
            term_to_kif(&args[1], sym_names),
        );
    }

    let pred_str = term_to_kif(pred, sym_names);
    if args.is_empty() {
        pred_str
    } else {
        let arg_strs: Vec<String> = args.iter().map(|a| term_to_kif(a, sym_names)).collect();
        format!("({} {})", pred_str, arg_strs.join(" "))
    }
}

fn term_to_kif(term: &CnfTerm, sym_names: &HashMap<u64, String>) -> String {
    match term {
        CnfTerm::Const(id) => {
            sym_names.get(id).cloned().unwrap_or_else(|| format!("sym_{}", id))
        }
        CnfTerm::Var(id) => {
            // Variable names are scope-tagged "X@5" — strip the scope for readability
            let raw = sym_names.get(id).map(|s| s.as_str()).unwrap_or("_");
            let base = raw.split('@').next().unwrap_or(raw);
            format!("?{}", base)
        }
        CnfTerm::SkolemFn { id, args } => {
            let name = sym_names.get(id).cloned().unwrap_or_else(|| format!("sk_{}", id));
            if args.is_empty() {
                name
            } else {
                let arg_strs: Vec<String> = args.iter().map(|a| term_to_kif(a, sym_names)).collect();
                format!("({} {})", name, arg_strs.join(" "))
            }
        }
        CnfTerm::Num(n) => n.clone(),
        CnfTerm::Str(s) => s.clone(),
    }
}
