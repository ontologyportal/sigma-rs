// crates/cli/src/cli/args_project.rs
//
// Project the `KBManager::options()` metadata table into the clap parser, so
// every configurable option is a real CLI flag — the design `manager::meta`
// intends ("a consumer projects this table into a clap arg parser").
//
// Flow:
//   1. take the derived `Cli` command;
//   2. add every table option that ISN'T already hand-declared as a flag, to
//      the subcommands its `Scope` covers (globals go on the root, `global`);
//   3. parse, then hand the user-/env-supplied values for those *projected*
//      options to `KBManager::apply_overrides` (serialize → patch json-paths →
//      deserialize), giving `flag > env > config.xml > default` precedence.
//
// Options already hand-declared in `args.rs` (e.g. `--scope`, `--timeout`) keep
// flowing through the derived `Cli` + the per-command merge; only the gap (the
// options the table has but the derive structs lack) is projected here.  As
// derive args migrate onto the table, that gap — and this duplication — shrinks.

use std::collections::HashSet;

use clap::{Arg, ArgAction, ArgMatches, Command, CommandFactory, FromArgMatches};
use clap::parser::ValueSource;
use sigmakee_rs_sdk::manager::KBManager;
use sigmakee_rs_sdk::manager::meta::{Kind, OptionMeta, Scope, Subsystem};

use super::args::Cli;

/// Clap subcommand name → the option [`Subsystem`] it maps to.
const SUBCOMMANDS: &[(&str, Subsystem)] = &[
    ("validate",  Subsystem::Validate),
    ("ask",       Subsystem::Ask),
    ("translate", Subsystem::Translate),
    ("test",      Subsystem::Test),
    ("load",      Subsystem::Load),
    ("man",       Subsystem::Man),
    ("search",    Subsystem::Search),
    ("audit",     Subsystem::Audit),
    ("sweep",     Subsystem::Sweep),
];

/// Parse argv with the table projected onto the derived command.  Returns the
/// parsed `Cli` plus the raw matches (for [`overrides`]).
pub fn parse() -> (Cli, ArgMatches) {
    let derived = Cli::command();
    let declared = collect_longs(&derived);
    let matches = augment_config(augment(derived, &declared)).get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    (cli, matches)
}

/// The user-/env-supplied values for *projected* options (those the derive
/// structs don't already declare), ready for `KBManager::apply_overrides`.
///
/// "Already declared" is judged per-subcommand (plus the root's flags) — a
/// hand-declared flag on one subcommand (`man`'s `--lang`) must not shadow the
/// table option of the same name on the others (`test`/`ask`/`translate`).
pub fn overrides(matches: &ArgMatches) -> Vec<(&'static OptionMeta, serde_json::Value)> {
    let root = Cli::command();
    let Some((sub_name, sub_m)) = matches.subcommand() else { return Vec::new() };
    let Some(subsystem) = subsystem_of(sub_name) else { return Vec::new() };
    let whole_tree = collect_longs(&root);
    let mut root_and_sub = shallow_longs(&root);
    if let Some(sc) = root.find_subcommand(sub_name) {
        root_and_sub.extend(shallow_longs(sc));
    }
    KBManager::options_for(subsystem)
        .filter(|o| is_projected(o, &whole_tree, &root_and_sub))
        .filter_map(|o| extract(o, sub_m).map(|v| (o, v)))
        .collect()
}

/// Is table option `o` projected as a CLI flag where `root_and_sub` describes
/// the target subcommand?  THE one collision predicate shared by [`augment`]
/// (which registers the args) and [`overrides`] (which extracts their values):
/// if the two disagreed, `overrides` would probe an arg id clap never
/// registered, which panics.  Globals must avoid hand-declared flags anywhere
/// in the tree (`whole_tree` — they propagate everywhere); subsystem options
/// only the root's and the target subcommand's own (`root_and_sub`).
fn is_projected(
    o: &OptionMeta,
    whole_tree: &HashSet<String>,
    root_and_sub: &HashSet<String>,
) -> bool {
    match o.scope {
        Scope::Global => !whole_tree.contains(o.long),
        Scope::Subsystems(_) => !root_and_sub.contains(o.long),
        Scope::ConfigOnly => false,
    }
}

