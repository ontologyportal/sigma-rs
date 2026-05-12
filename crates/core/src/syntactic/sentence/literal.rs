use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Literal {
    /// String literal -- includes surrounding double-quotes as stored in source.
    Str(String),
    /// Numeric literal (integer or decimal) as a raw string.
    Number(String),
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Str(s)    => write!(f, "{}", s),
            Literal::Number(n) => write!(f, "{}", n),
        }
    }
}