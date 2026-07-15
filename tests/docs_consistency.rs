//! Docs definition-of-done checks: the user-facing docs actually carry the surface they document.
//!
//! Cheap, hermetic assertions that the canonical docs stay in sync with the code/config:
//! `docs/keys.md` documents the key surface (e.g. the `L` line-select and `O`/`R` hand-off keys),
//! `docs/configuration.md` documents the config file + `[keys]` remapping, the bundled
//! `config.example.toml` carries a commented assignment for every config key, the front-door README
//! links out to the reference docs, and the CHANGELOG has the release entry for line-select. These
//! guard the "docs match the feature in the same PR" rule so a future edit can't silently drop the
//! surface from the docs.
//!
//! (The `docs/keys.md` `## Keys` table is *additionally* checked against the keybinding registry in
//! a `src/input.rs` unit test — `keys_doc_table_documents_every_registry_action_ac21` — which can
//! see the `pub(crate)` registry an integration test cannot.)

const README: &str = include_str!("../README.md");
const KEYS_DOC: &str = include_str!("../docs/keys.md");
const CONFIG_DOC: &str = include_str!("../docs/configuration.md");
const CHANGELOG: &str = include_str!("../CHANGELOG.md");
const CONFIG_EXAMPLE: &str = include_str!("../config.example.toml");

/// Whether `example` has a commented-out TOML assignment for `key` (a line that, after its leading
/// `#`, reads `key = ...`). Stronger than a bare substring: the key must appear as an actual
/// (commented) assignment, not merely as a word in prose.
fn has_commented_assignment(example: &str, key: &str) -> bool {
    example.lines().any(|l| {
        l.trim_start()
            .strip_prefix('#')
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix(key))
            .map(|after| after.trim_start().starts_with('='))
            .unwrap_or(false)
    })
}

#[test]
fn config_example_documents_every_config_key() {
    // Anti-drift: the bundled `config.example.toml` template must carry a commented-out ASSIGNMENT
    // for every scalar config key and the `[keys]` table header, so adding a `Config` field (or
    // demoting a key to prose only) without documenting it in the example fails the build. Keep this
    // list in lockstep with `Config`'s fields in `src/config.rs`.
    for key in [
        "editor",
        "markdown",
        "diff",
        "syntax",
        "open",
        "reveal",
        "hide_dotfiles",
        "update_check",
        "confirm_discard",
        "scroll_lines",
        "tree_width",
        "tree_position",
        "tree_max_cols",
    ] {
        assert!(
            has_commented_assignment(CONFIG_EXAMPLE, key),
            "config.example.toml must carry a commented-out `{key} = ...` assignment (not just prose)"
        );
    }
    assert!(
        CONFIG_EXAMPLE.lines().any(|l| l.trim() == "#[keys]"),
        "config.example.toml must carry the commented-out `[keys]` table header"
    );
    // The renderer stdin contract is the load-bearing correctness note (a custom renderer must read
    // stdin, e.g. glow/bat need a trailing `-`); pin that it is documented.
    assert!(
        CONFIG_EXAMPLE.contains("stdin"),
        "config.example.toml must document that renderers receive content on stdin"
    );
    // It must tell users to rename the copy to config.toml.
    assert!(
        CONFIG_EXAMPLE.contains("config.toml") && CONFIG_EXAMPLE.to_lowercase().contains("rename"),
        "config.example.toml must tell users to rename it to config.toml"
    );
    // Every setting line is commented out, so copying the file verbatim changes nothing: there must
    // be no active (uncommented) TOML assignment or table header.
    for (n, line) in CONFIG_EXAMPLE.lines().enumerate() {
        let t = line.trim_start();
        let active = !t.is_empty() && !t.starts_with('#');
        assert!(
            !active,
            "config.example.toml line {} must be commented out (got: {line:?})",
            n + 1
        );
    }
}

#[test]
fn configuration_doc_and_example_document_scroll_lines() {
    // AC-10: the mouse-wheel scroll-speed key must be documented in BOTH the configuration reference
    // and the bundled config.example.toml, so the feature ships with a discoverable, copy-pasteable
    // setting.
    assert!(
        CONFIG_DOC.contains("scroll_lines"),
        "docs/configuration.md must document the `scroll_lines` config key"
    );
    assert!(
        CONFIG_EXAMPLE.contains("scroll_lines"),
        "config.example.toml must document the `scroll_lines` config key"
    );
}

#[test]
fn configuration_doc_and_example_document_tree_layout() {
    // AC-13: the tree layout config keys must be documented in BOTH the configuration reference and
    // the bundled config.example.toml, so the feature ships with discoverable, copy-pasteable
    // settings.
    for key in ["tree_width", "tree_position", "tree_max_cols"] {
        assert!(
            CONFIG_DOC.contains(key),
            "docs/configuration.md must document the `{key}` config key"
        );
        assert!(
            CONFIG_EXAMPLE.contains(key),
            "config.example.toml must document the `{key}` config key"
        );
    }
}

