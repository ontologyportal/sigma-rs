//! E023 quantifier-vacuous: a variable bound by a quantifier but never used in
//! its body.

use std::collections::HashSet;

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::OperatorValidator;
use crate::{Element, OpKind, SentenceId};

use super::common::walk_vars;

#[derive(Debug, Clone, Error)]
#[error("variable '{var}' is bound by a quantifier but never used in the body")]
pub struct QuantifierVacuous {
    pub sid: SentenceId,
    pub var: String,
}
semantic_error!(
    QuantifierVacuous,
    "E023",
    "quantifier-vacuous",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], -1)
    },
    fn highlight_var(&self) -> Option<String> {
        Some(self.var.clone())
    },
);

pub(crate) struct QuantifierVacuousCheck;

impl OperatorValidator for QuantifierVacuousCheck {
    type Error = QuantifierVacuous;
    const OPS: &'static [OpKind] = &[OpKind::ForAll, OpKind::Exists];

    fn check(&self, cx: &Cx<'_>, sid: SentenceId, _op: &OpKind) -> Vec<QuantifierVacuous> {
        let Some(s) = cx.sentence(sid) else {
            return Vec::new();
        };
        // elements: [Op{ForAll|Exists}, Sub{varlist}, Sub{body}, ...]
        let Some(Element::Sub(varlist_sid)) = s.elements.get(1) else {
            return Vec::new();
        };
        let varlist = quantifier_varlist(cx, *varlist_sid);

        // KIF allows several body forms after the var list; accept the general
        // shape by collecting variables from every body sub.
        let mut body_vars: HashSet<String> = HashSet::new();
        for el in s.elements[2..].iter() {
            if let Element::Sub(body_sid) = el {
                walk_vars(cx, *body_sid, &mut |n: &str| {
                    body_vars.insert(n.to_string());
                });
            }
        }

        let mut vacuous: Vec<String> = varlist.difference(&body_vars).cloned().collect();
        vacuous.sort_unstable();
        vacuous
            .into_iter()
            .map(|var| QuantifierVacuous { sid, var })
            .collect()
    }
}

/// The non-row variables named in a quantifier's var-list.
fn quantifier_varlist(cx: &Cx<'_>, varlist_sid: SentenceId) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Some(vl) = cx.sentence(varlist_sid) {
        for el in &vl.elements {
            if let Element::Variable {
                name,
                is_row: false,
                ..
            } = el
            {
                out.insert(name.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, sub_by_op};

    #[test]
    fn e023_quantifier_vacuous() {
        // `?Y` is in the forall var-list but never used in the body. A
        // top-level `(forall ...)` is stripped at ingest, so nest it under a
        // connective to survive as its own sub-sentence.
        let layer = kif_layer(
            r#"
            (instance Animal Class)
            (=> (instance Animal Class) (forall (?X ?Y) (instance ?X Animal)))
        "#,
        );
        let sid = sub_by_op(&layer, crate::OpKind::ForAll);
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"E023"),
            "expected E023 quantifier-vacuous; got {codes:?}"
        );
    }

    #[test]
    fn e023_not_flagged_when_every_bound_var_is_used() {
        let layer = kif_layer(
            r#"
            (instance Animal Class)
            (=> (instance Animal Class) (forall (?X) (instance ?X Animal)))
        "#,
        );
        let sid = sub_by_op(&layer, crate::OpKind::ForAll);
        assert!(!codes_in(&layer, sid).contains(&"E023"));
    }
}
