//! Symbols — sorts, functions, predicates, interpreted theory symbols.
//!
//! All three are pure data: a name, an arity, an optional type signature, and
//! a few flags.

/// A sort (type) in TFF.
///
/// Built-ins (`$i`, `$int`, `$real`, `$rat`, `$o`) carry the `is_builtin`
/// flag so [`Sort::tptp_decl`] knows not to emit a redundant declaration
/// for them.
///
/// # Examples
///
/// ```
/// use crate::trans::ir::Sort;
///
/// // User-defined sort.
/// let person = Sort::new("person");
/// assert_eq!(person.tptp_name(), "person");
/// assert!(!person.is_builtin());
///
/// // Built-in sorts.
/// let i = Sort::default_sort();
/// let z = Sort::int();
/// assert!(i.is_builtin() && z.is_builtin());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Sort {
    name: String,
    is_builtin: bool,
}

impl Sort {
    /// Creates a user-defined sort with the given name.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::Sort;
    ///
    /// let animal = Sort::new("animal");
    /// assert_eq!(animal.tptp_name(), "animal");
    /// ```
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), is_builtin: false }
    }

    /// Returns the default individual sort (`$i`).
    pub fn default_sort() -> Self { Self::builtin("$i") }

    /// Returns the integer sort (`$int`).
    pub fn int() -> Self { Self::builtin("$int") }

    /// Returns the real number sort (`$real`).
    pub fn real() -> Self { Self::builtin("$real") }

    /// Returns the rational number sort (`$rat`).
    pub fn rational() -> Self { Self::builtin("$rat") }

    /// Returns the Boolean sort (`$o`).
    pub fn bool() -> Self { Self::builtin("$o") }

    fn builtin(name: &str) -> Self {
        Self { name: name.to_string(), is_builtin: true }
    }

    /// Returns the TPTP identifier for this sort (e.g. `"$int"`, or a
    /// user-chosen name).
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::Sort;
    ///
    /// assert_eq!(Sort::int().tptp_name(),        "$int");
    /// assert_eq!(Sort::new("person").tptp_name(), "person");
    /// ```
    pub fn tptp_name(&self) -> &str { &self.name }

    /// Returns `true` for the five built-in TPTP sorts (`$i`, `$int`,
    /// `$real`, `$rat`, `$o`).
    pub fn is_builtin(&self) -> bool { self.is_builtin }

    /// Returns the TPTP type declaration line for this sort, or `None` for
    /// built-in sorts that need no declaration.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::Sort;
    ///
    /// let animal = Sort::new("animal");
    /// assert_eq!(
    ///     animal.tptp_decl().unwrap(),
    ///     "tff(animal_type, type, animal: $tType).",
    /// );
    ///
    /// // Built-ins produce no declaration.
    /// assert_eq!(Sort::int().tptp_decl(), None);
    /// ```
    pub fn tptp_decl(&self) -> Option<String> {
        if self.is_builtin {
            return None;
        }
        Some(format!(
            "tff({name}_type, type, {name}: $tType).",
            name = self.name,
        ))
    }
}

/// A function symbol. Arity-0 functions are constants.
///
/// `arg_sorts` and `ret_sort` are populated when the symbol is created via
/// [`Function::typed`] or [`Function::interpreted`]; they are empty / `None`
/// for untyped FOF functions built via [`Function::new`].
///
/// Functions compare equal by `(name, arity, kind, sorts)` — structural
/// equality on the underlying data.
///
/// # Examples
///
/// ```
/// use crate::trans::ir::{Function, Sort};
///
/// // Untyped binary function.
/// let plus = Function::new("plus", 2);
/// assert_eq!(plus.arity(), 2);
/// assert!(!plus.is_typed());
///
/// // Typed constant of a user-defined sort.
/// let person = Sort::new("person");
/// let alice  = Function::typed("alice", &[], person.clone());
/// assert_eq!(alice.arity(), 0);
/// assert_eq!(alice.ret_sort(), Some(&person));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Function {
    name: String,
    arity: u32,
    arg_sorts: Vec<Sort>,
    ret_sort: Option<Sort>,
    kind: FuncKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
enum FuncKind {
    Untyped,
    Typed,
    Interpreted(Interp),
}

