use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::fs;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigXml {
    #[serde(rename = "preference", default)]
    pub parameters: Vec<Preference>,
    #[serde(rename = "kb", default)]
    pub kbs: Vec<KbConfig>,
}

#[derive(Debug, Deserialize)]
pub struct Preference {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@value")]
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct KbConfig {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "constituent", default)]
    pub constituents: Vec<Constituent>,
}

#[derive(Debug, Deserialize)]
pub struct Constituent {
    #[serde(rename = "@filename")]
    pub file: String,
}

impl ConfigXml {
    pub fn get_parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.as_str())
    }

    pub fn kb_dir(&self) -> Option<PathBuf> {
        self.get_parameter("kbDir").map(PathBuf::from)
    }

    pub fn vampire_path(&self) -> Option<PathBuf> {
        self.get_parameter("vampire").map(PathBuf::from)
    }

    pub fn log_level(&self) -> Option<&str> {
        self.get_parameter("logLevel")
    }

    pub fn default_kb_name(&self) -> Option<&str> {
        log::debug!("Default KB name: {:?}", self.get_parameter("sumokbname"));
        self.get_parameter("sumokbname")
    }

    /// Returns a list of absolute paths for the files in the specified KB,
    /// resolved relative to `kbDir` from the config.
    pub fn get_kb_files(&self, kb_name: &str) -> Option<Vec<PathBuf>> {
        let kb_dir = self.kb_dir().unwrap_or_else(|| PathBuf::from("."));
        self.get_kb_files_relative_to(kb_name, &kb_dir)
    }

    /// Like [`get_kb_files`] but resolves constituent paths relative to
    /// `base` instead of the config's `kbDir`.  Used by `--git` to
    /// treat all constituent paths as relative to the cloned repo root.
    pub fn get_kb_files_relative_to(&self, kb_name: &str, base: &Path) -> Option<Vec<PathBuf>> {
        let kb = self.kbs.iter().find(|k| k.name == kb_name)?;
        Some(kb.constituents.iter().map(|c| {
            let p = PathBuf::from(&c.file);
            if p.is_absolute() { p } else { base.join(&p) }
        }).collect())
    }

    /// Return the raw (un-joined) constituent path strings for a KB.
    /// Used by `--git` to build the sparse-checkout path list before
    /// cloning — at that point there is no repo root to join against yet.
    pub fn get_kb_constituents(&self, kb_name: &str) -> Option<Vec<&str>> {
        let kb = self.kbs.iter().find(|k| k.name == kb_name)?;
        Some(kb.constituents.iter().map(|c| c.file.as_str()).collect())
    }
}

/// Parse a SigmaKEE config.xml file.
pub fn parse_config_xml(path: &Path) -> Result<ConfigXml, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read config file {}: {}", path.display(), e))?;
    quick_xml::de::from_str(&content)
        .map_err(|e| format!("failed to parse config XML {}: {}", path.display(), e))
}

/// Resolve the path to config.xml based on user input or environment.
///
/// Tilde expansion: user-supplied paths starting with `~/` are expanded
/// against `$HOME` so that `--config "~/.sigmakee/config.xml"` works
/// even when the shell leaves the `~` literal (e.g. inside double
/// quotes).  Without this the config silently fails to load, the KB
/// is built empty, and callers like `-k /path` produce a one-line TPTP
/// dump containing only the conjecture.
pub fn resolve_config_path(
    manual_path: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(p) = manual_path {
        let p = expand_tilde(p);
        if p.is_dir() {
            return Some(p.join("config.xml"));
        }
        return Some(p);
    }

    if let Ok(sigma_home) = std::env::var("SIGMA_HOME") {
        let p = PathBuf::from(sigma_home).join("KBs").join("config.xml");
        if p.exists() {
            return Some(p);
        }
    }

    // Fallback: ~/.sigmakee/KBs/config.xml
    if let Some(home) = home_dir() {
        let p = home.join(".sigmakee").join("KBs").join("config.xml");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Replace a leading `~/` with `$HOME`.  Leaves other paths untouched,
/// matches the behaviour of typical shells when they *do* perform tilde
/// expansion.  Intentionally does not handle `~user/` (that requires a
/// passwd-DB lookup and isn't worth the crate cost for a one-off case).
pub(crate) fn expand_tilde(p: &Path) -> PathBuf {
    let s = match p.to_str() {
        Some(s) => s,
        None    => return p.to_path_buf(),
    };
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut out = PathBuf::from(home);
            out.push(rest);
            return out;
        }
    }
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    p.to_path_buf()
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}