// -- internals ---------------------------------------------------------------

/// Every long flag the derived command declares (root globals + subcommands).
/// Used for the GLOBAL table options only: a projected global propagates into
/// every subcommand (`global(true)`), so it must not collide with any
/// hand-declared flag anywhere in the tree.
fn collect_longs(cmd: &Command) -> HashSet<String> {
    let mut out = HashSet::new();
    for a in cmd.get_arguments() {
        if let Some(l) = a.get_long() { out.insert(l.to_string()); }
    }
    for sc in cmd.get_subcommands() {
        out.extend(collect_longs(sc));
    }
    out
}

/// Long flags declared directly on `cmd` (not recursing into subcommands).
fn shallow_longs(cmd: &Command) -> HashSet<String> {
    cmd.get_arguments()
        .filter_map(|a| a.get_long().map(str::to_string))
        .collect()
}

/// Add each not-yet-declared table option to the command: globals on the root
/// (`global(true)` so subcommands inherit them), subsystem options on their
/// subcommands.
///
/// Subsystem options are guarded per-subcommand (own flags + the root's), NOT
/// by the whole-tree set: `man` hand-declaring `--lang` must not block the
/// table's `--lang` from projecting onto `test`/`ask`/`translate`.
fn augment(mut cmd: Command, declared: &HashSet<String>) -> Command {
    let root_longs = shallow_longs(&cmd);
    for o in KBManager::options() {
        if matches!(o.scope, Scope::Global) && is_projected(o, declared, &root_longs) {
            cmd = cmd.arg(arg_of(o).global(true));
        }
    }
    for (name, subsystem) in SUBCOMMANDS {
        let Some(sc_ref) = cmd.find_subcommand(name) else { continue }; // feature-gated off
        let mut local: HashSet<String> = shallow_longs(sc_ref);
        local.extend(root_longs.iter().cloned());
        cmd = cmd.mut_subcommand(name, |sc| {
            let mut sc = sc;
            for o in KBManager::options() {
                if let Scope::Subsystems(subs) = o.scope {
                    if subs.contains(subsystem) && is_projected(o, declared, &local) {
                        sc = sc.arg(arg_of(o));
                    }
                }
            }
            sc
        });
    }
    cmd
}

/// Build a clap `Arg` from one option's metadata.  Value-taking (incl. bools,
/// e.g. `--want-proof true`) so an override can set either polarity.  No clap
/// default — an absent flag means "no override" (config/default stands).
fn arg_of(o: &'static OptionMeta) -> Arg {
    let mut a = Arg::new(o.field).long(o.long).help(o.help);
    if let Some(c) = o.short { a = a.short(c); }
    if let Some(e) = o.env   { a = a.env(e); }
    match o.kind {
        Kind::Bool  => a.value_parser(clap::value_parser!(bool)).value_name("BOOL").action(ArgAction::Set),
        Kind::Int   => a.value_parser(clap::value_parser!(u64)).value_name("N").action(ArgAction::Set),
        Kind::Float => a.value_parser(clap::value_parser!(f64)).value_name("F").action(ArgAction::Set),
        Kind::Str   => a.value_name("STR").action(ArgAction::Set),
        Kind::Path  => a.value_name("PATH").action(ArgAction::Set),
        // Repeatable string list (e.g. `-W E005 -W E010`).  Not generically
        // projected today (the only one, `--warning`, is hand-declared), but the
        // arm keeps `arg_of` total for any future list option.
        Kind::List  => a.value_name("VAL").action(ArgAction::Append),
        // Resolved (file read, if it names one) and JSON-validated at parse
        // time, so a bad --strategy fails like any other clap arg error
        // rather than surfacing later out of apply_overrides.
        Kind::Json  => a.value_name("JSON|FILE").action(ArgAction::Set)
            .value_parser(resolve_json_arg),
    }
}

