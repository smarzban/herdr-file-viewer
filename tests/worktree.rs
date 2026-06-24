//! T-1 — Worktree model + porcelain parser (AC-1, AC-2, AC-N4).
//! T-2 — Worktree Provider: live enumeration (AC-1, AC-2, AC-11, AC-N1, AC-N2).

mod common;

use common::{TempDir, git, init_repo_with_commit};
use herdr_file_viewer::worktree::{list, parse_porcelain};
use std::path::Path;

/// Canned `-z` output covering: main (current_root), linked, detached, bare, prunable.
///
/// With `-z` each attribute line ends with `\0`, and records are separated by an extra `\0`
/// (i.e. `\0\0` between records). Verified against `git help worktree` "Porcelain Format".
fn canned_bytes() -> Vec<u8> {
    // main worktree — on branch "main", will be marked is_current
    let main = b"worktree /repo/main\0HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0branch refs/heads/main\0";
    // linked worktree — on branch "feature"
    let linked = b"worktree /repo/linked\0HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\0branch refs/heads/feature\0";
    // detached worktree
    let detached =
        b"worktree /repo/detached\0HEAD cccccccccccccccccccccccccccccccccccccccc\0detached\0";
    // bare worktree — must be excluded from result
    let bare = b"worktree /repo/bare\0bare\0";
    // prunable worktree — detached + prunable
    let prunable = b"worktree /repo/prunable\0HEAD dddddddddddddddddddddddddddddddddddddddd\0detached\0prunable gitdir file points to non-existent location\0";

    // Records separated by an extra `\0` (the NUL after the last attribute acts as record
    // terminator; the separator between records is the extra NUL).
    let mut out = Vec::new();
    out.extend_from_slice(main);
    out.push(b'\0'); // record separator
    out.extend_from_slice(linked);
    out.push(b'\0');
    out.extend_from_slice(detached);
    out.push(b'\0');
    out.extend_from_slice(bare);
    out.push(b'\0');
    out.extend_from_slice(prunable);
    // trailing \0 already present in the last record's last attribute; no extra separator needed
    out
}

#[test]
fn parses_porcelain_worktrees() {
    let bytes = canned_bytes();
    let current_root = Path::new("/repo/main");

    let worktrees = parse_porcelain(&bytes, current_root);

    // Bare worktrees are excluded.
    assert_eq!(worktrees.len(), 4, "bare worktree must be excluded");

    // --- main (is_current) ---
    let main = worktrees
        .iter()
        .find(|w| w.path.ends_with("main"))
        .expect("main worktree");
    assert_eq!(
        main.branch,
        Some("main".to_string()),
        "refs/heads/ prefix stripped"
    );
    assert!(!main.detached, "main is not detached");
    assert!(main.is_current, "main is is_current");
    assert!(!main.is_prunable, "main is not prunable");

    // --- linked ---
    let linked = worktrees
        .iter()
        .find(|w| w.path.ends_with("linked"))
        .expect("linked worktree");
    assert_eq!(
        linked.branch,
        Some("feature".to_string()),
        "refs/heads/ prefix stripped"
    );
    assert!(!linked.detached);
    assert!(!linked.is_current);
    assert!(!linked.is_prunable);

    // --- detached ---
    let det = worktrees
        .iter()
        .find(|w| w.path.ends_with("detached"))
        .expect("detached worktree");
    assert!(det.detached, "detached worktree has detached == true");
    assert_eq!(det.branch, None, "detached worktree has branch == None");
    assert!(!det.is_current);
    assert!(!det.is_prunable);

    // --- prunable ---
    let prun = worktrees
        .iter()
        .find(|w| w.path.ends_with("prunable"))
        .expect("prunable worktree");
    assert!(
        prun.is_prunable,
        "prunable worktree has is_prunable == true"
    );
    assert!(prun.detached, "prunable worktree is also detached");
    assert!(!prun.is_current);
}

/// Like `canned_bytes` but the final record is terminated by `\0\0`, matching real
/// `git worktree list --porcelain -z` output.  This exercises the record-boundary
/// (`\0\0`-separated) parse branch rather than the EOF-fallback branch, and asserts
/// that the extra trailing NUL does **not** produce a phantom `Worktree`.
fn canned_bytes_real_framing() -> Vec<u8> {
    let main = b"worktree /repo/main\0HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0branch refs/heads/main\0";
    let linked = b"worktree /repo/linked\0HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\0branch refs/heads/feature\0";
    let detached =
        b"worktree /repo/detached\0HEAD cccccccccccccccccccccccccccccccccccccccc\0detached\0";
    let bare = b"worktree /repo/bare\0bare\0";
    let prunable = b"worktree /repo/prunable\0HEAD dddddddddddddddddddddddddddddddddddddddd\0detached\0prunable gitdir file points to non-existent location\0";

    let mut out = Vec::new();
    out.extend_from_slice(main);
    out.push(b'\0'); // record separator
    out.extend_from_slice(linked);
    out.push(b'\0');
    out.extend_from_slice(detached);
    out.push(b'\0');
    out.extend_from_slice(bare);
    out.push(b'\0');
    out.extend_from_slice(prunable);
    out.push(b'\0'); // trailing \0\0: the last record also ends with a record-boundary NUL
    out
}

