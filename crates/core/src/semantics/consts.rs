use once_cell::sync::Lazy;
use crate::types::Symbol;

// Defines every env-derived symbol and collects them into `ALL_SYMBOLS`.
// `static` (not `const`) is required: only a `static` can be referenced with
// the `'static` lifetime the iterable holds, and it preserves the once-only
// `Lazy` init.
macro_rules! define_symbols_from_env {
    ($( $name:ident => $env_var:expr ),+ $(,)?) => {
        $(
            pub(crate) static $name: Lazy<Symbol> =
                Lazy::new(|| Symbol::from(env!($env_var)));
        )+

        /// Every env-derived symbol constant, as `(identifier, &symbol)` pairs.
        /// Iterate to enumerate or force-initialize all well-known SUMO symbols,
        /// e.g. `ALL_SYMBOLS.iter().map(|(_, s)| (***s).clone())`.
        pub(crate) static ALL_SYMBOLS: &[(&'static str, &'static Lazy<Symbol>)] = &[
            $( (stringify!($name), &$name) ),+
        ];
    };
}

// Build a named subset iterable from symbols already defined above. Same
// element type as `ALL_SYMBOLS`; the members stay in `ALL_SYMBOLS` as well.
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

    // --- Arity Constants ---
    ARITY_TWO             => "SUMO_ARITY_TWO",
    ARITY_THREE           => "SUMO_ARITY_THREE",
    ARITY_FOUR            => "SUMO_ARITY_FOUR",
    ARITY_FIVE            => "SUMO_ARITY_FIVE",
    ARITY_VAR             => "SUMO_ARITY_VAR",

    // --- Taxonomy Relations ---
    SUBCLASS_RELATION     => "SUMO_SUBCLASS_RELATION",
    INSTANCE_RELATION     => "SUMO_INSTANCE_RELATION",
    SUBRELATION_RELATION  => "SUMO_SUBRELATION_RELATION",
    SUBATTRIBUTE_RELATION => "SUMO_SUBATTRIBUTE_RELATION",

    // --- Domain/Range Relations ---
    DOMAIN_RELATION          => "SUMO_DOMAIN_RELATION",
    DOMAIN_SUBCLASS_RELATION => "SUMO_DOMAIN_SUBCLASS_RELATION",
    RANGE_REL_CLASS          => "SUMO_RANGE_RELATION", // renamed to avoid collision
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
    /// `termFormat`) — a focused view over the subset of [`ALL_SYMBOLS`] carrying
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
