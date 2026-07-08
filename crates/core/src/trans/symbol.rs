use crate::types::InternedSym;

/// SUMO TPTP prefix. All TPTP symbols generated from a SUMO
/// symbol should be prefixed with this.
pub(super) const S: &str = "s__";
/// SUMO relation symbol suffix. This is used to indicate that a TPTP
/// symbol is, in fact, a SUMO relation turned into a term
pub(super) const M: &str = "__m";

fn tptp_name(name: &str) -> String {
    name.replace('.', "_").replace('-', "_")
}

impl InternedSym {
    /// Convert a symbol name to its TPTP symbol form (`s__Name`).
    pub(super) fn tptp_sym_name(&self) -> String {
        format!("{}{}", S, tptp_name(&self.name()))
    }

    /// Convert a symbol name to its TPTP relation-mention form (`s__Name__m`).
    pub(super) fn tptp_mention_name(&self) -> String {
        format!("{}{}{}", S, tptp_name(&self.name()), M)
    }
}