impl Function {
    /// Creates an untyped FOF function symbol with the given name and arity.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::Function;
    ///
    /// let succ = Function::new("succ", 1);
    /// assert_eq!(succ.arity(), 1);
    /// ```
    pub fn new(name: &str, arity: u32) -> Self {
        Self {
            name: name.to_string(),
            arity,
            arg_sorts: Vec::new(),
            ret_sort: None,
            kind: FuncKind::Untyped,
        }
    }

    /// Creates a typed TFF function symbol with explicit argument and return
    /// sorts.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Function, Sort};
    ///
    /// let person = Sort::new("person");
    /// let father_of = Function::typed("father_of", &[person.clone()], person);
    /// assert!(father_of.is_typed());
    /// ```
    pub fn typed(name: &str, arg_sorts: &[Sort], return_sort: Sort) -> Self {
        Self {
            name: name.to_string(),
            arity: arg_sorts.len() as u32,
            arg_sorts: arg_sorts.to_vec(),
            ret_sort: Some(return_sort),
            kind: FuncKind::Typed,
        }
    }

    /// Creates an interpreted theory function (e.g. `$sum`, `$product`).
    ///
    /// The arity is derived from the interpretation (unary for
    /// `IntUnaryMinus`, `IntSuccessor`, `IntAbs`; binary otherwise).
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Function, Interp};
    ///
    /// let plus = Function::interpreted("$sum", Interp::IntPlus);
    /// assert_eq!(plus.arity(), 2);
    /// ```
    pub fn interpreted(name: &str, interp: Interp) -> Self {
        Self {
            name: name.to_string(),
            arity: interp.default_arity(),
            arg_sorts: Vec::new(),
            ret_sort: None,
            kind: FuncKind::Interpreted(interp),
        }
    }

    /// The function's name.
    pub fn name(&self) -> &str { &self.name }

    /// The function's arity (number of arguments).
    pub fn arity(&self) -> u32 { self.arity }

    /// The argument sorts. Empty for untyped or interpreted functions.
    pub fn arg_sorts(&self) -> &[Sort] { &self.arg_sorts }

    /// The return sort. `None` for untyped or interpreted functions.
    pub fn ret_sort(&self) -> Option<&Sort> { self.ret_sort.as_ref() }

    /// `true` if the function was constructed via [`Function::typed`].
    pub fn is_typed(&self) -> bool { matches!(self.kind, FuncKind::Typed) }

    /// The interpretation, if any, attached to this function.
    pub fn interp(&self) -> Option<Interp> {
        match self.kind {
            FuncKind::Interpreted(i) => Some(i),
            _ => None,
        }
    }

    /// Returns the TPTP type declaration line for this function, or `None`
    /// for untyped and interpreted functions (which need no declaration).
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Function, Sort};
    ///
    /// let person = Sort::new("person");
    /// let nil    = Function::typed("nil", &[], person.clone());
    /// assert_eq!(
    ///     nil.tptp_decl().unwrap(),
    ///     "tff(fn_nil, type, nil: person).",
    /// );
    ///
    /// let child_of = Function::typed(
    ///     "child_of",
    ///     &[person.clone(), person.clone()],
    ///     person,
    /// );
    /// assert_eq!(
    ///     child_of.tptp_decl().unwrap(),
    ///     "tff(fn_child_of, type, child_of: (person * person) > person).",
    /// );
    ///
    /// assert_eq!(Function::new("f", 2).tptp_decl(), None);
    /// ```
    pub fn tptp_decl(&self) -> Option<String> {
        if !self.is_typed() {
            return None;
        }
        let ret = self.ret_sort.as_ref().unwrap().tptp_name();
        if self.arg_sorts.is_empty() {
            return Some(format!(
                "tff(fn_{name}, type, {name}: {ret}).",
                name = self.name, ret = ret,
            ));
        }
        let args: Vec<&str> = self.arg_sorts.iter().map(|s| s.tptp_name()).collect();
        let args_str = if args.len() == 1 {
            args[0].to_string()
        } else {
            format!("({})", args.join(" * "))
        };
        Some(format!(
            "tff(fn_{name}, type, {name}: {args} > {ret}).",
            name = self.name, args = args_str, ret = ret,
        ))
    }
}

