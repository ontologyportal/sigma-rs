// crates/sdk/src/manager/mod.rs
//
// Global KB configuration (`KBManager`) and constituent membership.  The struct
// is `serde`-(de)serializable for a modern config format (JSON/TOML/…), and
// `from_config_xml` ingests the legacy SUMO `config.xml` (a flat list of
// `<preference name=.. value=..>` plus `<kb>`/`<constituent>` elements).

mod sources;
mod write;
// Clap-agnostic option metadata — projects KBManager into a CLI parser.
pub mod meta;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use log::LevelFilter;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use serde::{Deserialize, Serialize};
use sigmakee_rs_core::{SineParams, TptpLang, TptpOptions};
#[cfg(feature = "native-prover")]
use sigmakee_rs_core::Strategy;

use crate::{SdkError, SdkResult, Source};

/// Primary struct used for storing global KB configuration
/// and constituent membership.
///
/// Field names are the canonical Rust form of the legacy `config.xml`
/// `<preference>` keys (e.g. `graphviz_dir` ⇔ `graphDir`, `tptp` ⇔ `TPTP`);
/// see [`KBManager::from_config_xml`].  Preferences in the XML with no matching
/// field are ignored.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KBManager {
    /// The base directory from which to resolve relative paths
    pub base_dir: PathBuf,
    /// Whether to enable auto caching
    pub cache: bool,
    /// The default prover backend to use when none is given on the
    /// command line: `native`, `subprocess`, `e`/`eprover`, or `embedded`.
    pub default_backend: String,
    /// Disable SInE axiom preselection — feed the prover the entire KB
    /// (the config home of the CLI `--full-kb` flag).
    pub disable_selection: bool,
    /// The directory where to place the `sumo.lmdb` cache file
    /// in (if not specified, will be placed in the PWD)
    pub edit_dir: PathBuf,
    /// Policy for elevating semantic warnings to hard errors (the config home
    /// of the CLI `-W` flag): none, all, or specific codes.
    pub elevate_warnings: ElevateWarnings,
    /// The path to the prover executable
    pub eprover: PathBuf,
    /// The directory where to place graphviz outputs
    pub graphviz_dir: PathBuf,
    /// Whether to render higher order statements with `s__hold`
    /// when translating to TPTP
    pub holds_prefix: bool,
    /// The directory to look for tests in
    pub inference_test_dir: PathBuf,
    /// The directory from which to derive relative paths
    /// for constituent files. If this is relative, will also
    /// be relative to [`KBManager::base_dir`]
    pub kb_dir: PathBuf,
    /// The default natural language for documentation, search, and
    /// natural-language proof rendering (e.g. `EnglishLanguage`).
    pub language: String,
    /// The path to the LEO ATP
    pub leo_executable: PathBuf,
    /// (audit) Stop after finding this many distinct contradictions.
    pub limit: usize,
    /// Where to store logs to automatically
    pub log_dir: PathBuf,
    /// The default logging level. Valid options are `error`,
    /// `warning`, `info`, `debug`, and `trace`.
    #[serde(with = "log_level_serde")]
    pub log_level: LevelFilter,
    /// Host info for ollama (or any OpenAI compatible endpoint)
    /// API for LLM filtered proof explanations
    pub ollama_host: String,
    /// Default proof-rendering format when a proof is found: `kif`,
    /// `tptp`/`casc`, or a SUMO language for natural-language rendering.
    pub proof: String,
    /// Whether to also render each proof as connected prose.
    pub prose: bool,
    /// Whether to emit a `% <original KIF>` comment before each TPTP formula
    /// when translating.
    pub show_kif: bool,
    /// The default KB to use when none are specified
    pub sumokbname: String,
    /// Directory to resolve relative prover-binary paths against (tried
    /// before `$PATH`).  See [`validate_prover_paths`](Self::validate_prover_paths).
    pub systems_dir: PathBuf,
    /// (audit) Fraction of a file's root sentences to sample for the
    /// consistency check, in (0.0, 1.0].
    pub thoroughness: f32,
    /// Whether to cache translation caches (results in faster
    /// TPTP translation but slower file ingest)
    pub tptp: bool,
    /// The default TPTP language variant to emit / prove with: `fof` or `tff`.
    pub tptp_lang: String,
    /// Reals-only TFF numerics: cast every numeric to `$real` (no `$int`/
    /// `$rat` sorts, no `$to_real` coercions).  `None` = backend default
    /// (ON for the E backend under TFF — works around E 3.2.5's
    /// `$to_real`-in-equality type-checker bug — OFF otherwise).
    pub real_numbers: Option<bool>,
    /// The path to the vampire binary
    pub vampire: PathBuf,
    /// The various KBs associated with the system
    pub kbs: Vec<KB>,
    /// Default options for the native saturation prover (from a
    /// `<prover type="native">` section).
    pub native_prover: NativeProverConfig,
    /// Default options for the external (subprocess) prover (from a
    /// `<prover type="external">` section).
    pub external_prover: ExternalProverConfig,
    /// Top-level `<preference name=".." value="..">` entries [`parse_config_xml_lenient`](Self::parse_config_xml_lenient)
    /// read but that don't map to any known field above — e.g. a legacy or
    /// third-party key. Round-tripped verbatim by [`to_config_xml`](Self::to_config_xml)
    /// so `sumo config --<setting> ...` (a full regenerate) doesn't silently
    /// drop preferences this build doesn't recognize.
    pub unknown_preferences: HashMap<String, String>,
    /// The currently selected KB
    #[serde(skip)]
    selected_kb: Option<usize>
}

impl Default for KBManager {
    fn default() -> Self {
        Self {
            base_dir:           PathBuf::new(),
            cache:              false,
            default_backend:    "native".into(),
            disable_selection:  false,
            edit_dir:           PathBuf::new(),
            elevate_warnings:   ElevateWarnings::None,
            eprover:            PathBuf::new(),
            graphviz_dir:       PathBuf::new(),
            holds_prefix:       false,
            inference_test_dir: PathBuf::new(),
            kb_dir:             PathBuf::new(),
            language:           "EnglishLanguage".into(),
            leo_executable:     PathBuf::new(),
            limit:              64,
            log_dir:            PathBuf::new(),
            log_level:          LevelFilter::Warn,
            ollama_host:        String::new(),
            proof:              "kif".into(),
            prose:              false,
            show_kif:           true,
            sumokbname:         String::new(),
            systems_dir:        PathBuf::new(),
            thoroughness:       1.0,
            tptp:               false,
            tptp_lang:          "fof".into(),
            real_numbers:       None,
            vampire:            PathBuf::new(),
            kbs:                Vec::new(),
            native_prover:      NativeProverConfig::default(),
            external_prover:    ExternalProverConfig::default(),
            unknown_preferences: HashMap::new(),
            selected_kb:        None
        }
    }
}

/// Default options for the native saturation prover — the serde-able config
/// subset of [`NativeOpts`](sigmakee_rs_core::NativeOpts) (its runtime fields
/// `session` / `cancel` are excluded).  Field names map to the `<prover
/// type="native">` `<preference>` keys in camelCase (`maxSteps`, `timeLimitSecs`,
/// `forwardClose`, `wantProof`, …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NativeProverConfig {
    pub max_steps: usize,
    pub max_lits: usize,
    pub time_limit_secs: u64,
    pub forward_close: bool,
    pub profile: bool,
    pub want_proof: bool,
    pub step: bool,
    /// SInE axiom-selection tuning (nested — supply as a JSON object value).
    pub selection: SineParams,
    /// Search-shaping genome (nested — supply as a JSON object value).  Present
    /// only when the `native-prover` feature is enabled.
    #[cfg(feature = "native-prover")]
    pub strategy: Strategy,
}

impl Default for NativeProverConfig {
    fn default() -> Self {
        // Mirrors `NativeOpts::default()`.
        Self {
            max_steps:       4000,
            max_lits:        8,
            time_limit_secs: 30,
            forward_close:   true,
            profile:         false,
            want_proof:      false,
            step:            false,
            selection:       SineParams::default(),
            #[cfg(feature = "native-prover")]
            strategy:        Strategy::default(),
        }
    }
}

/// Build a runtime [`NativeOpts`](sigmakee_rs_core::NativeOpts) seeded with the
/// configured defaults.  The per-query fields (`session`, `cancel`) are left at
/// their defaults for the caller to set.
#[cfg(feature = "native-prover")]
impl NativeProverConfig {
    pub fn to_native_opts(&self) -> sigmakee_rs_core::NativeOpts {
        sigmakee_rs_core::NativeOpts {
            selection:       self.selection,
            max_steps:       self.max_steps,
            max_lits:        self.max_lits,
            time_limit_secs: self.time_limit_secs,
            forward_close:   self.forward_close,
            profile:         self.profile,
            want_proof:      self.want_proof,
            strategy:        self.strategy.clone(),
            step:            self.step,
            ..Default::default()
        }
    }
}

/// Default options for the external (subprocess) prover — the serde-able config
/// subset of [`ProverOpts`](sigmakee_rs_core::ProverOpts) (the per-query
/// `mode` is excluded).  From a `<prover type="external">` section.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExternalProverConfig {
    pub timeout_secs: u64,
    pub tptp_lang:    String,
    pub selection:    SineParams
}

/// Build a runtime [`ProverOpts`](sigmakee_rs_core::prover::ProverOpts) seeded
/// with the configured timeout.  `mode` is left at its default (`Prove`).
#[cfg(feature = "ask")]
impl ExternalProverConfig {
    pub fn to_prover_opts(&self) -> sigmakee_rs_core::prover::ExternalOpts {
        sigmakee_rs_core::prover::ExternalOpts {
            timeout_secs: self.timeout_secs,
            selection:    self.selection,
            mode:    match self.tptp_lang.as_str() {
                "fof" => TptpLang::Fof,
                "tff" => TptpLang::Tff,
                "cnf" => TptpLang::Cnf,
                // Higher-order rides the `hol` flag; `mode` is inert then.
                "thf" => TptpLang::Fof,
                _     => TptpLang::Auto
            },
            hol: self.tptp_lang.eq_ignore_ascii_case("thf"),
            session: None
        }
    }
}

/// Bridge from a [`KBManager`]'s configuration to a proving layer's
/// consolidated options ([`ProvingLayer::Opts`](sigmakee_rs_core::ProvingLayer::Opts)).
///
/// A layer-generic command (`run_ask` / `run_test`) is generic over the proving
/// layer `L`, so it can't name `NativeOpts` vs `ExternalOpts` directly.  This
/// trait — implemented for both — lets it write `L::Opts::from_manager(&manager)`
/// and thread the configured prover settings (selection / timeout / proof
/// dialect / `--full-kb`) without matching on the backend.
pub trait ProverOptsFor: Sized {
    /// Build this options struct from the manager's configured prover defaults.
    fn from_manager(manager: &KBManager) -> Self;

