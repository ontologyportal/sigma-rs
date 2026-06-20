// crates/core/src/converter/operator.rs
// 
// Operator to TPTP implementations

use crate::OpKind;
use super::symbol::S;

impl OpKind {
    /// Alphanumeric TPTP name for a KIF operator, used when the operator
    /// appears in *term position* (reified into `s__<name>_op`).  The KIF
    /// surface names `=>` and `<=>` contain characters that TPTP reserves
    /// for connectives, so we can't reuse `OpKind::name()` here.  All
    /// other names are alphanumeric and passed through unchanged.
    pub(super) fn tptp_safe_name(&self) -> &'static str {
        match self {
            OpKind::And     => "and",
            OpKind::Or      => "or",
            OpKind::Not     => "not",
            OpKind::Implies => "imp",
            OpKind::Iff     => "iff",
            OpKind::Equal   => "equal",
            OpKind::ForAll  => "forall",
            OpKind::Exists  => "exists",
        }
    }

    pub(super) fn tptp_sym_name(&self) -> String {
        format!("{}{}_op", S, self.tptp_safe_name())
    }
}