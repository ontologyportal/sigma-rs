use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {

}
