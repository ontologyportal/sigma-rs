use once_cell::sync::Lazy;
use crate::types::Symbol;

// Defines every env-derived symbol.  `static` (not `const`) is required: only
// a `static` can be referenced with the `'static` lifetime the iterable holds,
// and it preserves the once-only `Lazy` init.
macro_rules! define_symbols_from_env {
    ($( $name:ident => $env_var:expr ),+ $(,)?) => {
        $(
            pub(crate) static $name: Lazy<Symbol> =
                Lazy::new(|| Symbol::from(env!($env_var)));
        )+
    };
}

// Build a named subset iterable from symbols already defined above, as
// `(identifier, &symbol)` pairs.
macro_rules! symbol_set {
    ($(#[$meta:meta])* $coll:ident => $( $name:ident ),+ $(,)?) => {
        $(#[$meta])*
        pub(crate) static $coll: &[(&'static str, &'static Lazy<Symbol>)] = &[
            $( (stringify!($name), &$name) ),+
        ];
    };
}

define_symbols_from_env! {
    // --- Relation Classes ---
    RELATION_CLASS        => "SUMO_RELATION_CLASS",
    PREDICATE_CLASS       => "SUMO_PREDICATE_CLASS",
    FUNCTION_CLASS        => "SUMO_FUNCTION_CLASS",

    // --- Taxonomy Relations ---
    SUBCLASS_RELATION     => "SUMO_SUBCLASS_RELATION",
    INSTANCE_RELATION     => "SUMO_INSTANCE_RELATION",
    SUBRELATION_RELATION  => "SUMO_SUBRELATION_RELATION",
    SUBATTRIBUTE_RELATION => "SUMO_SUBATTRIBUTE_RELATION",

    // --- Domain/Range Relations ---
    DOMAIN_SUBCLASS_RELATION => "SUMO_DOMAIN_SUBCLASS_RELATION",
    RANGE_SUB_REL_CLASS      => "SUMO_RANGE_SUBCLASS_RELATION",

    // --- Metadata Relations ---
    DOC_RELATION          => "SUMO_DOC_RELATION",
    TERM_RELATION         => "SUMO_TERM_RELATION",
    FORMAT_RELATION       => "SUMO_FORMAT_RELATION",

    // --- Root Symbol ---
    ROOT_SYMBOL           => "SUMO_ROOT_SYMBOL",
}

symbol_set!(
    /// The documentation/metadata relations (`documentation`, `format`,
    /// `termFormat`) — the subset of the env-derived symbols carrying
    /// human-facing descriptions, for code that processes only those.
    DOCUMENTATION_RELATIONS => DOC_RELATION, FORMAT_RELATION, TERM_RELATION
);

/// Maps SUMO arity-constant symbol names to their integer arity (`-1` = variable).
pub(crate) const ARITY: &[(&'static str, i32)] = &[
    (env!("SUMO_ARITY_TWO"),         2),
    (env!("SUMO_ARITY_THREE"),       3),
    (env!("SUMO_ARITY_FOUR"),        4),
    (env!("SUMO_ARITY_FIVE"),        5),
    (env!("SUMO_ARITY_VAR"),        -1),
];
