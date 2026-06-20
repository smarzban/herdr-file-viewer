//! T-23 — e2e (pty): in a plain (non-git) directory the viewer degrades to a working file
//! browser — the tree lists files and a file renders — while the git-only keys (changed-only,
//! baseline) are inert and never error (AC-26).

mod common;

use common::{viewer_command, TempDir};
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::time::Duration;

#[test]
fn non_git_directory_browses_and_renders_with_git_keys_inert() {
    let dir = TempDir::new();
    let p = dir.path();
    // No `git init` — a plain directory. One file, so it is the initial selection and its
    // content fills the (empty) content pane on the first draw.
    std::fs::write(p.join("notes.txt"), "PLAINVIEW\n").unwrap();

    let mut s = Session::spawn(viewer_command(p)).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    // Tree browsing works without git (AC-2 / AC-26).
    s.expect("notes.txt").expect("tree should list files in a non-git directory");
    // A file renders (content pane fills from empty with the selected file).
    s.expect("PLAINVIEW").expect("a file renders in a non-git directory");

    // The git-only keys must be inert here — no panic, no error. Driving them then exiting
    // cleanly proves they degrade gracefully (AC-26).
    for key in ["c", "b", "j", "v"] {
        s.send(key).expect("send git/other key");
    }

    s.send("q").expect("send close");
    s.expect(Eof).expect("the viewer terminates after the close key");
    match s.get_process().wait().expect("reap the viewer") {
        WaitStatus::Exited(_, code) => {
            assert_eq!(code, 0, "AC-26: git keys are inert in a non-git dir, no crash")
        }
        other => panic!("expected a clean exit, got {other:?}"),
    }
}
