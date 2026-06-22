//! T-17 — Host Adapter: parse injected context (AC-26) + open-pane request (AC-19).
//! The herdr CLI is injected and records calls; nothing real is launched.

use herdr_file_viewer::host::{HerdrCli, current_pane_id, from_env, open_pane, parse_context};
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;

/// Records every herdr CLI call and replays a canned `pane split` response.
struct FakeCli {
    calls: Vec<Vec<OsString>>,
    split_response: String,
}

impl FakeCli {
    fn new(split_response: &str) -> Self {
        Self {
            calls: Vec::new(),
            split_response: split_response.to_string(),
        }
    }
}

impl HerdrCli for FakeCli {
    fn run(&mut self, args: &[OsString]) -> io::Result<String> {
        self.calls.push(args.to_vec());
        if args.get(1).map(|a| a == "split").unwrap_or(false) {
            Ok(self.split_response.clone())
        } else {
            Ok(String::new())
        }
    }
}

fn osv(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

/// Parse a POSIX shell word (single quotes + `\` escapes) back to its literal value, so a
/// test can prove an escaped command recovers the original string when the shell runs it.
fn shell_unquote(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    let mut in_single = false;
    while let Some(c) = chars.next() {
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                out.push(c);
            }
        } else if c == '\'' {
            in_single = true;
        } else if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---- context parsing (AC-26) ----------------------------------------------------------

#[test]
fn populated_context_json_is_parsed() {
    let json = r#"{"cwd":"/w","worktree_root":"/w/wt","base_branch":"main","is_worktree":true}"#;
    let ctx = parse_context(Some(json), PathBuf::from("/fallback"));
    assert_eq!(ctx.cwd, PathBuf::from("/w"));
    assert_eq!(ctx.worktree_root, Some(PathBuf::from("/w/wt")));
    assert_eq!(ctx.base_branch, Some("main".to_string()));
    assert!(ctx.is_worktree);
}

#[test]
fn missing_json_degrades_to_cwd_only() {
    // AC-26: no context → a minimal { cwd } from the fallback, no panic.
    let ctx = parse_context(None, PathBuf::from("/fallback"));
    assert_eq!(ctx.cwd, PathBuf::from("/fallback"));
    assert_eq!(ctx.worktree_root, None);
    assert_eq!(ctx.base_branch, None);
    assert!(!ctx.is_worktree);
}

#[test]
fn malformed_json_degrades_without_panic() {
    // AC-26: garbage in → minimal { cwd }, never a crash.
    let ctx = parse_context(Some("{ this is not json"), PathBuf::from("/fallback"));
    assert_eq!(ctx.cwd, PathBuf::from("/fallback"));
    assert!(!ctx.is_worktree);
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
    assert!(!ctx.is_worktree);
    assert_eq!(ctx.worktree_root, None);
}

#[test]
fn current_pane_id_read_from_context() {
    assert_eq!(
        current_pane_id(Some(r#"{"pane_id":"p9"}"#)),
        Some("p9".to_string())
    );
    assert_eq!(current_pane_id(Some("garbage")), None);
    assert_eq!(current_pane_id(None), None);
}

// ---- open-pane sequence (AC-19) -------------------------------------------------------

#[test]
fn open_pane_splits_then_runs_editor_in_the_new_pane() {
    let mut cli = FakeCli::new(r#"{"result":{"pane":{"pane_id":"pane-77"}}}"#);
    open_pane(
        &PathBuf::from("/work/src/main.rs"),
        OsStr::new("vim"),
        "pane-12",
        &mut cli,
    )
    .unwrap();

    assert_eq!(
        cli.calls.len(),
        2,
        "exactly two herdr CLI calls (split, run)"
    );
    assert_eq!(
        cli.calls[0],
        osv(&[
            "pane",
            "split",
            "pane-12",
            "--direction",
            "right",
            "--no-focus"
        ]),
    );
    // The new pane id is parsed from the split's result.pane.pane_id and used for run.
    assert_eq!(
        cli.calls[1],
        osv(&["pane", "run", "pane-77", "vim '/work/src/main.rs'"])
    );
}

#[test]
fn hostile_file_name_is_shell_escaped_not_injected() {
    // A file name carrying shell metacharacters must be neutralized inside single quotes.
    let mut cli = FakeCli::new(r#"{"result":{"pane":{"pane_id":"p1"}}}"#);
    let file = PathBuf::from("/work/a'; rm -rf ~; '.txt");
    open_pane(&file, OsStr::new("vim"), "p0", &mut cli).unwrap();

    let cmd = cli.calls[1][3].to_string_lossy().into_owned();
    // POSIX single-quote escaping: each embedded ' becomes '\'' .
    assert_eq!(cmd, r#"vim '/work/a'\''; rm -rf ~; '\''.txt'"#);
    // Round-trip proof: a shell parsing the quoted word recovers the *literal* filename —
    // the metacharacters are data inside the quotes, never executed.
    let quoted = cmd.strip_prefix("vim ").unwrap();
    assert_eq!(shell_unquote(quoted), "/work/a'; rm -rf ~; '.txt");
}

#[test]
fn dash_leading_relative_path_is_made_flag_safe() {
    // A relative path beginning with '-' must not be readable as an editor flag.
    let mut cli = FakeCli::new(r#"{"result":{"pane":{"pane_id":"p1"}}}"#);
    open_pane(&PathBuf::from("-rf"), OsStr::new("vim"), "p0", &mut cli).unwrap();
    let cmd = cli.calls[1][3].to_string_lossy().into_owned();
    assert_eq!(cmd, "vim './-rf'");
}

#[test]
fn unparseable_split_output_errors_without_a_run_call() {
    let mut cli = FakeCli::new("not json at all");
    let r = open_pane(&PathBuf::from("/w/f"), OsStr::new("vim"), "p0", &mut cli);
    assert!(r.is_err(), "a bad split result must be an error");
    assert_eq!(cli.calls.len(), 1, "no run call after a failed split");
}

#[test]
fn flag_like_pane_id_from_split_is_rejected() {
    // Option injection: a split result whose pane_id looks like a flag is refused.
    let mut cli = FakeCli::new(r#"{"result":{"pane":{"pane_id":"--evil"}}}"#);
    let r = open_pane(&PathBuf::from("/w/f"), OsStr::new("vim"), "p0", &mut cli);
    assert!(r.is_err());
    assert_eq!(cli.calls.len(), 1, "no run call with an unsafe new pane id");
}

#[test]
fn flag_like_current_pane_id_is_rejected_before_any_call() {
    let mut cli = FakeCli::new("{}");
    let r = open_pane(
        &PathBuf::from("/w/f"),
        OsStr::new("vim"),
        "--evil",
        &mut cli,
    );
    assert!(r.is_err());
    assert!(
        cli.calls.is_empty(),
        "no split with an unsafe current pane id"
    );
}
