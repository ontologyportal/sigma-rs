// crates/sdk/src/manager/meta.rs
//
// KBManager option metadata — a clap-agnostic description of every configurable
// option, used downstream (the CLI) to *project* `KBManager` into a clap arg
// parser without the SDK depending on clap.
//
// Each [`OptionMeta`] ties one CLI flag to (a) one or more serde `json_paths`
// (so a single flat flag can patch one *or several* nested fields), and (b) a
// [`Scope`] saying which subcommands surface it.  The consumer:
//
//   1. builds clap args from the options whose scope matches a subcommand;
//   2. after parse, layers the user-supplied flags over the config-derived
//      `KBManager` (serialize → patch every `json_path` the flag targets →
//      deserialize), giving `flag > config.xml > default` precedence.
//
// A flag with several `json_paths` fans one value out to all of them — e.g.
// `--timeout` drives both the native (`native_prover.timeLimitSecs`) and the
// external (`external_prover.timeoutSecs`) prover budgets, so the user sets one
// number regardless of backend.
//
// Each `help` string here is CANONICAL: when the CLI is wired to project this
// table into its clap parser, it generates each flag's help from
// `OptionMeta.help` — so this table, not the CLI crate's arg doc-comments, is
// the single source of truth.  (The CLI still hand-declares its help today; some
// of these were seeded from the old CLI wording, but where they differ — e.g.
// `log_level` by level name, the backend-neutral `timeout`, the positive-framed
// `autoscale` — this wording wins.)  To add a configurable option: add the field
// to `KBManager` (or a prover config) and one row here.  The drift-guard tests
// below fail if a top-level field has no row, or a row's `json_path` doesn't
// resolve.

use super::KBManager;
use crate::{SdkError, SdkResult};

/// The subcommands an option can be scoped to.  Mirrors the CLI's `Cmd`
/// variants, minus `update` / `serve` (those are out of the SDK's surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsystem {
    Validate,
    Ask,
    Translate,
    Test,
    Load,
    Man,
    Search,
    Audit,
    Sweep,
}

/// Where an option is exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// A flag on every subcommand (clap `global = true`): logging, paths, …
    Global,
    /// A flag only on these subcommands (e.g. `eprover` → ask/test/audit).
    Subsystems(&'static [Subsystem]),
    /// Lives in the config file only — never a CLI flag (no SDK operation
    /// consumes it, e.g. `graphviz_dir`).
    ConfigOnly,
}

impl Scope {
    /// Does this option surface as a flag on `sub`?
    pub fn applies_to(&self, sub: Subsystem) -> bool {
        match self {
            Scope::Global         => true,
            Scope::Subsystems(ss) => ss.contains(&sub),
            Scope::ConfigOnly     => false,
        }
    }

    /// Is this a CLI flag at all (vs config-file-only)?
    pub fn is_cli(&self) -> bool {
        !matches!(self, Scope::ConfigOnly)
    }
}

/// The value type of an option — a hint for the clap consumer's `value_parser`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A boolean flag (`--forward-close` / config `forwardClose`).
    Bool,
    /// An unsigned integer (`usize` / `u64`).
    Int,
    /// A floating-point value (`f32` / `f64`).
    Float,
    /// A free-form string.
    Str,
    /// A filesystem path.
    Path,
    /// A repeatable / multi-value string flag (e.g. `-W E005 -W E010`).  The
    /// consumer collects the tokens and maps them to the target field's type
    /// (for `--warning`, via [`ElevateWarnings::from_tokens`](super::ElevateWarnings::from_tokens)).
    List,
}

