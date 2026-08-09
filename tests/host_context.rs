//! Host Adapter: parse the injected launch context (AC-26).

use herdr_file_viewer::host::{from_env, parse_context};
use std::path::PathBuf;

#[test]
fn populated_context_json_is_parsed() {
    // Unknown fields (e.g. worktree_root, is_worktree) are ignored gracefully.
    let json = r#"{"cwd":"/w","worktree_root":"/w/wt","base_branch":"main","is_worktree":true}"#;
    let ctx = parse_context(Some(json), PathBuf::from("/fallback"));
    assert_eq!(ctx.cwd, PathBuf::from("/w"));
    assert_eq!(ctx.base_branch, Some("main".to_string()));
}

#[test]
fn missing_json_degrades_to_cwd_only() {
    // AC-26: no context → a minimal { cwd } from the fallback, no panic.
    let ctx = parse_context(None, PathBuf::from("/fallback"));
    assert_eq!(ctx.cwd, PathBuf::from("/fallback"));
    assert_eq!(ctx.base_branch, None);
}

#[test]
fn malformed_json_degrades_without_panic() {
    // AC-26: garbage in → minimal { cwd }, never a crash.
    let ctx = parse_context(Some("{ this is not json"), PathBuf::from("/fallback"));
    assert_eq!(ctx.cwd, PathBuf::from("/fallback"));
    assert_eq!(ctx.base_branch, None);
}

