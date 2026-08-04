//! e2e (pty): pinned previews are session-only and every interaction remains read-only.
//!
//! Unix-only: see `tests/cli_smoke.rs` for why the `expectrl` pty suite is not ported to
//! Windows's `conpty` backend in this feature.
#![cfg(unix)]

mod common;

use common::{TempDir, git, init_repo_with_commit, viewer_command, workspace_fingerprint};
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::time::Duration;

/// AC-13 / AC-36 / AC-38 / AC-39: a real viewer session can pin, focus and search the frozen
/// preview, resize it from the keyboard, cross worktrees, and unpin without changing either
/// workspace or its git state. A fresh viewer then proves the pin was not persisted.
#[test]
fn pinned_preview_journey_is_read_only_and_does_not_outlive_its_process() {
    // Linked worktrees must be siblings: `git worktree add` requires its target not to exist.
    let main_dir = TempDir::new();
    let feature_dir = TempDir::new();
    let main = main_dir.path();
    let feature = feature_dir.path().join("feature-worktree");
    init_repo_with_commit(main);

    // This filename sorts before seed.txt, making it the launch selection in the main worktree.
    // The distinctive body is the completed-render barrier before we ask the controller to pin.
    std::fs::write(
        main.join("PINNED.txt"),
        "PINNED_TOP\nPINNED_SEARCH_MARKER\n",
    )
    .expect("write pin candidate");
    git(main, &["add", "PINNED.txt"]);
    git(
        main,
        &["commit", "-q", "-m", "add pinned-preview candidate"],
    );

    git(
        main,
        &[
            "worktree",
            "add",
            "-b",
            "pinned-preview-feature",
            feature.to_str().expect("utf-8 worktree path"),
        ],
    );
    // Its unique body lets the post-unpin focus cycle prove that the active preview, rather than
    // a retained frozen pin, receives search input.
    std::fs::write(
        main.join("UNPIN_TARGET.txt"),
        "UNPIN_ACTIVE_SEARCH_MARKER\n",
    )
    .expect("write post-unpin target");
    git(main, &["add", "UNPIN_TARGET.txt"]);
    git(main, &["commit", "-q", "-m", "add post-unpin target"]);
    // FEATURE.txt sorts before PINNED.txt in the feature worktree, which supplies a visible
    // outcome anchor after the switch without modifying the main worktree during the journey.
    std::fs::write(feature.join("FEATURE.txt"), "FEATURE_WORKTREE_MARKER\n")
        .expect("write feature-only file");
    git(&feature, &["add", "FEATURE.txt"]);
    git(&feature, &["commit", "-q", "-m", "add feature marker"]);

    // Capture both roots. `workspace_fingerprint` includes every non-.git byte, HEAD,
    // porcelain status, and `git worktree list --porcelain`; it deliberately stays unchanged.
    let main_before = workspace_fingerprint(main);
    let feature_before = workspace_fingerprint(&feature);

    let mut command = viewer_command(main);
    command.env("EDITOR", "true");
    let mut session = Session::spawn(command).expect("spawn the viewer in a pty");
    session.set_expect_timeout(Some(Duration::from_secs(15)));

    session
        .expect("PINNED_TOP")
        .expect("main file has rendered before pinning");
    session.send("p").expect("pin the settled active preview");

    // Focus begins on the tree, so the first Tab reaches the pinned preview. Search there, then
    // complete the other two focus transitions (pinned -> active -> tree) and return to pinned.
    session.send("\t").expect("focus pinned preview");
    session
        .expect("Pinned: PINNED.txt")
        .expect("a frozen pinned-preview identity is visibly present");
    session.send("/").expect("open pinned-preview search");
    session
        .expect("Search:")
        .expect("pinned-preview search prompt is visible");
    for key in "PINNED_SEARCH_MARKER".chars() {
        session
            .send(key.to_string())
            .expect("type pinned search query");
    }
    session
        .expect("(1/1)")
        .expect("the pinned preview owns the search state");
    session.send("\r").expect("commit pinned search");
    session
        .send("\t")
        .expect("cycle pinned focus to active preview");
    session.send("\t").expect("cycle active focus to tree");
    session
        .send("\t")
        .expect("cycle tree focus back to pinned preview");

    // `{` and `}` must resize only the preview divider. They are deliberately driven via the
    // keyboard: this end-to-end journey must never inject a mouse event.
    session.send("{").expect("shrink pinned preview share");
    session.send("}").expect("grow pinned preview share");

    // Worktree switching remains available from pinned focus. The picker begins on the current
    // main worktree; j chooses its one linked sibling. The feature marker proves the re-root.
    session
        .send("W")
        .expect("open worktree picker from pinned focus");
    session
        .expect("Switch worktree")
        .expect("worktree picker is visible");
    session.send("j").expect("select feature worktree");
    session.send("\r").expect("switch to feature worktree");
    // A re-root resets focus to the tree. In a narrow PTY, cycle tree -> pinned -> active so the
    // independently updated active region is the sole full-width preview before checking it.
    session
        .send("\t")
        .expect("cycle from tree to pinned after re-root");
    session
        .expect("Pinned: PINNED.txt")
        .expect("the pinned identity survives the switch to the feature worktree");
    session
        .send("\t")
        .expect("focus active preview after re-root");
    session
        .expect("FEATURE_WORKTREE_MARKER")
        .expect("active preview followed the re-root while the pin remained held");

    // Return to the original root. The picker starts on the feature row, so k selects main.
    session.send("W").expect("open worktree picker to return");
    session.send("k").expect("select original main worktree");
    session
        .send("\r")
        .expect("return to the pin's origin worktree");
    session
        .send("\t")
        .expect("cycle returned tree focus to pinned");
    session
        .expect("Pinned: PINNED.txt")
        .expect("the pinned identity survives the return to its origin worktree");
    session.send("\t").expect("focus returned active preview");
    session
        .expect("PINNED_TOP")
        .expect("main active preview returned to the pin's original file");
    session.send("p").expect("unpin the same active file");
    // With the pin gone, focus must cycle Content -> Tree -> active (not Content -> Tree ->
    // retained pinned preview). The narrow PTY may have no active-content column, so use the
    // finder to make the newly selected target visible. "UNPIN" uniquely selects
    // UNPIN_TARGET.txt in the current main worktree. `(1/1)` below is possible only when the
    // second Tab targets the active preview, because the former pin lacks this marker.
    session.send("\t").expect("focus tree after unpinning");
    session
        .send("f")
        .expect("open finder for the post-unpin active file");
    session
        .expect("Go to file")
        .expect("the post-unpin finder is visible");
    for key in "UNPIN".chars() {
        session
            .send(key.to_string())
            .expect("type the distinct post-unpin finder query");
    }
    session
        .expect("UNPIN_TARGET.txt")
        .expect("the finder selected the distinct post-unpin active file");
    session
        .send("\r")
        .expect("confirm the distinct post-unpin active file");
    session
        .expect("UNPIN_ACTIVE_SEARCH_MARKER")
        .expect("the tree selection updated the active preview after unpinning");
    session
        .send("\t")
        .expect("focus the sole active preview after unpinning");
    session
        .send("/")
        .expect("open search in the post-unpin active preview");
    session
        .expect("Search:")
        .expect("the post-unpin active-preview search prompt is visible");
    for key in "UNPIN_ACTIVE_SEARCH_MARKER".chars() {
        session
            .send(key.to_string())
            .expect("type the active-only post-unpin search query");
    }
    session
        .expect("(1/1)")
        .expect("unpin removed the former pin: the focus cycle now searches the active file");
    session.send("\r").expect("commit post-unpin active search");
    session
        .expect("Esc clear")
        .expect("the post-unpin search committed before dismissal and close");
    session
        .send("\x1b")
        .expect("dismiss post-unpin active search");
    // A bare ESC must be separated from the next byte or crossterm may decode the pair as Alt+q.
    // This is input framing, not a viewer-settling wait.
    std::thread::sleep(Duration::from_millis(150));
    // Send both close keys in one write. In the ordinary unzoomed journey the first quits and the
    // queued second byte is discarded; if a zoom-state perturbation is added above, the first
    // peels zoom and the second quits. Neither case relies on an unreliable runtime state probe.
    session
        .send("qq")
        .expect("close the viewer regardless of the zoom layer");
    session.expect(Eof).expect("the journey exits cleanly");
    match session.get_process().wait().expect("reap journey viewer") {
        WaitStatus::Exited(_, 0) => {}
        other => panic!("expected a clean journey exit, got {other:?}"),
    }

    assert_eq!(
        workspace_fingerprint(main),
        main_before,
        "the pinned-preview journey must not alter the main worktree or git state"
    );
    assert_eq!(
        workspace_fingerprint(&feature),
        feature_before,
        "the pinned-preview journey must not alter the feature worktree or git state"
    );

    // A fresh process has no pin. Pressing p must create (not remove) a fresh snapshot, giving
    // us a behaviorally observable, process-lifetime assertion without inspecting internals.
    let mut fresh_command = viewer_command(main);
    fresh_command.env("EDITOR", "true");
    let mut fresh = Session::spawn(fresh_command).expect("spawn fresh viewer in a pty");
    fresh.set_expect_timeout(Some(Duration::from_secs(15)));
    fresh
        .expect("PINNED_TOP")
        .expect("fresh process rendered the main file");
    fresh.send("p").expect("pin in fresh process");
    fresh.send("\t").expect("focus the fresh pinned preview");
    fresh
        .expect("Pinned: PINNED.txt")
        .expect("AC-13: fresh process started without a persisted pin");
    fresh.send("q").expect("close fresh viewer");
    fresh.expect(Eof).expect("fresh viewer exits cleanly");
    match fresh.get_process().wait().expect("reap fresh viewer") {
        WaitStatus::Exited(_, 0) => {}
        other => panic!("expected a clean fresh-process exit, got {other:?}"),
    }

    // Best-effort cleanup before TempDir drops the repository that owns the linked worktree.
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(main)
        .args([
            "worktree",
            "remove",
            "--force",
            feature.to_str().expect("utf-8 worktree path"),
        ])
        .output();
}
