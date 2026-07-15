//! Config Loader — parse the plugin's TOML config text into a [`Config`] (AC-14, AC-16, AC-17).
//!
//! Parsing is defensive: malformed or wrong-typed TOML degrades to `Config::default()` rather
//! than panicking (AC-14), and unknown keys are silently ignored so a partial or forward-looking
//! config file still loads the fields it recognizes (AC-16, AC-17). File reading and config-path
//! resolution are later tasks — this module is string-in, struct-out only.

use serde::Deserialize;

/// The built-in **scroll step**: how many lines (or finder list items, or help-overlay lines) the
/// mouse wheel advances per wheel event when the config supplies no valid `scroll_lines`. This is
/// the single source of truth for both the resolver's default and the controller's initial value.
pub const DEFAULT_SCROLL_LINES: u16 = 3;

/// The largest accepted `scroll_lines`. Past ~this many lines per event the wheel just jumps to the
/// pane edge (the content/finder/help views clamp to their bounds), so a larger configured value is
/// clamped down to this rather than taken literally — keeping the setting to sane line-scrolling
/// instead of page-jumping. The effective range is therefore `1..=MAX_SCROLL_LINES`.
pub const MAX_SCROLL_LINES: u16 = 10;

/// The built-in **tree width**: the directory tree column's share of the viewer pane (percent) at
/// startup, when the config supplies no valid `tree_width`. Also the controller's initial
/// `split_pct`. Owner-set to 30 (was 40) so the content pane gets more room out of the box.
pub const DEFAULT_TREE_WIDTH: u16 = 30;
/// The narrowest accepted **tree width**, in percent — the same floor the live keyboard/drag resize
/// enforces, so the tree column can never collapse. A configured value below this clamps up to it.
pub const MIN_TREE_WIDTH: u16 = 20;
/// The widest accepted **tree width**, in percent — the same ceiling the live resize enforces, so
/// the content column can never collapse. A configured value above this clamps down to it. The
/// effective range is therefore `MIN_TREE_WIDTH..=MAX_TREE_WIDTH`.
pub const MAX_TREE_WIDTH: u16 = 80;

/// The built-in **tree column cap**: the maximum width, in character columns, the directory tree may
/// occupy regardless of [`DEFAULT_TREE_WIDTH`]. On a wide pane (a full tab, a big monitor)
/// `tree_width` percent would over-allocate — a 50+-column tree that is mostly blank space after the
/// filenames — so the tree is `min(tree_width% of the pane, tree_max_cols)`. Owner-set to 30: a
/// compact tree that fits typical filenames, so the extra width of a wide pane goes to the content
/// pane. Bites once the pane is wider than ~`tree_max_cols * 100 / tree_width%` columns (~100 at the
/// defaults); below that the percentage governs. A hand resize (drag / grow key) lifts the cap.
pub const DEFAULT_TREE_MAX_COLS: u16 = 30;
/// The narrowest accepted **tree column cap**: a configured value below this clamps up to it, so the
/// tree can never be capped to an unreadable sliver.
pub const MIN_TREE_MAX_COLS: u16 = 10;
/// The widest accepted **tree column cap**. A value this large never bites on any real terminal, so
/// it is the escape hatch for "no cap"; larger values clamp down to it. The effective range is
/// `MIN_TREE_MAX_COLS..=MAX_TREE_MAX_COLS`.
pub const MAX_TREE_MAX_COLS: u16 = 1000;

/// The built-in **content preview line cap**: past this many lines a file (or a large diff) is shown
/// as a truncated preview plus a visible notice (AC-13), not whole. `preview_max_lines` overrides it.
/// Mirrors [`crate::render::Caps::default`]'s line cap so a config-absent run is unchanged; the
/// `render_caps_default_matches_config_defaults` test pins the two together.
pub const DEFAULT_PREVIEW_MAX_LINES: u32 = 10000;
/// The fewest lines a preview cap may be set to — below this the pane could truncate away almost
/// everything, so a smaller configured value clamps up to it.
pub const MIN_PREVIEW_MAX_LINES: u32 = 100;
/// The most lines a preview may show. Bounds the work the render pipeline (external CLI +
/// `ansi-to-tui` parse) does per file so a huge cap cannot wedge the UI; a larger value clamps down.
pub const MAX_PREVIEW_MAX_LINES: u32 = 100_000;

/// The built-in **content preview size cap**, in KiB: past this many kibibytes a file is previewed,
/// not shown whole, and it bounds the actual disk read so a giant/hostile file is never slurped
/// (AC-N1). `preview_max_kib` overrides it. 1024 KiB = 1 MiB. Mirrors [`crate::render::Caps::default`].
pub const DEFAULT_PREVIEW_MAX_KIB: u32 = 1024;
/// The smallest preview size cap (KiB); a smaller configured value clamps up to it.
pub const MIN_PREVIEW_MAX_KIB: u32 = 64;
/// The largest preview size cap (KiB) — 64 MiB. Even the maximum is a finite read, so the
/// bounded-read guarantee (AC-N1) holds at every setting; a larger value clamps down to it.
pub const MAX_PREVIEW_MAX_KIB: u32 = 65_536;

/// Which side of the content pane the directory tree is drawn on (`tree_position` config key). A
/// pure display preference; the config value is a lenient `Option<String>` resolved into this by
/// [`resolve`] (case-insensitive, trimmed), so this enum is never deserialized directly. `Left` is
/// the default (today's layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TreePosition {
    /// Tree on the left of the content pane (the default, today's layout).
    #[default]
    Left,
    /// Tree on the right of the content pane.
    Right,
}