    /// Whether a run with these options retains a proof transcript when the
    /// prover finds one.  External provers always emit their proof into the
    /// output we parse; the native prover only records one when `want_proof`
    /// is set.  Lets the CLI distinguish "no proof was recorded" from "no
    /// proof exists" when deciding what to print for an empty `proof_kif`.
    fn records_proof(&self) -> bool { true }
}

#[cfg(feature = "native-prover")]
impl ProverOptsFor for sigmakee_rs_core::NativeOpts {
    fn from_manager(manager: &KBManager) -> Self { manager.native_opts() }
    fn records_proof(&self) -> bool { self.want_proof }
}

#[cfg(feature = "ask")]
impl ProverOptsFor for sigmakee_rs_core::prover::ExternalOpts {
    fn from_manager(manager: &KBManager) -> Self { manager.external_prover().to_prover_opts() }
}

impl KBManager {
    /// The configured native-prover defaults (`<prover type="native">`).
    /// Call [`NativeProverConfig::to_native_opts`] for a runtime `NativeOpts`.
    pub fn native_prover(&self) -> &NativeProverConfig { &self.native_prover }

    /// The configured external-prover defaults (`<prover type="external">`).
    /// Call [`ExternalProverConfig::to_prover_opts`] for a runtime `ProverOpts`.
    pub fn external_prover(&self) -> &ExternalProverConfig { &self.external_prover }

    /// Build runtime native-prover opts from the configured defaults, folding
    /// in the top-level [`disable_selection`](Self::disable_selection) toggle.
    ///
    /// `disable_selection` reuses the prover's existing whole-KB mechanism —
    /// it forces [`SineParams::select_all`](sigmakee_rs_core::SineParams), so a
    /// `true` here feeds the engine every axiom (no SInE preselection) without
    /// any new core field.  Prefer this over
    /// [`native_prover()`](Self::native_prover)`.to_native_opts()` when you want
    /// the manager's `--full-kb` setting honored.
    #[cfg(feature = "native-prover")]
    pub fn native_opts(&self) -> sigmakee_rs_core::NativeOpts {
        let mut o = self.native_prover.to_native_opts();
        o.selection.select_all |= self.disable_selection;
        o
    }

    /// Resolve the configured Vampire binary to an existing path, or `Err`.
    /// See [`resolve_executable`] for the resolution order.
    pub fn resolve_vampire(&self) -> SdkResult<PathBuf> {
        resolve_executable(&self.vampire, "vampire", &self.systems_dir)
            .ok_or_else(|| SdkError::Config(missing_binary_msg("vampire", &self.vampire, &self.systems_dir)))
    }

    /// Resolve the configured E (`eprover`) binary to an existing path, or `Err`.
    /// See [`resolve_executable`] for the resolution order.
    pub fn resolve_eprover(&self) -> SdkResult<PathBuf> {
        resolve_executable(&self.eprover, "eprover", &self.systems_dir)
            .ok_or_else(|| SdkError::Config(missing_binary_msg("eprover", &self.eprover, &self.systems_dir)))
    }

    /// Validate that the configured prover binaries exist: both `vampire` and
    /// `eprover` must resolve (absolute paths must exist; relative or unset
    /// names are looked up under [`systems_dir`](Self::systems_dir) first, then
    /// on `$PATH`).  Both are checked so a single call reports every problem.
    ///
    /// Not run by [`from_config_xml`](Self::from_config_xml) /
    /// [`validate`](Self::validate): a config may legitimately name binaries
    /// that aren't installed where the config is *parsed*.  Call this at the
    /// point you're about to actually invoke a prover.
    pub fn validate_prover_paths(&self) -> SdkResult<()> {
        let mut errs = Vec::new();
        if let Err(e) = self.resolve_vampire() { errs.push(e.to_string()); }
        if let Err(e) = self.resolve_eprover() { errs.push(e.to_string()); }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(SdkError::Config(errs.join("; ")))
        }
    }
}

impl KBManager {
    /// Build a [`KBManager`] from the contents of a SUMO `config.xml`.
    ///
    /// Recognized `<preference name=".." value="..">` keys populate the matching
    /// field (case-sensitive on the legacy camelCase name); **unknown keys are
    /// silently ignored**, and keys absent from the XML keep their
    /// [`Default`] value.  Each `<kb name="..">` becomes a [`KB`], its
    /// `<constituent filename="..">` children becoming `Source::Local`
    /// constituents, each resolved against [`base_dir`](Self) / [`kb_dir`](Self)
    /// (see `resolve_constituent`): an absolute filename is used as-is; a
    /// relative one is joined onto `kb_dir` (itself joined onto `base_dir` when
    /// `kb_dir` is relative).
    ///
    /// Returns a fatal [`SdkError::Config`] when the parsed configuration fails
    /// [`validate`](Self::validate) — `sumokbname` is required and must name one
    /// of the parsed `<kb>`s.
    pub fn from_config_xml(xml: &str) -> SdkResult<Self> {
        let mut m = Self::parse_config_xml_lenient(xml)?;
        m.validate()?;
        let default_kb = m.sumokbname.clone();
        m.set_current_kb(&default_kb);
        Ok(m)
    }

    /// Like [`from_config_xml`](Self::from_config_xml), but skips
    /// [`validate`](Self::validate) and KB selection — for editing tools
    /// (`sumo config`) that must load a possibly-incomplete config.xml (e.g.
    /// no `sumokbname` set yet) without failing outright.
    pub fn parse_config_xml_lenient(xml: &str) -> SdkResult<Self> {
        let (prefs, kbs, provers, errors) = parse_config_xml(xml)?;

        let mut m = KBManager::default();
        let get = |k: &str| prefs.get(k).map(String::as_str);
        use pref_keys::*;

        if let Some(v) = get(BASE_DIR)           { m.base_dir = PathBuf::from(v); }
        if let Some(v) = get(CACHE)              { m.cache = parse_bool(v); }
        if let Some(v) = get(DEFAULT_BACKEND)    { m.default_backend = v.to_string(); }
        if let Some(v) = get(DISABLE_SELECTION)  { m.disable_selection = parse_bool(v); }
        if let Some(v) = get(EDIT_DIR)           { m.edit_dir = PathBuf::from(v); }
        if let Some(v) = get(EPROVER)            { m.eprover = PathBuf::from(v); }
        if let Some(v) = get(GRAPHVIZ_DIR)       { m.graphviz_dir = PathBuf::from(v); }
        if let Some(v) = get(HOLDS_PREFIX)       { m.holds_prefix = parse_bool(v); }
        if let Some(v) = get(INFERENCE_TEST_DIR) { m.inference_test_dir = PathBuf::from(v); }
        if let Some(v) = get(KB_DIR)             { m.kb_dir = PathBuf::from(v); }
        if let Some(v) = get(LANGUAGE)           { m.language = v.to_string(); }
        if let Some(v) = get(LEO_EXECUTABLE)     { m.leo_executable = PathBuf::from(v); }
        if let Some(v) = get(LIMIT)              { if let Ok(n) = v.parse() { m.limit = n; } }
        if let Some(v) = get(LOG_DIR)            { m.log_dir = PathBuf::from(v); }
        if let Some(v) = get(LOG_LEVEL)          { m.log_level = parse_severity(v); }
        if let Some(v) = get(OLLAMA_HOST)        { m.ollama_host = v.to_string(); }
        if let Some(v) = get(PROOF)              { m.proof = v.to_string(); }
        if let Some(v) = get(PROSE)              { m.prose = parse_bool(v); }
        if let Some(v) = get(REAL_NUMBERS)       { m.real_numbers = Some(parse_bool(v)); }
        if let Some(v) = get(SHOW_KIF)           { m.show_kif = parse_bool(v); }
        if let Some(v) = get(SUMOKBNAME)         { m.sumokbname = v.to_string(); }
        if let Some(v) = get(SYSTEMS_DIR)        { m.systems_dir = PathBuf::from(v); }
        if let Some(v) = get(THOROUGHNESS)       { if let Ok(f) = v.parse() { m.thoroughness = f; } }
        if let Some(v) = get(TPTP)               { m.tptp = parse_bool(v); }
        if let Some(v) = get(TPTP_LANG)          { m.tptp_lang = v.to_string(); }
        if let Some(v) = get(VAMPIRE)            { m.vampire = PathBuf::from(v); }
        m.kbs = kbs;

        // Classify constituents now that baseDir/kbDir are known: clean
        // root-relative names stay `Named` (re-rootable by kbDir/--git at load
        // time); absolute or `..`-bearing paths are PINNED — frozen to their
        // local form here and never swapped (see `Constituent` / `is_named`).
        let (base, kbd) = (m.base_dir.clone(), m.kb_dir.clone());
        for kb in &mut m.kbs {
            for c in &mut kb.constituents {
                if let Constituent::Named(p) = c {
                    if !is_named(p) {
                        *c = Constituent::Source(Source::Local(vec![resolve_constituent(p, &base, &kbd)]));
                    }
                }
            }
        }

        // Warning-elevation policy: `<error code=.../>` elements plus a
        // `<preference name="error" value="all">` both feed the token list;
        // `from_tokens` resolves "all" → All, codes → Codes, empty → None.
        let mut warn_tokens = errors;
        if let Some(v) = get(ERROR) { warn_tokens.push(v.to_string()); }
        m.elevate_warnings = ElevateWarnings::from_tokens(warn_tokens);

        // `<prover type="native|external">` sections.  Unknown types are ignored.
        for (kind, pp) in &provers {
            match kind.as_str() {
                "native"   => m.native_prover   = prover_config_from_prefs(pp)?,
                "external" => m.external_prover = prover_config_from_prefs(pp)?,
                _          => {}
            }
        }

        // Any top-level preference not consumed by a `get(..)` call above
        // (or by "error", handled separately just above) is preserved
        // verbatim so `to_config_xml` can round-trip it — see
        // `unknown_preferences`'s doc comment.
        m.unknown_preferences = prefs.into_iter()
            .filter(|(k, _)| !KNOWN_PREFERENCES.contains(&k.as_str()))
            .collect();

        Ok(m)
    }

