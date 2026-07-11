//! Docs definition-of-done checks for the copy-line-reference feature (T-10).
//!
//! Cheap, hermetic assertions that the user-facing docs actually carry the line-select surface:
//! the README `## Keys` table documents the `L` line-select key, and the CHANGELOG has a
//! `### Added` entry for it under the release that introduced it (`[1.9.0]`). These guard the
//! "docs match the feature in the same PR" rule so a future edit can't silently drop the key from
//! the front-door docs.

const README: &str = include_str!("../README.md");
const CHANGELOG: &str = include_str!("../CHANGELOG.md");
const CONFIG_EXAMPLE: &str = include_str!("../config.example.toml");

#[test]
fn config_example_documents_every_config_key() {
    // Anti-drift: the bundled `config.example.toml` template must mention every config key (and the
    // `[keys]` remap table), so adding a config field without documenting it in the example fails
    // the build. Keep this list in lockstep with `Config`'s fields in `src/config.rs`.
    for key in [
        "editor",
        "markdown",
        "diff",
        "syntax",
        "open",
        "reveal",
        "hide_dotfiles",
        "update_check",
        "[keys]",
    ] {
        assert!(
            CONFIG_EXAMPLE.contains(key),
            "config.example.toml must document the `{key}` config key"
        );
    }
    // It must state where the file goes and that it is renamed to config.toml.
    assert!(
        CONFIG_EXAMPLE.contains("config.toml"),
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
fn readme_points_to_the_config_example_template() {
    // The README's Configuration section must point users at the bundled template.
    let start = README
        .find("## Configuration")
        .expect("README.md must carry a `## Configuration` section");
    let rest = &README[start..];
    let end = rest[1..].find("\n## ").map(|i| i + 1).unwrap_or(rest.len());
    let section = &rest[..end];
    assert!(
        section.contains("config.example.toml"),
        "README `## Configuration` section must point users at config.example.toml"
    );
}

#[test]
fn readme_documents_line_select_key() {
    assert!(
        README.contains("line-select"),
        "README.md must document the `L` line-select mode"
    );
    assert!(
        README.contains("`L`"),
        "README.md must mention the `L` key for line-select"
    );
}

#[test]
fn readme_documents_reveal_open_keys() {
    assert!(
        README.contains("`O`"),
        "README.md must document the `O` open-with-default-app key"
    );
    assert!(
        README.contains("`R`"),
        "README.md must document the `R` reveal-in-file-manager key"
    );
    let lower = README.to_lowercase();
    assert!(
        lower.contains("open with default app"),
        "README.md `## Keys` must describe the `O` key as 'open with default app'"
    );
    assert!(
        lower.contains("reveal"),
        "README.md must describe the `R` key as 'reveal'"
    );
    assert!(
        lower.contains("file manager"),
        "README.md must describe the `R` key as revealing in the OS 'file manager'"
    );
}

#[test]
fn readme_documents_config_file() {
    // README must document the config file: its path (herdr-provided + XDG fallback) and every
    // key. Scope every assertion to the `## Configuration` section itself (heading to the next
    // `## ` heading) rather than the whole README -- several of these bare words (e.g. `editor`,
    // `markdown`, `diff`, `open`, `reveal`) also appear elsewhere (the Keys table, the `e`/`O`/`R`
    // key descriptions, the roadmap), so an unscoped `README.contains` would still pass even if
    // the Configuration section were deleted outright. Slicing on the heading keeps this a real
    // regression guard, the same way `changelog_documents_line_reference_release` slices the
    // CHANGELOG on its release heading.
    let start = README
        .find("## Configuration")
        .expect("README.md must carry a `## Configuration` section");
    let rest = &README[start + "## Configuration".len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    let section = &rest[..end];

    assert!(
        section.contains("config.toml"),
        "README `## Configuration` section must name the config file config.toml"
    );
    assert!(
        section.contains("HERDR_PLUGIN_CONFIG_DIR"),
        "README `## Configuration` section must name the herdr config-dir env var"
    );
    // XDG fallback location:
    assert!(
        section.contains(".config/herdr-file-viewer") || section.contains("XDG_CONFIG_HOME"),
        "README `## Configuration` section must document the XDG fallback location"
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
            section.contains(key),
            "README `## Configuration` section must document the `{key}` key"
        );
    }
}

#[test]
fn readme_documents_keys_remapping() {
    // AC-22: the README `## Configuration` section must document the `[keys]` remapping surface --
    // that a binding is written `intent_name = <key spec>` (a string AND an array example), that
    // only modifier-free keys are bindable (no Ctrl/Alt), and that a `[keys]` value replaces the
    // action's default keys. Scope every assertion to the `## Configuration` section (heading to
    // the next `## ` heading), the same way `readme_documents_config_file` does, so a mention
    // elsewhere in the README cannot satisfy the check.
    let start = README
        .find("## Configuration")
        .expect("README.md must carry a `## Configuration` section");
    let rest = &README[start + "## Configuration".len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    let section = &rest[..end];

    assert!(
        section.contains("[keys]"),
        "README `## Configuration` section must name the `[keys]` remapping table"
    );
    // The `intent_name = <key spec>` form, shown by example in BOTH the string and the array shape.
    assert!(
        section.contains("refresh = \"g\""),
        "README `## Configuration` section must show a single-string key spec (refresh = \"g\")"
    );
    assert!(
        section.contains("nav_up = [\"w\", \"Up\"]"),
        "README `## Configuration` section must show an array key spec (nav_up = [\"w\", \"Up\"])"
    );
    // Only modifier-free keys are bindable: no Ctrl / Alt chords.
    assert!(
        section.contains("Ctrl") && section.contains("Alt"),
        "README `## Configuration` section must state that Ctrl/Alt chords are not bindable"
    );
    // Precedence: a `[keys]` value replaces/overrides the action's default keys.
    let lower = section.to_lowercase();
    assert!(
        lower.contains("replace"),
        "README `## Configuration` section must state a `[keys]` value replaces the default keys"
    );
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