impl TreePosition {
    /// The lowercase label shown in the read-only Settings overlay (`left` / `right`).
    pub fn label(self) -> &'static str {
        match self {
            TreePosition::Left => "left",
            TreePosition::Right => "right",
        }
    }
}

/// A `[keys]` entry's value: the key(s) an intent binds to, written **either** as a single string
/// (`refresh = "g"`) **or** as a TOML array of strings (`nav_up = ["w", "Up"]`). `#[serde(untagged)]`
/// tries the variants in order, so `One(String)` must come first: a bare string deserializes to
/// `One` and an array to `Many` (order verified by `specs/keybinding-registry/probe-keyspec-untagged.txt`).
/// Semantic validation (bindable-key check, replace-semantics, clashes) happens later in the
/// Bindings Resolver (T-5); this type is deserialization-shape only.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum KeySpec {
    One(String),
    Many(Vec<String>),
}

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
    /// Whether quitting with unexported session annotations confirms first. `None` falls back to
    /// `true`: annotations are session-only, so quitting destroys them, and the confirm is the only
    /// thing standing between a stray `q` and losing the batch. Set `false` to quit immediately and
    /// discard them.
    pub confirm_discard: Option<bool>,
    /// The mouse-wheel **scroll step**: how many lines/items each wheel event advances. `None`
    /// falls back to [`DEFAULT_SCROLL_LINES`]; the resolver clamps any present value into
    /// `1..=`[`MAX_SCROLL_LINES`] (`0` would freeze scrolling; a larger value just page-jumps, so it
    /// caps at the max rather than being taken literally). Held as `u32` so even an out-of-`u16`
    /// number still clamps into range instead of tripping the parse; only a negative / non-integer /
    /// astronomically large value fails to parse and degrades the whole config to defaults.
    pub scroll_lines: Option<u32>,
    /// The **tree width**: the directory tree column's share of the viewer pane, in percent, at
    /// startup. `None` falls back to [`DEFAULT_TREE_WIDTH`]; the resolver clamps any present value
    /// into `MIN_TREE_WIDTH..=MAX_TREE_WIDTH`. Held as `u32` (like `scroll_lines`) so an
    /// out-of-`u16` number still clamps into range instead of tripping the parse; only a negative /
    /// non-integer value fails to parse and degrades the whole config to defaults.
    pub tree_width: Option<u32>,
    /// The **tree position**: which side the directory tree is drawn on. A lenient string
    /// (`"left"` / `"right"`, case-insensitive, trimmed) resolved into a [`TreePosition`] by
    /// [`resolve`]; `None` or an unrecognized value falls back to [`TreePosition::Left`].
    pub tree_position: Option<String>,
    /// The **tree column cap**: the maximum tree width in character columns (see
    /// [`DEFAULT_TREE_MAX_COLS`]). `None` falls back to that default; the resolver clamps any present
    /// value into `MIN_TREE_MAX_COLS..=MAX_TREE_MAX_COLS`. Held as `u32` (like `tree_width`) so an
    /// out-of-`u16` number clamps into range instead of tripping the parse; only a negative /
    /// non-integer value fails to parse and degrades the whole config to defaults.
    pub tree_max_cols: Option<u32>,
    /// The **content preview line cap**: past this many lines a file (or a large diff) is shown as a
    /// truncated preview plus a notice, not whole (AC-13). `None` falls back to
    /// [`DEFAULT_PREVIEW_MAX_LINES`]; the resolver clamps a present value into
    /// `MIN_PREVIEW_MAX_LINES..=MAX_PREVIEW_MAX_LINES`. Held as `u32` (like the other numeric keys)
    /// so an out-of-range number still clamps into range instead of tripping the parse.
    pub preview_max_lines: Option<u32>,
    /// The **content preview size cap**, in KiB: past this size a file is previewed, not shown whole,
    /// and it bounds the disk read (AC-N1). `None` falls back to [`DEFAULT_PREVIEW_MAX_KIB`]; the
    /// resolver clamps a present value into `MIN_PREVIEW_MAX_KIB..=MAX_PREVIEW_MAX_KIB`. 1024 = 1 MiB.
    pub preview_max_kib: Option<u32>,
    /// The `[keys]` remapping table: **intent name -> key spec** (T-4, Slice B). `None` when the
    /// config omits `[keys]`. A `BTreeMap` keeps the entries in deterministic order. Rides the
    /// existing defensive `load_config` / `parse_config` with no wiring change: a malformed `[keys]`
    /// table degrades the whole config to defaults via the existing `Malformed` path (AC-13), and a
    /// non-absolute config path is never read (AC-17).
    pub keys: Option<std::collections::BTreeMap<String, KeySpec>>,
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
    // The config path is trusted only when ABSOLUTE. With none of `HERDR_PLUGIN_CONFIG_DIR` /
    // `XDG_CONFIG_HOME` / `HOME` resolvable, `config_path` yields a cwd-relative fallback; reading
    // it would source a "trusted" config from the (possibly untrusted) working directory — a
    // browsed repo could plant `.config/herdr-file-viewer/config.toml` and inject `editor` /
    // `open` / `reveal` commands. Treat a non-absolute path as no config (AC-20 + the viewer's
    // untrusted-repo posture): never read settings from the CWD.
    if !path.is_absolute() {
        return (Config::default(), LoadOutcome::Absent);
    }
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
    /// The effective **confirm-before-discarding-annotations** switch: the config
    /// `confirm_discard` when present, else `true`. Config-or-default (no env var).
    pub confirm_discard: bool,
    /// The effective mouse-wheel **scroll step**: the config `scroll_lines` clamped to
    /// `1..=`[`MAX_SCROLL_LINES`] when present, else [`DEFAULT_SCROLL_LINES`]. No environment
    /// variable participates — this is a config-or-default UI preference.
    pub scroll_lines: u16,
    /// The effective **tree width**: the config `tree_width` clamped to
    /// `MIN_TREE_WIDTH..=MAX_TREE_WIDTH` when present, else [`DEFAULT_TREE_WIDTH`]. Seeds the
    /// controller's startup split percent. Config-or-default (no env var).
    pub tree_width: u16,
    /// The effective **tree position**: the config `tree_position` mapped to `Left`/`Right`, else
    /// [`TreePosition::Left`]. Config-or-default (no env var).
    pub tree_position: TreePosition,
    /// The effective **tree column cap**: the config `tree_max_cols` clamped to
    /// `MIN_TREE_MAX_COLS..=MAX_TREE_MAX_COLS` when present, else [`DEFAULT_TREE_MAX_COLS`]. The tree
    /// is drawn at `min(tree_width% of the pane, tree_max_cols)`. Config-or-default (no env var).
    pub tree_max_cols: u16,
    /// The effective **content preview line cap**: the config `preview_max_lines` clamped to
    /// `MIN_PREVIEW_MAX_LINES..=MAX_PREVIEW_MAX_LINES` when present, else [`DEFAULT_PREVIEW_MAX_LINES`].
    /// Wired into the Content Renderer's [`crate::render::Caps`] via [`Self::preview_caps`].
    /// Config-or-default (no env var).
    pub preview_max_lines: u32,
    /// The effective **content preview size cap**, in KiB: the config `preview_max_kib` clamped to
    /// `MIN_PREVIEW_MAX_KIB..=MAX_PREVIEW_MAX_KIB` when present, else [`DEFAULT_PREVIEW_MAX_KIB`].
    /// Config-or-default (no env var).
    pub preview_max_kib: u32,
}

