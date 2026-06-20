//! T-21 — e2e (pty): the viewer is fully keyboard-operable (AC-18). We drive ONLY the
//! keyboard over a pseudo-terminal — no mouse — exercising every viewer function, and
//! confirm a clean exit (a key that panicked the run loop would fail the exit assertion).
//!
//! Functional assertions are limited to what a raw pty stream can prove *robustly*: text that
//! lands in previously-blank cells (the initial full draw, and content drawn into the empty
//! content pane on first selection of a file) appears contiguously. ratatui's differential
//! redraw fragments *overwritten* regions (row shifts on toggle/filter) across cursor-move
//! escapes, so those keys' precise effects are asserted by the unit/snapshot tests
//! (tree_filters.rs, presenter*.rs, controller.rs); here they are driven for liveness.
//!
//! Expand is proven *functionally* and robustly: after expanding the directory and navigating
//! onto the revealed child, the child's content fills the empty pane — which can only happen
//! if expand actually revealed it. Content markers are single tokens, so the syntax renderer
//! (bat, when present) cannot split them and the assertion holds whether or not it is installed.

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
    // A directory at the top (cursor starts here → empty content pane) holding a committed
    // file whose content we assert once expand + navigation reveal and select it.
    std::fs::create_dir(p.join("subdir")).unwrap();
    std::fs::write(p.join("subdir").join("grand.txt"), "GRANDCHILD\n").unwrap();
    std::fs::write(p.join("aaa.txt"), "ALPACAMARK\n").unwrap();
    git(p, &["add", "aaa.txt", "subdir/grand.txt"]);
    git(p, &["commit", "-q", "-m", "files"]);
    // An untracked file so the changed-only / baseline keys have something to act on.
    std::fs::write(p.join("bbb.txt"), "BRAVO\n").unwrap();

    // A trivial, instantly-exiting "editor" so the open-in-editor key is safe to drive here.
    let mut cmd = viewer_command(p);
    cmd.env("EDITOR", "true");
    let mut s = Session::spawn(cmd).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    // Initial full draw lists the tree (AC-3 display, AC-17 launch).
    s.expect("aaa.txt").expect("tree should list files on launch");

    // Expand the selected directory, then navigate onto the revealed child: its content fills
    // the empty pane — proving expand (l) AND navigation (j) AND content render functionally.
    s.send("l").expect("send expand");
    s.send("j").expect("send nav-down onto the revealed child");
    s.expect("GRANDCHILD").expect("expand revealed the child and navigation rendered it");

    // Back up onto the directory and collapse it (h acts on the selected directory).
    s.send("k").expect("send nav-up to the directory");
    s.send("h").expect("send collapse on the directory");

    // Drive every remaining keyboard function; each must be wired and must not crash the loop.
    for key in [
        "i",  // toggle ignored
        "c",  // changed-only
        "b",  // toggle baseline
        "v",  // cycle view
        "\t", // toggle focus
        "e",  // open-in-editor (hands off to `true`, which exits immediately)
    ] {
        s.send(key).expect("send key");
    }
    // The editor hand-off suspends/resumes the terminal; let it settle before the close key.
    std::thread::sleep(Duration::from_millis(200));

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
