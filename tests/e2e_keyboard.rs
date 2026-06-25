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

use common::{TempDir, git, init_repo_with_commit, viewer_command};
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
    s.expect("aaa.txt")
        .expect("tree should list files on launch");

    // Expand the selected directory, then navigate onto the revealed child: its content fills
    // the empty pane — proving expand (l) AND navigation (j) AND content render functionally.
    s.send("l").expect("send expand");
    s.send("j").expect("send nav-down onto the revealed child");
    s.expect("GRANDCHILD")
        .expect("expand revealed the child and navigation rendered it");

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
    s.expect(Eof)
        .expect("the viewer terminates after the close key");
    match s.get_process().wait().expect("reap the viewer") {
        WaitStatus::Exited(_, code) => {
            assert_eq!(
                code, 0,
                "AC-18/AC-20: no keyboard action crashed the viewer"
            )
        }
        other => panic!("expected a clean exit, got {other:?}"),
    }
}

/// M-1 (AC-5's e2e oracle): the worktree picker is fully keyboard-operable end to end. We
/// open it (`W`), confirm the overlay renders (its `Switch worktree` title — a stable,
/// blank-cell anchor, proving AC-1/AC-5), `Esc` to cancel (AC-6), then re-open, navigate
/// (`j`) onto a *second* worktree, confirm (`Enter`) → the viewer re-roots to it (AC-5/AC-7).
/// Finally `q` exits cleanly — proving no picker key crashed the run loop.
///
/// Outcome anchor: the feature worktree holds a uniquely-named file (`FEATONLY.txt`) whose
/// single-token content (`FEATMARK`) only the feature root can show. We prove the re-root by
/// *opening that file* and asserting its content fills the (previously blank) content pane —
/// the most robust anchor (the brief's recommended fallback), since a marker landing in a
/// previously-blank cell appears contiguously, whereas a tree ROW that *overwrites* the old
/// root's tree is fragmented across cursor-move escapes by ratatui's differential redraw (the
/// same caveat the keyboard e2e above documents). `FEATONLY.txt` sorts first in the feature
/// tree, so the cursor (reset to the top by the re-root) lands on it; `Enter` zooms it. The
/// cancel sub-case runs first so its "root unchanged + loop healthy" guarantee is proven by
/// the confirm path that follows still working.
#[test]
fn worktree_picker_switches_root_by_keyboard_and_exits_cleanly() {
    // The two worktrees live at sibling temp paths (a linked worktree must be outside its repo).
    let repo = TempDir::new();
    let main = repo.path();
    init_repo_with_commit(main);

    // A second worktree on its own branch at a sibling path, holding a file whose content only
    // the feature root can show (the re-root oracle). The uppercase name sorts ahead of the
    // repo's lowercase `seed.txt`, so it is the feature tree's first row.
    let feature = TempDir::new();
    // `git worktree add` requires the target dir to NOT already exist, so use a child path of
    // the (existing) temp dir.
    let feature_path = feature.path().join("wt");
    git(
        main,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            feature_path.to_str().unwrap(),
        ],
    );
    std::fs::write(feature_path.join("FEATONLY.txt"), "FEATMARK\n").unwrap();
    git(&feature_path, &["add", "FEATONLY.txt"]);
    git(&feature_path, &["commit", "-q", "-m", "feature-only file"]);

    // Spawn the viewer rooted at the MAIN worktree.
    let mut cmd = viewer_command(main);
    cmd.env("EDITOR", "true");
    let mut s = Session::spawn(cmd).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    // Launch lists main's tree (its committed seed file) — the viewer is up on the main root.
    s.expect("seed.txt")
        .expect("tree should list main's files on launch");

    // --- Cancel sub-case first (AC-6): open the picker, confirm the overlay renders (its
    // `Switch worktree` title — a stable blank-cell anchor, proving AC-1/AC-5), then Esc to
    // cancel. Cancel leaving the root unchanged AND the run loop healthy is then proven by the
    // confirm path below still working: a cancel that re-rooted or crashed would break it. ---
    s.send("W").expect("send open-picker");
    s.expect("Switch worktree")
        .expect("the picker overlay should render its title");
    // Settle before/after Esc so crossterm sees a lone ESC (a bare Esc immediately followed by
    // a char is decoded as Alt+char, which maps to no intent).
    std::thread::sleep(Duration::from_millis(150));
    s.send("\u{1b}")
        .expect("send Esc to cancel the picker (AC-6)");
    std::thread::sleep(Duration::from_millis(150));

    // --- Confirm path: W → j → Enter → re-root to the feature worktree (AC-5/AC-7). ---
    s.send("W").expect("re-open the picker");
    s.send("j")
        .expect("send nav-down onto the feature worktree row");
    s.send("\r").expect("send Enter to confirm the switch");
    // The re-root happened via the keyboard. Open the feature root's unique file (the cursor
    // reset to the top row = FEATONLY.txt) so its single-token content fills the previously
    // blank content pane — a robust outcome anchor that ONLY the feature root can produce. If
    // the earlier cancel had re-rooted, crashed the loop, or left the picker open, this fails.
    s.send("\r").expect("activate the feature file (zoom)");
    s.expect("FEATMARK")
        .expect("after the switch the feature worktree's file content is shown");

    // The close key returns control and exits cleanly (AC-20) — no picker key crashed the loop.
    // From the zoomed file the first `q` un-zooms (close_or_unzoom), the second quits. We use
    // two `q` presses (not Esc-then-q): an ESC immediately followed by a char is read by
    // crossterm as Alt+char, which maps to no intent — so a trailing Esc could swallow the quit.
    s.send("q").expect("send close (un-zoom)");
    std::thread::sleep(Duration::from_millis(150));
    s.send("q").expect("send close (quit)");
    s.expect(Eof)
        .expect("the viewer terminates after the close key");
    let status = s.get_process().wait().expect("reap the viewer");
    // Remove the linked worktree before the temp dirs are dropped (best-effort cleanup).
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(main)
        .args([
            "worktree",
            "remove",
            "--force",
            feature_path.to_str().unwrap(),
        ])
        .output();
    match status {
        WaitStatus::Exited(_, code) => {
            assert_eq!(code, 0, "AC-5/AC-20: no picker key crashed the viewer")
        }
        other => panic!("expected a clean exit, got {other:?}"),
    }
}
