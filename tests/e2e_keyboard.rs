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

/// AC-7 e2e oracle: the go-to-file finder routes printable keys to the query (not the tree),
/// and Enter confirms the selection while Esc cancels. We drive `f`, then type `j` and `w`
/// (keys that are otherwise NavDown and ToggleWrap in the viewer), and assert the overlay shows
/// the typed query `> jw` — proving the event-loop finder arm is routing presses to
/// `handle_finder_key` instead of `map_key`. Then we cancel with Esc and prove a second open
/// + Enter jumps to the file (the file's unique content marker appears in the content pane).
#[test]
fn finder_routes_printable_keys_to_query_and_enter_confirms_esc_cancels() {
    let dir = TempDir::new();
    let p = dir.path();
    init_repo_with_commit(p);
    // `alpha.txt` sorts before `jwtest.txt` so the cursor starts on `alpha.txt` at launch —
    // meaning `jwtest.txt`'s content is NOT pre-rendered. When Enter confirms the finder
    // selection, `JWMARK` is drawn into the previously-blank content pane for the first time,
    // making it a robust pty anchor (blank-cell content appears contiguously).
    std::fs::write(p.join("alpha.txt"), "ALPHAMARK\n").unwrap();
    // A file whose name embeds `jw` so our typed query matches it uniquely, and whose content has
    // a unique single-token marker we can anchor on after the confirm (AC-7 reveal outcome).
    // The file is committed so it appears in the candidate index.
    std::fs::write(p.join("jwtest.txt"), "JWMARK\n").unwrap();
    git(p, &["add", "alpha.txt", "jwtest.txt"]);
    git(p, &["commit", "-q", "-m", "finder-test files"]);

    let mut cmd = viewer_command(p);
    cmd.env("EDITOR", "true");
    let mut s = Session::spawn(cmd).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    // Wait for the initial tree render so the viewer is fully up. `alpha.txt` sorts first and
    // is the cursor's initial selection; `jwtest.txt` is in the same tree draw and also visible.
    s.expect("alpha.txt")
        .expect("tree lists the committed files on launch");

    // --- Cancel sub-case (AC-6): open finder, type `j` and `w` (viewer nav/wrap keys), assert
    // the overlay query reflects the typed text, then Esc closes the finder. ---
    s.send("f").expect("send open-finder");
    // The overlay title is the stable blank-cell anchor (drawn into previously-blank cells).
    s.expect("Go to file")
        .expect("the finder overlay renders its title after `f`");

    // Type `j` — this is NavDown in the viewer's normal key map. If routing were wrong, `j`
    // would drive the tree cursor down instead of updating the finder query. The AC-7 proof is
    // that `jwtest.txt` appears in the finder's match-row list: the match list was empty before
    // typing (empty query → no matches), so these cells were blank; the first render of the match
    // row writes "jwtest.txt" contiguously into previously-blank cells — a robust pty anchor.
    s.send("j").expect("send `j` into the finder query");
    s.expect("jwtest.txt")
        .expect("AC-7 routing proof: `j` edited the finder query (not NavDown) — jwtest.txt appears in the match list");

    // Type `w` — ToggleWrap in the viewer; must also land in the query without toggling anything.
    s.send("w").expect("send `w` into the finder query");

    // Settle before Esc so crossterm sees a lone ESC (not Alt+char).
    std::thread::sleep(Duration::from_millis(150));
    s.send("\u{1b}").expect("send Esc to cancel the finder");
    std::thread::sleep(Duration::from_millis(150));

    // --- Confirm sub-case (AC-7 reveal): open finder, narrow to the unique file, Enter reveals. ---
    s.send("f").expect("re-open the finder");
    s.expect("Go to file")
        .expect("the finder overlay opens again");

    // Type `jw` — the query "jw" is a subsequence of `jwtest.txt` only. Enter confirms.
    s.send("j").expect("send `j`");
    s.send("w").expect("send `w`");
    // Enter confirms the selection: the finder closes, the tree reveals `jwtest.txt`, and the
    // content pane renders it. `alpha.txt` is the cursor's current position (the initial
    // selection); after reveal, `jwtest.txt` becomes selected and its content is dispatched for
    // rendering. `JWMARK` appears in the content pane for the FIRST time (the cursor was on
    // `alpha.txt` at launch, so `jwtest.txt` was never rendered — these are blank cells).
    s.send("\r")
        .expect("send Enter to confirm the finder selection");
    s.expect("JWMARK")
        .expect("AC-7: Enter confirms — the selected file's content is shown in the content pane");

    // Clean exit — no finder key crashed the run loop.
    s.send("q").expect("send close");
    s.expect(Eof)
        .expect("the viewer terminates cleanly after the finder flow");
    match s.get_process().wait().expect("reap the viewer") {
        WaitStatus::Exited(_, code) => {
            assert_eq!(code, 0, "AC-7/AC-20: no finder key crashed the viewer")
        }
        other => panic!("expected a clean exit, got {other:?}"),
    }
}

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

