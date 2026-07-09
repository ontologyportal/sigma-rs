// crates/cli/src/cli/config_cmd.rs
//
// `sumo config` — print the resolved `KBManager` configuration (from
// config.xml when found and `--no-config` wasn't passed, else built-in
// defaults) and how each option maps to its CLI flag.
//
// The option table `KBManager::options()` is the single source of truth that
// ties every option to its CLI flag(s) and its serde `json_paths`.  We
// serialize the live manager to JSON once and resolve each option's value
// against it — the same view `KBManager::apply_overrides` patches — so the
// printed value is exactly what the runtime would use.

use std::path::{Path, PathBuf};

use sigmakee_rs_sdk::Source;
use sigmakee_rs_sdk::manager::{Constituent, KBManager};
use sigmakee_rs_sdk::manager::meta::{OptionMeta, Scope};

use crate::style::*;

/// Entry point for `sumo config`.
///
/// `config_path` is the config.xml the CLI resolved (if any); `loaded` is
/// whether it was actually parsed into `manager` (i.e. `--no-config` wasn't
/// passed and a file was found).
pub fn run_config(manager: &KBManager, config_path: Option<PathBuf>, loaded: bool) -> bool {
    let doc = match serde_json::to_value(manager) {
        Ok(v) => v,
        Err(e) => { log::error!("config: cannot serialize manager: {e}"); return false; }
    };

    match (&config_path, loaded) {
        (Some(p), true) =>
            println!("{style_bold}Config:{style_reset} {color_bright_green}{}{color_reset}", p.display()),
        (Some(p), false) =>
            println!("{style_bold}Config:{style_reset} {color_bright_yellow}found but not loaded{color_reset} \
                      — {} (--no-config was passed; showing built-in defaults)", p.display()),
        (None, _) =>
            println!("{style_bold}Config:{style_reset} built-in defaults (no config.xml found)"),
    }
    println!("{color_bright_black}Each row: CLI flag = configured value   [config.xml key(s)]{color_reset}");

    let opts = KBManager::options();
    group("Global flags",
        opts.iter().filter(|o| matches!(o.scope, Scope::Global)), &doc);
    group("Prover options (CLI flags)",
        opts.iter().filter(|o| matches!(o.scope, Scope::Subsystems(_)) && is_prover(o)), &doc);
    group("Other subcommand flags",
        opts.iter().filter(|o| matches!(o.scope, Scope::Subsystems(_)) && !is_prover(o)), &doc);
    print_kbs(manager);
    group("Config-file only (no CLI flag)",
        opts.iter().filter(|o| matches!(o.scope, Scope::ConfigOnly)), &doc);

    true
}

/// Entry point for `sumo config --<setting> value ...` — patch just the given
/// settings and persist the whole (regenerated) config.xml.  Loads `target`
/// leniently if it exists (an in-progress edit needn't already satisfy
/// [`KBManager::validate`] — e.g. the very first `sumo config --edit-dir ...`
/// on a brand-new file has no `sumokbname` yet), else starts from built-in
/// defaults.
pub fn run_config_write(target: &Path, overrides: Vec<(&OptionMeta, serde_json::Value)>) -> bool {
    let mut manager = if target.exists() {
        match KBManager::from_config_xml_path_lenient(target) {
            Ok(m) => m,
            Err(e) => { log::error!("config: cannot parse {}: {e}", target.display()); return false; }
        }
    } else {
        KBManager::default()
    };

    // Snapshot "before" values for the confirmation summary.
    let before = serde_json::to_value(&manager).unwrap_or_default();
    let changed: Vec<(&str, String, String)> = overrides.iter()
        .map(|(o, v)| {
            let old = o.json_paths.first().and_then(|p| resolve(&before, p))
                .map(fmt_value).unwrap_or_else(|| "—".to_string());
            (o.long, old, fmt_value(v))
        })
        .collect();

    if let Err(e) = manager.apply_overrides(overrides) {
        log::error!("config: {e}");
        return false;
    }

    let xml = manager.to_config_xml();
    if let Some(dir) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::error!("config: cannot create {}: {e}", dir.display());
            return false;
        }
    }
    if let Err(e) = std::fs::write(target, &xml) {
        log::error!("config: cannot write {}: {e}", target.display());
        return false;
    }

    println!("{style_bold}Wrote config:{style_reset} {color_bright_green}{}{color_reset}", target.display());
    for (flag, old, new) in changed {
        println!("  {color_bright_cyan}--{flag}{color_reset}  {color_bright_black}{old}{color_reset} → {color_bright_green}{new}{color_reset}");
    }
    true
}

