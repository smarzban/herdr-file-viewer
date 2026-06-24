//! T-17 — Host Adapter: parse the injected launch context (AC-26).

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

// T-3 — workspace_id parsing (AC-3, AC-15)

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