#[test]
fn json_without_cwd_falls_back_but_keeps_other_fields() {
    let ctx = parse_context(Some(r#"{"base_branch":"dev"}"#), PathBuf::from("/fallback"));
    assert_eq!(ctx.cwd, PathBuf::from("/fallback"));
    assert_eq!(ctx.base_branch, Some("dev".to_string()));
}

#[test]
fn from_env_without_context_is_cwd_only() {
    // HERDR_PLUGIN_CONTEXT_JSON is unset in the test env → degrade to cwd (AC-26).
    let ctx = from_env();
    assert_eq!(ctx.cwd, std::env::current_dir().unwrap());
    assert_eq!(ctx.base_branch, None);
}

#[test]
fn focused_pane_cwd_is_used_as_the_root() {
    // herdr 0.7.0's real context shape names the invoking pane's directory `focused_pane_cwd`
    // (not `cwd`). The viewer must root there — not at its own process cwd (the fallback),
    // which is the plugin's install dir. Regression test for the "tree shows the plugin's own
    // files" bug.
    let json = r#"{"workspace_cwd":"/ws","focused_pane_cwd":"/work/project","tab_id":"wE:tD"}"#;
    let ctx = parse_context(Some(json), PathBuf::from("/plugin-dir"));
    assert_eq!(ctx.cwd, PathBuf::from("/work/project"));
}

#[test]
fn workspace_cwd_is_the_fallback_when_no_focused_pane_cwd() {
    let ctx = parse_context(
        Some(r#"{"workspace_cwd":"/ws"}"#),
        PathBuf::from("/plugin-dir"),
    );
    assert_eq!(ctx.cwd, PathBuf::from("/ws"));
}

#[test]
fn focused_pane_cwd_wins_over_a_co_present_legacy_cwd() {
    // Precedence is the whole point of the change: the invoking pane's dir beats a bare `cwd`.
    let ctx = parse_context(
        Some(r#"{"focused_pane_cwd":"/a","cwd":"/b"}"#),
        PathBuf::from("/fallback"),
    );
    assert_eq!(ctx.cwd, PathBuf::from("/a"));
}

#[test]
fn an_empty_cwd_field_is_ignored_in_favor_of_the_fallback() {
    // A malformed host value (empty string) must not root at an empty path.
    let ctx = parse_context(
        Some(r#"{"focused_pane_cwd":""}"#),
        PathBuf::from("/fallback"),
    );
    assert_eq!(ctx.cwd, PathBuf::from("/fallback"));
}

// workspace_id parsing (AC-3, AC-15)

#[test]
fn workspace_id_is_parsed_from_json() {
    // AC-3: the workspace_id field is threaded through to LaunchContext.
    let json = r#"{"cwd":"/w","workspace_id":"ws-abc123"}"#;
    let ctx = parse_context(Some(json), PathBuf::from("/fallback"));
    assert_eq!(ctx.workspace_id, Some("ws-abc123".to_string()));
}

#[test]
fn absent_workspace_id_degrades_to_none() {
    // AC-15: missing workspace_id must degrade silently to None.
    let json = r#"{"cwd":"/w","base_branch":"main"}"#;
    let ctx = parse_context(Some(json), PathBuf::from("/fallback"));
    assert_eq!(ctx.workspace_id, None);
}

#[test]
fn empty_workspace_id_is_treated_as_none() {
    // An empty string from the host is treated as absent, consistent with cwd filtering.
    let json = r#"{"cwd":"/w","workspace_id":""}"#;
    let ctx = parse_context(Some(json), PathBuf::from("/fallback"));
    assert_eq!(ctx.workspace_id, None);
}

#[test]
fn malformed_json_still_yields_none_workspace_id() {
    // AC-26: malformed JSON → minimal context with no workspace_id, no panic.
    let ctx = parse_context(Some("{ this is not json"), PathBuf::from("/fallback"));
    assert_eq!(ctx.workspace_id, None);
}

#[test]
fn a_focused_pane_inside_the_plugins_own_install_dir_falls_through_to_the_workspace() {
    // Opening the viewer while a VIEWER pane is focused: herdr launches the pane from the plugin
    // root (the manifest command is relative), so the focused pane's cwd is the plugin's own
    // install directory. Rooting there showed the user
    // `~/.config/herdr/plugins/github/herdr-file-viewer-…` instead of their project.
    let plugin = "/Users/x/.config/herdr/plugins/github/herdr-file-viewer-abc123";
    let json =
        format!(r#"{{"focused_pane_cwd":"{plugin}","workspace_cwd":"/Users/x/dev/project"}}"#);
    let ctx = herdr_file_viewer::host::parse_context_from(
        Some(&json),
        PathBuf::from("/fallback"),
        Some(PathBuf::from(format!(
            "{plugin}/target/release/herdr-file-viewer"
        ))),
    );
    assert_eq!(
        ctx.cwd,
        PathBuf::from("/Users/x/dev/project"),
        "the plugin's own dir must never be the viewed root when a workspace is known"
    );
}

#[test]
fn an_ordinary_focused_pane_still_wins_over_the_workspace() {
    // The skip must be narrow: only our OWN install dir is ignored. A normal project directory —
    // even one that happens to sit near the plugin — is still the most specific answer.
    let json =
        r#"{"focused_pane_cwd":"/Users/x/dev/project/src","workspace_cwd":"/Users/x/dev/project"}"#;
    let ctx = herdr_file_viewer::host::parse_context_from(
        Some(json),
        PathBuf::from("/fallback"),
        Some(PathBuf::from(
            "/Users/x/.config/herdr/plugins/github/hfv/target/release/herdr-file-viewer",
        )),
    );
    assert_eq!(ctx.cwd, PathBuf::from("/Users/x/dev/project/src"));
}

#[test]
fn the_plugin_dir_is_still_used_when_there_is_nothing_better() {
    // Degrade, don't break: with no workspace and no cwd in the context, the plugin dir is all we
    // have and is better than an empty root.
    let plugin = "/plugins/hfv";
    let json = format!(r#"{{"focused_pane_cwd":"{plugin}"}}"#);
    let ctx = herdr_file_viewer::host::parse_context_from(
        Some(&json),
        PathBuf::from("/fallback"),
        Some(PathBuf::from(format!(
            "{plugin}/target/release/herdr-file-viewer"
        ))),
    );
    assert_eq!(ctx.cwd, PathBuf::from("/fallback"));
}

#[test]
fn a_sibling_pane_supplies_the_root_when_the_context_only_offers_the_plugin_dir() {
    // herdr derives BOTH context cwds from the focused pane, so opening the viewer while a viewer
    // is focused reports the plugin's install dir twice over and the fallback chain has nothing
    // better. The workspace's other panes do.
    let plugin = "/Users/x/.config/herdr/plugins/github/hfv";
    let exe = PathBuf::from(format!("{plugin}/target/release/herdr-file-viewer"));
    let json = format!(
        r#"{{"result":{{"panes":[
             {{"pane_id":"w1:p1","workspace_id":"w1","cwd":"{plugin}"}},
             {{"pane_id":"w1:p2","workspace_id":"w1","cwd":"/Users/x/dev/project"}}
           ]}}}}"#
    );
    let root = herdr_file_viewer::host::root_from_sibling_panes(&json, Some("w1"), Some(&exe));
    assert_eq!(root, Some(PathBuf::from("/Users/x/dev/project")));
}

#[test]
fn sibling_panes_in_another_workspace_are_not_borrowed() {
    // A pane in a different workspace is different work; rooting there would be worse than the
    // plugin dir, because it would look plausible.
    let plugin = "/plugins/hfv";
    let exe = PathBuf::from(format!("{plugin}/target/release/herdr-file-viewer"));
    let json = format!(
        r#"{{"result":{{"panes":[
             {{"pane_id":"w1:p1","workspace_id":"w1","cwd":"{plugin}"}},
             {{"pane_id":"w2:p1","workspace_id":"w2","cwd":"/somewhere/else"}}
           ]}}}}"#
    );
    assert_eq!(
        herdr_file_viewer::host::root_from_sibling_panes(&json, Some("w1"), Some(&exe)),
        None
    );
}

#[test]
fn malformed_pane_json_yields_no_root_rather_than_panicking() {
    for bad in [
        "not json",
        "{}",
        r#"{"result":{}}"#,
        r#"{"result":{"panes":[]}}"#,
    ] {
        assert_eq!(
            herdr_file_viewer::host::root_from_sibling_panes(bad, Some("w1"), None),
            None,
            "input {bad:?}"
        );
    }
}