#[test]
fn configuration_doc_points_to_the_config_example_template() {
    // The configuration reference must point users at the bundled template and tell them to rename
    // the copy to config.toml.
    assert!(
        CONFIG_DOC.contains("config.example.toml"),
        "docs/configuration.md must point users at config.example.toml"
    );
    assert!(
        CONFIG_DOC.contains("config.toml") && CONFIG_DOC.to_lowercase().contains("rename"),
        "docs/configuration.md must tell users to rename the copy to config.toml"
    );
}

#[test]
fn keys_doc_documents_line_select_key() {
    assert!(
        KEYS_DOC.contains("line-select"),
        "docs/keys.md must document the `L` line-select mode"
    );
    assert!(
        KEYS_DOC.contains("`L`"),
        "docs/keys.md must mention the `L` key for line-select"
    );
}

#[test]
fn keys_doc_documents_reveal_open_keys() {
    assert!(
        KEYS_DOC.contains("`O`"),
        "docs/keys.md must document the `O` open-with-default-app key"
    );
    assert!(
        KEYS_DOC.contains("`R`"),
        "docs/keys.md must document the `R` reveal-in-file-manager key"
    );
    let lower = KEYS_DOC.to_lowercase();
    assert!(
        lower.contains("open with default app"),
        "docs/keys.md `## Keys` must describe the `O` key as 'open with default app'"
    );
    assert!(
        lower.contains("reveal"),
        "docs/keys.md must describe the `R` key as 'reveal'"
    );
    assert!(
        lower.contains("file manager"),
        "docs/keys.md must describe the `R` key as revealing in the OS 'file manager'"
    );
}

#[test]
fn configuration_doc_documents_config_file() {
    // The configuration reference must document the config file: its path (herdr-provided + XDG
    // fallback) and every key.
    assert!(
        CONFIG_DOC.contains("config.toml"),
        "docs/configuration.md must name the config file config.toml"
    );
    assert!(
        CONFIG_DOC.contains("HERDR_PLUGIN_CONFIG_DIR"),
        "docs/configuration.md must name the herdr config-dir env var"
    );
    // XDG fallback location:
    assert!(
        CONFIG_DOC.contains(".config/herdr-file-viewer") || CONFIG_DOC.contains("XDG_CONFIG_HOME"),
        "docs/configuration.md must document the XDG fallback location"
    );
    for key in [
        "editor",
        "markdown",
        "diff",
        "syntax",
        "open",
        "reveal",
        "hide_dotfiles",
        "update_check",
    ] {
        assert!(
            CONFIG_DOC.contains(key),
            "docs/configuration.md must document the `{key}` key"
        );
    }
}

#[test]
fn configuration_doc_documents_keys_remapping() {
    // AC-22: the configuration reference must document the `[keys]` remapping surface -- that a
    // binding is written `intent_name = <key spec>` (a string AND an array example), that only
    // modifier-free keys are bindable (no Ctrl/Alt), and that a `[keys]` value replaces the action's
    // default keys.
    assert!(
        CONFIG_DOC.contains("[keys]"),
        "docs/configuration.md must name the `[keys]` remapping table"
    );
    // The `intent_name = <key spec>` form, shown by example in BOTH the string and the array shape.
    assert!(
        CONFIG_DOC.contains("refresh = \"g\""),
        "docs/configuration.md must show a single-string key spec (refresh = \"g\")"
    );
    assert!(
        CONFIG_DOC.contains("nav_up = [\"w\", \"Up\"]"),
        "docs/configuration.md must show an array key spec (nav_up = [\"w\", \"Up\"])"
    );
    // Only modifier-free keys are bindable: no Ctrl / Alt chords.
    assert!(
        CONFIG_DOC.contains("Ctrl") && CONFIG_DOC.contains("Alt"),
        "docs/configuration.md must state that Ctrl/Alt chords are not bindable"
    );
    // Precedence: a `[keys]` value replaces/overrides the action's default keys.
    assert!(
        CONFIG_DOC.to_lowercase().contains("replace"),
        "docs/configuration.md must state a `[keys]` value replaces the default keys"
    );
}

#[test]
fn readme_links_to_the_reference_docs() {
    // The slimmed front-door README must route readers to the moved reference pages, so the detail
    // that used to live inline is still one click away (and the link check keeps those targets real).
    for target in [
        "docs/keys.md",
        "docs/configuration.md",
        "docs/usage.md",
        "docs/README.md",
    ] {
        assert!(
            README.contains(target),
            "README.md must link to `{target}` so the reference docs are discoverable"
        );
    }
}

#[test]
fn changelog_documents_line_reference_release() {
    // The feature shipped in `[1.9.0]`; that section is its permanent CHANGELOG home. Slice from
    // its heading to the next release heading so the check stays anchored to this release's block.
    let start = CHANGELOG
        .find("## [1.9.0]")
        .expect("CHANGELOG.md must carry the `## [1.9.0]` section that introduced line-select");
    let rest = &CHANGELOG[start + "## [1.9.0]".len()..];
    let end = rest.find("\n## [").unwrap_or(rest.len());
    let section = &rest[..end];
    assert!(
        section.contains("### Added"),
        "the `## [1.9.0]` section must have an `### Added` heading (Keep-a-Changelog)"
    );
    assert!(
        section.to_lowercase().contains("line reference")
            || section.to_lowercase().contains("line-select"),
        "the `## [1.9.0]` `### Added` block must document the copy-line-reference feature"
    );
}
