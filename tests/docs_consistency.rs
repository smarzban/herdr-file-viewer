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
const REMOTE_NOTICES_DOC: &str = include_str!("../docs/remote-notices.md");
const DOCS_INDEX: &str = include_str!("../docs/README.md");
const ARCHITECTURE: &str = include_str!("../ARCHITECTURE.md");
const SECURITY: &str = include_str!("../SECURITY.md");
const AGENTS: &str = include_str!("../AGENTS.md");
const CONTEXT: &str = include_str!("../CONTEXT.md");
const USAGE_DOC: &str = include_str!("../docs/usage.md");
const INSTALL_DOC: &str = include_str!("../docs/install.md");

/// Remove Markdown emphasis and normalize whitespace so wording guards do not depend on line wraps.
fn normalized_markdown_text(document: &str) -> String {
    document
        .replace("**", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

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
        "show_ignored",
        "compact_dirs",
        "update_check",
        "confirm_discard",
        "scroll_lines",
        "tree_width",
        "tree_position",
        "tree_max_cols",
        "preview_max_lines",
        "preview_max_kib",
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
fn configuration_doc_and_example_document_preview_caps() {
    // The content-preview cap keys must be documented in BOTH the configuration reference and the
    // bundled config.example.toml, so the feature ships with discoverable, copy-pasteable settings.
    for key in ["preview_max_lines", "preview_max_kib"] {
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
        "show_ignored",
        "compact_dirs",
        "update_check",
        "confirm_discard",
    ] {
        assert!(
            CONFIG_DOC.contains(key),
            "docs/configuration.md must document the `{key}` key"
        );
    }
}

#[test]
fn one_update_control_documents_all_remote_behavior() {
    // AC-57: `update_check` is the one resolved control for every advisory remote-notice
    // projection. The reference and copyable template must keep that whole boundary together,
    // without inventing a narrower setting or environment-variable surface.
    for (document_name, document) in [
        ("docs/configuration.md", CONFIG_DOC),
        ("config.example.toml", CONFIG_EXAMPLE),
    ] {
        let text = normalized_markdown_text(&document.replace("\n# ", " "));
        for required in [
            "update_check",
            "HERDR_FILE_VIEWER_NO_UPDATE_CHECK",
            "config > env > default",
            "Effective off disables all remote behavior and cached projection",
            "release discovery",
            "tagged release details",
            "default-HEAD project spotlight retrieval",
            "cached remote content",
            "status row",
            "remote additions to What's New",
            "Effective on permits the bounded, fail-silent daily pipeline",
            "does not guarantee network availability or content",
            "no spotlight-specific, release-details-specific, status-only, or cache-projection setting or environment variable",
        ] {
            assert!(
                text.contains(required),
                "{document_name} must document that update_check controls `{required}`"
            );
        }
    }
    assert!(
        CONFIG_DOC.contains("Dismiss the whole remote-notice status row for this session"),
        "the dismiss_update action must describe its whole remote-notice row/session behavior"
    );
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
fn keys_doc_documents_altgr_windows_scope() {
    // The AltGr explanation must retain: the term "AltGr" itself, that the inference is
    // Windows-only in scope, and the Crossterm 0.29 Windows-input rationale for why the chord is
    // ambiguous: the three facts a reader needs to trust the behavior on their platform. A
    // positive-content check (not a negative/brittle prose assertion), so future wording edits are
    // free as long as these three facts stay documented.
    assert!(
        KEYS_DOC.contains("AltGr"),
        "docs/keys.md must mention AltGr"
    );
    assert!(
        KEYS_DOC.contains("On Windows only") || KEYS_DOC.contains("Windows only"),
        "docs/keys.md must state the AltGr inference is Windows-only in scope"
    );
    assert!(
        KEYS_DOC.contains("Crossterm 0.29"),
        "docs/keys.md must explain the Crossterm 0.29 Windows-input behavior behind the AltGr \
         ambiguity"
    );
}

#[test]
fn remote_notices_reference_documents_publishing_and_trust_contract() {
    // AC-58: this page is the canonical maintainer reference for the one official repository's
    // advisory notices. It must name the source pins, bounded/silent acquisition, cache semantics,
    // rendering boundary, and explicit non-effects rather than turning the spotlight into a general
    // publishing channel.
    let remote_notices = normalized_markdown_text(REMOTE_NOTICES_DOC);
    let security = normalized_markdown_text(SECURITY);
    for required in [
        "https://github.com/smarzban/herdr-file-viewer",
        "https://raw.githubusercontent.com",
        "git ls-remote",
        "exact object ID resolved for the detected release tag",
        "For an annotated tag, the resolved ID is its peeled commit",
        "vMAJOR.MINOR.PATCH",
        "no prerelease or build suffixes",
        "current default-branch HEAD object",
        "CHANGELOG.md",
        "project-spotlight.md",
        "first nonblank `# ` heading",
        "remaining document body",
        "immutable",
        "1 MiB",
        "20 MiB",
        "Each cached remote",
        "15 seconds",
        "24 hours",
        "session start",
        "Stale and future spotlight content is hidden",
        "shared daily refresh is eligible or already underway",
        "Available",
        "Missing",
        "Unavailable",
        "fail silently",
        "update-check.json",
        "atomic",
        "advisory",
        "session-only",
        "remembered",
        "display-only",
        "trusted local code",
        "possible side effects",
        "no automatic install",
        "no download",
        "no URL open",
        "no clipboard",
        "no viewed-root or Git mutation",
        "sends no application credentials",
        "no telemetry",
        "no shell interpretation",
        "no raw terminal control",
    ] {
        assert!(
            remote_notices.contains(required),
            "docs/remote-notices.md must document `{required}`"
        );
    }

    assert!(
        DOCS_INDEX.contains("remote-notices.md"),
        "docs/README.md must link the canonical remote-notices reference"
    );
    assert!(
        ARCHITECTURE.contains("Official Repository Gateway")
            && ARCHITECTURE.contains("remote-notices"),
        "ARCHITECTURE.md must map the official gateway and point to its reference"
    );
    assert!(
        ARCHITECTURE.contains("update-check.json") && ARCHITECTURE.contains("spotlight"),
        "ARCHITECTURE.md must state the advisory-cache persistent-state exception"
    );
    assert!(
        SECURITY.contains("rustls")
            && SECURITY.contains("ureq")
            && SECURITY.contains("credentials")
            && SECURITY.contains("remote notices")
            && SECURITY.contains("exact object ID resolved for the detected release tag")
            && SECURITY.contains("For an annotated tag")
            && SECURITY.contains("resolved ID is its peeled commit")
            && SECURITY.contains("trusted local code")
            && SECURITY.contains("possible side effects")
            && security.contains("accepted only when it is at most 1 MiB")
            && security.contains("one additional sentinel byte")
            && security.contains("sends no application credentials"),
        "SECURITY.md must cover the remote-notice transport audit surface, accurate release pin, document acceptance cap, credential scope, and renderer trust"
    );
    assert!(
        remote_notices.contains("accepted only when it is at most 1 MiB")
            && remote_notices.contains("one additional sentinel byte")
            && remote_notices.contains("sends no application credentials"),
        "docs/remote-notices.md must distinguish the 1 MiB acceptance cap, sentinel-byte oversize detection, and application-credential scope"
    );
    assert!(
        !REMOTE_NOTICES_DOC.contains("exact detected release tag object"),
        "docs/remote-notices.md must not call an annotated tag object the fetched source pin"
    );
    assert!(
        !SECURITY.contains("Residual trust is limited"),
        "SECURITY.md must not limit residual trust to GitHub and TLS while executing a configured renderer"
    );
    for prohibited in [
        "1 MiB plus one byte",
        "cap-plus-one limit of **1 MiB**",
        "The viewer has no credentials",
    ] {
        assert!(
            !remote_notices.contains(prohibited),
            "docs/remote-notices.md must not make the contradictory claim `{prohibited}`"
        );
    }
    assert!(
        !security.contains("1 MiB plus one byte"),
        "SECURITY.md must not describe the sentinel byte as accepted document capacity"
    );
    assert!(
        AGENTS.contains("remote notices") && AGENTS.contains("ureq"),
        "AGENTS.md must preserve durable remote-notice implementation constraints"
    );
}

#[test]
fn remote_notice_user_docs_cover_whats_new_status_and_dismissal() {
    // AC-56: user docs must predict the advisory remote-notice surface without duplicating the
    // maintainer trust contract. Keep the exact status copy grounded in update::status.
    let usage = normalized_markdown_text(USAGE_DOC);
    let install = normalized_markdown_text(INSTALL_DOC);
    let keys = normalized_markdown_text(KEYS_DOC);

    for term in ["**remote notice**", "**project spotlight**"] {
        assert!(
            CONTEXT.contains(term),
            "CONTEXT.md must define the approved `{term}` glossary term"
        );
    }
    assert!(
        README.contains("docs/usage.md#staying-up-to-date"),
        "README.md must keep a lean remote-notice taste linked to the usage guide"
    );
    assert!(
        USAGE_DOC.contains("remote-notices.md"),
        "docs/usage.md must link the maintainer remote-notice contract instead of duplicating it"
    );

    for status in [
        "Update vX.Y.Z available · ? details · u dismiss",
        "Spotlight: <title> · ? details · u dismiss",
        "Update vX.Y.Z available · Spotlight: <title> · ? details · u dismiss",
        "F2 / F3 details · d dismiss",
        "(unbound) details · (unbound) dismiss",
    ] {
        assert!(
            usage.contains(status),
            "docs/usage.md must show the `{status}` status form"
        );
    }
    for required in [
        "no status row",
        "empty, disabled, or dismissed",
        "What's New is the first section",
        "selected when you press `?`",
        "project spotlight, Available updates, then the full embedded released history",
        "`[Unreleased]` is excluded",
        "readable in What's New after you dismiss the footer",
        "at most once every 24 hours",
        "at session start",
        "stale or absent spotlight",
        "daily refresh is eligible or already underway",
        "immutable cached release details",
        "exact detected release",
        "fail silently and independently",
        "no automatic action",
    ] {
        assert!(
            usage.contains(required),
            "docs/usage.md must document `{required}`"
        );
    }
    for required in [
        "whole status row for the current session",
        "returns next session while you are still behind",
        "exact spotlight content is remembered until that content changes",
        "does not remove its body from What's New",
    ] {
        assert!(
            keys.contains(required),
            "docs/keys.md must document `{required}` for `u`"
        );
    }
    for required in [
        "What's New",
        "herdr plugin install smarzban/herdr-file-viewer",
        "copy only",
        "never runs it",
    ] {
        assert!(
            install.contains(required),
            "docs/install.md must document `{required}` for updates"
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