/// Resolve one `Kind::Json` argument: if `raw` names an existing file, read
/// and use its contents; otherwise treat `raw` itself as inline JSON. Either
/// way the result must parse as JSON, checked here so the error surfaces at
/// argument-parsing time with the raw flag context clap already has.
fn resolve_json_arg(raw: &str) -> Result<String, String> {
    let is_file = std::path::Path::new(raw).is_file();
    let text = if is_file {
        std::fs::read_to_string(raw).map_err(|e| format!("reading '{raw}': {e}"))?
    } else {
        raw.to_string()
    };
    serde_json::from_str::<serde_json::Value>(&text).map_err(|e| {
        if is_file { format!("invalid JSON in '{raw}': {e}") } else { format!("invalid JSON: {e}") }
    })?;
    Ok(text)
}

/// Extract a user-/env-supplied value for `o` from the subcommand matches, as a
/// typed JSON value matching the option's serde target.
fn extract(o: &OptionMeta, m: &ArgMatches) -> Option<serde_json::Value> {
    match m.value_source(o.field) {
        Some(ValueSource::CommandLine) | Some(ValueSource::EnvVariable) => {}
        _ => return None,
    }
    Some(match o.kind {
        Kind::Bool             => serde_json::json!(m.get_one::<bool>(o.field)?),
        Kind::Int              => serde_json::json!(m.get_one::<u64>(o.field)?),
        Kind::Float            => serde_json::json!(m.get_one::<f64>(o.field)?),
        Kind::Str | Kind::Path => serde_json::json!(m.get_one::<String>(o.field)?),
        // Already validated + resolved to plain JSON text by `resolve_json_arg`
        // at parse time — parse it into the object itself (NOT wrapped as a
        // JSON string like Str/Path above), since it patches a nested struct
        // (`native_prover.strategy`), not a scalar leaf.
        Kind::Json             => serde_json::from_str(m.get_one::<String>(o.field)?).ok()?,
        // Not projected today (the only list option is hand-declared); a future
        // one would need a target-typed mapping, so refuse rather than guess.
        Kind::List             => return None,
    })
}

