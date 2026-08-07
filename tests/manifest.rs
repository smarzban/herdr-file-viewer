//! the herdr plugin manifest is the Host Adapter's static surface.
//!
//! AC-17: the viewer declares a split-pane launch of the release binary.
//! AC-N4: the viewer never auto-launches — the manifest declares no event hooks.
//!
//! Per the plan, these read `herdr-plugin.toml` to a string and assert on its contents.

use std::fs;
use std::path::PathBuf;

fn manifest_raw() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("herdr-plugin.toml");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// The manifest with `#` line-comments stripped, so assertions match actual
/// declarations (table headers, keys/values) rather than prose in comments.
/// (The manifest uses no `#` inside string values, so cutting at the first `#`
/// per line is sufficient and keeps the test free of a TOML-parser dependency.)
fn manifest() -> String {
    manifest_raw()
        .lines()
        .map(|line| match line.find('#') {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn declares_split_pane_launching_the_release_binary() {
    let m = manifest();
    assert!(
        m.contains("[[panes]]"),
        "manifest must declare a [[panes]] entry"
    );
    assert!(
        m.contains(r#"placement = "split""#),
        "AC-17: the pane must declare placement = \"split\""
    );
    assert!(
        m.contains(r#"command = ["./target/release/herdr-file-viewer"]"#),
        "AC-17: the pane command must launch the release binary"
    );
}

#[test]
fn declares_no_windows_pane_entry() {
    // On Windows herdr cannot spawn the manifest's relative pane command (CreateProcessW resolves a
    // relative program against herdr's own dir, not any `--cwd`), so there is deliberately NO Windows
    // [[panes]] entry — the Windows launchers spawn the viewer by absolute path via
    // `pane split`/`tab create` + `pane run` (verified on real hardware, GH #58). Guard that we
    // didn't leave a dead `.exe` pane entry behind.
    let m = manifest();
    assert!(
        !m.contains(r#"id = "file-viewer-windows""#),
        "there must be no Windows pane entry (absolute-spawn launcher, not a manifest pane): {m}"
    );
    assert!(
        !m.contains("herdr-file-viewer.exe\"]"),
        "no [[panes]] command should name the .exe (the launcher spawns it by absolute path): {m}"
    );
}

#[test]
fn declares_at_least_one_action() {
    assert!(
        manifest().contains("[[actions]]"),
        "manifest must declare an [[actions]] entry to summon the viewer"
    );
}

#[test]
fn declares_split_and_tab_open_actions() {
    // The viewer can be summoned as a split pane or in its own tab; each action runs its
    // dedicated launcher script.
    let m = manifest();
    assert!(
        m.contains(r#"id = "open-file-viewer""#),
        "split-pane action present"
    );
    assert!(
        m.contains(r#"id = "open-file-viewer-tab""#),
        "tab action present"
    );
    assert!(
        m.contains("scripts/open-file-viewer.sh"),
        "split action runs its launcher"
    );
    assert!(
        m.contains("scripts/open-file-viewer-tab.sh"),
        "tab action runs its launcher"
    );
}

#[test]
fn declares_overlay_and_popup_open_actions() {
    // v1.16.0 adds two more ways to summon the same single [[panes]] entry — placement is chosen at
    // CLI *open* time by the launcher, exactly as the tab action already does. Each new action runs
    // its OWN script: the overlay launcher differs from the split one in its open argv *and* in its
    // FOCUS branch, and the popup launcher is open-only (a popup has no pane id to toggle).
    let m = manifest();
    assert!(
        m.contains(r#"id = "open-file-viewer-overlay""#),
        "overlay action present"
    );
    assert!(
        m.contains(r#"id = "open-file-viewer-popup""#),
        "popup action present"
    );
    assert!(
        m.contains(r#"command = ["bash", "scripts/open-file-viewer-overlay.sh"]"#),
        "overlay action runs its own launcher"
    );
    assert!(
        m.contains(r#"command = ["bash", "scripts/open-file-viewer-popup.sh"]"#),
        "popup action runs its own launcher"
    );
}

#[test]
fn overlay_and_popup_actions_are_unix_only() {
    // Windows is deferred for both layouts (no `.ps1` launcher, no `-windows` action id): the
    // Windows launchers don't go through `plugin pane open` at all — they spawn the viewer by
    // absolute path via `pane split`/`tab create` + `pane run` (GH #58) — so an overlay/popup
    // variant there is a separate piece of work needing real hardware. Guard both halves: each new
    // action is gated to unix, and no `-windows` sibling sneaked in.
    let m = manifest();
    for id in ["open-file-viewer-overlay", "open-file-viewer-popup"] {
        assert!(
            m.contains(&format!(
                "id = \"{id}\"\nplatforms = [\"linux\", \"macos\"]"
            )),
            "{id} must be gated to exactly [\"linux\", \"macos\"]: {m}"
        );
        assert!(
            !m.contains(&format!("id = \"{id}-windows\"")),
            "Windows is deferred for the {id} layout — there must be no {id}-windows action: {m}"
        );
    }
}

#[test]
fn pins_minimum_herdr_version() {
    // 0.7.0 → 0.7.4 in v1.16.0, and this is a sanctioned change to a spec-backed assertion, not a
    // test tidy-up: the floor genuinely moved when the popup layout shipped. herdr 0.7.4 (#1125) is
    // the release that added session-modal popup PLUGIN panes with cell/percentage sizing, so an
    // older host cannot honour `open-file-viewer-popup` at all. Declaring the floor the plugin
    // actually requires beats a manifest that promises a placement the host will reject at runtime.
    // (Overlay needed no bump — it predates 0.7.4.)
    assert!(
        manifest().contains(r#"min_herdr_version = "0.7.4""#),
        "manifest must pin min_herdr_version = \"0.7.4\" (the popup floor, herdr #1125)"
    );
}

#[test]
fn declares_a_release_build_command() {
    let m = manifest();
    assert!(
        m.contains("[[build]]"),
        "manifest must declare a [[build]] step"
    );
    assert!(
        m.contains("scripts/fetch-or-build.sh"),
        "the build step must run the fetch-or-build script (prebuilt binary, cargo fallback)"
    );
}

#[test]
fn declares_linux_macos_and_windows_platforms() {
    // AC-20: Windows is a declared platform, alongside the existing two.
    assert!(
        manifest().contains(r#"platforms = ["linux", "macos", "windows"]"#),
        "manifest must declare platforms = [\"linux\", \"macos\", \"windows\"]"
    );
}

#[test]
fn build_step_is_platform_gated_unix_and_windows() {
    // AC-14: exactly one [[build]] entry runs per host — the /bin/sh entry on unix, the
    // PowerShell entry on Windows — via herdr's platform filter on the item-level `platforms`
    // key (which overrides the top-level list).
    let m = manifest();
    assert!(
        m.contains("[[build]]\nplatforms = [\"linux\", \"macos\"]\ncommand = [\"/bin/sh\", \"scripts/fetch-or-build.sh\"]"),
        "unix [[build]] must be gated to [\"linux\", \"macos\"] and run fetch-or-build.sh: {m}"
    );
    assert!(
        m.contains("[[build]]\nplatforms = [\"windows\"]\ncommand = [\"powershell\", \"-NoProfile\", \"-ExecutionPolicy\", \"Bypass\", \"-File\", \"scripts/fetch-or-build.ps1\"]"),
        "Windows [[build]] must be gated to [\"windows\"] and run fetch-or-build.ps1: {m}"
    );
}

#[test]
fn open_file_viewer_action_is_platform_gated_unix_and_windows() {
    // AC-14, AC-16: the open-file-viewer action has a unix (bash .sh) variant and a Windows
    // (PowerShell .ps1) variant, each gated to its platform. The Windows variant carries a
    // DISTINCT id (`open-file-viewer-windows`) because herdr rejects a duplicate action id at
    // load time regardless of platform (verified live against herdr 0.7.1) — a same-id pair
    // would fail to load on EVERY platform.
    let m = manifest();
    assert!(
        m.contains("id = \"open-file-viewer\"\nplatforms = [\"linux\", \"macos\"]")
            && m.contains("command = [\"bash\", \"scripts/open-file-viewer.sh\"]"),
        "open-file-viewer's unix variant must be gated to [\"linux\", \"macos\"] and run the .sh launcher: {m}"
    );
    assert!(
        m.contains("id = \"open-file-viewer-windows\"\nplatforms = [\"windows\"]"),
        "open-file-viewer's Windows variant must use the distinct id open-file-viewer-windows, gated to [\"windows\"]: {m}"
    );
    // The Windows action runs the launcher via `-Command`, locating it by asking herdr for its own
    // plugin root (`plugin list`), since the action's cwd is unreliable under herdr's `\\?\` server cwd.
    assert!(
        m.contains("plugin list --json") && m.contains("'open-file-viewer.ps1'"),
        "open-file-viewer's Windows variant must locate the .ps1 via herdr's plugin root: {m}"
    );
}

#[test]
fn open_file_viewer_tab_action_is_platform_gated_unix_and_windows() {
    let m = manifest();
    assert!(
        m.contains("id = \"open-file-viewer-tab\"\nplatforms = [\"linux\", \"macos\"]")
            && m.contains("command = [\"bash\", \"scripts/open-file-viewer-tab.sh\"]"),
        "open-file-viewer-tab's unix variant must be gated to [\"linux\", \"macos\"] and run the .sh launcher: {m}"
    );
    assert!(
        m.contains("id = \"open-file-viewer-tab-windows\"\nplatforms = [\"windows\"]"),
        "open-file-viewer-tab's Windows variant must use the distinct id open-file-viewer-tab-windows, gated to [\"windows\"]: {m}"
    );
    assert!(
        m.contains("plugin list --json") && m.contains("'open-file-viewer-tab.ps1'"),
        "open-file-viewer-tab's Windows variant must locate the .ps1 via herdr's plugin root: {m}"
    );
}

#[test]
fn action_ids_are_unique() {
    // herdr rejects a manifest with two [[actions]] sharing an id (`duplicate_plugin_action_id`)
    // at LOAD time — before any platform filtering — so a same-id-per-platform pair would break
    // `herdr plugin install` on EVERY platform, not just Windows. Guard that every declared
    // action id is unique. (Verified live against herdr 0.7.1.)
    let m = manifest();
    let mut ids: Vec<&str> = m
        .lines()
        .filter_map(|l| l.trim().strip_prefix("id = \""))
        .filter_map(|s| s.strip_suffix('"'))
        // the [[panes]] entry also has an `id`; only action/pane ids share the key space we care
        // about here — dedupe checks all declared `id = "..."` values, which must each be unique.
        .collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        n,
        ids.len(),
        "every declared id in the manifest must be unique (herdr rejects duplicates at load)"
    );
}

#[test]
fn no_entry_declares_an_aarch64_windows_target() {
    // AC-N4: v1 targets x86_64-pc-windows-msvc only — no Windows-on-ARM declaration (comments
    // are stripped first, so this checks actual entries, not explanatory prose).
    assert!(
        !manifest().contains("aarch64"),
        "manifest must not declare any aarch64 (Windows-on-ARM) target"
    );
}

#[test]
fn declares_no_event_hooks() {
    let m = manifest();
    // AC-N4 (finder): no event-hook table — the viewer only ever opens via an explicit action.
    // AC-N6 (in-file-nav): search and go-to-line also have no auto/event trigger — they open
    // only via the explicit `/` (OpenSearch) and `:` (OpenGoToLine) key bindings. The manifest
    // declaring no `[[events]]` is the Host Adapter proof of this: nothing in the manifest
    // can cause herdr to call back into the viewer to open a prompt automatically.
    assert!(
        !m.contains("[[events]]"),
        "AC-N4/AC-N6: manifest must declare no [[events]] hooks"
    );
    assert!(
        !m.contains("[[link_handlers]]"),
        "manifest must declare no [[link_handlers]] (no automatic invocation path)"
    );
}