    /// Read a `config.xml` from disk and parse it (see [`from_config_xml`](Self::from_config_xml)).
    pub fn from_config_xml_path(path: impl AsRef<Path>) -> SdkResult<Self> {
        let path = path.as_ref();
        let xml = std::fs::read_to_string(path)
            .map_err(|source| SdkError::Io { path: path.to_path_buf(), source })?;
        Self::from_config_xml(&xml)
    }

    /// Read a `config.xml` from disk with [`parse_config_xml_lenient`](Self::parse_config_xml_lenient)
    /// (no `validate()`) — for editing tools.
    pub fn from_config_xml_path_lenient(path: impl AsRef<Path>) -> SdkResult<Self> {
        let path = path.as_ref();
        let xml = std::fs::read_to_string(path)
            .map_err(|source| SdkError::Io { path: path.to_path_buf(), source })?;
        Self::parse_config_xml_lenient(&xml)
    }

    /// Enforce the configuration's required invariants and normalize paths:
    ///
    /// 1. Resolve [`edit_dir`](Self) (where the LMDB store lives): a relative
    ///    path hangs off [`base_dir`](Self); an unspecified or non-existent
    ///    directory falls back to the current working directory.
    /// 2. [`sumokbname`](Self) (the default KB) must be set, and
    /// 3. it must name one of the [`KB`]s in [`kbs`](Self).
    ///
    /// [`from_config_xml`](Self::from_config_xml) calls this and propagates a
    /// fatal [`SdkError::Config`] on failure; consumers building a `KBManager`
    /// another way (e.g. deserializing from JSON) can call it explicitly.
    pub fn validate(&mut self) -> SdkResult<()> {
        // (1) Resolve the DB directory.
        if self.edit_dir.is_relative() && !self.edit_dir.as_os_str().is_empty() {
            self.edit_dir = self.base_dir.join(&self.edit_dir);
        }
        if self.edit_dir.as_os_str().is_empty() || !self.edit_dir.is_dir() {
            if let Ok(cwd) = std::env::current_dir() {
                self.edit_dir = cwd;
            }
        }

        if self.sumokbname.trim().is_empty() {
            return Err(SdkError::Config(
                "config: `sumokbname` (the default KB) is required".into()));
        }
        if !self.kbs.iter().any(|kb| kb.name == self.sumokbname) {
            let defined: Vec<&str> = self.kbs.iter().map(|kb| kb.name.as_str()).collect();
            return Err(SdkError::Config(format!(
                "config: `sumokbname` = \"{}\" does not match any defined <kb> (defined: [{}])",
                self.sumokbname, defined.join(", "))));
        }

        // (4) Every *local* constituent of the active KB must exist on disk.
        // Skipped when no KB is selected yet (e.g. `from_config_xml` validates
        // before selecting).  Resolved locally (no `--git`); a `--git` run
        // re-roots `Named` constituents to remote `Source::Git`, which this
        // local check skips.
        if let Some(kb) = self.current_kb() {
            for src in kb.resolve(&self.base_dir, &self.kb_dir, None, None) {
                if let Source::Local(paths) = src {
                    for p in &paths {
                        if !p.exists() {
                            return Err(SdkError::Config(format!(
                                "constituent file not found: `{}`", p.display())));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Select a KB to use for this manager instance
    pub fn set_current_kb(&mut self, kb: &str) {
        self.selected_kb = self.kbs
            .iter()
            .enumerate()
            .find_map(|(i, v)| 
                if v.name == kb { Some(i) } else { None });
    }

    /// Select a KB to use for this manager instance
    pub fn current_kb(&self) -> Option<&KB> {
        self.kbs.get(self.selected_kb?)
    }

    /// Drop the selected KB's config-declared constituents, keeping the KB
    /// itself selected (so [`db_path`](Self::db_path) and the like still see
    /// its name) but excluding its ontology files from
    /// [`current_sources_owned`](Self::current_sources_owned). No-op when no
    /// KB is selected.
    ///
    /// Consumers that read config.xml for its *preferences* without opting
    /// into loading the whole configured ontology (e.g. the CLI without
    /// `-c`) call this before [`add_cli_sources`](Self::add_cli_sources), so
    /// only explicitly-supplied `-f`/`-d`/`--git` sources end up in the
    /// ingest list.
    pub fn clear_kb_constituents(&mut self) {
        if let Some(idx) = self.selected_kb {
            self.kbs[idx].constituents.clear();
        }
    }

    /// Filesystem path of the selected KB's LMDB store: `<edit_dir>/<kb
    /// name>.lmdb` (the KB name is used verbatim — case-sensitive).  `None` when
    /// no KB is selected.  Resolve [`edit_dir`](Self) first via
    /// [`validate`](Self::validate).
    pub fn db_path(&self) -> Option<PathBuf> {
        let name = &self.current_kb()?.name;
        Some(self.edit_dir.join(format!("{name}.lmdb")))
    }

    /// Augment the selected KB's constituents with CLI-supplied sources so a
    /// command-line `-f`/`-d`/`--git` extends (or, in practice, overrides) what
    /// the config declared.  With `git` set, all `-f`/`-d` paths become the
    /// in-repo paths of a single [`Source::Git`]; otherwise they become one
    /// [`Source::Local`].  No-op when no KB is selected.
    pub fn add_cli_sources(&mut self, files: Vec<PathBuf>, dirs: Vec<PathBuf>, git: Option<String>)
        -> SdkResult<()>
    {
        let Some(idx) = self.selected_kb else { return Ok(()) };
        let mut paths = files;
        paths.extend(dirs);
        match git {
            // Under `--git`, clean-relative CLI paths are in-repo names → `Named`
            // (they join the KB's git swap in `resolve`); absolute / `..` paths
            // stay pinned-local (omitted from the swap), and aren't disk-checked.
            Some(_) => {
                for p in paths {
                    if is_named(&p) {
                        self.kbs[idx].constituents.push(Constituent::Named(p));
                    } else {
                        self.kbs[idx].constituents.push(Constituent::Source(Source::Local(vec![p])));
                    }
                }
            }
            None => {
                // Local: every supplied `-f`/`-d` path must exist on disk, and
                // is pinned as a CWD-relative local source (not kbDir-cascaded).
                for p in &paths {
                    if !p.exists() {
                        return Err(SdkError::Config(format!(
                            "source path not found: `{}`", p.display())));
                    }
                }
                if !paths.is_empty() {
                    self.kbs[idx].constituents.push(Constituent::Source(Source::Local(paths)));
                }
            }
        }
        Ok(())
    }

    /// Persistently create an empty KB named `kb_name` — a no-op if it
    /// already exists. Decoupled from [`add_constituents_to_kb`](Self::add_constituents_to_kb)
    /// (which also creates on demand, but requires at least one file) so a
    /// caller — e.g. the `sumo config` TUI's "new KB" action — can create a
    /// KB before it has any constituents. Adopts `kb_name` as
    /// [`sumokbname`](Self) when this is the first KB on an otherwise-empty
    /// manager, same policy as `add_constituents_to_kb`.
    pub fn create_kb(&mut self, kb_name: &str) {
        if self.kbs.iter().any(|k| k.name == kb_name) {
            return;
        }
        self.kbs.push(KB { name: kb_name.to_string(), constituents: Vec::new() });
        if self.sumokbname.trim().is_empty() {
            self.sumokbname = kb_name.to_string();
        }
    }

    /// Persistently add `files`/`dirs` as constituents of the KB named
    /// `kb_name`, creating it (empty) first if it doesn't already exist —
    /// the mutation behind `sumo config --kb NAME -f FILE -d DIR`. Unlike
    /// [`add_cli_sources`](Self::add_cli_sources) (transient, in-memory-only,
    /// always pins), each path is classified via [`is_named`]: a
    /// clean-relative path that actually resolves under [`kb_dir`](Self) is
    /// stored as [`Constituent::Named`] (portable — re-rooted at load time);
    /// anything else (absolute, `..`-bearing, or relative-but-not-under-kbDir)
    /// is canonicalized and pinned as [`Constituent::Source`]. Additive: a
    /// path already present (by resolved identity) is skipped, not
    /// duplicated. If this is the first KB ever added to an otherwise-empty
    /// manager, it's also adopted as [`sumokbname`](Self) so the result isn't
    /// left in the "no default KB" state — an already-set `sumokbname` is
    /// never overridden.
    ///
    /// `verify_exists = false` (`sumo config --kb NAME --declare -f ...`)
    /// skips every existence check and classifies by path shape alone
    /// (clean-relative → `Named`, else pinned as given, uncanonicalized) —
    /// for declaring constituents ahead of actually fetching them (e.g. an
    /// installer seeding a starter KB before the ontology is cloned).
    pub fn add_constituents_to_kb(
        &mut self, kb_name: &str, files: Vec<PathBuf>, dirs: Vec<PathBuf>, verify_exists: bool,
    ) -> SdkResult<()> {
        // Classify (and existence-check) every path FIRST, before mutating
        // anything — a rejected path must leave the manager untouched (no
        // half-created KB), matching `add_cli_sources`'s all-or-nothing shape.
        let (base, kbd) = (self.base_dir.clone(), self.kb_dir.clone());
        let mut candidates = Vec::with_capacity(files.len() + dirs.len());
        for p in files.into_iter().chain(dirs) {
            if !verify_exists {
                candidates.push(if is_named(&p) {
                    Constituent::Named(p)
                } else {
                    Constituent::Source(Source::Local(vec![p]))
                });
                continue;
            }
            // A clean-relative path that resolves under kbDir is portable —
            // store it as `Named` so it re-roots at load time. Otherwise fall
            // back to existence as given (CWD-relative or absolute) and pin
            // it to that resolved, canonical location.
            if is_named(&p) && resolve_constituent(&p, &base, &kbd).exists() {
                candidates.push(Constituent::Named(p));
            } else if p.exists() {
                candidates.push(Constituent::Source(Source::Local(vec![p.canonicalize().unwrap_or(p)])));
            } else {
                return Err(SdkError::Config(format!(
                    "source path not found: `{}` (checked as given, and under kbDir `{}`)",
                    p.display(), kbd.display())));
            }
        }

        self.create_kb(kb_name);
        let idx = self.kbs.iter().position(|k| k.name == kb_name).expect("just created");

        for candidate in candidates {
            let already_present = self.kbs[idx].constituents.iter()
                .any(|c| constituent_path(c) == constituent_path(&candidate));
            if !already_present {
                self.kbs[idx].constituents.push(candidate);
            }
        }
        Ok(())
    }

    /// Persistently remove constituents of the KB named `kb_name` whose
    /// stored path exactly matches one of `paths` — the mutation behind
    /// `sumo config --kb NAME --exclude PATH`. Matches the path as actually
    /// stored (a `Named` constituent's relative name, or a pinned
    /// constituent's canonicalized absolute form); pass the same value shown
    /// in `sumo config`'s "Knowledge bases" listing. Returns how many
    /// constituents were removed (0 if none matched). `Err` only when
    /// `kb_name` itself doesn't exist.
    pub fn remove_constituents_from_kb(&mut self, kb_name: &str, paths: Vec<PathBuf>) -> SdkResult<usize> {
        let idx = self.kbs.iter().position(|k| k.name == kb_name)
            .ok_or_else(|| SdkError::Config(format!("no such KB: `{kb_name}`")))?;
        let before = self.kbs[idx].constituents.len();
        self.kbs[idx].constituents.retain(|c| {
            match constituent_path(c) {
                Some(p) => !paths.iter().any(|rp| rp == p),
                None => true,
            }
        });
        Ok(before - self.kbs[idx].constituents.len())
    }
}

/// The single filesystem path a constituent's identity boils down to, for
/// dedup/removal comparisons — `None` for a multi-path or non-local pinned
/// source (can't happen for anything [`add_constituents_to_kb`](KBManager::add_constituents_to_kb)
/// itself produces, but existing config.xml-parsed constituents are always
/// single-path anyway).
fn constituent_path(c: &Constituent) -> Option<&Path> {
    match c {
        Constituent::Named(p) => Some(p),
        Constituent::Source(Source::Local(paths)) if paths.len() == 1 => Some(&paths[0]),
        _ => None,
    }
}

/// A KB constituent — one member file, modeled so its *identity* (a name) is
/// separate from its *resolution* (where it's actually fetched).  That split is
/// what lets `--git` (or a different `kbDir`) re-root the whole KB at load time.
#[derive(Debug, Serialize, Deserialize)]
pub enum Constituent {
    /// A clean root-relative name — relative, with no `..` parent offset
    /// (`Merge.kif`, `development/Muscles.kif`).  Re-rooted at load time: under
    /// `kbDir` locally (the dynamic cascade), or the repo under `--git`.
    Named(PathBuf),
    /// A source pinned to its declared location — an absolute config path, or a
    /// relative one with a `..` parent offset (frozen to its local form at parse
    /// time), or any non-file source.  **Never** swapped by `--git`/`kbDir`;
    /// always local.
    Source(Source),
}

/// Is `p` a clean root-relative name — relative AND free of any `..` parent
/// offset?  Only these are re-rootable ([`Constituent::Named`]); absolute paths
/// and `..`-bearing paths are pinned local.
fn is_named(p: &Path) -> bool {
    p.is_relative()
        && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

/// An individual KnowledgeBase, comprised of its member
/// constituent files; these files contain the axioms which
/// form the KB.
#[derive(Debug, Serialize, Deserialize)]
pub struct KB {
    name: String,
    constituents: Vec<Constituent>,
}

impl KB {
    /// This KB's name (the `<kb name="…">` from config.xml).
    pub fn name(&self) -> &str { &self.name }

    /// This KB's declared constituents, in order.
    pub fn constituents(&self) -> &[Constituent] { &self.constituents }

    /// Resolve the constituents into concrete [`Source`]s for ingestion.
    ///
    /// * Without `git`: each [`Named`](Constituent::Named) is resolved against
    ///   `base_dir`/`kb_dir` via the dynamic cascade (`resolve_constituent`);
    ///   pinned [`Source`](Constituent::Source) constituents pass through.
    /// * With `git`: every `Named` collapses into a single [`Source::Git`]
    ///   (their names as in-repo paths); pinned `Source` constituents still pass
    ///   through verbatim — absolute / `..` paths are *omitted from the swap*.
    ///   `branch` is only meaningful here: `None` defers to the remote's own
    ///   default branch, `Some(name)` pins it (see [`Source::Git`]).
    pub fn resolve(&self, base_dir: &Path, kb_dir: &Path, git: Option<&str>, branch: Option<&str>) -> Vec<Source> {
        #[cfg(feature = "git")]
        if let Some(uri) = git {
            let mut named: Vec<PathBuf> = Vec::new();
            let mut out:   Vec<Source>  = Vec::new();
            for c in &self.constituents {
                match c {
                    Constituent::Named(name) => named.push(name.clone()),
                    Constituent::Source(s)   => out.extend(s.try_clone()),
                }
            }
            if !named.is_empty() {
                out.insert(0, Source::Git {
                    uri: uri.to_string(), paths: named, branch: branch.map(str::to_string),
                });
            }
            return out;
        }
        let _ = git; // unused when the `git` feature is off
        let _ = branch;
        self.constituents
            .iter()
            .filter_map(|c| match c {
                Constituent::Named(name) => Some(Source::Local(vec![resolve_constituent(name, base_dir, kb_dir)])),
                Constituent::Source(s)   => s.try_clone(),
            })
            .collect()
    }
}

// `Source` has no `PartialEq` (it can hold a reader), so derive it for `KB`
// (and thus `KBManager`) structurally over the comparable fields only.
impl PartialEq for KB {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.constituents.len() == other.constituents.len()
    }
}

/// Policy for elevating semantic warnings to hard errors — the config-side home
/// of the CLI's `-W` flag.  Serializes as `"none"`, `"all"`, or
/// `{ "codes": [..] }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElevateWarnings {
    /// Elevate nothing (default): warnings stay warnings.
    None,
    /// Elevate every warning to an error (`-W all` /
    /// `<preference name="error" value="all">`).
    All,
    /// Elevate only these warning codes/names (`-W E005` /
    /// `<error code="E005"/>`).
    Codes(Vec<String>),
}

impl Default for ElevateWarnings {
    fn default() -> Self { ElevateWarnings::None }
}

impl ElevateWarnings {
    /// Build a policy from a raw `-W` / `<error>` token list: any `all` token
    /// (case-insensitive) wins as [`All`](Self::All); an empty list is
    /// [`None`](Self::None); otherwise the tokens are [`Codes`](Self::Codes).
    /// This is the single normalizer both the config parser and a future CLI
    /// `-W` consumer feed their raw tokens through.
    pub fn from_tokens(tokens: Vec<String>) -> Self {
        if tokens.iter().any(|t| t.eq_ignore_ascii_case("all")) {
            Self::All
        } else if tokens.is_empty() {
            Self::None
        } else {
            Self::Codes(tokens)
        }
    }

    /// Is warning `code` elevated to an error under this policy?
    pub fn elevates(&self, code: &str) -> bool {
        match self {
            Self::None      => false,
            Self::All       => true,
            Self::Codes(cs) => cs.iter().any(|c| c.eq_ignore_ascii_case(code)),
        }
    }
}

// -- config.xml parsing ------------------------------------------------------

/// The literal `<preference name="..">` key for every top-level [`KBManager`]
/// field — the single source of truth both [`parse_config_xml_lenient`](KBManager::parse_config_xml_lenient)
/// (`get(pref_keys::X)`) and `write.rs`'s writer (`pref(w, pref_keys::X,
/// ..)`) read from, so renaming a key only means changing it here.
/// [`KNOWN_PREFERENCES`] is generated from this same list.
pub(crate) mod pref_keys {
    pub const BASE_DIR:            &str = "baseDir";
    pub const CACHE:               &str = "cache";
    pub const DEFAULT_BACKEND:     &str = "defaultBackend";
    pub const DISABLE_SELECTION:   &str = "disableSelection";
    pub const EDIT_DIR:            &str = "editDir";
    pub const EPROVER:             &str = "eproverExec";
    pub const GRAPHVIZ_DIR:        &str = "graphDir";
    pub const HOLDS_PREFIX:        &str = "holdsPrefix";
    pub const INFERENCE_TEST_DIR:  &str = "inferenceTestDir";
    pub const KB_DIR:              &str = "kbDir";
    pub const LANGUAGE:            &str = "language";
    pub const LEO_EXECUTABLE:      &str = "leoExec";
    pub const LIMIT:               &str = "limit";
    pub const LOG_DIR:             &str = "logDir";
    pub const LOG_LEVEL:           &str = "logLevel";
    pub const OLLAMA_HOST:         &str = "ollamaHost";
    pub const PROOF:               &str = "proof";
    pub const PROSE:               &str = "prose";
    pub const REAL_NUMBERS:        &str = "realNumbers";
    pub const SHOW_KIF:            &str = "showKif";
    pub const SUMOKBNAME:          &str = "sumokbname";
    pub const SYSTEMS_DIR:         &str = "systemsDir";
    pub const THOROUGHNESS:        &str = "thoroughness";
    pub const TPTP:                &str = "TPTP";
    pub const TPTP_LANG:           &str = "tptpLang";
    pub const VAMPIRE:             &str = "vampireExec";
    /// Warning-elevation policy (`-W all` / `ElevateWarnings::All`), not a
    /// `KBManager` field directly, but still a "known" top-level key.
    pub const ERROR:               &str = "error";

    /// Every key above, for the `unknown_preferences` classification —
    /// generated (not hand-duplicated) so it can't drift from the constants.
    pub const ALL: &[&str] = &[
        BASE_DIR, CACHE, DEFAULT_BACKEND, DISABLE_SELECTION, EDIT_DIR, EPROVER,
        GRAPHVIZ_DIR, HOLDS_PREFIX, INFERENCE_TEST_DIR, KB_DIR, LANGUAGE, LEO_EXECUTABLE,
        LIMIT, LOG_DIR, LOG_LEVEL, OLLAMA_HOST, PROOF, PROSE, REAL_NUMBERS, SHOW_KIF,
        SUMOKBNAME, SYSTEMS_DIR, THOROUGHNESS, TPTP, TPTP_LANG, VAMPIRE,
        ERROR,
    ];
}

/// Every top-level `<preference name="..">` key [`parse_config_xml_lenient`](KBManager::parse_config_xml_lenient)
/// maps to a known [`KBManager`] field (including `"error"`, consumed by the
/// warning-elevation policy). Anything else lands in
/// [`unknown_preferences`](KBManager::unknown_preferences) instead of being
/// silently dropped.
const KNOWN_PREFERENCES: &[&str] = pref_keys::ALL;

/// One `<prover type="..">` section: its `type` plus its `<preference>` map.
type ProverSection = (String, HashMap<String, String>);

/// Parse a `config.xml` into its top-level `<preference>` map, its `<kb>` list,
/// and its `<prover type="..">` sections.
fn parse_config_xml(xml: &str)
    -> SdkResult<(HashMap<String, String>, Vec<KB>, Vec<ProverSection>, Vec<String>)>
{
    let mut reader = Reader::from_str(xml);
    let mut prefs: HashMap<String, String> = HashMap::new();
    let mut kbs: Vec<KB> = Vec::new();
    let mut provers: Vec<ProverSection> = Vec::new();
    // `<error name=../code=..>` elements: warning codes to elevate to errors.
    let mut errors: Vec<String> = Vec::new();
    let mut cur_kb: Option<KB> = None;
    let mut cur_prover: Option<ProverSection> = None;
    let mut buf = Vec::new();

    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| {
            SdkError::Config(format!("config.xml parse error at byte {}: {e}", reader.buffer_position()))
        })?;
        match event {
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"preference" => {
                    if let (Some(name), Some(value)) = (attr(&e, b"name"), attr(&e, b"value")) {
                        // A preference inside a <prover> belongs to that prover;
                        // otherwise it is a top-level configuration preference.
                        match cur_prover.as_mut() {
                            Some((_, pp)) => { pp.insert(name, value); }
                            None          => { prefs.insert(name, value); }
                        }
                    }
                }
                b"kb" => {
                    cur_kb = Some(KB { name: attr(&e, b"name").unwrap_or_default(), constituents: Vec::new() });
                }
                b"constituent" => {
                    if let (Some(kb), Some(file)) = (cur_kb.as_mut(), attr(&e, b"filename")) {
                        // Provisionally a Named (root-relative) constituent; the
                        // post-pass in `from_config_xml` pins absolute / `..`
                        // ones to their frozen local form.
                        kb.constituents.push(Constituent::Named(PathBuf::from(file)));
                    }
                }
                b"prover" => {
                    cur_prover = Some((attr(&e, b"type").unwrap_or_default(), HashMap::new()));
                }
                b"error" => {
                    // `<error code="E005"/>` or `<error name="E005"/>` — a
                    // top-level warning code to elevate to a hard error.
                    if cur_prover.is_none() {
                        if let Some(code) = attr(&e, b"code").or_else(|| attr(&e, b"name")) {
                            errors.push(code);
                        }
                    }
                }
                _ => {} // `configuration` and anything else: ignored
            },
            Event::End(e) => match e.name().as_ref() {
                b"kb"     => { if let Some(kb) = cur_kb.take() { kbs.push(kb); } }
                b"prover" => { if let Some(p) = cur_prover.take() { provers.push(p); } }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok((prefs, kbs, provers, errors))
}

/// Build a prover-config struct from a `<prover>` section's preference map.
/// Each `name`/`value` becomes a JSON field (the value typed best-effort as
/// bool / integer / float / nested-JSON / string), then deserialized into `T`
/// (whose `rename_all = "camelCase"` matches the preference keys).  Unknown
/// keys are ignored; an empty section yields `T::default()`.
fn prover_config_from_prefs<T>(prefs: &HashMap<String, String>) -> SdkResult<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    if prefs.is_empty() {
        return Ok(T::default());
    }
    let mut obj = serde_json::Map::new();
    for (k, v) in prefs {
        insert_dotted(&mut obj, k, json_value_of(v));
    }
    serde_json::from_value(serde_json::Value::Object(obj))
        .map_err(|e| SdkError::Config(format!("invalid <prover> preference: {e}")))
}

/// Fold a possibly dot-separated preference name (`selection.tolerance`)
/// into `obj` as a nested object path. Plain names insert at the top level;
/// a dotted path merges into any object already present at its prefix (so
/// `selection.tolerance` and a legacy JSON-valued `selection` compose,
/// dotted leaves winning).
fn insert_dotted(obj: &mut serde_json::Map<String, serde_json::Value>, name: &str, value: serde_json::Value) {
    let mut parts = name.split('.');
    let first = parts.next().expect("split yields at least one part");
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        match (obj.get_mut(first), &value) {
            // A dotted leaf may already have created the nested object;
            // merge a legacy whole-object value under it, existing wins.
            (Some(serde_json::Value::Object(existing)), serde_json::Value::Object(incoming)) => {
                for (k, v) in incoming {
                    existing.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            _ => { obj.insert(first.to_string(), value); }
        }
        return;
    }
    let mut cur = obj
        .entry(first.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    for p in &rest[..rest.len() - 1] {
        if !cur.is_object() {
            *cur = serde_json::Value::Object(serde_json::Map::new());
        }
        cur = cur
            .as_object_mut()
            .expect("just ensured object")
            .entry(p.to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }
    if !cur.is_object() {
        *cur = serde_json::Value::Object(serde_json::Map::new());
    }
    cur.as_object_mut()
        .expect("just ensured object")
        .insert(rest.last().expect("rest is non-empty").to_string(), value);
}

/// Best-effort JSON typing of a preference value string, so it lands in the
/// right serde field type: SUMO/JSON booleans, integers, floats, nested JSON
/// (object/array — for legacy `selection` / `strategy` values), else a plain
/// string.
fn json_value_of(v: &str) -> serde_json::Value {
    let t = v.trim();
    match t.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on"  => return serde_json::Value::Bool(true),
        "false" | "no" | "off" => return serde_json::Value::Bool(false),
        _ => {}
    }
    if let Ok(i) = t.parse::<i64>() { return serde_json::Value::from(i); }
    if let Ok(f) = t.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) { return serde_json::Value::Number(n); }
    }
    if matches!(t.as_bytes().first(), Some(b'{') | Some(b'[')) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(t) { return val; }
    }
    serde_json::Value::String(v.to_string())
}

/// Look up attribute `key` on an element, returning its unescaped value.
fn attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned()))
}

