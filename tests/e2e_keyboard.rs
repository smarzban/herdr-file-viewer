//! T-21 — e2e (pty): the viewer is fully keyboard-operable (AC-18). We drive ONLY the
//! keyboard over a pseudo-terminal — no mouse — exercising every viewer function, and
//! confirm a clean exit (a key that panicked the run loop would fail the exit assertion).
//!
//! Screen assertions are limited to what a raw pty stream can prove *robustly*: the initial
//! full draw, and content drawn into the (previously empty) content pane on first selection
//! of a file — both land in blank cells and so appear contiguously. ratatui's differential
//! redraw fragments *overwritten* regions (row shifts on expand/filter) across cursor-move
//! escapes, so the precise effects of those keys are asserted by the unit/snapshot tests
//! (presenter.rs, presenter_narrow.rs, controller.rs); here those keys are driven for
//! liveness (they must not crash the loop).

mod common;

use common::{git, init_repo_with_commit, viewer_command, TempDir};
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::time::Duration;

#[test]
fn every_keyboard_function_drives_the_viewer_and_it_exits_cleanly() {
    let dir = TempDir::new();
    let p = dir.path();
    init_repo_with_commit(p);
    // A directory at the top (cursor starts here → empty content pane), and a committed file
    // whose content we can assert when navigation first fills the pane.
    std::fs::create_dir(p.join("subdir")).unwrap();
    std::fs::write(p.join("subdir").join("grand.txt"), "GRANDCHILD\n").unwrap();
    std::fs::write(p.join("aaa.txt"), "ALPACAMARK\n").unwrap();
    git(p, &["add", "aaa.txt", "subdir/grand.txt"]);
    git(p, &["commit", "-q", "-m", "files"]);
    // An untracked file so the changed-only / baseline keys have something to act on (these
    // are driven for liveness, not asserted). No root `.gitignore`, so the first file row is
    // deterministically `aaa.txt`.
    std::fs::write(p.join("bbb.txt"), "BRAVO\n").unwrap();

    let mut s = Session::spawn(viewer_command(p)).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    // Initial full draw lists the tree (AC-3 display, AC-17 launch).
    s.expect("aaa.txt").expect("tree should list files on launch");

    // Navigate from the directory onto the first file → the content pane fills from empty
    // with that file's content (AC-18 navigation + AC-10 content render).
    s.send("j").expect("send nav-down");
    s.expect("ALPACAMARK").expect("navigating renders the selected file's content");

    // Exercise every remaining keyboard function. Each must be wired and must not crash the
    // loop; their precise screen effects are covered by the unit/snapshot tests.
    for key in [
        "l",  // expand
        "j",  // nav onto the revealed child
        "h",  // collapse
        "i",  // toggle ignored
        "c",  // changed-only
        "b",  // toggle baseline
        "v",  // cycle view
        "\t", // toggle focus
        "k",  // nav up
    ] {
        s.send(key).expect("send key");
    }

    // The close key returns control and exits cleanly (AC-20).
    s.send("q").expect("send close");
    s.expect(Eof).expect("the viewer terminates after the close key");
    match s.get_process().wait().expect("reap the viewer") {
        WaitStatus::Exited(_, code) => {
            assert_eq!(code, 0, "AC-18/AC-20: no keyboard action crashed the viewer")
        }
        other => panic!("expected a clean exit, got {other:?}"),
    }
}
