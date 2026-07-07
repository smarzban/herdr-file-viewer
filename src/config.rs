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

#[cfg(test)]
mod tests {
    use super::*;

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
}