/// Parse a SUMO boolean preference: `yes` / `true` / `1` (any case) are true.
fn parse_bool(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "yes" | "true" | "1" | "on")
}

/// Resolve a configured executable to an existing path:
///
/// * unset (empty) → fall back to `default_name` (the conventional binary name);
/// * absolute → it must exist as given;
/// * relative (including a bare name) → try `<systems_dir>/<path>` first (when
///   `systems_dir` is set), then search each `$PATH` entry.
///
/// Returns the first existing candidate, or `None` if nothing resolves.
fn resolve_executable(configured: &Path, default_name: &str, systems_dir: &Path) -> Option<PathBuf> {
    let target: &Path = if configured.as_os_str().is_empty() {
        Path::new(default_name)
    } else {
        configured
    };

    if target.is_absolute() {
        return target.is_file().then(|| target.to_path_buf());
    }

    // Relative: systemsDir first (when configured), then $PATH.
    if !systems_dir.as_os_str().is_empty() {
        let candidate = systems_dir.join(target);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(target))
        .find(|candidate| candidate.is_file())
}

/// Error text for a prover binary that [`resolve_executable`] couldn't find.
fn missing_binary_msg(name: &str, configured: &Path, systems_dir: &Path) -> String {
    let what = if configured.as_os_str().is_empty() {
        format!("`{name}` (unset)")
    } else {
        format!("`{}`", configured.display())
    };
    format!(
        "{name} binary not found: {what} is not an existing file, \
         nor under systemsDir `{}`, nor on $PATH",
        systems_dir.display()
    )
}

