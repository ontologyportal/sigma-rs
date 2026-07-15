// crates/core/src/syntactic/sine/params.rs
//
// SInE tuning knobs: tolerance / budget defaults, the autoscaling factors, and
// the `SineParams` struct.  Pure configuration — no dependency on `SineIndex`.

// -- Default tolerance -------------------------------------------------------

/// Returns the default SInE tolerance factor.
///
/// Currently a fixed constant read from the `SINE_TOLERANCE` compile-time
/// environment variable, falling back to `2.0`.  Exposed as a function
/// rather than a constant so the body can later be replaced with a
/// KB-size-aware heuristic without changing any call sites.
///
/// Note: when [`SineParams::auto_budget`] is `Some` (the default), this
/// fixed value is *ignored* — the tolerance is chosen automatically by
/// [`SineIndex::select_within_budget`].  It is only used when an explicit
/// fixed tolerance is requested.
pub fn default_tolerance() -> f32 {
    option_env!("SINE_TOLERANCE")
        .and_then(|s| s.parse().ok())
        .unwrap_or(2.0)
}

/// Returns the default axiom budget for auto-tolerance selection.
///
/// Read from the `SINE_BUDGET` compile-time environment variable, falling
/// back to `2000`.  Auto-tolerance picks the largest tolerance whose
/// selected-axiom count stays at or below this budget (see
/// [`SineIndex::select_within_budget`]).  The value brackets the empirical
/// SUMO sweet spot reported by Hoder & Voronkov (solvable SUMO problems
/// sat in the ~1k–8k selected-axiom range).
pub fn default_budget() -> usize {
    option_env!("SINE_BUDGET")
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000)
}

/// Upper bound on the tolerance the auto-selector will climb to.
///
/// Beyond the empirical plateau (~20 for the AFP; far lower for SUMO/CYC)
/// raising tolerance only inflates the selected set without admitting any
/// further *useful* axioms, so the climb stops here even if the budget is
/// not yet reached.
pub const MAX_AUTO_TOLERANCE: f32 = 64.0;

/// Hard cap on the number of breakpoint steps [`SineIndex::tolerance_breakpoints`]
/// will climb, guarding against pathological conjectures with very many
/// distinct activation thresholds.
// -- Parameters --------------------------------------------------------------

/// Tuning knobs for SInE axiom selection.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SineParams {
    /// Tolerance factor (≥ 1.0).  A symbol `s` triggers axiom `A` iff
    /// `occ(s) ≤ tolerance · min{occ(s') | s' ∈ symbols(A)}`.
    ///
    /// - `1.0`: only the least-general symbol(s) trigger (smallest premise sets).
    /// - `1.2`: common empirical default; modest benevolence.
    /// - `3.0+`: generous selection; use when strict is losing needed premises.
    ///
    /// Values below `1.0` are clamped to `1.0` during use.
    ///
    /// Ignored when [`Self::auto_budget`] is `Some` — auto-tolerance
    /// overrides this with a value chosen from the KB's activation
    /// thresholds.
    pub tolerance: f32,
    /// Maximum BFS depth.  `None` = unlimited (run to fixed point).
    /// `Some(0)` returns the empty set (no expansion performed).
    pub depth_limit: Option<usize>,
    /// Auto-tolerance budget.
    ///
    /// - `Some(budget)` (the **default**): ignore [`Self::tolerance`] and
    ///   instead select the largest tolerance whose selected-axiom count
    ///   stays `≤ budget` (see [`SineIndex::select_within_budget`]).
    /// - `None`: use the fixed [`Self::tolerance`] value.
    ///
    /// Constructing via [`Self::benevolent`] / [`Self::strict`] (i.e. a
    /// user-supplied tolerance) sets this to `None`; [`Self::default`]
    /// leaves it `Some(default_budget())`.
    pub auto_budget: Option<usize>,
    /// Bypass SInE entirely and select the **whole KB** (every axiom).
    ///
    /// When `true`, callers (`KnowledgeBase::ask` / `ask_embedded`) skip
    /// seed-based selection and feed the prover all axioms plus the
    /// session assertions and conjecture — i.e. *no* axiom preselection,
    /// matching the legacy Java SigmaKEE behaviour.  `tolerance`,
    /// `depth_limit`, and `auto_budget` are ignored in this mode.  Used
    /// for prover-vs-prover benchmarking where both backends must solve
    /// the identical, un-pruned problem.
    pub select_all: bool,
    /// Drive the prover-feedback **autoscaling loop** (see
    /// `KnowledgeBase::ask`).  When `true` (the **default**), `auto_budget`
    /// is used only as the *starting* budget: the prover is re-run with the
    /// budget widened on under-selection (disproof / saturation) and
    /// narrowed on over-selection (wall-clock timeout), within the call's
    /// time budget.  Ignored unless `auto_budget` is `Some` and
    /// `select_all` is `false`; a fixed tolerance ([`Self::benevolent`] /
    /// [`Self::strict`]) and whole-KB mode run single-shot.
    pub autoscale: bool,
}

