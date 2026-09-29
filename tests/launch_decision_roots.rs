//! The binary's `--launch-decision-tab` with the REAL root resolver (`src/main.rs`): a pane cwd
//! maps to its worktree top level, canonicalized, so a shell in a subdirectory (or behind a
//! symlink such as macOS `/tmp` → `/private/tmp`) matches the viewer showing that repo, and a
//! viewer the root picker opened on another directory is not switched to.
#![cfg(unix)]

mod common;

use common::{TempDir, canon, init_repo_with_commit};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn decide(panes: &[String]) -> String {
    let json = format!(r#"{{"result":{{"panes":[{}]}}}}"#, panes.join(","));
    let mut child = Command::new(env!("CARGO_BIN_EXE_herdr-file-viewer"))
        .arg("--launch-decision-tab")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the decision");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn pane(id: &str, label: &str, focused: bool, tab: &str, cwd: &Path) -> String {
    format!(
        r#"{{"pane_id":"{id}","label":"{label}","focused":{focused},"tab_id":"{tab}","cwd":"{}"}}"#,
        cwd.display()
    )
}

#[test]
fn switches_to_the_viewer_of_the_focused_repo_from_a_subdirectory() {
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    std::fs::create_dir_all(repo.path().join("src/deep")).unwrap();
    let elsewhere = TempDir::new();

    let d = decide(&[
        // The shell's cwd as typed (un-canonicalized, a subdirectory of the repo).
        pane("w1:p1", "", true, "w1:t1", &repo.path().join("src/deep")),
        // A picker-opened viewer on an unrelated directory comes first in the list...
        pane("w1:pA", "Files", false, "w1:t2", &canon(elsewhere.path())),
        // ...and the repo's viewer, whose cwd is its (canonical) root.
        pane("w1:pB", "Files", false, "w1:t3", &canon(repo.path())),
    ]);
    assert_eq!(d, "SWITCHTAB w1:t3");
}

#[test]
fn opens_when_only_a_viewer_on_another_root_exists() {
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    let elsewhere = TempDir::new();

    let d = decide(&[
        pane("w1:p1", "", true, "w1:t1", repo.path()),
        pane("w1:pA", "Files", false, "w1:t2", &canon(elsewhere.path())),
    ]);
    assert_eq!(d, "OPEN");
}