impl EffectiveSettings {
    /// The Content Renderer size caps (line + byte) derived from the resolved preview settings —
    /// the single place `preview_max_kib` is converted to bytes (× 1024). Wired into
    /// [`crate::render::classify`] / [`crate::render::render`] at startup.
    pub fn preview_caps(&self) -> crate::render::Caps {
        crate::render::Caps {
            max_lines: self.preview_max_lines as usize,
            max_bytes: self.preview_max_kib as u64 * 1024,
        }
    }
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

    // Config > default; no env var. Defaults ON: the confirm only fires when annotations are held,
    // so a session that never annotates never sees it, and the one that does has work to lose.
    let confirm_discard = config.confirm_discard.unwrap_or(true);

    let update_check = match config.update_check {
        Some(b) => b,
        None => get_env("HERDR_FILE_VIEWER_NO_UPDATE_CHECK").is_none(),
    };

    // Config > default; no env var. Clamp to `1..=MAX_SCROLL_LINES`: a configured `0` can never
    // freeze scrolling and an over-large value is capped to a sane line step rather than page-jumping
    // (AC-3). A value that isn't a representable non-negative integer never reaches here — it failed
    // the parse and arrived as `None` on a defaulted `Config` (AC-4).
    let scroll_lines = config
        .scroll_lines
        .map(|n| n.clamp(1, MAX_SCROLL_LINES as u32) as u16)
        .unwrap_or(DEFAULT_SCROLL_LINES);

    // Config > default; no env var. Clamp to `MIN_TREE_WIDTH..=MAX_TREE_WIDTH` — the same range the
    // live keyboard/drag resize enforces — so neither column can collapse (AC-3). A value that isn't
    // a representable non-negative integer never reaches here: it failed the parse and arrived as
    // `None` on a defaulted `Config` (AC-4).
    let tree_width = config
        .tree_width
        .map(|n| n.clamp(MIN_TREE_WIDTH as u32, MAX_TREE_WIDTH as u32) as u16)
        .unwrap_or(DEFAULT_TREE_WIDTH);

    // Config > default; no env var. Lenient string match (trimmed, case-insensitive): only `right`
    // selects the non-default side; anything else — absent, unrecognized, differently-typed — keeps
    // the default `Left` so a fat-fingered value loses the customization without crashing (AC-5..7).
    let tree_position = match config
        .tree_position
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("right") => TreePosition::Right,
        _ => TreePosition::Left,
    };

    // Config > default; no env var. Clamp to `MIN_TREE_MAX_COLS..=MAX_TREE_MAX_COLS` so the cap can
    // never shrink the tree to an unreadable sliver, and a huge value just becomes the effective
    // "no cap" (it never bites on a real terminal). A non-representable value degraded the whole
    // config to defaults and arrived as `None`.
    let tree_max_cols = config
        .tree_max_cols
        .map(|n| n.clamp(MIN_TREE_MAX_COLS as u32, MAX_TREE_MAX_COLS as u32) as u16)
        .unwrap_or(DEFAULT_TREE_MAX_COLS);

    // Config > default; no env var. Clamp to `MIN_PREVIEW_MAX_LINES..=MAX_PREVIEW_MAX_LINES` so a
    // preview can never truncate to near-nothing, and a huge cap can't wedge the render pipeline. A
    // non-representable value degraded the whole config to defaults and arrived as `None`.
    let preview_max_lines = config
        .preview_max_lines
        .map(|n| n.clamp(MIN_PREVIEW_MAX_LINES, MAX_PREVIEW_MAX_LINES))
        .unwrap_or(DEFAULT_PREVIEW_MAX_LINES);

    // Config > default; no env var. Clamp to `MIN_PREVIEW_MAX_KIB..=MAX_PREVIEW_MAX_KIB`: the upper
    // bound keeps the bounded-read guarantee (AC-N1) even at the max, so no configured value can ask
    // the viewer to slurp an arbitrary file whole.
    let preview_max_kib = config
        .preview_max_kib
        .map(|n| n.clamp(MIN_PREVIEW_MAX_KIB, MAX_PREVIEW_MAX_KIB))
        .unwrap_or(DEFAULT_PREVIEW_MAX_KIB);

    EffectiveSettings {
        editor,
        markdown,
        diff,
        syntax,
        open,
        reveal,
        hide_dotfiles,
        update_check,
        confirm_discard,
        scroll_lines,
        tree_width,
        tree_position,
        tree_max_cols,
        preview_max_lines,
        preview_max_kib,
    }
}