fn subsystem_of(name: &str) -> Option<Subsystem> {
    SUBCOMMANDS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// Project *every* non-global option onto the `config` subcommand — `sumo
/// config` is a config editor, not scoped to one subsystem's needs, so it
/// exposes the whole table. `Scope::Global` options need no extra work here:
/// [`augment`]'s `global(true)` registration already reaches every
/// subcommand, `config` included. `Scope::ConfigOnly` options, by contrast,
/// have no flag anywhere else — `config` is the *only* place they surface.
fn augment_config(mut cmd: Command) -> Command {
    let Some(sc_ref) = cmd.find_subcommand("config") else { return cmd }; // feature-gated off
    let local: HashSet<String> = shallow_longs(sc_ref);
    cmd = cmd.mut_subcommand("config", |sc| {
        let mut sc = sc;
        for o in KBManager::options() {
            if !matches!(o.scope, Scope::Global) && !local.contains(o.long) {
                sc = sc.arg(arg_of(o));
            }
        }
        sc
    });
    cmd
}

/// The user-supplied `sumo config` flags, ready for `KBManager::apply_overrides`
/// — unfiltered by [`Subsystem`] (mirrors [`augment_config`]'s unfiltered
/// registration), so every table option is a candidate regardless of which
/// other subcommand(s) it's normally scoped to.
///
/// `Scope::Global` options still need the same collision check `augment`
/// applies: one of them (`warning`) is hand-declared elsewhere (as `suppress`,
/// backing `-W`/`--warning`) and so was never registered under its own
/// `field` id — calling `ArgMatches::value_source` for an unregistered id
/// panics, not just misses. `Scope::Subsystems`/`ConfigOnly` options need no
/// such check: `augment_config` registers all of them unconditionally (there
/// being nothing hand-declared on the empty `Cmd::Config {}` to collide
/// with).
pub fn config_overrides(matches: &ArgMatches) -> Vec<(&'static OptionMeta, serde_json::Value)> {
    let Some(("config", sub_m)) = matches.subcommand() else { return Vec::new() };
    let whole_tree = collect_longs(&Cli::command());
    KBManager::options()
        .iter()
        .filter(|o| !matches!(o.scope, Scope::Global) || !whole_tree.contains(o.long))
        .filter_map(|o| extract(o, sub_m).map(|v| (o, v)))
        .collect()
}

#[cfg(all(test, feature = "ask"))]
mod tests {
    use super::*;

    fn matches_for(argv: &[&str]) -> ArgMatches {
        let derived = Cli::command();
        let declared = collect_longs(&derived);
        augment(derived, &declared)
            .try_get_matches_from(argv)
            .expect("argv should parse with the projected flags")
    }

    fn config_matches_for(argv: &[&str]) -> ArgMatches {
        let derived = Cli::command();
        let declared = collect_longs(&derived);
        augment_config(augment(derived, &declared))
            .try_get_matches_from(argv)
            .expect("argv should parse with the projected flags")
    }

    #[test]
    fn table_only_flag_is_projected_and_reaches_the_manager() {
        // `--max-steps` is in the OptionMeta table but NOT hand-declared, so it
        // must project onto `ask`, extract as an override, and patch the manager.
        let declared = collect_longs(&Cli::command());
        assert!(!declared.contains("max-steps"), "precondition: --max-steps is table-only");

        let m = matches_for(&["sumo", "ask", "(instance Rex Animal)", "--max-steps", "9999"]);
        let ov = overrides(&m);
        let hit = ov.iter().find(|(o, _)| o.field == "max_steps")
            .expect("max_steps should be extracted as an override");
        assert_eq!(hit.1, serde_json::json!(9999u64));

        let mut mgr = KBManager::default();
        mgr.apply_overrides(ov).expect("apply_overrides");
        assert_eq!(mgr.native_prover.max_steps, 9999, "override must reach the manager field");
    }

    #[test]
    fn unsupplied_flag_produces_no_override() {
        let m = matches_for(&["sumo", "ask", "(instance Rex Animal)"]);
        assert!(overrides(&m).iter().all(|(o, _)| o.field != "max_steps"),
            "an absent flag must not produce an override");
    }

    #[test]
    fn config_exposes_a_subsystem_scoped_option_unfiltered() {
        // `--thoroughness` is normally Audit-only, but `sumo config` exposes
        // every non-global option regardless of subsystem.
        let m = config_matches_for(&["sumo", "config", "--thoroughness", "0.5"]);
        let ov = config_overrides(&m);
        let hit = ov.iter().find(|(o, _)| o.field == "thoroughness")
            .expect("thoroughness should be extracted as a config override");
        assert_eq!(hit.1, serde_json::json!(0.5));
    }

    #[test]
    fn config_ignores_the_hand_declared_warning_flag_without_panicking() {
        // Regression: `warning`'s OptionMeta id ("warning") differs from its
        // hand-declared clap arg id ("suppress", backing `-W`/`--warning`),
        // so `config_overrides` iterating the whole table unconditionally
        // used to panic in `ArgMatches::value_source("warning")` — that id
        // was never registered anywhere. `-W` still parses (through
        // `suppress`); it just produces no config-write override.
        let m = config_matches_for(&["sumo", "config", "-W", "E005", "--thoroughness", "0.5"]);
        let ov = config_overrides(&m); // must not panic
        assert!(ov.iter().all(|(o, _)| o.field != "warning"));
        assert!(ov.iter().any(|(o, _)| o.field == "thoroughness"));
    }

    #[test]
    fn config_only_option_is_reachable_via_config_but_nowhere_else() {
        // `graphviz_dir` has Scope::ConfigOnly — no flag on any other
        // subcommand, but `sumo config` is specifically where it should
        // surface (otherwise it could never be set at all).
        let m = config_matches_for(&["sumo", "config", "--graphviz-dir", "/tmp/gv"]);
        let ov = config_overrides(&m);
        let hit = ov.iter().find(|(o, _)| o.field == "graphviz_dir")
            .expect("graphviz_dir should be extracted as a config override");
        assert_eq!(hit.1, serde_json::json!("/tmp/gv"));
    }
}