/// One configurable option: how it appears on the CLI and where it lands in the
/// serialized [`KBManager`].
#[derive(Debug, Clone, Copy)]
pub struct OptionMeta {
    /// Stable identifier — the clap arg id (unique across the table).
    pub field: &'static str,
    /// Dot-path(s) into the serialized `KBManager` this option patches, using
    /// the serde key names (e.g. `native_prover.maxSteps`,
    /// `native_prover.selection.tolerance`).  Usually one; a flag that
    /// configures the same concept across backends lists several and the value
    /// fans out to all of them (e.g. `--timeout` → native + external budgets).
    pub json_paths: &'static [&'static str],
    /// Long flag (without the `--`).
    pub long: &'static str,
    /// Optional short flag.
    pub short: Option<char>,
    /// Optional environment-variable fallback.
    pub env: Option<&'static str>,
    /// Which subcommands surface the flag.
    pub scope: Scope,
    /// One-line, user-facing help.  Canonical: the CLI generates each flag's
    /// help text from this string.
    pub help: &'static str,
    /// Value type.
    pub kind: Kind,
}

impl KBManager {
    /// The full option table — the single source of truth a consumer projects
    /// into a CLI parser + a `flag > config.xml > default` merge.
    pub fn options() -> &'static [OptionMeta] {
        OPTIONS
    }

    /// The options that surface as a flag on `sub` (CLI flags only; global +
    /// in-scope subsystem options).
    pub fn options_for(sub: Subsystem) -> impl Iterator<Item = &'static OptionMeta> {
        OPTIONS.iter().filter(move |o| o.scope.applies_to(sub))
    }

    /// Layer CLI/env overrides on top of this config — the bottom of the
    /// `flag > env > config.xml > default` ladder (the first three having
    /// already built `self`).
    ///
    /// Each override pairs an [`OptionMeta`] with the already-typed JSON value
    /// the user supplied; the value is written to **every** one of the option's
    /// [`json_paths`](OptionMeta::json_paths), so a single flag fans out across
    /// fields (e.g. `--timeout` patches both prover backends' budgets).
    ///
    /// Clap-free by design: the CLI extracts `(option, value)` pairs from its
    /// `ArgMatches` — keeping only the args whose value-source is the command
    /// line / environment — and hands them here.  Implemented as
    /// serialize → patch the json-paths → deserialize (the same idiom
    /// `<prover>` preference parsing uses), so a type-mismatched override
    /// surfaces as a [`SdkError::Config`] rather than silently corrupting state.
    pub fn apply_overrides<'a>(
        &mut self,
        overrides: impl IntoIterator<Item = (&'a OptionMeta, serde_json::Value)>,
    ) -> SdkResult<()> {
        let mut doc = serde_json::to_value(&*self)
            .map_err(|e| SdkError::Config(format!("serializing config for override: {e}")))?;
        for (opt, value) in overrides {
            for path in opt.json_paths {
                set_json_path(&mut doc, path, value.clone());
            }
        }
        *self = serde_json::from_value(doc)
            .map_err(|e| SdkError::Config(format!("applying CLI overrides: {e}")))?;
        // `selected_kb` is `#[serde(skip)]` (transient runtime state), so the
        // serialize→patch→deserialize round-trip above resets it to `None` —
        // silently deselecting the KB chosen by `from_config_xml` and turning
        // every subsequent `current_sources_owned()` into an empty list.
        // Re-select the configured default; a later `--kb` still overrides.
        let name = self.sumokbname.clone();
        if !name.trim().is_empty() {
            self.set_current_kb(&name);
        }
        Ok(())
    }
}