/// Resolve a KB constituent path against the configured base/kb dirs:
///
/// * `filename` absolute → `filename` (used as-is);
/// * `filename` relative, `kb_dir` absolute → `kb_dir/filename`;
/// * `filename` relative, `kb_dir` relative → `base_dir/kb_dir/filename`.
fn resolve_constituent(filename: &Path, base_dir: &Path, kb_dir: &Path) -> PathBuf {
    if filename.is_absolute() {
        filename.to_path_buf()
    } else if kb_dir.is_absolute() {
        kb_dir.join(filename)
    } else {
        base_dir.join(kb_dir).join(filename)
    }
}

/// Map a `logLevel` string to a [`Severity`] (defaults to `Warning`).  `debug`
/// and `trace` collapse to `Hint`, the lowest core severity.
fn parse_severity(v: &str) -> log::LevelFilter {
    match v.trim().to_ascii_lowercase().as_str() {
        "error"                   => log::LevelFilter::Error,
        "warning" | "warn"        => log::LevelFilter::Warn,
        "info"                    => log::LevelFilter::Info,
        "hint" | "debug"          => log::LevelFilter::Debug,
        "trace"                   => log::LevelFilter::Trace,
        "none"                    => log::LevelFilter::Off,
        _                         => log::LevelFilter::Warn,
    }
}

fn severity_str(s: LevelFilter) -> &'static str {
    match s {
        LevelFilter::Error   => "error",
        LevelFilter::Warn    => "warning",
        LevelFilter::Info    => "info",
        LevelFilter::Debug   => "debug",
        LevelFilter::Trace   => "trace",
        LevelFilter::Off     => "none",
    }
}

