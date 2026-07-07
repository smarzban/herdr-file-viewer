//! Config Loader — parse the plugin's TOML config text into a [`Config`] (AC-14, AC-16, AC-17).
//!
//! Parsing is defensive: malformed or wrong-typed TOML degrades to `Config::default()` rather
//! than panicking (AC-14), and unknown keys are silently ignored so a partial or forward-looking
//! config file still loads the fields it recognizes (AC-16, AC-17). File reading and config-path
//! resolution are later tasks — this module is string-in, struct-out only.

use serde::Deserialize;

/// The shape of the plugin's TOML config file. Every field is optional so a partial config
/// loads only the fields it recognizes; unknown keys are ignored by default (no
/// `deny_unknown_fields`), so a forward-looking or partially-understood config file still
/// parses cleanly (AC-16, AC-17).
#[derive(Deserialize, Default, Debug, Clone)]
pub struct Config {
    pub editor: Option<String>,
    pub markdown: Option<String>,
    pub diff: Option<String>,
    pub syntax: Option<String>,
    pub open: Option<String>,
    pub reveal: Option<String>,
    pub hide_dotfiles: Option<bool>,
    pub update_check: Option<bool>,
}

/// How a config load went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    /// Parsed successfully (including an empty or all-unknown-keys input).
    Loaded,
    /// No config source was found (later tasks; unused by `parse_config` itself).
    Absent,
    /// The input was present but failed to parse; carries a short reason.
    Malformed(String),
}

/// Pure parser: TOML text -> `(Config, LoadOutcome)`. Never panics (AC-14) -- malformed or
/// wrong-typed input degrades to `Config::default()` with a `Malformed` outcome rather than
/// propagating an error or aborting.
pub fn parse_config(s: &str) -> (Config, LoadOutcome) {
    match toml::from_str::<Config>(s) {
        Ok(config) => (config, LoadOutcome::Loaded),
        Err(e) => (Config::default(), LoadOutcome::Malformed(e.to_string())),
    }
}

/// Resolve the plugin's config file path from the environment, via an **injected getter** so
/// the resolution logic is testable without touching process env (mirrors the
/// env-var-with-fallback idiom in `herdr::resolve_program` / `host::parse_context`).
///
/// Precedence: `HERDR_PLUGIN_CONFIG_DIR` (non-empty) wins outright; otherwise fall back to the
/// XDG-style `$XDG_CONFIG_HOME/herdr-file-viewer/config.toml`, or `$HOME/.config/herdr-file-viewer/config.toml`,
/// or (no HOME) the relative `.config/herdr-file-viewer/config.toml` as a last resort. Empty-string
/// env values are treated as absent, same as `host::parse_context` does for its context fields.
pub fn config_path(get: impl Fn(&str) -> Option<String>) -> std::path::PathBuf {
    if let Some(dir) = get("HERDR_PLUGIN_CONFIG_DIR").filter(|s| !s.is_empty()) {
        return std::path::PathBuf::from(dir).join("config.toml");
    }
    let base = if let Some(xdg) = get("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        std::path::PathBuf::from(xdg)
    } else if let Some(home) = get("HOME").filter(|s| !s.is_empty()) {
        std::path::PathBuf::from(home).join(".config")
    } else {
        std::path::PathBuf::from(".config")
    };
    base.join("herdr-file-viewer").join("config.toml")
}

/// Thin convenience wrapper over [`config_path`] using real process env (untested by unit tests;
/// `config_path` is the tested unit).
pub fn config_path_from_env() -> std::path::PathBuf {
    config_path(|k| std::env::var(k).ok())
}

/// File-loading layer over [`config_path`] (T-2) and [`parse_config`] (T-1), with the filesystem
/// **injected** via `get`/`read` so it is hermetic and testable without touching the real
/// filesystem. This is the sole trust boundary for config loading (AC-20): it only reads, never
/// writes, creates, or modifies anything, and it never panics or propagates an error — every path
/// returns a `(Config, LoadOutcome)`.
///
/// - A missing file (`NotFound`) is the normal no-config case (AC-13): `(default, Absent)`.
/// - Any other read error (permission denied, etc.) degrades to `(default, Malformed(..))` rather
///   than surfacing the error.
/// - A present file is handed to [`parse_config`], so a present-but-malformed file yields
///   `(default, Malformed(..))` there too.
pub fn load_config(
    get: impl Fn(&str) -> Option<String>,
    read: impl Fn(&std::path::Path) -> std::io::Result<String>,
) -> (Config, LoadOutcome) {
    let path = config_path(get);
    match read(&path) {
        Ok(contents) => parse_config(&contents),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (Config::default(), LoadOutcome::Absent)
        }
        Err(e) => (Config::default(), LoadOutcome::Malformed(e.to_string())),
    }
}