#[test]
fn parses_porcelain_worktrees_real_framing() {
    // Uses real-git `\0\0` framing (record-boundary branch, not the EOF-fallback branch).
    let bytes = canned_bytes_real_framing();
    let current_root = Path::new("/repo/main");

    let worktrees = parse_porcelain(&bytes, current_root);

    // Same 4 non-bare worktrees; the trailing record-boundary NUL must NOT add a phantom row.
    assert_eq!(
        worktrees.len(),
        4,
        "trailing \\0\\0 must not produce a phantom worktree"
    );

    // Spot-check the key fields — same expectations as the EOF-fallback test above.
    let main = worktrees
        .iter()
        .find(|w| w.path.ends_with("main"))
        .expect("main worktree");
    assert!(main.is_current, "main is is_current");
    assert_eq!(main.branch, Some("main".to_string()));

    let prun = worktrees
        .iter()
        .find(|w| w.path.ends_with("prunable"))
        .expect("prunable worktree");
    assert!(
        prun.is_prunable,
        "prunable worktree has is_prunable == true"
    );
    assert!(prun.detached, "prunable worktree is also detached");
}

// ---------------------------------------------------------------------------
// T-2 — live enumeration via `list()` against a real temp git repo
// ---------------------------------------------------------------------------

/// `list` returns both the main and linked worktree, and marks `is_current` correctly
/// (AC-1, AC-2, AC-11).
#[test]
fn list_returns_main_and_linked_worktrees_with_current_flag() {
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());

    // Create a linked worktree alongside the main one.
    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "linked-branch",
        ],
    );

    // Call list with current_root = the main repo root (canonical).
    let worktrees = list(repo.path(), repo.path());

    // Both the main worktree and the linked one must appear.
    assert!(
        worktrees.len() >= 2,
        "expected at least 2 worktrees, got {}: {worktrees:#?}",
        worktrees.len()
    );

    // The row matching the main repo root has is_current == true (AC-11).
    let canon_repo = common::canon(repo.path());
    let current_rows: Vec<_> = worktrees.iter().filter(|w| w.is_current).collect();
    assert_eq!(
        current_rows.len(),
        1,
        "exactly one worktree should be marked is_current"
    );
    assert_eq!(
        common::canon(&current_rows[0].path),
        canon_repo,
        "is_current row path must match current_root"
    );

    // The linked worktree also appears (AC-1, AC-2).
    let canon_linked = common::canon(linked.path());
    let linked_row = worktrees
        .iter()
        .find(|w| common::canon(&w.path) == canon_linked)
        .expect("linked worktree should appear in list output");
    assert!(!linked_row.is_current, "linked worktree must not be current");
    assert_eq!(
        linked_row.branch,
        Some("linked-branch".to_string()),
        "linked worktree should report its branch name"
    );
}

/// `list` does not mutate the worktree set or the filesystem (AC-N1, AC-N2): the set of
/// worktrees reported by `git worktree list` is identical before and after the call.
#[test]
fn list_does_not_mutate_worktree_set() {
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());

    // One linked worktree so there's something interesting to not mutate.
    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "mut-test-branch",
        ],
    );

    // Snapshot the worktree list before the call.
    let before = git(repo.path(), &["worktree", "list", "--porcelain"]);
    let head_before = git(repo.path(), &["rev-parse", "HEAD"]);

    let _ = list(repo.path(), repo.path());

    // The worktree list and HEAD must be unchanged.
    let after = git(repo.path(), &["worktree", "list", "--porcelain"]);
    let head_after = git(repo.path(), &["rev-parse", "HEAD"]);

    assert_eq!(
        before, after,
        "AC-N1: worktree list unchanged after list()"
    );
    assert_eq!(
        head_before, head_after,
        "AC-N2: HEAD unchanged after list()"
    );
}

/// `list` degrades gracefully (returns empty Vec) when called on a non-repo directory.
#[test]
fn list_returns_empty_vec_for_non_repo() {
    let dir = TempDir::new();
    let result = list(dir.path(), dir.path());
    assert!(
        result.is_empty(),
        "expected empty Vec for non-repo, got: {result:#?}"
    );
}
