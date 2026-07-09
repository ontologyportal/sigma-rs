// crates/sdk/src/manager/write.rs
//
// Serialize a KBManager back to config.xml — the write half of
// `parse_config_xml_lenient` / `from_config_xml`. "Full regenerate": every
// value on `self` is emitted in a canonical form; comments, formatting, and
// element order from any original hand-edited file are not preserved. Key
// names/casing mirror `parse_config_xml_lenient` exactly (same irregulars:
// `TPTP`, `vampire_hol`, `realNumbers`), so a round trip through
// `to_config_xml` → `parse_config_xml_lenient` is value-lossless.

use quick_xml::events::{BytesDecl, Event};
use quick_xml::writer::Writer;
use serde_json::Value;

use super::{Constituent, ElevateWarnings, KBManager, Source, KB};

impl KBManager {
    /// Serialize this manager back to a `config.xml` document.
    pub fn to_config_xml(&self) -> String {
        let mut buf: Vec<u8> = Vec::new();
        let mut writer = Writer::new_with_indent(&mut buf, b' ', 2);
        writer
            .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
            .expect("write XML declaration");

        writer
            .create_element("configuration")
            .write_inner_content(|w| {
                write_preferences(w, self);
                write_error_elevation(w, &self.elevate_warnings);
                write_prover(w, "native", &self.native_prover);
                write_prover(w, "external", &self.external_prover);
                for kb in &self.kbs {
                    write_kb(w, kb);
                }
                Ok(())
            })
            .expect("write <configuration>");

        String::from_utf8(buf).expect("XML output is valid UTF-8")
    }
}

type W<'a> = Writer<&'a mut Vec<u8>>;

fn pref(w: &mut W, name: &str, value: &str) {
    w.create_element("preference")
        .with_attribute(("name", name))
        .with_attribute(("value", value))
        .write_empty()
        .expect("write <preference>");
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn write_preferences(w: &mut W, m: &KBManager) {
    pref(w, "baseDir", &m.base_dir.display().to_string());
    pref(w, "cache", yn(m.cache));
    pref(w, "defaultBackend", &m.default_backend);
    pref(w, "disableSelection", yn(m.disable_selection));
    pref(w, "editDir", &m.edit_dir.display().to_string());
    pref(w, "eprover", &m.eprover.display().to_string());
    pref(w, "graphVizDir", &m.graphviz_dir.display().to_string());
    pref(w, "holdsPrefix", yn(m.holds_prefix));
    pref(w, "inferenceTestDir", &m.inference_test_dir.display().to_string());
    pref(w, "kbDir", &m.kb_dir.display().to_string());
    pref(w, "language", &m.language);
    pref(w, "leoExecutable", &m.leo_executable.display().to_string());
    pref(w, "limit", &m.limit.to_string());
    pref(w, "logDir", &m.log_dir.display().to_string());
    pref(w, "logLevel", super::severity_str(m.log_level));
    pref(w, "ollamaHost", &m.ollama_host);
    pref(w, "proof", &m.proof);
    pref(w, "prose", yn(m.prose));
    if let Some(rn) = m.real_numbers {
        pref(w, "realNumbers", yn(rn));
    }
    pref(w, "showKif", yn(m.show_kif));
    pref(w, "sumokbname", &m.sumokbname);
    pref(w, "systemsDir", &m.systems_dir.display().to_string());
    pref(w, "thoroughness", &m.thoroughness.to_string());
    pref(w, "TPTP", yn(m.tptp));
    pref(w, "tptpLang", &m.tptp_lang);
    pref(w, "vampire", &m.vampire.display().to_string());
    pref(w, "vampire_hol", &m.vampire_hol.display().to_string());
}

fn write_error_elevation(w: &mut W, ew: &ElevateWarnings) {
    match ew {
        ElevateWarnings::None => {}
        ElevateWarnings::All => pref(w, "error", "all"),
        ElevateWarnings::Codes(codes) => {
            for c in codes {
                w.create_element("error")
                    .with_attribute(("code", c.as_str()))
                    .write_empty()
                    .expect("write <error>");
            }
        }
    }
}

/// Write a `<prover type="{kind}">` section from any serde-serializable
/// prover config struct — mirrors `prover_config_from_prefs`'s read side:
/// each top-level field becomes one `<preference>`, with a nested object
/// (`selection`/`strategy`) serialized as compact JSON text in its `value`
/// attribute, exactly what `json_value_of` expects to parse back.
fn write_prover<T: serde::Serialize>(w: &mut W, kind: &str, cfg: &T) {
    let value = serde_json::to_value(cfg).expect("prover config serializes to JSON");
    let obj = value.as_object().expect("prover config serializes to a JSON object");
    w.create_element("prover")
        .with_attribute(("type", kind))
        .write_inner_content(|w| {
            for (k, v) in obj {
                pref(w, k, &json_value_to_pref_str(v));
            }
            Ok(())
        })
        .expect("write <prover>");
}

fn json_value_to_pref_str(v: &Value) -> String {
    match v {
        Value::Bool(b)   => yn(*b).to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Null       => String::new(),
        Value::Object(_) | Value::Array(_) =>
            serde_json::to_string(v).expect("nested prover value serializes"),
    }
}

fn write_kb(w: &mut W, kb: &KB) {
    w.create_element("kb")
        .with_attribute(("name", kb.name()))
        .write_inner_content(|w| {
            for c in kb.constituents() {
                match c {
                    Constituent::Named(p) => write_constituent(w, p),
                    Constituent::Source(Source::Local(paths)) => {
                        for p in paths {
                            write_constituent(w, p);
                        }
                    }
                    // No XML form (a transient runtime source, e.g. `--git`
                    // re-rooting) — shouldn't occur on a manager freshly
                    // parsed from disk, the only thing `to_config_xml` is
                    // meant to run on. Skip defensively rather than panic.
                    Constituent::Source(_) => {}
                }
            }
            Ok(())
        })
        .expect("write <kb>");
}

fn write_constituent(w: &mut W, p: &std::path::Path) {
    w.create_element("constituent")
        .with_attribute(("filename", p.to_string_lossy().as_ref()))
        .write_empty()
        .expect("write <constituent>");
}