/// serde adapter for [`Severity`] (core type has no serde impl): a lowercase
/// string, matching the `config.xml` / log-level convention.
mod log_level_serde {
    use log::LevelFilter;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(s: &LevelFilter, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(super::severity_str(*s))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<log::LevelFilter, D::Error> {
        let s = String::deserialize(de)?;
        Ok(super::parse_severity(&s))
    }
}

impl Into<TptpOptions> for KBManager {
    fn into(self) -> TptpOptions {
        TptpOptions {
            lang: match self.tptp_lang.as_str() {
                "fof" => TptpLang::Fof,
                "tff" => TptpLang::Tff,
                "cnf" => TptpLang::Cnf,
                _ => TptpLang::Auto
            },
            excluded: HashSet::new(),
            show_kif_comment: self.show_kif,
            .. TptpOptions::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<configuration>
  <preference name="adminBrowserLimit" value="200"/>
  <preference name="baseDir" value="/home/u/.sigmakee"/>
  <preference name="cache" value="yes"/>
  <preference name="editDir" value="/home/u/sumo"/>
  <preference name="graphDir" value="/usr/bin"/>
  <preference name="holdsPrefix" value="no"/>
  <preference name="kbDir" value="/home/u/sumo"/>
  <preference name="logLevel" value="warning"/>
  <preference name="sumokbname" value="SUMO"/>
  <preference name="TPTP" value="yes"/>
  <preference name="vampireExec" value="/usr/local/bin/vampire"/>
  <preference name="ollamaHost" value="http://127.0.0.1:11434"/>
  <preference name="someUnknownOption" value="ignore me"/>
  <kb name="SUMO">
    <constituent filename="Merge.kif"/>
    <constituent filename="Mid-level-ontology.kif"/>
    <constituent filename="development/Muscles.kif"/>
  </kb>
</configuration>"#;

    #[test]
    fn maps_camelcase_preferences_to_snake_fields() {
        let m = KBManager::from_config_xml(SAMPLE).unwrap();
        assert_eq!(m.base_dir, PathBuf::from("/home/u/.sigmakee"));
        // `edit_dir` is resolved by `validate()` (the configured /home/u/sumo
        // doesn't exist here → CWD); see `validate_resolves_edit_dir`.
        assert_eq!(m.graphviz_dir, PathBuf::from("/usr/bin")); // graphDir
        assert_eq!(m.kb_dir, PathBuf::from("/home/u/sumo"));
        assert_eq!(m.sumokbname, "SUMO");
        assert_eq!(m.vampire, PathBuf::from("/usr/local/bin/vampire"));
        assert_eq!(m.ollama_host, "http://127.0.0.1:11434");
        assert_eq!(m.log_level, LevelFilter::Warn);
    }

    #[test]
    fn parses_sumo_style_booleans() {
        let m = KBManager::from_config_xml(SAMPLE).unwrap();
        assert!(m.cache, "cache=yes → true");
        assert!(!m.holds_prefix, "holdsPrefix=no → false");
        assert!(m.tptp, "TPTP=yes → true");
    }

    #[test]
    fn unknown_keys_ignored_and_missing_keys_default() {
        let m = KBManager::from_config_xml(SAMPLE).unwrap();
        // `eprover` is absent from the XML → its `Default` (empty path).
        assert_eq!(m.eprover, PathBuf::default());
        assert_eq!(m.leo_executable, PathBuf::default());
    }

    /// `KB`/`Constituent` compare shallow (see `impl PartialEq for KB` —
    /// only name + constituent *count*), so a round-trip check needs the
    /// full JSON structural comparison rather than `assert_eq!` on the
    /// `KBManager`s themselves to actually catch a constituent-path bug.
    fn assert_structurally_eq(a: &KBManager, b: &KBManager) {
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap(),
        );
    }

    #[test]
    fn to_config_xml_round_trips_the_sample_fixture() {
        let m = KBManager::parse_config_xml_lenient(SAMPLE).unwrap();
        let xml = m.to_config_xml();
        let reparsed = KBManager::parse_config_xml_lenient(&xml).unwrap();
        assert_structurally_eq(&m, &reparsed);
        // Spot-check the values a byte-for-byte diff would hide: the
        // regenerated form uses different formatting entirely.
        assert_eq!(reparsed.sumokbname, "SUMO");
        assert_eq!(reparsed.kbs[0].constituents().len(), 3);
    }

    #[test]
    fn prover_prefs_flatten_to_dotted_names() {
        let mut m = KBManager::parse_config_xml_lenient(SAMPLE).unwrap();
        m.native_prover.selection.tolerance = 4.25;
        let xml = m.to_config_xml();
        assert!(xml.contains(r#"<preference name="selection.tolerance" value="4.25"/>"#),
            "nested prover config flattens to dotted names:\n{xml}");
        assert!(!xml.contains("&quot;"),
            "no JSON-in-attribute values remain:\n{xml}");
        let reparsed = KBManager::parse_config_xml_lenient(&xml).unwrap();
        assert!((reparsed.native_prover.selection.tolerance - 4.25).abs() < 1e-6);
        assert_structurally_eq(&m, &reparsed);
    }

    #[test]
    fn legacy_json_valued_prover_prefs_still_parse() {
        // Pre-dotted config.xml files carried nested objects as compact JSON
        // in the value attribute; both forms must read, dotted leaves winning.
        let legacy = r#"<configuration>
  <preference name="sumokbname" value="SUMO"/>
  <kb name="SUMO"><constituent filename="Merge.kif"/></kb>
  <prover type="native">
    <preference name="selection" value="{&quot;tolerance&quot;:2.5,&quot;autoscale&quot;:true}"/>
    <preference name="selection.tolerance" value="9.0"/>
  </prover>
</configuration>"#;
        let m = KBManager::parse_config_xml_lenient(legacy).unwrap();
        assert!((m.native_prover.selection.tolerance - 9.0).abs() < 1e-6,
            "dotted leaf wins over the legacy JSON object");
        assert!(m.native_prover.selection.autoscale,
            "legacy JSON fields still land");
    }

    #[test]
    fn unknown_preferences_survive_a_regenerate() {
        // SAMPLE has `adminBrowserLimit` and `someUnknownOption` — neither
        // maps to a KBManager field, so they used to be silently dropped by
        // `to_config_xml`. They must now round-trip via `unknown_preferences`.
        let m = KBManager::parse_config_xml_lenient(SAMPLE).unwrap();
        assert_eq!(m.unknown_preferences.get("adminBrowserLimit").map(String::as_str), Some("200"));
        assert_eq!(m.unknown_preferences.get("someUnknownOption").map(String::as_str), Some("ignore me"));

        // A `sumo config --<setting> ...` regenerate must not lose them.
        let xml = m.to_config_xml();
        assert!(xml.contains(r#"<preference name="adminBrowserLimit" value="200"/>"#));
        assert!(xml.contains(r#"<preference name="someUnknownOption" value="ignore me"/>"#));
        let reparsed = KBManager::parse_config_xml_lenient(&xml).unwrap();
        assert_eq!(reparsed.unknown_preferences, m.unknown_preferences);

        // A known field is never miscategorized as unknown.
        assert!(!m.unknown_preferences.contains_key("sumokbname"));
        assert!(!m.unknown_preferences.contains_key("vampireExec"));
    }

    #[test]
    fn to_config_xml_after_apply_overrides_round_trips() {
        let mut m = KBManager::parse_config_xml_lenient(SAMPLE).unwrap();
        let opts = KBManager::options();
        let by = |id: &str| opts.iter().find(|o| o.field == id).unwrap();
        m.apply_overrides([
            (by("thoroughness"), serde_json::json!(0.5)),
            (by("backend"),      serde_json::json!("subprocess")),
        ]).unwrap();

        let xml = m.to_config_xml();
        let reparsed = KBManager::parse_config_xml_lenient(&xml).unwrap();
        assert_structurally_eq(&m, &reparsed);
        assert!((reparsed.thoroughness - 0.5).abs() < 1e-6);
        assert_eq!(reparsed.default_backend, "subprocess");
        // Untouched fields survive the regenerate unchanged.
        assert_eq!(reparsed.sumokbname, "SUMO");
        assert_eq!(reparsed.vampire, PathBuf::from("/usr/local/bin/vampire"));
    }

    #[test]
    fn severity_str_trace_round_trips() {
        // Regression: `severity_str` had a `"tracd"` typo that silently lost
        // Trace on a config.xml round trip.
        assert_eq!(severity_str(LevelFilter::Trace), "trace");
        assert_eq!(parse_severity("trace"), LevelFilter::Trace);
    }

    #[test]
    fn real_numbers_round_trips() {
        for value in [Some(true), Some(false), None] {
            let mut m = KBManager::parse_config_xml_lenient(SAMPLE).unwrap();
            m.real_numbers = value;
            let reparsed = KBManager::parse_config_xml_lenient(&m.to_config_xml()).unwrap();
            assert_eq!(reparsed.real_numbers, value, "real_numbers = {value:?}");
        }
    }

    fn kb_with(name: &str) -> KBManager {
        let mut m = KBManager::default();
        m.sumokbname = name.into();
        m.kbs.push(KB { name: name.into(), constituents: vec![] });
        m.set_current_kb(name);
        m
    }

    #[test]
    fn validate_resolves_edit_dir() {
        let tmp = std::env::temp_dir();
        let sub = tmp.join("sdk_editdir_resolve_test");
        std::fs::create_dir_all(&sub).unwrap();

        // Relative editDir under an existing baseDir → baseDir/editDir (kept).
        let mut m = kb_with("K");
        m.base_dir = tmp.clone();
        m.edit_dir = PathBuf::from("sdk_editdir_resolve_test");
        m.validate().unwrap();
        assert_eq!(m.edit_dir, sub);

        // Non-existent editDir → current working directory.
        let mut m = kb_with("K");
        m.edit_dir = PathBuf::from("/no/such/dir/xyz123");
        m.validate().unwrap();
        assert_eq!(m.edit_dir, std::env::current_dir().unwrap());
    }

    #[test]
    fn db_path_is_kbname_lmdb_under_edit_dir() {
        let mut m = kb_with("SUMO");
        m.edit_dir = PathBuf::from("/var/db");
        // Case-sensitive: the KB name is used verbatim.
        assert_eq!(m.db_path(), Some(PathBuf::from("/var/db/SUMO.lmdb")));
    }

    #[test]
    fn add_cli_sources_appends_local_and_git() {
        let dir = std::env::temp_dir().join("sdk_add_cli_sources");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.kif");
        std::fs::write(&f, "").unwrap();

        let mut m = kb_with("K");
        // Local: an existing file + an existing dir → one pinned local source.
        m.add_cli_sources(vec![f.clone()], vec![dir.clone()], None).unwrap();
        // Git: a clean-relative in-repo name → Named (joins the git swap in
        // `resolve`); not existence-checked on disk.
        m.add_cli_sources(vec!["in/repo.kif".into()], vec![], Some("https://example/repo".into())).unwrap();
        let kb = m.current_kb().unwrap();
        assert_eq!(kb.constituents.len(), 2);
        assert!(matches!(&kb.constituents[0], Constituent::Source(Source::Local(p)) if p.len() == 2));
        assert!(matches!(&kb.constituents[1], Constituent::Named(p) if p == Path::new("in/repo.kif")));
    }

    #[test]
    fn add_cli_sources_rejects_a_missing_local_path() {
        let mut m = kb_with("K");
        let err = m.add_cli_sources(vec!["/no/such/file.kif".into()], vec![], None).unwrap_err();
        assert!(matches!(err, SdkError::Config(_)));
    }

    #[test]
    fn create_kb_is_idempotent_and_adopts_sumokbname_once() {
        let mut m = KBManager::default();
        m.create_kb("SUMO");
        assert_eq!(m.kbs.len(), 1);
        assert_eq!(m.kbs[0].name(), "SUMO");
        assert!(m.kbs[0].constituents().is_empty());
        assert_eq!(m.sumokbname, "SUMO");

        // Re-creating the same name is a no-op (no duplicate KB).
        m.create_kb("SUMO");
        assert_eq!(m.kbs.len(), 1);

        // A second, different KB doesn't steal the already-set default.
        m.create_kb("OTHER");
        assert_eq!(m.kbs.len(), 2);
        assert_eq!(m.sumokbname, "SUMO");
    }

    #[test]
    fn add_constituents_to_kb_creates_kb_and_adopts_sumokbname() {
        let dir = std::env::temp_dir().join("sdk_add_constituents_create");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.kif");
        std::fs::write(&f, "").unwrap();

        let mut m = KBManager::default();
        assert!(m.sumokbname.is_empty());
        m.add_constituents_to_kb("NEW", vec![f.clone()], vec![], true).unwrap();
        assert_eq!(m.kbs.len(), 1);
        assert_eq!(m.kbs[0].name(), "NEW");
        assert_eq!(m.kbs[0].constituents().len(), 1);
        // First KB on an empty manager is adopted as the default.
        assert_eq!(m.sumokbname, "NEW");

        // A second `add_constituents_to_kb` on a DIFFERENT kb must not steal
        // the already-set default.
        m.add_constituents_to_kb("OTHER", vec![f.clone()], vec![], true).unwrap();
        assert_eq!(m.sumokbname, "NEW");
    }

    #[test]
    fn add_constituents_to_kb_classifies_named_vs_pinned_by_kbdir() {
        // Classification resolves the relative path against `kb_dir`
        // directly (not the process CWD), so this needs no `set_current_dir`
        // (unsafe to do in a parallel test run anyway).
        let root = std::env::temp_dir().join("sdk_add_constituents_classify");
        let kbd  = root.join("kbdir");
        std::fs::create_dir_all(&kbd).unwrap();
        std::fs::write(kbd.join("InKbDir.kif"), "").unwrap();
        let outside = root.join("Outside.kif");
        std::fs::write(&outside, "").unwrap();

        let mut m = KBManager::default();
        m.kb_dir = kbd.clone();

        // A clean-relative name that resolves under kbDir → Named.
        m.add_constituents_to_kb("K", vec![PathBuf::from("InKbDir.kif")], vec![], true).unwrap();
        // An absolute path outside kbDir → pinned, regardless of kbDir.
        m.add_constituents_to_kb("K", vec![outside.clone()], vec![], true).unwrap();

        let kb = &m.kbs[0];
        assert!(matches!(&kb.constituents()[0], Constituent::Named(p) if p == Path::new("InKbDir.kif")));
        assert!(matches!(&kb.constituents()[1], Constituent::Source(Source::Local(p)) if p[0] == outside.canonicalize().unwrap()));
    }

    #[test]
    fn add_constituents_to_kb_is_additive_and_deduplicates() {
        let dir = std::env::temp_dir().join("sdk_add_constituents_dedup");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.kif");
        std::fs::write(&f, "").unwrap();

        let mut m = KBManager::default();
        m.add_constituents_to_kb("K", vec![f.clone()], vec![], true).unwrap();
        m.add_constituents_to_kb("K", vec![f.clone()], vec![], true).unwrap();
        assert_eq!(m.kbs[0].constituents().len(), 1, "re-adding the same path is a no-op");
    }

    #[test]
    fn add_constituents_to_kb_rejects_a_missing_path() {
        let mut m = KBManager::default();
        let err = m.add_constituents_to_kb("K", vec!["/no/such/file.kif".into()], vec![], true).unwrap_err();
        assert!(matches!(err, SdkError::Config(_)));
        assert!(m.kbs.is_empty(), "KB is not created on a failed add");
    }

    #[test]
    fn add_constituents_to_kb_declare_mode_skips_existence_check() {
        // verify_exists = false: nonexistent bare relative names are still
        // accepted and classified purely by shape (Named for clean-relative).
        let mut m = KBManager::default();
        m.add_constituents_to_kb(
            "SUMO",
            vec!["Merge.kif".into(), "Mid-level-ontology.kif".into()],
            vec![],
            false,
        ).unwrap();
        assert_eq!(m.sumokbname, "SUMO");
        let kb = &m.kbs[0];
        assert_eq!(kb.constituents().len(), 2);
        assert!(matches!(&kb.constituents()[0], Constituent::Named(p) if p == Path::new("Merge.kif")));
        assert!(matches!(&kb.constituents()[1], Constituent::Named(p) if p == Path::new("Mid-level-ontology.kif")));
    }

    #[test]
    fn remove_constituents_from_kb_removes_matching_paths_only() {
        let dir = std::env::temp_dir().join("sdk_remove_constituents");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.kif");
        let b = dir.join("b.kif");
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();

        let mut m = KBManager::default();
        m.add_constituents_to_kb("K", vec![a.clone(), b.clone()], vec![], true).unwrap();
        assert_eq!(m.kbs[0].constituents().len(), 2);

        let removed = m.remove_constituents_from_kb("K", vec![a.canonicalize().unwrap()]).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(m.kbs[0].constituents().len(), 1);
    }

    #[test]
    fn remove_constituents_from_kb_errors_on_unknown_kb() {
        let mut m = KBManager::default();
        let err = m.remove_constituents_from_kb("NOPE", vec!["x.kif".into()]).unwrap_err();
        assert!(matches!(err, SdkError::Config(_)));
    }

    #[test]
    fn collects_kbs_and_constituents() {
        let m = KBManager::from_config_xml(SAMPLE).unwrap();
        assert_eq!(m.kbs.len(), 1);
        assert_eq!(m.kbs[0].name, "SUMO");
        assert_eq!(m.kbs[0].constituents.len(), 3);
    }

    /// Extract the single path from a resolved `Source::Local`.
    fn local_one(s: &Source) -> &Path {
        match s { Source::Local(p) => p[0].as_path(), other => panic!("expected Local, got {other:?}") }
    }

    fn kb_with_constituents(cs: Vec<Constituent>) -> KB {
        KB { name: "K".into(), constituents: cs }
    }

    // -- existing behavior: the dynamic local cascade -----------------------
    #[test]
    fn resolve_local_uses_dynamic_cascade() {
        let kb = kb_with_constituents(vec![
            Constituent::Named("Merge.kif".into()),
            Constituent::Named("development/Muscles.kif".into()),   // subdir preserved
            Constituent::Source(Source::Local(vec!["/abs/Other.kif".into()])), // pinned absolute
        ]);
        // kbDir absolute → kbDir/name; baseDir ignored.
        let abs = kb.resolve(Path::new("/base"), Path::new("/sumo"), None, None);
        assert_eq!(local_one(&abs[0]), Path::new("/sumo/Merge.kif"));
        assert_eq!(local_one(&abs[1]), Path::new("/sumo/development/Muscles.kif"));
        assert_eq!(local_one(&abs[2]), Path::new("/abs/Other.kif"));
        // kbDir relative → baseDir/kbDir/name.
        let rel = kb.resolve(Path::new("/base"), Path::new("kbs"), None, None);
        assert_eq!(local_one(&rel[0]), Path::new("/base/kbs/Merge.kif"));
        assert_eq!(local_one(&rel[2]), Path::new("/abs/Other.kif")); // pinned ignores roots
    }

    // -- wholesale swap to git: only Named move; absolute stays local -------
    #[cfg(feature = "git")]
    #[test]
    fn resolve_git_swaps_named_only() {
        let kb = kb_with_constituents(vec![
            Constituent::Named("Merge.kif".into()),
            Constituent::Named("development/Muscles.kif".into()),
            Constituent::Source(Source::Local(vec!["/abs/Other.kif".into()])),
        ]);
        let srcs = kb.resolve(Path::new("/base"), Path::new("/sumo"), Some("https://example/repo"), None);
        match &srcs[0] {
            Source::Git { uri, paths, branch } => {
                assert_eq!(uri, "https://example/repo");
                assert_eq!(paths, &[PathBuf::from("Merge.kif"), PathBuf::from("development/Muscles.kif")]);
                assert_eq!(branch, &None, "no branch pinned defers to the remote's default");
            }
            other => panic!("expected Git, got {other:?}"),
        }
        // The absolute constituent is OMITTED from the swap.
        assert_eq!(local_one(&srcs[1]), Path::new("/abs/Other.kif"));

        // A pinned branch reaches `Source::Git` unmodified.
        let pinned = kb.resolve(Path::new("/base"), Path::new("/sumo"), Some("https://example/repo"), Some("dev"));
        match &pinned[0] {
            Source::Git { branch, .. } => assert_eq!(branch.as_deref(), Some("dev")),
            other => panic!("expected Git, got {other:?}"),
        }
    }

    // -- wholesale swap of the kbDir base: only Named follow ----------------
    #[test]
    fn resolve_kbdir_swap_moves_named_only() {
        let kb = kb_with_constituents(vec![
            Constituent::Named("Merge.kif".into()),
            Constituent::Source(Source::Local(vec!["/abs/Other.kif".into()])),
        ]);
        let a = kb.resolve(Path::new("/base"), Path::new("/sumo"),  None, None);
        let b = kb.resolve(Path::new("/base"), Path::new("/other"), None, None);
        assert_eq!(local_one(&a[0]), Path::new("/sumo/Merge.kif"));
        assert_eq!(local_one(&b[0]), Path::new("/other/Merge.kif")); // Named follows kbDir
        assert_eq!(local_one(&a[1]), Path::new("/abs/Other.kif"));
        assert_eq!(local_one(&b[1]), Path::new("/abs/Other.kif"));   // pinned, didn't move
    }

    #[test]
    fn from_config_xml_classifies_constituents() {
        // A.kif → Named; /abs/X.kif → pinned (absolute); ../up.kif → pinned
        // (`..` offset), frozen to its kbDir-resolved local form.
        let xml = r#"<configuration>
  <preference name="sumokbname" value="SUMO"/>
  <preference name="baseDir" value="/base"/>
  <preference name="kbDir" value="/sumo"/>
  <kb name="SUMO">
    <constituent filename="A.kif"/>
    <constituent filename="/abs/X.kif"/>
    <constituent filename="../up.kif"/>
  </kb>
</configuration>"#;
        let m = KBManager::from_config_xml(xml).unwrap();
        let kb = m.current_kb().unwrap();
        assert!(matches!(&kb.constituents[0], Constituent::Named(p) if p == Path::new("A.kif")));
        assert!(matches!(&kb.constituents[1], Constituent::Source(Source::Local(p)) if p[0] == Path::new("/abs/X.kif")));
        assert!(matches!(&kb.constituents[2], Constituent::Source(Source::Local(p)) if p[0] == Path::new("/sumo/../up.kif")));
    }

    const WITH_PROVERS: &str = r#"<configuration>
  <preference name="sumokbname" value="SUMO"/>
  <kb name="SUMO"><constituent filename="Merge.kif"/></kb>
  <prover type="native">
    <preference name="maxSteps" value="999"/>
    <preference name="timeLimitSecs" value="12"/>
    <preference name="forwardClose" value="no"/>
    <preference name="wantProof" value="yes"/>
  </prover>
  <prover type="external">
    <preference name="timeoutSecs" value="45"/>
  </prover>
</configuration>"#;

    #[test]
    fn parses_native_and_external_prover_sections() {
        let m = KBManager::from_config_xml(WITH_PROVERS).unwrap();
        assert_eq!(m.native_prover.max_steps, 999);
        assert_eq!(m.native_prover.time_limit_secs, 12);
        assert!(!m.native_prover.forward_close, "forwardClose=no → false");
        assert!(m.native_prover.want_proof, "wantProof=yes → true");
        // A field absent from the section keeps its NativeOpts-mirroring default.
        assert_eq!(m.native_prover.max_lits, 8);
        assert_eq!(m.external_prover.timeout_secs, 45);
    }

    #[test]
    fn prover_sections_default_when_absent() {
        // SAMPLE has no <prover> sections.
        let m = KBManager::from_config_xml(SAMPLE).unwrap();
        assert_eq!(m.native_prover, NativeProverConfig::default());
        assert_eq!(m.external_prover, ExternalProverConfig::default());
    }

    #[test]
    fn native_prover_accepts_a_nested_selection_json_value() {
        let xml = r#"<configuration>
  <preference name="sumokbname" value="SUMO"/>
  <kb name="SUMO"><constituent filename="Merge.kif"/></kb>
  <prover type="native">
    <preference name="selection" value="{&quot;tolerance&quot;:1.5,&quot;autoscale&quot;:false}"/>
  </prover>
</configuration>"#;
        let m = KBManager::from_config_xml(xml).unwrap();
        assert!((m.native_prover.selection.tolerance - 1.5).abs() < 1e-6);
        assert!(!m.native_prover.selection.autoscale);
        // Other native fields untouched (default).
        assert_eq!(m.native_prover.max_steps, 4000);
    }

    #[cfg(feature = "ask")]
    #[test]
    fn external_config_builds_prover_opts() {
        let m = KBManager::from_config_xml(WITH_PROVERS).unwrap();
        let opts = m.external_prover().to_prover_opts();
        assert_eq!(opts.timeout_secs, 45);
    }

    #[test]
    fn parses_warning_elevation_codes() {
        let xml = r#"<configuration>
  <preference name="sumokbname" value="SUMO"/>
  <kb name="SUMO"><constituent filename="Merge.kif"/></kb>
  <error code="E005"/>
  <error name="E010"/>
</configuration>"#;
        let m = KBManager::from_config_xml(xml).unwrap();
        assert_eq!(m.elevate_warnings,
            ElevateWarnings::Codes(vec!["E005".into(), "E010".into()]));
        assert!(m.elevate_warnings.elevates("e005"), "case-insensitive");
        assert!(!m.elevate_warnings.elevates("E999"));
    }

    #[test]
    fn warning_elevation_all_subsumes_codes() {
        let xml = r#"<configuration>
  <preference name="sumokbname" value="SUMO"/>
  <kb name="SUMO"><constituent filename="Merge.kif"/></kb>
  <preference name="error" value="all"/>
  <error code="E005"/>
</configuration>"#;
        let m = KBManager::from_config_xml(xml).unwrap();
        assert_eq!(m.elevate_warnings, ElevateWarnings::All);
        assert!(m.elevate_warnings.elevates("anything"));
    }

    #[test]
    fn warning_elevation_defaults_to_none() {
        // SAMPLE declares no <error> elements / `error` preference.
        let m = KBManager::from_config_xml(SAMPLE).unwrap();
        assert_eq!(m.elevate_warnings, ElevateWarnings::None);
        assert!(!m.elevate_warnings.elevates("E005"));
    }

    #[cfg(feature = "native-prover")]
    #[test]
    fn native_config_builds_native_opts() {
        let m = KBManager::from_config_xml(WITH_PROVERS).unwrap();
        let opts = m.native_prover().to_native_opts();
        assert_eq!(opts.max_steps, 999);
        assert_eq!(opts.time_limit_secs, 12);
        assert!(!opts.forward_close);
        assert!(opts.want_proof);
        assert_eq!(opts.max_lits, 8); // default carried through
        // Per-query runtime fields are left default for the caller to set.
        assert!(opts.session.is_none());
        assert!(opts.cancel.is_none());
    }

    #[test]
    fn validate_prover_paths_accepts_absolute_existing_binaries() {
        let dir = std::env::temp_dir().join("sdk_prover_paths_abs");
        std::fs::create_dir_all(&dir).unwrap();
        let vampire = dir.join("vampire");
        let eprover = dir.join("eprover");
        std::fs::write(&vampire, b"#!/bin/sh\n").unwrap();
        std::fs::write(&eprover, b"#!/bin/sh\n").unwrap();

        let mut m = KBManager::default();
        m.vampire = vampire.clone();
        m.eprover = eprover.clone();
        m.validate_prover_paths().unwrap();
        assert_eq!(m.resolve_vampire().unwrap(), vampire);
        assert_eq!(m.resolve_eprover().unwrap(), eprover);
    }

    #[test]
    fn validate_prover_paths_reports_a_missing_absolute_binary() {
        let mut m = KBManager::default();
        m.vampire = PathBuf::from("/definitely/not/here/vampire");
        m.eprover = PathBuf::from("/definitely/not/here/eprover");
        let err = m.validate_prover_paths().unwrap_err();
        let SdkError::Config(msg) = err else { panic!("expected Config error") };
        // Both failures surface in one message.
        assert!(msg.contains("vampire binary not found"), "{msg}");
        assert!(msg.contains("eprover binary not found"), "{msg}");
    }

    #[test]
    fn relative_binary_resolves_under_systems_dir() {
        let dir = std::env::temp_dir().join("sdk_prover_paths_systems");
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("vampire");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        let mut m = KBManager::default();
        m.systems_dir = dir.clone();
        m.vampire = PathBuf::from("vampire"); // bare name → systemsDir first
        assert_eq!(m.resolve_vampire().unwrap(), bin);
    }

    #[cfg(feature = "native-prover")]
    #[test]
    fn native_opts_folds_disable_selection_into_select_all() {
        let mut m = KBManager::default();
        assert!(!m.native_opts().selection.select_all, "off by default");
        m.disable_selection = true;
        assert!(m.native_opts().selection.select_all,
            "disable_selection forces whole-KB (select_all)");
    }

    #[test]
    fn serde_roundtrips_through_json() {
        let m = KBManager::from_config_xml(SAMPLE).unwrap();
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"warning\""), "log_level serializes as a lowercase string");
        let mut back: KBManager = serde_json::from_str(&json).unwrap();
        // `selected_kb` is `#[serde(skip)]` (transient runtime state), so it does
        // not survive the round-trip — re-derive it from `sumokbname`, exactly as
        // `from_config_xml` does after a parse.
        let default = back.sumokbname.clone();
        back.set_current_kb(&default);
        assert_eq!(back, m, "the persisted config round-trips losslessly");
    }

    #[test]
    fn missing_sumokbname_is_rejected() {
        // A KB is defined, but no `sumokbname` preference → fatal.
        let xml = r#"<configuration>
  <kb name="SUMO"><constituent filename="Merge.kif"/></kb>
</configuration>"#;
        let err = KBManager::from_config_xml(xml).unwrap_err();
        assert!(matches!(err, SdkError::Config(_)), "got {err:?}");
        assert!(err.to_string().contains("sumokbname"));
    }

    #[test]
    fn empty_config_is_rejected() {
        // No `sumokbname` at all.
        assert!(matches!(
            KBManager::from_config_xml("<configuration/>").unwrap_err(),
            SdkError::Config(_)));
    }

    #[test]
    fn sumokbname_without_a_matching_kb_is_rejected() {
        let xml = r#"<configuration>
  <preference name="sumokbname" value="SUMO"/>
  <kb name="OtherKB"><constituent filename="Merge.kif"/></kb>
</configuration>"#;
        let err = KBManager::from_config_xml(xml).unwrap_err();
        match err {
            SdkError::Config(msg) => {
                assert!(msg.contains("SUMO") && msg.contains("OtherKB"),
                    "message should name the missing default and the defined KBs: {msg}");
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn validate_accepts_a_well_formed_config() {
        // Structurally valid (sumokbname matches a <kb>) with no constituents to
        // miss.  (SAMPLE itself parses fine — `from_config_xml` validates before
        // selecting a KB — but re-validating it would now fail the file check,
        // since its constituents are placeholder paths; see
        // `validate_checks_selected_kb_constituents_exist`.)
        let mut m = kb_with("SUMO");
        assert!(m.validate().is_ok());
    }

    #[test]
    fn validate_checks_selected_kb_constituents_exist() {
        let dir = std::env::temp_dir().join("sdk_validate_constituents");
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real.kif");
        std::fs::write(&real, "").unwrap();

        // Existing local constituent → ok.
        let mut m = kb_with("K");
        m.kbs[0].constituents.push(Constituent::Source(Source::Local(vec![real])));
        assert!(m.validate().is_ok());

        // Missing local constituent → error.
        let mut m = kb_with("K");
        m.kbs[0].constituents.push(Constituent::Source(Source::Local(vec!["/no/such/Merge.kif".into()])));
        assert!(matches!(m.validate().unwrap_err(), SdkError::Config(_)));

        // Remote (Git) constituents are not disk-checked.
        #[cfg(feature = "git")]
        {
            let mut m = kb_with("K");
            m.kbs[0].constituents.push(Constituent::Source(Source::Git {
                uri: "https://example/repo".into(),
                paths: vec!["Merge.kif".into()],
                branch: None,
            }));
            assert!(m.validate().is_ok());
        }
    }

    #[test]
    fn from_config_xml_selects_the_default_kb() {
        let m = KBManager::from_config_xml(SAMPLE).unwrap();
        let kb = m.current_kb().expect("a default KB is selected");
        assert_eq!(kb.name, "SUMO", "selection defaults to sumokbname");
        // `sources()` returns the selected KB's constituents.
        assert_eq!(m.resolve_sources(None, None).len(), 3);
    }

    #[test]
    #[ignore = "reads the developer's ~/.sigmakee/config.xml (machine-specific)"]
    fn parses_real_sigmakee_config() {
        let home = std::env::var("HOME").expect("HOME set");
        let path = std::path::Path::new(&home).join(".sigmakee/config.xml");
        let m = KBManager::from_config_xml_path(&path).expect("real config.xml parses");
        assert_eq!(m.sumokbname, "SUMO");
        assert_eq!(m.log_level, LevelFilter::Warn);
        assert_eq!(m.kbs.len(), 1);
        assert!(m.kbs[0].constituents.len() > 10, "the SUMO KB has many constituents");
    }
}
