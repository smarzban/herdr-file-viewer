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
const USAGE_DOC: &str = include_str!("../docs/usage.md");
const INSTALL_DOC: &str = include_str!("../docs/install.md");
const SECURITY: &str = include_str!("../SECURITY.md");
const AGENT_SKILL: &str = include_str!("../skills/herdr-file-viewer/SKILL.md");
const OPEN_PANE_SCRIPT: &str = include_str!("../scripts/open-file-viewer.sh");
const OPEN_TAB_SCRIPT: &str = include_str!("../scripts/open-file-viewer-tab.sh");
const OPEN_OVERLAY_SCRIPT: &str = include_str!("../scripts/open-file-viewer-overlay.sh");
const OPEN_POPUP_SCRIPT: &str = include_str!("../scripts/open-file-viewer-popup.sh");
const MANIFEST: &str = include_str!("../herdr-plugin.toml");
const SUMMONING_DOC: &str = include_str!("../docs/summoning.md");
const WINDOWS_DOC: &str = include_str!("../docs/windows.md");

/// A launch block is any run of lines joined by trailing `\` continuations that mentions the
/// plugin-pane-open verb — i.e. one whole `herdr plugin pane open …` invocation, flattened to a
/// single string. Shared by the `--cwd` guard and the argv guard below so both judge the same
/// slice of text.
fn launch_blocks(doc: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    for line in doc.lines() {
        let trimmed = line.trim_end();
        current.push(' ');
        current.push_str(trimmed.trim_end_matches('\\').trim());
        if !trimmed.ends_with('\\') {
            if current.contains("plugin pane open") {
                blocks.push(current.clone());
            }
            current.clear();
        }
    }
    if current.contains("plugin pane open") {
        blocks.push(current);
    }
    blocks
}

