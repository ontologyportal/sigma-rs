use std::collections::HashSet;

/// TPTP language variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TptpLang {
    #[default]
    Fof,
    Tff,
}

impl TptpLang {
    pub fn as_str(self) -> &'static str {
        match self {
            TptpLang::Fof => "fof",
            TptpLang::Tff => "tff",
        }
    }
}

/// Options controlling TPTP output.
#[derive(Debug, Clone)]
pub struct TptpOptions {
    pub lang:         TptpLang,
    /// Wrap free variables in `?` (existential) instead of `!` (universal).
    pub query:        bool,
    /// Replace numeric literals with `n__N` tokens (default false).
    pub hide_numbers: bool,
    /// Head predicates whose sentences are omitted from KB output.
    pub excluded:     HashSet<String>,
}

impl Default for TptpOptions {
    fn default() -> Self {
        let mut excluded = HashSet::new();
        // Exclude the SUMO relations that are more for internal use and will just muck up the
        //  theorem prover
        excluded.insert("documentation".to_string());
        excluded.insert("domain".to_string());
        excluded.insert("format".to_string());
        excluded.insert("termFormat".to_string());
        excluded.insert("externalImage".to_string());
        excluded.insert("relatedExternalConcept".to_string());
        excluded.insert("relatedInternalConcept".to_string());
        excluded.insert("formerName".to_string());
        excluded.insert("abbreviation".to_string());
        excluded.insert("conventionalShortName".to_string());
        excluded.insert("conventionalLongName".to_string());
        TptpOptions { lang: TptpLang::default(), query: false, hide_numbers: false, excluded }
    }
}

impl TptpOptions {
    pub fn default_with_hide_numbers() -> Self {
        Self { hide_numbers: true, ..Self::default() }
    }
}