/// Settings Applier: the editor hand-off's platform-default layer (AC-6). `eff.editor` already
/// encodes config > `$EDITOR` (T-5's [`resolve`]), so falling back to `platform_default` here
/// yields the full **config > `$EDITOR` > platform-default** precedence chain. Pure: no
/// `std::env`, no FS -- the caller (T-8) supplies `platform_default` from `resolve_editor(None)`.
pub fn effective_editor(
    eff: &EffectiveSettings,
    platform_default: Option<std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    eff.editor.clone().or(platform_default)
}

/// Settings Applier: overlay `eff`'s optional renderer argv overrides onto `base` (the built-in
/// defaults from `app::default_renderers()`), field by field (AC-7).
///
/// `full_diff` derives from the *effective* `diff` rather than being independently overridable:
/// when `eff.diff` is set, the augmentation is whatever `base.full_diff` adds on top of
/// `base.diff` (e.g. `--line-numbers`), appended to the overridden diff argv -- so a custom diff
/// tool still gets a full-file variant. When `eff.diff` is unset, `full_diff` stays at its own
/// base default, unchanged.
///
/// `timeout` is never configurable (NC-5: renderer guards are not exposed) and is always copied
/// from `base`. Pure: no `std::env`, no FS, no globals.
pub fn effective_renderers(
    eff: &EffectiveSettings,
    base: &crate::render::Renderers,
) -> crate::render::Renderers {
    let diff = eff.diff.clone().unwrap_or_else(|| base.diff.clone());
    let full_diff = match &eff.diff {
        Some(overridden_diff) => {
            let augmentation = if base.full_diff.starts_with(base.diff.as_slice()) {
                base.full_diff[base.diff.len()..].to_vec()
            } else {
                Vec::new()
            };
            let mut full = overridden_diff.clone();
            full.extend(augmentation);
            full
        }
        None => base.full_diff.clone(),
    };
    crate::render::Renderers {
        markdown: eff
            .markdown
            .clone()
            .unwrap_or_else(|| base.markdown.clone()),
        diff,
        full_diff,
        syntax: eff.syntax.clone().unwrap_or_else(|| base.syntax.clone()),
        // Document converters are built-in defaults (not user-configurable in v1); carry the
        // base's set through unchanged.
        documents: base.documents.clone(),
        timeout: base.timeout,
    }
}