/// A prover-tuning or prover-binary option (native/external prover config, or a
/// solver path/backend selector).
fn is_prover(o: &OptionMeta) -> bool {
    o.json_paths.iter().any(|p| p.starts_with("native_prover") || p.starts_with("external_prover"))
        || matches!(o.field, "vampire" | "vampire_hol" | "eprover" | "backend")
}

/// Print the configured knowledge bases + their *effective* constituent list
/// — the config.xml `<kb>` sections, but only populated when `-c` was passed
/// (otherwise empty: config.xml's ontology doesn't auto-load), plus any
/// `-f`/`-d`/`--git` sources merged in.  These aren't CLI flags themselves
/// (you can't add a whole KB from the command line); the active one is
/// selected by `--sumokbname`.
fn print_kbs(manager: &KBManager) {
    println!("\n{style_bold}Knowledge bases{style_reset}  \
              {color_bright_black}(config.xml <kb> sections, effective after -c/-f/-d/--git — active set by --sumokbname){color_reset}");
    if manager.kbs.is_empty() {
        println!("  {color_bright_black}(none configured){color_reset}");
        return;
    }
    for kb in &manager.kbs {
        let active = kb.name() == manager.sumokbname;
        let (marker, tag) = if active {
            (format!("{color_bright_green}●{color_reset}"), format!("  {color_bright_green}(active){color_reset}"))
        } else {
            (format!("{color_bright_black}○{color_reset}"), String::new())
        };
        println!("  {marker} {color_bright_cyan}{}{color_reset}{tag}  \
                  {color_bright_black}({} constituent(s)){color_reset}",
            kb.name(), kb.constituents().len());
        for c in kb.constituents() {
            match c {
                Constituent::Named(p) =>
                    println!("      {}  {color_bright_black}[named → kbDir]{color_reset}", p.display()),
                Constituent::Source(s) =>
                    println!("      {}  {color_bright_black}[pinned]{color_reset}", render_source(s)),
            }
        }
    }
}

/// One-line rendering of a pinned constituent source (the common case is a
/// local absolute / `..`-bearing path).
fn render_source(s: &Source) -> String {
    match s {
        Source::Local(paths) =>
            paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
        _ => "<non-local source>".to_string(),
    }
}

/// Print one scope group: a header, then `flag = value [keys]` + help per option.
fn group<'a>(title: &str, opts: impl Iterator<Item = &'a OptionMeta>, doc: &serde_json::Value) {
    let rows: Vec<&OptionMeta> = opts.collect();
    if rows.is_empty() { return; }
    println!("\n{style_bold}{title}{style_reset}");
    for o in rows {
        // The CLI flag this option maps to (config-only options have none — show
        // the config field instead).
        let flag = match o.scope {
            Scope::ConfigOnly => o.field.to_string(),
            _ => {
                let mut s = format!("--{}", o.long);
                if let Some(c) = o.short { s.push_str(&format!(" -{c}")); }
                s
            }
        };
        // The configured value: resolve the first json-path against the manager.
        let value = o.json_paths.first()
            .and_then(|p| resolve(doc, p))
            .map(fmt_value)
            .unwrap_or_else(|| "—".to_string());
        // Which subcommands surface the flag (subsystem-scoped only).
        let scope_note = match o.scope {
            Scope::Subsystems(subs) => format!("  {color_bright_black}({}){color_reset}",
                subs.iter().map(|s| format!("{s:?}").to_lowercase()).collect::<Vec<_>>().join("/")),
            _ => String::new(),
        };
        let env_note = o.env
            .map(|e| format!("  {color_bright_black}[env {e}]{color_reset}"))
            .unwrap_or_default();

        println!("  {color_bright_cyan}{:<26}{color_reset} = {color_bright_green}{}{color_reset}{}{}",
            flag, value, scope_note, env_note);
        println!("      {color_bright_black}{}  [{}]{color_reset}", o.help, o.json_paths.join(", "));
    }
}

/// Read the value at dot-`path` (serde key segments) in `doc`.
fn resolve<'a>(doc: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = doc;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Render a JSON value for display (numbers/bools verbatim; empty strings + null
/// flagged so an unset option is obvious).
fn fmt_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "—".to_string(),
        serde_json::Value::String(s) if s.is_empty() => "(unset)".to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
