use crate::Literal;

impl Literal {
    /// FNV-1a 32-bit hash.  Used to disambiguate string literals whose
    /// non-ASCII characters collapse to `_` during TPTP-identifier
    /// sanitisation — keeps the emitted constants unique per source string
    /// without pulling in a hashing dependency.
    pub(super) fn hash(&self) -> u32 {
        match self {
            Self::Str(s) | Self::Number(s) => {
                let mut h: u32 = 0x811c9dc5;
                for b in s.as_bytes() {
                    h ^= *b as u32;
                    h = h.wrapping_mul(0x01000193);
                }
                h
            },
        }
    }
}