/// Settings Applier: whether to start the once-a-day update check (AC-10). A pure passthrough of
/// the already-resolved `EffectiveSettings.update_check` (config > env > default, from T-5's
/// [`resolve`]).
pub fn should_start_update_check(eff: &EffectiveSettings) -> bool {
    eff.update_check
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
        assert_eq!(config.confirm_discard, None);
        assert_eq!(config.scroll_lines, None);
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
        assert_eq!(config.scroll_lines, None);
        assert_eq!(outcome, LoadOutcome::Loaded);
    }

    #[test]
    fn bool_fields_parse() {
        let (config, _outcome) =
            parse_config("hide_dotfiles = true\nupdate_check = false\nconfirm_discard = false\n");
        assert_eq!(config.hide_dotfiles, Some(true));
        assert_eq!(config.update_check, Some(false));
        assert_eq!(config.confirm_discard, Some(false));
    }

    #[test]
    fn confirm_discard_resolves_config_over_default() {
        let on = resolve(&Config::default(), |_| None);
        assert!(on.confirm_discard, "absent falls back to on");

        let off = resolve(
            &Config {
                confirm_discard: Some(false),
                ..Default::default()
            },
            |_| None,
        );
        assert!(!off.confirm_discard, "an explicit false wins");

        // No env tier: unlike `update_check`, this is a config-or-default UI preference, so a
        // stray environment variable must not reach it.
        let env_ignored = resolve(&Config::default(), |_| Some("1".to_string()));
        assert!(
            env_ignored.confirm_discard,
            "no environment variable participates in this key"
        );
    }

    // --- [keys] table (T-4, AC-9, AC-13, AC-17) ---

    #[test]
    fn keys_table_parses_string_and_array_specs() {
        // AC-9: a single string binds one key (One), a TOML array binds several (Many). Mirrors the
        // kept probe `specs/keybinding-registry/probe-keyspec-untagged.txt`.
        let (config, outcome) = parse_config("[keys]\nrefresh = \"g\"\nnav_up = [\"w\", \"Up\"]\n");
        assert_eq!(outcome, LoadOutcome::Loaded);
        let keys = config.keys.expect("[keys] table should be present");
        assert_eq!(keys.get("refresh"), Some(&KeySpec::One("g".to_string())));
        assert_eq!(
            keys.get("nav_up"),
            Some(&KeySpec::Many(vec!["w".to_string(), "Up".to_string()])),
        );
    }

    #[test]
    fn keys_wrong_typed_value_yields_default_and_malformed() {
        // AC-13: an integer where a key spec is expected fails the whole parse -> defaults + Malformed.
        let (config, outcome) = parse_config("[keys]\nrefresh = 42\n");
        assert_eq!(config.keys, None);
        match outcome {
            LoadOutcome::Malformed(_) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn keys_invalid_toml_yields_default_and_malformed() {
        // AC-13: invalid TOML in the [keys] section degrades to defaults without panicking.
        let (config, outcome) = parse_config("[keys]\nx = = [");
        assert_eq!(config.keys, None);
        match outcome {
            LoadOutcome::Malformed(_) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn cwd_relative_config_with_keys_table_is_never_read() {
        // AC-17 (untrusted-repo posture): with none of HERDR_PLUGIN_CONFIG_DIR / XDG_CONFIG_HOME /
        // HOME set, `config_path` yields a cwd-relative path. Even though this `read` would return a
        // planted `[keys]` table, `load_config` must treat a non-absolute path as no config, so the
        // planted bindings are never sourced from the (possibly untrusted) working directory.
        let get = |_: &str| None; // no env at all -> non-absolute (cwd-relative) path
        let read = |_: &std::path::Path| Ok("[keys]\nrefresh = \"/evil\"\n".to_string());
        let (config, outcome) = load_config(get, read);
        assert_eq!(
            config.keys, None,
            "a cwd-relative config's [keys] must be ignored"
        );
        assert_eq!(
            outcome,
            LoadOutcome::Absent,
            "a non-absolute config path is treated as no config, not Loaded"
        );
    }

    /// A `get` that resolves `HERDR_PLUGIN_CONFIG_DIR` to an **absolute** dir (the OS temp dir),
    /// so `load_config` reaches the injected `read`. `load_config` treats a non-absolute config
    /// path as "no config" (never reads from the CWD), so these read-outcome tests must anchor to
    /// an absolute path. Cross-platform: `temp_dir()` is absolute on unix and Windows alike.
    fn abs_cfg_get(k: &str) -> Option<String> {
        (k == "HERDR_PLUGIN_CONFIG_DIR")
            .then(|| std::env::temp_dir().to_string_lossy().into_owned())
    }

    #[test]
    fn missing_file_yields_default_and_absent() {
        let get = abs_cfg_get;
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
    fn cwd_relative_config_path_is_never_read_from_the_working_directory() {
        // Security (untrusted-repo posture): with none of HERDR_PLUGIN_CONFIG_DIR / XDG_CONFIG_HOME
        // / HOME set, `config_path` yields a cwd-relative `.config/...` path. `load_config` must
        // NOT read a config from the (possibly untrusted) working directory — even though this
        // `read` would happily return a planted config, the outcome is `Absent` + defaults.
        let get = |_: &str| None; // no env at all → non-absolute (cwd-relative) path
        let read = |_: &std::path::Path| Ok("editor = \"/evil\"\nopen = \"/evil\"\n".to_string());
        let (config, outcome) = load_config(get, read);
        // The planted `editor`/`open` are ignored entirely — defaults, not the CWD file.
        assert_eq!(config.editor, None, "a cwd-relative config must be ignored");
        assert_eq!(config.open, None, "a cwd-relative config must be ignored");
        assert_eq!(
            outcome,
            LoadOutcome::Absent,
            "a non-absolute config path is treated as no config, not Loaded"
        );
    }

    #[test]
    fn present_valid_file_loads() {
        let get = abs_cfg_get;
        let read = |_: &std::path::Path| Ok("editor = \"vim\"\n".to_string());
        let (config, outcome) = load_config(get, read);
        assert_eq!(config.editor, Some("vim".to_string()));
        assert_eq!(outcome, LoadOutcome::Loaded);
    }

    #[test]
    fn present_malformed_file_yields_default_and_malformed() {
        let get = abs_cfg_get;
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
        let get = abs_cfg_get;
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
        assert!(
            effective.confirm_discard,
            "the quit guard defaults ON: annotations are session-only, so the confirm is the only \
             thing between a stray q and losing the batch"
        );
        assert_eq!(effective.markdown, None);
        assert_eq!(effective.diff, None);
        assert_eq!(effective.syntax, None);
        assert_eq!(effective.open, None);
        assert_eq!(effective.reveal, None);
        assert_eq!(effective.scroll_lines, 3);
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
        assert_eq!(effective.scroll_lines, 3);
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

    // --- scroll_lines: the effective scroll step (AC-1..AC-4) ---

    #[test]
    fn scroll_lines_valid_value_parses() {
        // Happy-path deserialize: a valid integer lands in `Config.scroll_lines` as `Some(n)` and
        // the load succeeds (the malformed path is covered by `scroll_lines_non_representable_*`).
        let (config, outcome) = parse_config("scroll_lines = 5\n");
        assert_eq!(config.scroll_lines, Some(5));
        assert_eq!(outcome, LoadOutcome::Loaded);
    }

    #[test]
    fn preview_cap_keys_parse_from_toml() {
        // Proves the TOML key names bind to the fields (the `Config { .. }` resolve tests bypass
        // parsing). Both are plain integers landing in their `Option<u32>` fields.
        let (config, outcome) = parse_config("preview_max_lines = 20000\npreview_max_kib = 4096\n");
        assert_eq!(config.preview_max_lines, Some(20000));
        assert_eq!(config.preview_max_kib, Some(4096));
        assert_eq!(outcome, LoadOutcome::Loaded);
    }

    #[test]
    fn resolve_scroll_lines_config_value_wins() {
        // AC-1: a valid config value (>= 1) is the effective scroll step (config > default).
        let config = Config {
            scroll_lines: Some(5),
            ..Default::default()
        };
        let effective = resolve(&config, |_| None);
        assert_eq!(effective.scroll_lines, 5);
    }

    #[test]
    fn resolve_scroll_lines_defaults_when_absent() {
        // AC-2: omitted -> the built-in default (DEFAULT_SCROLL_LINES = 3).
        let effective = resolve(&Config::default(), |_| None);
        assert_eq!(effective.scroll_lines, DEFAULT_SCROLL_LINES);
        assert_eq!(effective.scroll_lines, 3);
    }

    #[test]
    fn resolve_scroll_lines_zero_clamps_to_one() {
        // AC-3: 0 would freeze scrolling -> clamp to the floor of 1.
        let config = Config {
            scroll_lines: Some(0),
            ..Default::default()
        };
        let effective = resolve(&config, |_| None);
        assert_eq!(effective.scroll_lines, 1);
    }

    #[test]
    fn resolve_scroll_lines_clamps_to_max() {
        // AC-3: any over-large value is capped to MAX_SCROLL_LINES (page-jumping is pointless — the
        // views clamp to their bounds), INCLUDING values beyond u16, which are accepted (the field
        // is u32) and clamped rather than degrading the whole config to defaults.
        assert_eq!(MAX_SCROLL_LINES, 10);
        for over in [11u32, 1000, 100_000, u32::MAX] {
            let cfg = Config {
                scroll_lines: Some(over),
                ..Default::default()
            };
            assert_eq!(
                resolve(&cfg, |_| None).scroll_lines,
                MAX_SCROLL_LINES,
                "value {over} must clamp to the max"
            );
        }
        // The boundary value passes through unchanged.
        let at_max = Config {
            scroll_lines: Some(MAX_SCROLL_LINES as u32),
            ..Default::default()
        };
        assert_eq!(resolve(&at_max, |_| None).scroll_lines, MAX_SCROLL_LINES);
    }

    #[test]
    fn scroll_lines_non_representable_degrades_to_default() {
        // AC-4: a value that isn't a representable non-negative integer (here, negative) fails the
        // parse, so the whole config degrades to defaults (Malformed); the resolver then yields the
        // default step (3). (An over-large-but-representable value clamps instead — see
        // resolve_scroll_lines_clamps_to_max.)
        let (config, outcome) = parse_config("scroll_lines = -1\n");
        assert_eq!(config.scroll_lines, None);
        match outcome {
            LoadOutcome::Malformed(_) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
        let effective = resolve(&config, |_| None);
        assert_eq!(effective.scroll_lines, 3);
    }

    #[test]
    fn resolve_tree_width_config_value_wins() {
        // AC-1: a valid config value (in range) is the effective tree width (config > default).
        let config = Config {
            tree_width: Some(25),
            ..Default::default()
        };
        assert_eq!(resolve(&config, |_| None).tree_width, 25);
    }

    #[test]
    fn resolve_tree_width_defaults_when_absent() {
        // AC-2: omitted -> the built-in default (DEFAULT_TREE_WIDTH = 30).
        let effective = resolve(&Config::default(), |_| None);
        assert_eq!(effective.tree_width, DEFAULT_TREE_WIDTH);
        assert_eq!(effective.tree_width, 30);
    }

    #[test]
    fn resolve_tree_width_clamps_to_range() {
        // AC-3: a value below the floor clamps up to MIN_TREE_WIDTH, above the ceiling clamps down to
        // MAX_TREE_WIDTH — the same range the live resize enforces, so neither column can collapse.
        // Values beyond u16 are accepted (the field is u32) and clamped, not degraded.
        assert_eq!((MIN_TREE_WIDTH, MAX_TREE_WIDTH), (20, 80));
        for (input, want) in [
            (0u32, 20u16),
            (5, 20),
            (19, 20),
            (81, 80),
            (95, 80),
            (u32::MAX, 80),
        ] {
            let cfg = Config {
                tree_width: Some(input),
                ..Default::default()
            };
            assert_eq!(
                resolve(&cfg, |_| None).tree_width,
                want,
                "value {input} must clamp to {want}"
            );
        }
        // The boundary values pass through unchanged.
        for edge in [MIN_TREE_WIDTH, MAX_TREE_WIDTH] {
            let cfg = Config {
                tree_width: Some(edge as u32),
                ..Default::default()
            };
            assert_eq!(resolve(&cfg, |_| None).tree_width, edge);
        }
    }

    #[test]
    fn tree_width_non_representable_degrades_to_default() {
        // AC-4: a non-representable value fails the parse, so the whole config degrades to defaults
        // (Malformed); the resolver then yields the default width (30). AC-4 names both a negative
        // and a string, so cover both — a `u32` field rejects each (an over-large-but-representable
        // value clamps instead — see resolve_tree_width_clamps_to_range).
        // Each fixture also carries a VALID key (`hide_dotfiles = true`) to prove the malformed value
        // defaults the WHOLE config, not just its own field (the valid key is dropped too).
        for src in [
            "tree_width = -1\nhide_dotfiles = true\n",
            "tree_width = \"wide\"\nhide_dotfiles = true\n",
        ] {
            let (config, outcome) = parse_config(src);
            assert_eq!(
                config.tree_width, None,
                "{src:?} must not populate tree_width"
            );
            assert_eq!(
                config.hide_dotfiles, None,
                "{src:?} must default the WHOLE config (the valid hide_dotfiles is dropped too)"
            );
            match outcome {
                LoadOutcome::Malformed(_) => {}
                other => panic!("expected Malformed for {src:?}, got {other:?}"),
            }
            assert_eq!(
                resolve(&config, |_| None).tree_width,
                30,
                "{src:?} → default 30"
            );
        }
    }

    #[test]
    fn resolve_tree_position_config_value_wins() {
        // AC-5: "right" -> Right, "left" -> Left (config > default).
        for (value, want) in [("right", TreePosition::Right), ("left", TreePosition::Left)] {
            let cfg = Config {
                tree_position: Some(value.to_string()),
                ..Default::default()
            };
            assert_eq!(resolve(&cfg, |_| None).tree_position, want);
        }
    }

    #[test]
    fn resolve_tree_position_defaults_when_absent() {
        // AC-6: omitted -> the default side (Left, today's layout).
        assert_eq!(
            resolve(&Config::default(), |_| None).tree_position,
            TreePosition::Left
        );
    }

    #[test]
    fn resolve_tree_position_lenient_and_case_insensitive() {
        // AC-7: an unrecognized value degrades to the default Left without panicking, and a valid
        // value is matched case-insensitively after trimming.
        for (value, want) in [
            ("sideways", TreePosition::Left),
            ("", TreePosition::Left),
            (" RIGHT ", TreePosition::Right),
            ("Left", TreePosition::Left),
            ("RiGhT", TreePosition::Right),
        ] {
            let cfg = Config {
                tree_position: Some(value.to_string()),
                ..Default::default()
            };
            assert_eq!(
                resolve(&cfg, |_| None).tree_position,
                want,
                "{value:?} must resolve to {want:?}"
            );
        }
    }

    #[test]
    fn resolve_tree_max_cols_config_value_wins() {
        // A valid config value (in range) is the effective tree column cap (config > default).
        let cfg = Config {
            tree_max_cols: Some(60),
            ..Default::default()
        };
        assert_eq!(resolve(&cfg, |_| None).tree_max_cols, 60);
    }

    #[test]
    fn resolve_tree_max_cols_defaults_when_absent() {
        // Omitted -> the built-in default (DEFAULT_TREE_MAX_COLS = 30).
        let effective = resolve(&Config::default(), |_| None);
        assert_eq!(effective.tree_max_cols, DEFAULT_TREE_MAX_COLS);
        assert_eq!(effective.tree_max_cols, 30);
    }

    #[test]
    fn resolve_tree_max_cols_clamps_to_range() {
        // Below the floor clamps up (never an unreadable sliver); above the ceiling clamps down (a
        // huge value is the "no cap" escape hatch). Values beyond u16 are accepted (u32 field) and
        // clamped, not degraded.
        assert_eq!((MIN_TREE_MAX_COLS, MAX_TREE_MAX_COLS), (10, 1000));
        for (input, want) in [(0u32, 10u16), (5, 10), (1001, 1000), (u32::MAX, 1000)] {
            let cfg = Config {
                tree_max_cols: Some(input),
                ..Default::default()
            };
            assert_eq!(
                resolve(&cfg, |_| None).tree_max_cols,
                want,
                "value {input} must clamp to {want}"
            );
        }
    }

    #[test]
    fn resolve_preview_max_lines_config_value_wins() {
        // A valid in-range config value is the effective preview line cap (config > default).
        let cfg = Config {
            preview_max_lines: Some(12_000),
            ..Default::default()
        };
        assert_eq!(resolve(&cfg, |_| None).preview_max_lines, 12_000);
    }

    #[test]
    fn resolve_preview_max_lines_defaults_when_absent() {
        let effective = resolve(&Config::default(), |_| None);
        assert_eq!(effective.preview_max_lines, DEFAULT_PREVIEW_MAX_LINES);
        assert_eq!(effective.preview_max_lines, 10000);
    }

    #[test]
    fn resolve_preview_max_lines_clamps_to_range() {
        // Below the floor clamps up (a preview can't shrink to near-nothing); above the ceiling
        // clamps down (a huge cap can't wedge the render pipeline). u32 field so a giant value is
        // clamped, not degraded to default.
        assert_eq!(
            (MIN_PREVIEW_MAX_LINES, MAX_PREVIEW_MAX_LINES),
            (100, 100_000)
        );
        for (input, want) in [
            (0u32, 100u32),
            (99, 100),
            (100_001, 100_000),
            (u32::MAX, 100_000),
        ] {
            let cfg = Config {
                preview_max_lines: Some(input),
                ..Default::default()
            };
            assert_eq!(
                resolve(&cfg, |_| None).preview_max_lines,
                want,
                "value {input} must clamp to {want}"
            );
        }
    }

    #[test]
    fn resolve_preview_max_kib_config_value_wins() {
        let cfg = Config {
            preview_max_kib: Some(4096),
            ..Default::default()
        };
        assert_eq!(resolve(&cfg, |_| None).preview_max_kib, 4096);
    }

    #[test]
    fn resolve_preview_max_kib_defaults_when_absent() {
        let effective = resolve(&Config::default(), |_| None);
        assert_eq!(effective.preview_max_kib, DEFAULT_PREVIEW_MAX_KIB);
        assert_eq!(effective.preview_max_kib, 1024); // 1 MiB
    }

    #[test]
    fn resolve_preview_max_kib_clamps_to_range() {
        // The upper clamp keeps the bounded-read guarantee (AC-N1) at every setting: even the max
        // is a finite read, never "slurp an arbitrary file whole".
        assert_eq!((MIN_PREVIEW_MAX_KIB, MAX_PREVIEW_MAX_KIB), (64, 65_536));
        for (input, want) in [
            (0u32, 64u32),
            (63, 64),
            (65_537, 65_536),
            (u32::MAX, 65_536),
        ] {
            let cfg = Config {
                preview_max_kib: Some(input),
                ..Default::default()
            };
            assert_eq!(
                resolve(&cfg, |_| None).preview_max_kib,
                want,
                "value {input} must clamp to {want}"
            );
        }
    }

    #[test]
    fn preview_caps_converts_kib_to_bytes() {
        // The single conversion point: preview_max_kib × 1024 → Caps.max_bytes; lines pass through.
        let cfg = Config {
            preview_max_lines: Some(3000),
            preview_max_kib: Some(512),
            ..Default::default()
        };
        let caps = resolve(&cfg, |_| None).preview_caps();
        assert_eq!(caps.max_lines, 3000);
        assert_eq!(caps.max_bytes, 512 * 1024);
    }

    #[test]
    fn render_caps_default_matches_config_defaults() {
        // Lockstep guard: render::Caps::default() (used by the width-less/help paths and tests) must
        // equal what a config-absent resolve produces, so a config-less run behaves identically no
        // matter which side computes the caps. If someone changes one default, this fails.
        let from_config = resolve(&Config::default(), |_| None).preview_caps();
        assert_eq!(crate::render::Caps::default(), from_config);
    }

    #[test]
    fn tree_max_cols_non_representable_degrades_to_default() {
        // A non-representable value fails the parse, degrading the whole config to defaults; the
        // resolver then yields the default cap (30).
        // A valid key rides alongside to prove the whole config defaults, not just this field.
        for src in [
            "tree_max_cols = -1\nhide_dotfiles = true\n",
            "tree_max_cols = \"wide\"\nhide_dotfiles = true\n",
        ] {
            let (config, outcome) = parse_config(src);
            assert_eq!(
                config.tree_max_cols, None,
                "{src:?} must not populate the field"
            );
            assert_eq!(
                config.hide_dotfiles, None,
                "{src:?} must default the WHOLE config (valid hide_dotfiles dropped too)"
            );
            match outcome {
                LoadOutcome::Malformed(_) => {}
                other => panic!("expected Malformed for {src:?}, got {other:?}"),
            }
            assert_eq!(
                resolve(&config, |_| None).tree_max_cols,
                30,
                "{src:?} → default 30"
            );
        }
    }

    // --- Settings Applier: effective_editor / effective_renderers / should_start_update_check
    // (T-7, AC-6, AC-7, AC-10) ---

    /// Mirrors `app::default_renderers()`'s shape (markdown/syntax/diff/full_diff argv), so the
    /// override/derive logic can be tested without reaching into `app.rs` (T-7 stays confined to
    /// `config.rs`).
    fn test_base_renderers() -> crate::render::Renderers {
        crate::render::Renderers {
            markdown: vec!["glow".to_string(), "-".to_string()],
            diff: vec!["delta".to_string()],
            full_diff: vec!["delta".to_string(), "--line-numbers".to_string()],
            syntax: vec!["bat".to_string(), "-".to_string()],
            documents: crate::document::DocConverters::defaults(),
            timeout: std::time::Duration::from_secs(2),
        }
    }

    #[test]
    fn effective_editor_config_wins_over_platform_default() {
        let eff = EffectiveSettings {
            editor: Some(std::ffi::OsString::from("nano")),
            ..resolve(&Config::default(), |_| None)
        };
        assert_eq!(
            effective_editor(&eff, Some(std::ffi::OsString::from("notepad"))),
            Some(std::ffi::OsString::from("nano"))
        );
    }

    #[test]
    fn effective_editor_falls_back_to_platform_default_when_unset() {
        let eff = resolve(&Config::default(), |_| None);
        assert_eq!(eff.editor, None);
        assert_eq!(
            effective_editor(&eff, Some(std::ffi::OsString::from("notepad"))),
            Some(std::ffi::OsString::from("notepad"))
        );
    }

    #[test]
    fn effective_renderers_markdown_override_leaves_others_at_base() {
        let base = test_base_renderers();
        let eff = EffectiveSettings {
            markdown: Some(vec!["mdcat".to_string()]),
            ..resolve(&Config::default(), |_| None)
        };
        let result = effective_renderers(&eff, &base);
        assert_eq!(result.markdown, vec!["mdcat".to_string()]);
        assert_eq!(result.syntax, base.syntax);
        assert_eq!(result.diff, base.diff);
        assert_eq!(result.timeout, base.timeout);
    }

    #[test]
    fn effective_renderers_full_diff_derives_from_overridden_diff() {
        let base = test_base_renderers();
        let eff = EffectiveSettings {
            diff: Some(vec!["mydiff".to_string(), "-u".to_string()]),
            ..resolve(&Config::default(), |_| None)
        };
        let result = effective_renderers(&eff, &base);
        assert_eq!(
            result.full_diff,
            vec![
                "mydiff".to_string(),
                "-u".to_string(),
                "--line-numbers".to_string()
            ]
        );
    }

    #[test]
    fn effective_renderers_full_diff_stays_at_base_when_diff_not_overridden() {
        let base = test_base_renderers();
        let eff = resolve(&Config::default(), |_| None);
        assert_eq!(eff.diff, None);
        let result = effective_renderers(&eff, &base);
        assert_eq!(result.full_diff, base.full_diff);
        assert_eq!(result.diff, base.diff);
    }

    #[test]
    fn should_start_update_check_reflects_eff_update_check() {
        let mut eff = resolve(&Config::default(), |_| None);
        eff.update_check = false;
        assert!(!should_start_update_check(&eff));
        eff.update_check = true;
        assert!(should_start_update_check(&eff));
    }
}