/// A shell script with its whole-line `#` comments (and shebang) removed, so an assertion about
/// the *argv* can't be satisfied — or broken — by the header prose. The launcher headers are
/// required to name the flags they deliberately omit, so a negative assertion over the raw file
/// would fail on its own documentation.
fn code_lines(script: &str) -> String {
    script
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `--cwd` drift guard (#139).
///
/// `[[panes]].command` is relative, and herdr resolves it against `--cwd` — so a documented
/// `plugin pane open ... --cwd ...` fails to spawn the viewer (or, inside a built plugin checkout,
/// silently runs THAT checkout's binary). The shipped launchers never passed `--cwd`, so the docs
/// and the working argv had diverged unnoticed until a user hit it. Every doc that teaches the
/// launch is checked against the same rule the launchers follow, so this specific divergence fails
/// the build instead of reaching an agent.
#[test]
fn no_documented_launch_passes_cwd_to_plugin_pane_open() {
    // `--cwd` must not appear inside any `launch_blocks` slice.
    for (name, doc) in [
        ("skills/herdr-file-viewer/SKILL.md", AGENT_SKILL),
        ("docs/usage.md", USAGE_DOC),
        ("scripts/open-file-viewer.sh", OPEN_PANE_SCRIPT),
        ("scripts/open-file-viewer-tab.sh", OPEN_TAB_SCRIPT),
        ("scripts/open-file-viewer-overlay.sh", OPEN_OVERLAY_SCRIPT),
        ("scripts/open-file-viewer-popup.sh", OPEN_POPUP_SCRIPT),
    ] {
        for block in launch_blocks(doc) {
            assert!(
                !block.contains("--cwd"),
                "#139: {name} pairs `plugin pane open` with `--cwd`, which cannot spawn the \
                 relative pane command. Set the root by launching from a focused pane whose cwd is \
                 the target repository instead. Offending block: {block}"
            );
        }
    }

    // The guard is only meaningful while these docs actually teach the launch.
    assert!(
        AGENT_SKILL.contains("plugin pane open") && USAGE_DOC.contains("plugin pane open"),
        "the agent skill and usage doc must still document the launch command"
    );
}

/// Every unix launcher's `plugin pane open` argv is exactly the one verified against a live host.
///
/// Verified 2026-08-07 against **herdr 0.8.0** via the BARE `herdr plugin pane` usage line — the
/// surface to trust. Two of herdr's own surfaces under-report the placement enum in 0.8.0: the
/// `open --help` output and `herdr completion zsh` both list `overlay split tab zoomed` and omit
/// `popup`, `--width` and `--height`. Only the bare usage string and the API schema
/// (`$defs.PluginPanePlacement`) are right, and a live probe opened all four layouts successfully.
///
/// Every positive AND negative here runs against the *extracted invocation* ([`launch_blocks`] over
/// [`code_lines`]), never the whole file: each launcher's header is required to name the flags it
/// deliberately omits, so a whole-file `contains` negative would fail on its own documentation.
#[test]
fn every_launcher_uses_the_verified_argv() {
    // (script name, source, placement marker) — all four unix launchers. The two Windows `.ps1`
    // launchers are deliberately absent: they don't use `plugin pane open` at all (absolute-path
    // `pane split`/`tab create` + `pane run`, GH #58), so they carry no marker and parse as
    // `Placement::Unknown`, which keeps today's host-zoom behaviour — correct, since what they open
    // really is a split pane.
    let launchers = [
        ("scripts/open-file-viewer.sh", OPEN_PANE_SCRIPT, "split"),
        ("scripts/open-file-viewer-tab.sh", OPEN_TAB_SCRIPT, "tab"),
        (
            "scripts/open-file-viewer-overlay.sh",
            OPEN_OVERLAY_SCRIPT,
            "overlay",
        ),
        (
            "scripts/open-file-viewer-popup.sh",
            OPEN_POPUP_SCRIPT,
            "popup",
        ),
    ];

    for (name, script, placement) in launchers {
        let blocks = launch_blocks(&code_lines(script));
        assert_eq!(
            blocks.len(),
            1,
            "{name} must contain exactly one `plugin pane open` invocation (found {})",
            blocks.len()
        );
        let argv = &blocks[0];

        assert!(
            argv.contains(&format!("--placement {placement}")),
            "{name} must open with `--placement {placement}`: {argv}"
        );
        // The marker the viewer reads back (`src/host::PLACEMENT_ENV`) to decide whether `Z` may
        // issue a host `pane zoom`. It must match the placement it is marking, or an overlay/popup
        // viewer would zoom a pane it does not own.
        assert!(
            argv.contains(&format!("--env HERDR_FILE_VIEWER_PLACEMENT={placement}")),
            "{name} must tag its argv with `--env HERDR_FILE_VIEWER_PLACEMENT={placement}`: {argv}"
        );
    }

    let overlay = launch_blocks(&code_lines(OPEN_OVERLAY_SCRIPT)).remove(0);
    let popup = launch_blocks(&code_lines(OPEN_POPUP_SCRIPT)).remove(0);

    // Sizing is popup-only. herdr rejects it elsewhere: `invalid_params`, exit 1, "width and height
    // are only supported when placement is popup".
    for flag in ["--width", "--height"] {
        assert!(
            !overlay.contains(flag),
            "an overlay open must not carry {flag} (herdr rejects it): {overlay}"
        );
    }
    // 90% x 85% measured 147x43 on a 167-column tab area, against `NARROW_SPLIT = 80` in
    // `src/presenter.rs`; an unsized popup (herdr's half-size default) measured 80x25 there — on the
    // single-column boundary with nothing to spare.
    assert!(
        popup.contains("--width 90%") && popup.contains("--height 85%"),
        "the popup open must carry the sized `--width 90% --height 85%`: {popup}"
    );

    // Neither placement may target a pane or workspace: herdr answers `invalid_params`, exit 1,
    // "overlay and popup plugin panes target the active pane".
    for (name, argv) in [
        ("scripts/open-file-viewer-overlay.sh", &overlay),
        ("scripts/open-file-viewer-popup.sh", &popup),
    ] {
        for flag in ["--target-pane", "--workspace"] {
            assert!(
                !argv.contains(flag),
                "{name} must not carry {flag} (herdr rejects it for this placement): {argv}"
            );
        }
    }
}

/// The overlay launcher's FOCUS branch must NOT be the split launcher's zoom cycle.
///
/// herdr implements an overlay as a 50/50 split whose new half is tab-zoomed (`pane layout
/// --current` during a live overlay: `splits:[{direction:"right",ratio:0.5}]` + `zoomed:true`;
/// verified 2026-08-07, herdr 0.8.0). So `pane zoom <id> --on` → `--off`, which is how
/// `scripts/open-file-viewer.sh` pulls focus, *succeeds* against an overlay and thereby strips its
/// covering zoom — leaving a permanent ordinary split. `plugin pane focus <id>` pulls focus and
/// preserves `zoomed: true` (verified the same sitting), so that is the verb here. The CLOSE branch
/// is unaffected and keeps the generic `pane close <id>`, also verified against a live overlay.
///
/// Asserted over the script's non-comment lines only — the header explains the zoom cycle at
/// length, so a raw `contains` would false-positive on the explanation.
#[test]
fn the_overlay_launcher_focuses_without_the_zoom_cycle() {
    let code = code_lines(OPEN_OVERLAY_SCRIPT);
    assert!(
        code.contains(r#"exec "$herdr_bin" plugin pane focus "$pid""#),
        "the overlay launcher's FOCUS branch must run `plugin pane focus <pane_id>`: {code}"
    );
    assert!(
        !code.contains("pane zoom"),
        "the overlay launcher must never run `pane zoom` — it would flatten the overlay into a \
         plain split: {code}"
    );
    // The split launcher keeps the zoom cycle: for a genuine split viewer it is correct, and
    // changing it is out of scope.
    assert!(
        code_lines(OPEN_PANE_SCRIPT).contains("pane zoom"),
        "the split launcher's FOCUS branch must still use the `pane zoom` cycle"
    );
}

/// Every `[[actions]]` id the manifest declares is documented for users.
///
/// `docs/summoning.md` is the owning page, but the corpus is deliberately summoning.md **plus**
/// `docs/windows.md`, scanned as one: that union is the actual doc split, not a loophole. The
/// `open-file-viewer-windows` / `open-file-viewer-tab-windows` ids are documented only on the
/// Windows page, which summoning.md links to as its annex, so scanning summoning.md alone would
/// fail on day one. The name keeps the `summoning_doc_` prefix because summoning.md owns the topic.
///
/// Matching is whole-token, not substring: every id is a prefix of a longer one
/// (`open-file-viewer` ⊂ `open-file-viewer-tab` ⊂ `open-file-viewer-tab-windows`), so a bare
/// `contains` would let one long id satisfy three others and the guard would pass while documenting
/// nothing. A match must be bounded on both sides by a non-id character — backtick, quote, comma,
/// whitespace, `.` (as in `herdr-file-viewer.open-file-viewer-tab`), end of input.
#[test]
fn summoning_doc_documents_every_open_action() {
    // Ids come from `[[actions]]` entries only. The `[[panes]]` entry also has an `id` (`file-viewer`)
    // and it is NOT a user-invocable action — worse, it only ever appears inside the action ids, where
    // the token boundary rule would (correctly) reject it. Track the current table header, and strip
    // comment lines first so the manifest's own prose about `[[actions]]` doesn't open a section.
    let mut section = "";
    let mut ids: Vec<&str> = Vec::new();
    for line in MANIFEST.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("[[") {
            section = trimmed;
        } else if section == "[[actions]]"
            && let Some(id) = trimmed
                .strip_prefix("id = \"")
                .and_then(|s| s.strip_suffix('"'))
        {
            ids.push(id);
        }
    }
    assert!(
        ids.len() >= 6,
        "expected at least the six declared [[actions]] ids (4 unix + 2 Windows), found {ids:?}"
    );

    let corpus = format!("{SUMMONING_DOC}\n{WINDOWS_DOC}");
    for id in ids {
        assert!(
            mentions_token(&corpus, id),
            "the `{id}` action must be documented as a whole token in docs/summoning.md or \
             docs/windows.md — a bare substring inside a longer id does not count"
        );
    }
}

/// Whether `token` occurs in `corpus` bounded on both sides by a non-id character, so a match
/// inside a longer identifier doesn't count.
fn mentions_token(corpus: &str, token: &str) -> bool {
    fn is_id_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    }
    corpus.match_indices(token).any(|(i, _)| {
        corpus[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !is_id_char(c))
            && corpus[i + token.len()..]
                .chars()
                .next()
                .is_none_or(|c| !is_id_char(c))
    })
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

fn section<'a>(document: &'a str, start: &str, end: &str) -> &'a str {
    let (_, remainder) = document
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start: {start}"));
    remainder
        .split_once(end)
        .map_or(remainder, |(section, _)| section)
}