/// A predicate symbol. Equivalent to a function returning `$o`, but kept
/// as a distinct type to match TPTP's syntactic distinction.
///
/// # Examples
///
/// ```
/// use crate::trans::ir::{Predicate, Sort};
///
/// let mortal = Predicate::new("mortal", 1);
/// assert_eq!(mortal.arity(), 1);
/// assert!(!mortal.is_typed());
///
/// let person = Sort::new("person");
/// let likes  = Predicate::typed("likes", &[person.clone(), person]);
/// assert_eq!(likes.arity(), 2);
/// assert!(likes.is_typed());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Predicate {
    name: String,
    arity: u32,
    arg_sorts: Vec<Sort>,
    kind: PredKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
enum PredKind {
    Untyped,
    Typed,
    Interpreted(Interp),
}

impl Predicate {
    /// Creates an untyped FOF predicate symbol with the given name and arity.
    pub fn new(name: &str, arity: u32) -> Self {
        Self {
            name: name.to_string(),
            arity,
            arg_sorts: Vec::new(),
            kind: PredKind::Untyped,
        }
    }

    /// Creates a typed TFF predicate symbol with explicit argument sorts.
    pub fn typed(name: &str, arg_sorts: &[Sort]) -> Self {
        Self {
            name: name.to_string(),
            arity: arg_sorts.len() as u32,
            arg_sorts: arg_sorts.to_vec(),
            kind: PredKind::Typed,
        }
    }

    /// Creates an interpreted theory predicate (e.g. `$less`, `$greater`).
    pub fn interpreted(name: &str, interp: Interp) -> Self {
        Self {
            name: name.to_string(),
            arity: interp.default_arity(),
            arg_sorts: Vec::new(),
            kind: PredKind::Interpreted(interp),
        }
    }

    /// The predicate's name.
    pub fn name(&self) -> &str { &self.name }

    /// The predicate's arity (number of arguments).
    pub fn arity(&self) -> u32 { self.arity }

    /// The argument sorts. Empty for untyped or interpreted predicates.
    pub fn arg_sorts(&self) -> &[Sort] { &self.arg_sorts }

    /// `true` if the predicate was constructed via [`Predicate::typed`].
    pub fn is_typed(&self) -> bool { matches!(self.kind, PredKind::Typed) }

    /// The interpretation, if any, attached to this predicate.
    pub fn interp(&self) -> Option<Interp> {
        match self.kind {
            PredKind::Interpreted(i) => Some(i),
            _ => None,
        }
    }

    /// Returns the TPTP type declaration line for this predicate, or `None`
    /// for untyped and interpreted predicates.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Predicate, Sort};
    ///
    /// let person = Sort::new("person");
    /// let likes  = Predicate::typed("likes", &[person.clone(), person]);
    /// assert_eq!(
    ///     likes.tptp_decl().unwrap(),
    ///     "tff(pred_likes_2, type, likes: (person * person) > $o).",
    /// );
    ///
    /// assert_eq!(Predicate::new("P", 3).tptp_decl(), None);
    /// ```
    pub fn tptp_decl(&self) -> Option<String> {
        if !self.is_typed() {
            return None;
        }
        if self.arg_sorts.is_empty() {
            return Some(format!(
                "tff(pred_{name}_{arity}, type, {name}: $o).",
                name = self.name, arity = self.arity,
            ));
        }
        let args: Vec<&str> = self.arg_sorts.iter().map(|s| s.tptp_name()).collect();
        let args_str = if args.len() == 1 {
            args[0].to_string()
        } else {
            format!("({})", args.join(" * "))
        };
        Some(format!(
            "tff(pred_{name}_{arity}, type, {name}: {args} > $o).",
            name = self.name, arity = self.arity, args = args_str,
        ))
    }
}

