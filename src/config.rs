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

/// The fully-resolved, downstream-ready settings after applying the config > env > default
/// precedence (AC-3, AC-4, AC-5). `None` on a renderer/opener field means "use the built-in
/// default"; `None` on `editor` means no editor is configured at all (a platform default, if
/// any, is applied later at wiring time — T-8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSettings {
    pub editor: Option<std::ffi::OsString>,
    pub markdown: Option<Vec<String>>,
    pub diff: Option<Vec<String>>,
    pub syntax: Option<Vec<String>>,
    pub open: Option<Vec<String>>,
    pub reveal: Option<Vec<String>>,
    pub hide_dotfiles: bool,
    pub update_check: bool,
}

/// Pure resolver: `Config` + injected env getter -> `EffectiveSettings` (AC-3, AC-4, AC-5,
/// AC-12, AC-16). Reads env ONLY via `get_env` (no `std::env`), touches no filesystem or global
/// state, and never panics -- a total function (AC-21). Command-string fields are tokenized via
/// [`crate::editor::tokenize_command`] into argv, never a shell string (AC-12).
pub fn resolve(config: &Config, get_env: impl Fn(&str) -> Option<String>) -> EffectiveSettings {
    let editor = config
        .editor
        .clone()
        .map(std::ffi::OsString::from)
        .or_else(|| {
            get_env("EDITOR")
                .filter(|s| !s.is_empty())
                .map(std::ffi::OsString::from)
        });

    let markdown = config
        .markdown
        .as_deref()
        .map(crate::editor::tokenize_command);
    let diff = config.diff.as_deref().map(crate::editor::tokenize_command);
    let syntax = config
        .syntax
        .as_deref()
        .map(crate::editor::tokenize_command);
    let open = config.open.as_deref().map(crate::editor::tokenize_command);
    let reveal = config
        .reveal
        .as_deref()
        .map(crate::editor::tokenize_command);

    let hide_dotfiles = config.hide_dotfiles.unwrap_or(false);

    let update_check = match config.update_check {
        Some(b) => b,
        None => get_env("HERDR_FILE_VIEWER_NO_UPDATE_CHECK").is_none(),
    };

    EffectiveSettings {
        editor,
        markdown,
        diff,
        syntax,
        open,
        reveal,
        hide_dotfiles,
        update_check,
    }
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

    #[test]
    fn tokenize_command_is_reachable_via_crate_path_and_quote_aware() {
        assert_eq!(
            crate::editor::tokenize_command("code --wait"),
            vec!["code", "--wait"]
        );
        assert_eq!(
            crate::editor::tokenize_command("\"/a b/c\" -x"),
            vec!["/a b/c", "-x"]
        );
    }

    // --- resolve: AC-3 config wins over env and default ---

    #[test]
    fn resolve_config_editor_wins_over_env() {
        let config = Config {
            editor: Some("nano".to_string()),
            ..Default::default()
        };
        let get_env = |k: &str| match k {
            "EDITOR" => Some("vim".to_string()),
            _ => None,
        };
        let effective = resolve(&config, get_env);
        assert_eq!(effective.editor, Some(std::ffi::OsString::from("nano")));
    }

    #[test]
    fn resolve_config_update_check_wins_over_env() {
        let config = Config {
            update_check: Some(true),
            ..Default::default()
        };
        let get_env = |k: &str| match k {
            "HERDR_FILE_VIEWER_NO_UPDATE_CHECK" => Some("1".to_string()),
            _ => None,
        };
        let effective = resolve(&config, get_env);
        assert!(effective.update_check);
    }

    #[test]
    fn resolve_config_markdown_wins_over_default() {
        let config = Config {
            markdown: Some("mdcat --x".to_string()),
            ..Default::default()
        };
        let effective = resolve(&config, |_| None);
        assert_eq!(
            effective.markdown,
            Some(vec!["mdcat".to_string(), "--x".to_string()])
        );
    }

    // --- resolve: AC-4 env fallback over default, when config omits ---

    #[test]
    fn resolve_env_editor_fallback_when_config_absent() {
        let config = Config::default();
        let get_env = |k: &str| match k {
            "EDITOR" => Some("vim".to_string()),
            _ => None,
        };
        let effective = resolve(&config, get_env);
        assert_eq!(effective.editor, Some(std::ffi::OsString::from("vim")));
    }

    #[test]
    fn resolve_env_no_update_check_fallback_when_config_absent() {
        let config = Config::default();
        let get_env = |k: &str| match k {
            "HERDR_FILE_VIEWER_NO_UPDATE_CHECK" => Some("1".to_string()),
            _ => None,
        };
        let effective = resolve(&config, get_env);
        assert!(!effective.update_check);
    }

    // --- resolve: AC-5 default when neither config nor env set ---

    #[test]
    fn resolve_defaults_when_config_and_env_absent() {
        let config = Config::default();
        let effective = resolve(&config, |_| None);
        assert_eq!(effective.editor, None);
        assert!(effective.update_check);
        assert!(!effective.hide_dotfiles);
        assert_eq!(effective.markdown, None);
        assert_eq!(effective.diff, None);
        assert_eq!(effective.syntax, None);
        assert_eq!(effective.open, None);
        assert_eq!(effective.reveal, None);
    }

    // --- resolve: AC-16 partial config -- unset fields fall to their own default ---

    #[test]
    fn resolve_partial_config_only_sets_specified_field() {
        let config = Config {
            editor: Some("code".to_string()),
            ..Default::default()
        };
        let effective = resolve(&config, |_| None);
        assert_eq!(effective.editor, Some(std::ffi::OsString::from("code")));
        assert!(effective.update_check);
        assert!(!effective.hide_dotfiles);
        assert_eq!(effective.markdown, None);
        assert_eq!(effective.diff, None);
        assert_eq!(effective.syntax, None);
        assert_eq!(effective.open, None);
        assert_eq!(effective.reveal, None);
    }

    // --- resolve: AC-12 tokenized argv, no shell ---

    #[test]
    fn resolve_open_tokenizes_to_distinct_argv() {
        let config = Config {
            open: Some("myopen --flag a".to_string()),
            ..Default::default()
        };
        let effective = resolve(&config, |_| None);
        assert_eq!(
            effective.open,
            Some(vec![
                "myopen".to_string(),
                "--flag".to_string(),
                "a".to_string()
            ])
        );
    }

    #[test]
    fn resolve_reveal_tokenizes_quoted_path() {
        let config = Config {
            reveal: Some("\"/a b/c\" -R".to_string()),
            ..Default::default()
        };
        let effective = resolve(&config, |_| None);
        assert_eq!(
            effective.reveal,
            Some(vec!["/a b/c".to_string(), "-R".to_string()])
        );
    }

    #[test]
    fn resolve_empty_string_editor_env_treated_as_absent() {
        let config = Config::default();
        let get_env = |k: &str| match k {
            "EDITOR" => Some("".to_string()),
            _ => None,
        };
        let effective = resolve(&config, get_env);
        assert_eq!(effective.editor, None);
    }
}