/// Thin convenience wrapper over [`load_config`] using real process env and filesystem (untested
/// by unit tests; `load_config` is the tested unit). Used by later tasks (T-8).
pub fn load_config_from_env() -> (Config, LoadOutcome) {
    load_config(|k| std::env::var(k).ok(), |p| std::fs::read_to_string(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_env_wins() {
        let get = |k: &str| match k {
            "HERDR_PLUGIN_CONFIG_DIR" => Some("/x/cfg".to_string()),
            _ => None,
        };
        assert_eq!(
            config_path(get),
            std::path::PathBuf::from("/x/cfg/config.toml")
        );
    }

    #[test]
    fn xdg_config_home_fallback_when_config_dir_absent() {
        let get = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("/x/xdg".to_string()),
            _ => None,
        };
        assert_eq!(
            config_path(get),
            std::path::PathBuf::from("/x/xdg/herdr-file-viewer/config.toml")
        );
    }

    #[test]
    fn home_fallback_when_config_dir_and_xdg_absent() {
        let get = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(
            config_path(get),
            std::path::PathBuf::from("/home/u/.config/herdr-file-viewer/config.toml")
        );
    }

    #[test]
    fn empty_config_dir_falls_through_to_xdg_fallback() {
        let get = |k: &str| match k {
            "HERDR_PLUGIN_CONFIG_DIR" => Some("".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(
            config_path(get),
            std::path::PathBuf::from("/home/u/.config/herdr-file-viewer/config.toml")
        );
    }

    #[test]
    fn partial_input_loads_known_field_ignores_unknown() {
        let (config, outcome) = parse_config("editor = \"code --wait\"\nunknown_key = 42\n");
        assert_eq!(config.editor, Some("code --wait".to_string()));
        assert_eq!(config.markdown, None);
        assert_eq!(config.diff, None);
        assert_eq!(config.syntax, None);
        assert_eq!(config.open, None);
        assert_eq!(config.reveal, None);
        assert_eq!(config.hide_dotfiles, None);
        assert_eq!(config.update_check, None);
        assert_eq!(outcome, LoadOutcome::Loaded);
    }

    #[test]
    fn invalid_toml_yields_default_and_malformed() {
        let (config, outcome) = parse_config("not = = valid [");
        assert_eq!(config.editor, None);
        match outcome {
            LoadOutcome::Malformed(_) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn wrong_typed_value_yields_default_and_malformed() {
        let (config, outcome) = parse_config("editor = 123\n");
        assert_eq!(config.editor, None);
        match outcome {
            LoadOutcome::Malformed(_) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn empty_input_yields_default_and_loaded() {
        let (config, outcome) = parse_config("");
        assert_eq!(config.editor, None);
        assert_eq!(config.markdown, None);
        assert_eq!(config.diff, None);
        assert_eq!(config.syntax, None);
        assert_eq!(config.open, None);
        assert_eq!(config.reveal, None);
        assert_eq!(config.hide_dotfiles, None);
        assert_eq!(config.update_check, None);
        assert_eq!(outcome, LoadOutcome::Loaded);
    }

    #[test]
    fn bool_fields_parse() {
        let (config, _outcome) = parse_config("hide_dotfiles = true\nupdate_check = false\n");
        assert_eq!(config.hide_dotfiles, Some(true));
        assert_eq!(config.update_check, Some(false));
    }

    #[test]
    fn missing_file_yields_default_and_absent() {
        let get = |_: &str| None;
        let read = |_: &std::path::Path| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such file",
            ))
        };
        let (config, outcome) = load_config(get, read);
        assert_eq!(config.editor, None);
        assert_eq!(outcome, LoadOutcome::Absent);
    }

    #[test]
    fn present_valid_file_loads() {
        let get = |_: &str| None;
        let read = |_: &std::path::Path| Ok("editor = \"vim\"\n".to_string());
        let (config, outcome) = load_config(get, read);
        assert_eq!(config.editor, Some("vim".to_string()));
        assert_eq!(outcome, LoadOutcome::Loaded);
    }

    #[test]
    fn present_malformed_file_yields_default_and_malformed() {
        let get = |_: &str| None;
        let read = |_: &std::path::Path| Ok("bad = = [".to_string());
        let (config, outcome) = load_config(get, read);
        assert_eq!(config.editor, None);
        match outcome {
            LoadOutcome::Malformed(_) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn read_error_other_than_not_found_yields_default_and_malformed() {
        let get = |_: &str| None;
        let read = |_: &std::path::Path| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            ))
        };
        let (config, outcome) = load_config(get, read);
        assert_eq!(config.editor, None);
        match outcome {
            LoadOutcome::Malformed(_) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