impl Default for SineParams {
    fn default() -> Self {
        Self {
            tolerance:   default_tolerance(),
            depth_limit: None,
            auto_budget: Some(default_budget()),
            select_all:  false,
            autoscale:   true,
        }
    }
}

impl SineParams {
    /// Strict: tolerance 1.0, unlimited depth — only least-general symbols
    /// trigger.  Disables auto-tolerance and autoscaling.
    pub fn strict() -> Self {
        Self { tolerance: 1.0, depth_limit: None, auto_budget: None, select_all: false, autoscale: false }
    }
    /// Benevolent: a fixed user-supplied tolerance, clamped to ≥ 1.0.
    /// Disables auto-tolerance and autoscaling — the value is used verbatim.
    pub fn benevolent(tolerance: f32) -> Self {
        Self { tolerance: tolerance.max(1.0), depth_limit: None, auto_budget: None, select_all: false, autoscale: false }
    }
    /// Auto-tolerance with the given starting budget, autoscaling enabled.
    pub fn auto(budget: usize) -> Self {
        Self { tolerance: default_tolerance(), depth_limit: None, auto_budget: Some(budget), select_all: false, autoscale: true }
    }
    /// No preselection: select the entire KB (every axiom).  Equivalent
    /// to the Java SigmaKEE path which hands Vampire the whole ontology.
    pub fn whole_kb() -> Self {
        Self { tolerance: default_tolerance(), depth_limit: None, auto_budget: None, select_all: true, autoscale: false }
    }

    /// `true` iff the prover-feedback autoscaling loop should run for these
    /// params: autoscaling requested, a budget start point exists, and we're
    /// not in whole-KB mode.
    pub fn autoscaling(&self) -> bool {
        self.autoscale && self.auto_budget.is_some() && !self.select_all
    }
}

// -- Autoscaling configuration ----------------------------------------------

/// Budget multiplier for each autoscale step (widen ×, narrow ÷).
/// `SINE_SCALE_FACTOR`, default `2`.
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub fn scale_factor() -> usize {
    option_env!("SINE_SCALE_FACTOR").and_then(|s| s.parse().ok()).filter(|&f| f >= 2).unwrap_or(2)
}

/// Give-up threshold for the widen path: stop after this many consecutive
/// under-selection verdicts (disproof / saturation) that fail to prove.
/// `SINE_MAX_DISPROOFS`, default `6`.
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub fn scale_max_disproofs() -> usize {
    option_env!("SINE_MAX_DISPROOFS").and_then(|s| s.parse().ok()).filter(|&n| n >= 1).unwrap_or(6)
}

/// Number of full-length prover runs the total timeout is split across for
/// the narrow path.  `SINE_MAX_TIME_RUNS`, default `4`.
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub fn scale_max_time_runs() -> usize {
    option_env!("SINE_MAX_TIME_RUNS").and_then(|s| s.parse().ok()).filter(|&n| n >= 1).unwrap_or(4)
}

/// Floor on the axiom budget when narrowing.  `SINE_MIN_BUDGET`, default `64`.
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub fn scale_min_budget() -> usize {
    option_env!("SINE_MIN_BUDGET").and_then(|s| s.parse().ok()).filter(|&n| n >= 1).unwrap_or(64)
}

/// Per-schema cap for predicate-variable instantiation: skip a property
/// schema if more than this many of the problem's relations are instances of
/// its guard class (prevents a broad guard class from bloating the axiom set
/// even within a single problem).  `SINE_PREDVAR_CAP`, default `32`.
#[cfg(feature = "ask")]
pub fn scale_predvar_cap() -> usize {
    option_env!("SINE_PREDVAR_CAP").and_then(|s| s.parse().ok()).filter(|&n| n >= 1).unwrap_or(32)
}