/// Interpreted theory symbols for arithmetic and comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Interp {
    Equal,
    // Integer comparisons
    IntGreater,
    IntGreaterEqual,
    IntLess,
    IntLessEqual,
    IntDivides,
    // Integer arithmetic
    IntSuccessor,
    IntUnaryMinus,
    IntPlus,
    IntMinus,
    IntMultiply,
    IntAbs,
    /// Euclidean integer division (`$quotient_e`).
    IntQuotientE,
    /// Integer remainder truncated toward zero (`$remainder_t`).
    IntRemainderT,
    // Rational comparisons
    RatGreater,
    RatGreaterEqual,
    RatLess,
    RatLessEqual,
    // Rational arithmetic
    RatPlus,
    RatMinus,
    RatMultiply,
    RatQuotient,
    // Real comparisons
    RealGreater,
    RealGreaterEqual,
    RealLess,
    RealLessEqual,
    // Real arithmetic
    RealPlus,
    RealMinus,
    RealMultiply,
    RealQuotient,
    // Rounding (argument sort → integer result)
    IntFloor,
    IntCeiling,
    IntTruncate,
    IntRound,
    RatFloor,
    RatCeiling,
    RatTruncate,
    RatRound,
    RealFloor,
    RealCeiling,
    RealTruncate,
    RealRound,
    // Type coercions ({src}To{dst})
    IntToInt,
    IntToRat,
    IntToReal,
    RatToInt,
    RatToRat,
    RatToReal,
    RealToInt,
    RealToRat,
    RealToReal,
}

impl Interp {
    /// Default arity used when the caller only supplies a name plus an
    /// interpretation (the usual TPTP convention).
    ///
    /// Unary: `IntUnaryMinus`, `IntSuccessor`, `IntAbs`, the rounding
    /// functions, and the type-coercion functions. All others are binary.
    pub fn default_arity(self) -> u32 {
        match self {
            Interp::IntUnaryMinus | Interp::IntSuccessor | Interp::IntAbs
            | Interp::IntFloor    | Interp::IntCeiling   | Interp::IntTruncate  | Interp::IntRound
            | Interp::RatFloor    | Interp::RatCeiling   | Interp::RatTruncate  | Interp::RatRound
            | Interp::RealFloor   | Interp::RealCeiling  | Interp::RealTruncate | Interp::RealRound
            | Interp::IntToInt    | Interp::IntToRat     | Interp::IntToReal
            | Interp::RatToInt    | Interp::RatToRat     | Interp::RatToReal
            | Interp::RealToInt   | Interp::RealToRat    | Interp::RealToReal => 1,
            _ => 2,
        }
    }

    /// The TPTP symbol name for this interpreted operation.
    pub fn tptp_name(self) -> &'static str {
        match self {
            Interp::Equal                                            => "=",
            Interp::IntGreater    | Interp::RatGreater    | Interp::RealGreater    => "$greater",
            Interp::IntGreaterEqual | Interp::RatGreaterEqual | Interp::RealGreaterEqual => "$greatereq",
            Interp::IntLess       | Interp::RatLess       | Interp::RealLess       => "$less",
            Interp::IntLessEqual  | Interp::RatLessEqual  | Interp::RealLessEqual  => "$lesseq",
            Interp::IntDivides                                       => "$divides",
            Interp::IntSuccessor                                     => "$succ",
            Interp::IntUnaryMinus                                    => "$uminus",
            Interp::IntPlus       | Interp::RatPlus       | Interp::RealPlus       => "$sum",
            Interp::IntMinus      | Interp::RatMinus      | Interp::RealMinus      => "$difference",
            Interp::IntMultiply   | Interp::RatMultiply   | Interp::RealMultiply   => "$product",
            Interp::IntAbs                                           => "$abs",
            Interp::IntQuotientE                                     => "$quotient_e",
            Interp::IntRemainderT                                    => "$remainder_t",
            Interp::RatQuotient   | Interp::RealQuotient             => "$quotient",
            Interp::IntFloor    | Interp::RatFloor    | Interp::RealFloor    => "$floor",
            Interp::IntCeiling  | Interp::RatCeiling  | Interp::RealCeiling  => "$ceiling",
            Interp::IntTruncate | Interp::RatTruncate | Interp::RealTruncate => "$truncate",
            Interp::IntRound    | Interp::RatRound    | Interp::RealRound    => "$round",
            Interp::IntToInt    | Interp::RatToInt    | Interp::RealToInt    => "$to_int",
            Interp::IntToRat    | Interp::RatToRat    | Interp::RealToRat    => "$to_rat",
            Interp::IntToReal   | Interp::RatToReal   | Interp::RealToReal   => "$to_real",
        }
    }
}
