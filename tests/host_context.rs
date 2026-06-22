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