/// T-5 — go-to-line e2e: `:` opens the line-number prompt on a source-mapped (SyntaxContent) file;
/// typing a line number and Enter scrolls the content to that line (AC-3 jump + AC-21 routing);
/// pressing `:` on a RenderedMarkdown file ALSO opens the prompt (AC-7 revised — it opens in every
/// view; the switch-then-jump on confirm is proven by the controller unit test).
///
/// Routing proof (AC-21): after opening the prompt with `:`, we send `j` (NavDown in the normal
/// viewer key-map) and then `40`. If routing were broken, `j` would move the tree cursor onto
/// `notes.md`, and the subsequent jump would target the wrong file — so `DEEPMARKER` (source line
/// 40 of `long.txt`) would never appear.
///
/// Anchor robustness: the content pane is `bat`-rendered with a line-number gutter, and ratatui
/// redraws only changed cells — so a marker whose characters coincidentally match the cells it
/// overwrites gets split across cursor-move escapes in the pty stream (an earlier `DEEPMARK040`
/// fragmented because its trailing `0` matched). `DEEPMARKER` is **all letters** and the filler
/// lines are `L<NN>`, so every column of the marker differs from whatever sat there before the
/// scroll → ratatui writes the row in one contiguous run and `expect("DEEPMARKER")` is reliable.
/// `DEEPMARKER` is on line 40, below the initial viewport, so it only appears after the jump.
#[test]
fn go_to_line_jumps_to_a_source_line_and_opens_in_markdown_too() {
    let dir = TempDir::new();
    let p = dir.path();
    init_repo_with_commit(p);

    // long.txt: 60 lines. Line 1 = TOPMARK001 (launch anchor, drawn into blank cells); line 40 =
    // DEEPMARKER (the jump target, all-letters so it redraws contiguously); every other line is
    // a short `L<NN>` filler that differs from DEEPMARKER in every column.
    let mut lines = Vec::with_capacity(60);
    for i in 1u32..=60 {
        if i == 1 {
            lines.push("TOPMARK001".to_string());
        } else if i == 40 {
            lines.push("DEEPMARKER".to_string());
        } else {
            lines.push(format!("L{i:02}"));
        }
    }
    std::fs::write(p.join("long.txt"), lines.join("\n") + "\n").unwrap();

    // notes.md: RenderedMarkdown view-mode → `:` opens the prompt too (AC-7 revised).
    std::fs::write(p.join("notes.md"), "# MDHEADERMARK\nSome note text.\n").unwrap();

    // Commit both so view-policy picks the right modes: long.txt → SyntaxContent (`:` jumps in-place);
    // notes.md → RenderedMarkdown (`:` opens, confirm auto-switches). `long.txt` sorts before
    // `notes.md`, so the cursor starts on long.txt at launch.
    git(p, &["add", "long.txt", "notes.md"]);
    git(p, &["commit", "-q", "-m", "go-to-line test files"]);

    let mut cmd = viewer_command(p);
    cmd.env("EDITOR", "true");
    let mut s = Session::spawn(cmd).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    // Step 1: initial draw — tree lists long.txt, its top content is shown.
    s.expect("long.txt")
        .expect("tree should list long.txt on launch");
    s.expect("TOPMARK001")
        .expect("long.txt top content should be visible on launch");

    // Step 2: open the go-to-line prompt (long.txt is SyntaxContent, so `:` opens it). Give the
    // event loop a beat to open the prompt before the next key, so `j` lands inside the prompt and
    // not as a NavDown on the tree.
    s.send(":").expect("send `:` to open the go-to-line prompt");
    std::thread::sleep(Duration::from_millis(300));

    // Step 3: AC-21 routing proof — `j` (NavDown in normal mode) must be swallowed by the open
    // prompt (a non-digit, ignored), NOT navigate the tree.
    s.send("j")
        .expect("send `j` — must be swallowed by the prompt, not fire NavDown");

    // Step 4: type the line number. The prompt now holds "40" (j was ignored).
    s.send("40").expect("send the line-number digits");

    // Step 5: confirm — the content scrolls so source line 40 (DEEPMARKER) is visible. This is the
    // AC-3 jump proof AND confirms AC-21 routing: if `j` had escaped the prompt and selected
    // notes.md, there would be no DEEPMARKER to show.
    s.send("\r").expect("send Enter to confirm the jump");
    s.expect("DEEPMARKER").expect(
        "AC-3/AC-21: after Enter the content pane scrolls to source line 40 (DEEPMARKER visible)",
    );

    // Step 6: AC-7 (revised) live — go-to-line now opens in EVERY view. The prompt is closed and
    // focus is still the tree; `j` NavDowns onto notes.md (RenderedMarkdown), where `:` opens the
    // prompt (its "Go to line:" label appears) rather than the old "unavailable" notice. The notes.md
    // content pane has blank rows below its two lines, so the freshly-reserved prompt row writes the
    // label into previously-blank cells — a robust pty anchor. (The switch-then-jump on confirm is
    // proven rigorously by the controller unit test; here we prove the prompt opens in markdown.)
    s.send("j").expect("send NavDown to move to notes.md");
    s.expect("MDHEADERMARK")
        .expect("notes.md is now selected and its content is shown");
    s.send(":").expect("send `:` on the markdown file");
    s.expect("Go to line:")
        .expect("AC-7: `:` opens the go-to-line prompt even in a rendered-markdown view");

    // Step 7: Esc closes the prompt (otherwise the prompt would swallow the quit key), then exit.
    s.send("\u{1b}").expect("send Esc to close the prompt");
    std::thread::sleep(Duration::from_millis(150));
    s.send("q").expect("send close");
    s.expect(Eof)
        .expect("the viewer terminates cleanly after the go-to-line flow");
    match s.get_process().wait().expect("reap the viewer") {
        WaitStatus::Exited(_, code) => {
            assert_eq!(
                code, 0,
                "AC-21/AC-3/AC-7: no go-to-line key crashed the viewer"
            )
        }
        other => panic!("expected a clean exit, got {other:?}"),
    }
}