#[test]
fn remote_notice_docs_keep_their_controls_and_boundaries() {
    let config = section(CONFIG_DOC, "update_check = true", "`tree_width`");
    assert!(
        config.contains("`update_check` governs"),
        "the update_check passage must say it governs remote notices"
    );
    assert!(
        config.contains("release details and project spotlights"),
        "the update_check passage must cover release details and project spotlights"
    );

    let usage = section(USAGE_DOC, "## Staying up to date", "\n## ");
    assert!(
        usage.contains("display-only"),
        "remote notices must retain their display-only boundary"
    );
    assert!(
        usage.contains("never installs"),
        "remote notices must not imply an install action"
    );

    for document in [CONFIG_DOC, INSTALL_DOC] {
        assert!(
            document.contains("system `curl` is optional"),
            "docs must describe system curl as optional"
        );
        assert!(
            document.contains("document retrieval is unavailable"),
            "without curl, only document retrieval must be unavailable"
        );
    }

    let remote_notice = section(SECURITY, "**Remote notices", "- **Untrusted repository");
    assert!(
        remote_notice.contains("fixed official HTTPS sources"),
        "remote notices must use fixed official HTTPS sources"
    );
    assert!(
        remote_notice.contains("`404` withdraws a spotlight"),
        "an HTTP 404 must withdraw a spotlight"
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