/// Write `leaf` at dot-`path` in `doc`, creating intermediate objects as
/// needed.  The inverse of the `resolve` reader the drift tests use.
fn set_json_path(doc: &mut serde_json::Value, path: &str, leaf: serde_json::Value) {
    let segs: Vec<&str> = path.split('.').collect();
    let Some((last, parents)) = segs.split_last() else { return };
    let mut cur = doc;
    for seg in parents {
        if !cur.is_object() {
            *cur = serde_json::Value::Object(serde_json::Map::new());
        }
        cur = cur
            .as_object_mut()
            .unwrap()
            .entry((*seg).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }
    if let Some(obj) = cur.as_object_mut() {
        obj.insert((*last).to_string(), leaf);
    }
}

// Scope helpers — named subsystem sets reused across rows.
use Subsystem::*;
/// Prover-driven subcommands that all honor the prover paths / opts.
const PROVERS: &[Subsystem] = &[Ask, Test, Audit];
/// Subcommands that produce TPTP (and so honor the translation knobs).
const TRANSLATORS: &[Subsystem] = &[Translate, Ask, Test];
/// Native-prover knobs are also tuned by the sweep harness.
const NATIVE: &[Subsystem] = &[Ask, Test, Audit, Sweep];

const OPTIONS: &[OptionMeta] = &[
    // -- Global (every subcommand) -------------------------------------------
    OptionMeta { field: "base_dir", json_paths: &["base_dir"], long: "base-dir", short: None,
        env: Some("SIGMA_BASE_DIR"), scope: Scope::Global, kind: Kind::Path,
        help: "Base directory for resolving relative paths." },
    OptionMeta { field: "kb_dir", json_paths: &["kb_dir"], long: "kb-dir", short: None,
        env: Some("SIGMA_KB_DIR"), scope: Scope::Global, kind: Kind::Path,
        help: "Directory relative paths for constituent files resolve against." },
    OptionMeta { field: "systems_dir", json_paths: &["systems_dir"], long: "systems-dir", short: None,
        env: Some("SIGMA_SYSTEMS_DIR"), scope: Scope::Global, kind: Kind::Path,
        help: "Directory to resolve relative prover-binary paths against (tried before $PATH)." },
    OptionMeta { field: "edit_dir", json_paths: &["edit_dir"], long: "edit-dir", short: None,
        env: None, scope: Scope::Global, kind: Kind::Path,
        help: "Path to the LMDB database directory. Defaults to `./sumo.lmdb` in the current working directory." },
    OptionMeta { field: "log_dir", json_paths: &["log_dir"], long: "log-dir", short: None,
        env: None, scope: Scope::Global, kind: Kind::Path,
        help: "Directory to write logs to automatically." },
    OptionMeta { field: "log_level", json_paths: &["log_level"], long: "log-level", short: None,
        env: Some("SIGMA_LOG_LEVEL"), scope: Scope::Global, kind: Kind::Str,
        help: "Logging verbosity (error | warn | info | debug | trace)." },
    OptionMeta { field: "cache", json_paths: &["cache"], long: "cache", short: None,
        env: None, scope: Scope::Global, kind: Kind::Bool,
        help: "Enable automatic caching." },
    OptionMeta { field: "sumokbname", json_paths: &["sumokbname"], long: "sumokbname", short: None,
        env: None, scope: Scope::Global, kind: Kind::Str,
        help: "Knowledge base name from config.xml to load." },
    OptionMeta { field: "warning", json_paths: &["elevate_warnings"], long: "warning", short: Some('W'),
        env: None, scope: Scope::Global, kind: Kind::List,
        help: "Warning control (mimics GCC). By default semantic errors are warnings; `-W all` treats all as errors, `-W <CODE>` (e.g. -W E005) a specific one." },

    // -- Backend / language defaults -----------------------------------------
    OptionMeta { field: "backend", json_paths: &["default_backend"], long: "backend", short: None,
        env: Some("SIGMA_BACKEND"), scope: Scope::Subsystems(PROVERS), kind: Kind::Str,
        help: "Prover backend: 'native' (in-process saturation), 'subprocess' (forks `vampire` on PATH), 'e'/'eprover', or 'embedded' (requires the integrated-prover build)." },
    // `--lang` fans out like `--timeout`: the root field drives translation /
    // export, `external_prover.tptpLang` drives the subprocess/embedded prover
    // mode (`ExternalOpts.mode`) — patching only the root left the prover on
    // its own default and silently ran FOF under `--lang tff`.
    OptionMeta { field: "lang", json_paths: &["tptp_lang", "external_prover.tptpLang"], long: "lang", short: None,
        env: None, scope: Scope::Subsystems(TRANSLATORS), kind: Kind::Str,
        help: "TPTP language variant: 'fof' (default) or 'tff'." },
    OptionMeta { field: "real_numbers", json_paths: &["real_numbers"], long: "real-numbers", short: None,
        env: None, scope: Scope::Subsystems(TRANSLATORS), kind: Kind::Bool,
        help: "Cast every TFF numeric to $real (no $int/$rat, no $to_real coercions). Default: on for the E backend under TFF, off otherwise." },
    OptionMeta { field: "language", json_paths: &["language"], long: "language", short: None,
        env: None, scope: Scope::Subsystems(&[Man, Search, Ask, Audit]), kind: Kind::Str,
        help: "Natural language for documentation / term-format entries and proof rendering (e.g. EnglishLanguage)." },

    // -- Translation knobs (translate / ask / test) --------------------------
    OptionMeta { field: "holds_prefix", json_paths: &["holds_prefix"], long: "holds-prefix", short: None,
        env: None, scope: Scope::Subsystems(TRANSLATORS), kind: Kind::Bool,
        help: "Render higher-order statements with the `s__hold` prefix when translating to TPTP." },
    OptionMeta { field: "tptp", json_paths: &["tptp"], long: "cache-tptp", short: None,
        env: None, scope: Scope::Subsystems(TRANSLATORS), kind: Kind::Bool,
        help: "Cache TPTP translation results (faster translate, slower ingest)." },
    OptionMeta { field: "show_kif", json_paths: &["show_kif"], long: "show-kif", short: None,
        env: None, scope: Scope::Subsystems(&[Translate]), kind: Kind::Bool,
        help: "Emit a `% <original KIF>` comment before each TPTP formula. (Default on; CLI exposes the inverse.)" },

    // -- Proof output (ask / test / audit) -----------------------------------
    OptionMeta { field: "proof", json_paths: &["proof"], long: "proof", short: None,
        env: None, scope: Scope::Subsystems(PROVERS), kind: Kind::Str,
        help: "Proof-rendering format when one is found: 'kif' (default), 'tptp'/'casc'/'graphviz', or a SUMO language for natural-language rendering." },
    OptionMeta { field: "prose", json_paths: &["prose"], long: "prose", short: None,
        env: None, scope: Scope::Subsystems(&[Ask, Test]), kind: Kind::Bool,
        help: "Also render each proof as connected prose." },
    OptionMeta { field: "disable_selection", json_paths: &["disable_selection"], long: "full-kb", short: None,
        env: None, scope: Scope::Subsystems(PROVERS), kind: Kind::Bool,
        help: "Disable SInE axiom preselection: feed the prover the entire KB plus the assertions and conjecture." },

    // -- Audit-specific ------------------------------------------------------
    OptionMeta { field: "thoroughness", json_paths: &["thoroughness"], long: "thoroughness", short: None,
        env: None, scope: Scope::Subsystems(&[Audit]), kind: Kind::Float,
        help: "Fraction of a file's root sentences to sample for the consistency check, in (0.0, 1.0]." },
    OptionMeta { field: "limit", json_paths: &["limit"], long: "limit", short: None,
        env: None, scope: Scope::Subsystems(&[Audit]), kind: Kind::Int,
        help: "Stop after finding N distinct contradictions (native backend)." },

    // -- External prover binaries (ask / test / audit) -----------------------
    OptionMeta { field: "vampire", json_paths: &["vampire"], long: "vampire", short: None,
        env: Some("SIGMA_VAMPIRE"), scope: Scope::Subsystems(PROVERS), kind: Kind::Path,
        help: "Path to the Vampire executable (default: 'vampire' on PATH)." },
    OptionMeta { field: "eprover", json_paths: &["eprover"], long: "eprover", short: None,
        env: Some("SIGMA_EPROVER"), scope: Scope::Subsystems(PROVERS), kind: Kind::Path,
        help: "Path to the E prover executable." },
    OptionMeta { field: "leo", json_paths: &["leo_executable"], long: "leo", short: None,
        env: None, scope: Scope::Subsystems(PROVERS), kind: Kind::Path,
        help: "Path to the LEO higher-order ATP." },

    // -- Test discovery ------------------------------------------------------
    OptionMeta { field: "test_dir", json_paths: &["inference_test_dir"], long: "test-dir", short: None,
        env: None, scope: Scope::Subsystems(&[Test]), kind: Kind::Path,
        help: "Directory of `.kif.tq` tests (config.xml `inferenceTestDir`); used when no PATH is given." },

    // -- Native prover options (ask / test / audit / sweep) ------------------
    OptionMeta { field: "max_steps", json_paths: &["native_prover.maxSteps"], long: "max-steps", short: None,
        env: None, scope: Scope::Subsystems(NATIVE), kind: Kind::Int,
        help: "Given-clause step cap per run (the primary objective bound)." },
    OptionMeta { field: "max_lits", json_paths: &["native_prover.maxLits"], long: "max-lits", short: None,
        env: None, scope: Scope::Subsystems(NATIVE), kind: Kind::Int,
        help: "Native prover: maximum literals per derived clause." },
    // One budget, both backends: native wall-clock + external subprocess timeout.
    OptionMeta { field: "timeout",
        json_paths: &["native_prover.timeLimitSecs", "external_prover.timeoutSecs"],
        long: "timeout", short: None,
        env: None, scope: Scope::Subsystems(PROVERS), kind: Kind::Int,
        help: "Proof-search timeout in seconds." },
    OptionMeta { field: "forward_close", json_paths: &["native_prover.forwardClose"], long: "forward-close", short: None,
        env: None, scope: Scope::Subsystems(PROVERS), kind: Kind::Bool,
        help: "Native prover: run the bounded forward closure before the main loop." },
    OptionMeta { field: "native_profile", json_paths: &["native_prover.profile"], long: "native-profile", short: None,
        env: None, scope: Scope::Subsystems(PROVERS), kind: Kind::Bool,
        help: "Native prover: collect per-mechanism timing inside the saturation loop." },
    OptionMeta { field: "want_proof", json_paths: &["native_prover.wantProof"], long: "want-proof", short: None,
        env: None, scope: Scope::Subsystems(PROVERS), kind: Kind::Bool,
        help: "Render the refutation into a proof transcript when the prover finds one." },
    OptionMeta { field: "step", json_paths: &["native_prover.step"], long: "step", short: None,
        env: None, scope: Scope::Subsystems(&[Ask, Test]), kind: Kind::Bool,
        help: "Interactively single-step the native prover: pause at each given-clause and inference. Use with ONE problem; runs single-threaded." },
    OptionMeta { field: "scope", json_paths: &["native_prover.selection.tolerance"], long: "scope", short: None,
        env: None, scope: Scope::Subsystems(&[Ask, Test, Translate, Audit, Sweep]), kind: Kind::Float,
        help: "SInE tolerance factor (>= 1.0) for the relevance filter. Higher values pull in more axioms — more recall but slower proving. Values below 1.0 are rejected." },
    OptionMeta { field: "autoscale", json_paths: &["native_prover.selection.autoscale"], long: "autoscale", short: None,
        env: None, scope: Scope::Subsystems(&[Ask, Test, Sweep]), kind: Kind::Bool,
        help: "Drive prover-feedback autoscaling: the axiom budget is widened when the conjecture isn't entailed and narrowed on timeout. The CLI exposes the inverse `--no-autoscale`." },

    // -- Config-only (no SDK operation consumes these → no CLI flag) ---------
    OptionMeta { field: "graphviz_dir", json_paths: &["graphviz_dir"], long: "graphviz-dir", short: None,
        env: None, scope: Scope::ConfigOnly, kind: Kind::Path,
        help: "Directory for graphviz outputs." },
    OptionMeta { field: "ollama_host", json_paths: &["ollama_host"], long: "ollama-host", short: None,
        env: None, scope: Scope::ConfigOnly, kind: Kind::Str,
        help: "Ollama / OpenAI-compatible host for LLM proof explanations." },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Walk a dot-path into a serde value.
    fn resolve<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
        let mut cur = v;
        for seg in path.split('.') {
            cur = cur.get(seg)?;
        }
        Some(cur)
    }

    #[test]
    fn flags_and_ids_are_unique() {
        let mut longs = HashSet::new();
        let mut fields = HashSet::new();
        for o in KBManager::options() {
            assert!(fields.insert(o.field), "duplicate option id `{}`", o.field);
            assert!(longs.insert(o.long), "duplicate flag `--{}`", o.long);
            assert!(!o.json_paths.is_empty(), "option `{}` targets no json_path", o.field);
        }
    }

    #[test]
    fn every_json_path_resolves() {
        let v = serde_json::to_value(KBManager::default()).unwrap();
        for o in KBManager::options() {
            for path in o.json_paths {
                assert!(resolve(&v, path).is_some(),
                    "json_path `{}` for `--{}` does not resolve in a serialized KBManager",
                    path, o.long);
            }
        }
    }

    /// Drift guard: every top-level config field must be targeted by some row
    /// (catches "added a field, forgot the option entry").  Collections /
    /// nested config are covered by their own nested paths.
    #[test]
    fn every_top_level_field_is_covered() {
        let v = serde_json::to_value(KBManager::default()).unwrap();
        let obj = v.as_object().expect("KBManager serializes to a JSON object");
        let covered: HashSet<&str> = KBManager::options()
            .iter()
            .flat_map(|o| o.json_paths.iter().copied())
            .collect();
        for key in obj.keys() {
            if matches!(key.as_str(), "kbs" | "native_prover" | "external_prover" | "unknown_preferences") {
                continue; // collections + nested configs handled via nested paths
            }
            assert!(covered.contains(key.as_str()),
                "KBManager field `{key}` has no OptionMeta row — add one (or it's intentionally uncovered)");
        }
    }

    #[test]
    fn one_flag_can_drive_multiple_fields() {
        // `--timeout` fans out to both prover backends' budgets.
        let timeout = KBManager::options().iter().find(|o| o.field == "timeout").unwrap();
        assert!(timeout.json_paths.contains(&"native_prover.timeLimitSecs"));
        assert!(timeout.json_paths.contains(&"external_prover.timeoutSecs"));
    }

    #[test]
    fn apply_overrides_patches_nested_and_fans_out() {
        let mut m = KBManager::default();
        let opts = KBManager::options();
        let by = |id: &str| opts.iter().find(|o| o.field == id).unwrap();
        m.apply_overrides([
            (by("backend"), serde_json::json!("subprocess")),
            (by("scope"),   serde_json::json!(3.5)),       // nested 2 levels deep
            (by("timeout"), serde_json::json!(90)),        // fans out to both backends
        ])
        .unwrap();
        assert_eq!(m.default_backend, "subprocess");
        assert!((m.native_prover.selection.tolerance - 3.5).abs() < 1e-6);
        assert_eq!(m.native_prover.time_limit_secs, 90);
        assert_eq!(m.external_prover.timeout_secs, 90);
    }

    #[test]
    fn apply_overrides_rejects_a_type_mismatch() {
        let mut m = KBManager::default();
        let timeout = KBManager::options().iter().find(|o| o.field == "timeout").unwrap();
        // timeLimitSecs is u64 — a string can't deserialize back into it.
        let err = m.apply_overrides([(timeout, serde_json::json!("not-a-number"))]).unwrap_err();
        assert!(matches!(err, SdkError::Config(_)));
    }

    #[test]
    fn scope_filtering_works() {
        // `--eprover` is a prover flag, not a validate flag.
        let ask: Vec<&str> = KBManager::options_for(Subsystem::Ask).map(|o| o.long).collect();
        let validate: Vec<&str> = KBManager::options_for(Subsystem::Validate).map(|o| o.long).collect();
        assert!(ask.contains(&"eprover"));
        assert!(!validate.contains(&"eprover"));
        // globals appear everywhere.
        assert!(ask.contains(&"log-level") && validate.contains(&"log-level"));
        // config-only never surfaces.
        assert!(!ask.contains(&"graphviz-dir"));
    }
}
