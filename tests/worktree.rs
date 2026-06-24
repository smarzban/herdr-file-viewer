//! T-1 — Worktree model + porcelain parser (AC-1, AC-2, AC-N4).
//!
//! Feeds canned `git worktree list --porcelain -z` bytes and asserts the parser's output.
//! No git process is spawned; no filesystem is touched.

use herdr_file_viewer::worktree::parse_porcelain;
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
