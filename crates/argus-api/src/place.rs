//! Where the host keeps its data and serves its socket unless its settings say otherwise.
//! The host's defaults and the `argus` CLI both read these, so they cannot disagree.
//!
//! `var` reads the environment as `std::env::var` would; a relative XDG directory is
//! ignored, as the XDG Base Directory spec says.

use std::path::PathBuf;

fn absolute(var: &impl Fn(&str) -> Option<String>, name: &str) -> Option<PathBuf> {
    var(name).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// `$HOME`, when it is set to an absolute path.
pub fn home(var: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    absolute(var, "HOME")
}

/// `$XDG_DATA_HOME/argus`, else `~/.local/share/argus`.
pub fn data_root(var: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let data = absolute(var, "XDG_DATA_HOME").or_else(|| Some(home(var)?.join(".local/share")))?;
    Some(data.join("argus"))
}

/// `$XDG_CONFIG_HOME/argus/config.json`, else `~/.config/argus/config.json`.
pub fn config_file(var: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let config = absolute(var, "XDG_CONFIG_HOME").or_else(|| Some(home(var)?.join(".config")))?;
    Some(config.join("argus").join("config.json"))
}

/// `$XDG_RUNTIME_DIR/argus.sock`, else `argus.sock` in the data root.
pub fn socket(var: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    match absolute(var, "XDG_RUNTIME_DIR") {
        Some(run) => Some(run.join("argus.sock")),
        None => Some(data_root(var)?.join("argus.sock")),
    }
}
