//! Worktree model + porcelain parser (AC-1, AC-2, AC-N4).
//! Worktree Provider: live enumeration (AC-1, AC-2, AC-11, AC-N1, AC-N2).

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
// live enumeration via `list()` against a real temp git repo
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
    assert!(
        !linked_row.is_current,
        "linked worktree must not be current"
    );
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

    assert_eq!(before, after, "AC-N1: worktree list unchanged after list()");
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

/// `list` flags the current worktree even when git's *emitted* worktree path is a symlink that
/// resolves elsewhere — i.e. when git's raw path text and the canonical `current_root` differ
/// (the macOS `/tmp` vs `/private/tmp` case, generalized). `list` recomputes `is_current` by
/// canonicalizing each row's own path against the canonical `current_root`, so the row whose
/// path resolves to the current worktree is flagged regardless of textual mismatch (AC-4).
///
/// We reproduce a divergent git path on Linux by registering a linked worktree, then replacing
/// its directory with a symlink to the moved real directory: git keeps emitting the original
/// (now-symlinked) path, while `current_root` is the canonical (moved) path.
///
/// Pre-fix (`parse_porcelain` compares git's RAW path against `canonical_current`) the moved
/// worktree is marked NOT current — so the picker would preselect row 0 instead of it.
#[cfg(unix)]
#[test]
fn list_flags_current_when_path_is_a_symlink() {
    use std::os::unix::fs::symlink;

    let repo = TempDir::new();
    init_repo_with_commit(repo.path());

    // A linked worktree at `wt_orig`. git records this exact path.
    let parent = TempDir::new();
    let wt_orig = parent.path().join("wt");
    let wt_moved = parent.path().join("wt-moved");
    git(
        repo.path(),
        &["worktree", "add", wt_orig.to_str().unwrap(), "-b", "feat"],
    );

    // Move the real directory aside and put a symlink in its place, so git's recorded path
    // (`wt_orig`) is now a symlink that canonicalizes to `wt_moved`.
    std::fs::rename(&wt_orig, &wt_moved).expect("move worktree dir");
    symlink(&wt_moved, &wt_orig).expect("symlink original path to moved dir");

    // current_root = the canonical (moved) path; git still emits the symlinked `wt_orig`.
    let worktrees = list(repo.path(), &wt_moved);

    let canon_moved = common::canon(&wt_moved);
    let current_rows: Vec<_> = worktrees.iter().filter(|w| w.is_current).collect();
    assert_eq!(
        current_rows.len(),
        1,
        "exactly one row must be current even when git emits a symlinked path: {worktrees:#?}"
    );
    assert_eq!(
        common::canon(&current_rows[0].path),
        canon_moved,
        "the current row must be the worktree the symlinked git path resolves to"
    );
}

// ---------------------------------------------------------------------------
// agent_active: tiered pre-selection resolution (AC-3, AC-4, AC-15)
// ---------------------------------------------------------------------------

use herdr_file_viewer::worktree::agent_active;
use std::path::PathBuf;

/// Canned `herdr worktree list --json` output with three worktrees.
///
/// wt-a → ws-1, wt-b → ws-2, wt-c → ws-3 (no agent).
fn worktree_json_three() -> &'static str {
    // Real herdr 0.7.x nests the entries under `result.worktrees`.
    r#"{"id": 1, "result": {"type": "worktree_list", "worktrees": [
        {"path": "/repo/wt-a", "open_workspace_id": "ws-1", "branch": "main",     "is_bare": false, "is_detached": false},
        {"path": "/repo/wt-b", "open_workspace_id": "ws-2", "branch": "feat",     "is_bare": false, "is_detached": false},
        {"path": "/repo/wt-c", "open_workspace_id": "ws-3", "branch": "other",    "is_bare": false, "is_detached": false}
    ]}}"#
}

/// Canned `herdr agent list` output — agents in ws-1 and ws-2 (both active). Each entry carries
/// an `agent` field so the real-agent filter detects it (a non-agent pane omits `agent`).
fn agent_json_two_workspaces() -> &'static str {
    // Real herdr 0.7.x nests the entries under `result.agents`.
    r#"{"id": 2, "result": {"type": "agent_list", "agents": [
        {"id": "agent-abc", "agent": "claude", "agent_status": "working", "workspace_id": "ws-1"},
        {"id": "agent-xyz", "agent": "claude", "agent_status": "idle", "workspace_id": "ws-2"}
    ]}}"#
}

/// Canned agent list — only ws-2 has an agent.
fn agent_json_one_workspace() -> &'static str {
    r#"{"id": 2, "result": {"type": "agent_list", "agents": [{"id": "agent-only", "agent": "claude", "agent_status": "working", "workspace_id": "ws-2"}]}}"#
}

/// Canned agent list — no agents at all.
fn agent_json_empty() -> &'static str {
    r#"{"id": 2, "result": {"type": "agent_list", "agents": []}}"#
}

/// Build a `&[Worktree]` slice from `parse_porcelain` canned bytes + direct `Worktree`
/// construction so the path-normalization step is exercised. Paths must be absolute and
/// exist on this machine for canonicalize to succeed; we use a non-existent-but-absolute
/// path and rely on the canonicalize-fallback in `agent_active`.
fn make_worktrees() -> Vec<herdr_file_viewer::worktree::Worktree> {
    // Construct directly using parse_porcelain with a fabricated current_root.
    // The worktrees don't need to exist on disk — `agent_active` compares path strings
    // when canonicalize fails (the paths are fake).
    parse_porcelain(
        &{
            let mut b = Vec::new();
            b.extend_from_slice(b"worktree /repo/wt-a\0branch refs/heads/main\0");
            b.push(b'\0');
            b.extend_from_slice(b"worktree /repo/wt-b\0branch refs/heads/feat\0");
            b.push(b'\0');
            b.extend_from_slice(b"worktree /repo/wt-c\0branch refs/heads/other\0");
            b
        },
        std::path::Path::new("/repo/wt-a"),
    )
}

/// **Tier 1** — our workspace wins even when multiple agent-hosting worktrees exist.
///
/// Setup: agents in ws-1 (wt-a) and ws-2 (wt-b), our_workspace_id = "ws-2".
/// Expected: `Some("/repo/wt-b")` — Tier-1 match on our own workspace.
#[test]
fn agent_active_tier1_prefers_own_workspace() {
    let worktrees = make_worktrees();
    let result = agent_active(
        &worktrees,
        worktree_json_three(),
        agent_json_two_workspaces(),
        Some("ws-2"),
    );
    assert_eq!(result, Some(PathBuf::from("/repo/wt-b")));
}

/// **Tier 2** — no own-workspace hint; exactly one agent-hosting worktree → return it.
///
/// Setup: only ws-2 has an agent (wt-b). our_workspace_id = None.
/// Expected: `Some("/repo/wt-b")`.
#[test]
fn agent_active_tier2_unique_agent_worktree() {
    let worktrees = make_worktrees();
    let result = agent_active(
        &worktrees,
        worktree_json_three(),
        agent_json_one_workspace(),
        None,
    );
    assert_eq!(result, Some(PathBuf::from("/repo/wt-b")));
}

/// **Tier 3 — zero** — no agents running at all → `None`.
#[test]
fn agent_active_tier3_no_agents_returns_none() {
    let worktrees = make_worktrees();
    let result = agent_active(&worktrees, worktree_json_three(), agent_json_empty(), None);
    assert_eq!(result, None);
}

/// **Tier 3 — two (ambiguous)** — two agent-hosting worktrees, no own-workspace hint → `None`.
#[test]
fn agent_active_tier3_ambiguous_returns_none() {
    let worktrees = make_worktrees();
    let result = agent_active(
        &worktrees,
        worktree_json_three(),
        agent_json_two_workspaces(),
        None,
    );
    assert_eq!(result, None);
}

/// **Tier 1 fallback to Tier 2** — our workspace id is set but has NO running agent, exactly
/// one other worktree does → Tier 2 fires.
#[test]
fn agent_active_tier1_miss_falls_to_tier2() {
    let worktrees = make_worktrees();
    // our_workspace_id = "ws-3" has no agent; only ws-2 does
    let result = agent_active(
        &worktrees,
        worktree_json_three(),
        agent_json_one_workspace(),
        Some("ws-3"),
    );
    assert_eq!(result, Some(PathBuf::from("/repo/wt-b")));
}

/// **Malformed worktree_json** → `None`, no panic.
#[test]
fn agent_active_malformed_worktree_json_returns_none() {
    let worktrees = make_worktrees();
    let result = agent_active(
        &worktrees,
        "this is not json {{{{",
        agent_json_one_workspace(),
        None,
    );
    assert_eq!(result, None);
}

/// **Malformed agent_json** → `None`, no panic.
#[test]
fn agent_active_malformed_agent_json_returns_none() {
    let worktrees = make_worktrees();
    let result = agent_active(&worktrees, worktree_json_three(), "{ broken", None);
    assert_eq!(result, None);
}

/// **Both JSON args malformed** → `None`, no panic.
#[test]
fn agent_active_both_malformed_returns_none() {
    let worktrees = make_worktrees();
    let result = agent_active(&worktrees, "bad", "also bad", Some("ws-1"));
    assert_eq!(result, None);
}

/// **Tier 1 — empty `our_workspace_id` must NOT match** — `Some("")` is not a valid workspace
/// id and must not spuriously trigger Tier 1. With two qualifying worktrees and an empty
/// own-workspace hint the function must fall through to Tier 2/3 logic, which here returns
/// `None` (ambiguous: two qualifying entries).
#[test]
fn agent_active_empty_our_workspace_id_does_not_trigger_tier1() {
    let worktrees = make_worktrees();
    // Two agents active (ws-1 → wt-a, ws-2 → wt-b); empty own workspace.
    let result = agent_active(
        &worktrees,
        worktree_json_three(),
        agent_json_two_workspaces(),
        Some(""),
    );
    // Empty string is never in the agent_workspaces set, so Tier 1 is skipped.
    // Tier 2 sees two qualifying entries → ambiguous → None.
    assert_eq!(result, None);
}

/// **Two worktrees share the same `open_workspace_id`** — when two distinct worktree entries
/// both open the same agent-hosting workspace the qualifying count is 2, which is >1, so the
/// result is `None` (ambiguous). Confirms the "exactly one" check operates on worktree
/// entries, not on distinct workspace ids.
#[test]
fn agent_active_two_worktrees_same_workspace_is_ambiguous() {
    // Build a JSON where wt-a and wt-b both point to ws-1 (real nested herdr shape).
    let wt_json = r#"{"id": 1, "result": {"worktrees": [
        {"path": "/repo/wt-a", "open_workspace_id": "ws-1"},
        {"path": "/repo/wt-b", "open_workspace_id": "ws-1"},
        {"path": "/repo/wt-c", "open_workspace_id": "ws-3"}
    ]}}"#;
    // Agent list — only ws-1 has an agent.
    let ag_json = r#"{"id": 2, "result": {"agents": [{"id": "agent-only", "agent": "claude", "agent_status": "working", "workspace_id": "ws-1"}]}}"#;

    let worktrees = make_worktrees();
    // No own-workspace hint: Tier 2 must count two qualifying entries → None.
    let result = agent_active(&worktrees, wt_json, ag_json, None);
    assert_eq!(
        result, None,
        "two worktrees in the same workspace must be ambiguous"
    );
}

/// **Path not in worktrees slice** — agent points to a path not in the slice → `None`.
///
/// The worktrees slice only contains wt-a; the agent targets wt-b (ws-2) which is absent.
#[test]
fn agent_active_path_not_in_slice_returns_none() {
    // Slice only contains wt-a
    let worktrees = parse_porcelain(
        b"worktree /repo/wt-a\0branch refs/heads/main\0",
        std::path::Path::new("/repo/wt-a"),
    );
    // JSON includes wt-b → ws-2, but wt-b is not in the slice
    let result = agent_active(
        &worktrees,
        worktree_json_three(),
        agent_json_one_workspace(), // ws-2 → wt-b
        None,
    );
    assert_eq!(result, None);
}

/// **Real-agent filter guard (AC-3)** — a plain (non-agent) herdr pane in our OWN workspace must
/// NOT count toward the agent-active pre-select. Without `.filter(|e| e.agent.is_some())` in
/// `agent_active`, the plain pane's workspace would qualify and Tier 1 would return OUR (current)
/// worktree; with the filter, our workspace is excluded, leaving the real agent's worktree as the
/// unique Tier-2 winner. This test is RED if the filter is removed — the guard the mutation found
/// missing.
///
/// Setup: two worktrees — the current one in "ws-current" (a PLAIN pane, no `agent`) and a second
/// in "ws-agent" (a REAL `claude` agent). our_workspace_id = "ws-current".
/// Expected: `Some("/repo/wt-agent")` — the real agent's worktree, NOT the current one.
#[test]
fn agent_active_ignores_non_agent_panes() {
    // Worktree list (real nested herdr shape): current → ws-current, second → ws-agent.
    let wt_json = r#"{"id": 1, "result": {"worktrees": [
        {"path": "/repo/wt-current", "open_workspace_id": "ws-current"},
        {"path": "/repo/wt-agent",   "open_workspace_id": "ws-agent"}
    ]}}"#;
    // Agent list: ws-current is a PLAIN PANE (no `agent` field), ws-agent is a REAL agent.
    let ag_json = r#"{"id": 2, "result": {"agents": [
        {"id": "pane",  "agent_status": "unknown", "workspace_id": "ws-current"},
        {"id": "claude", "agent": "claude", "agent_status": "working", "workspace_id": "ws-agent"}
    ]}}"#;
    // The worktrees slice must contain both rows so the resolved path can be matched back.
    let worktrees = parse_porcelain(
        &{
            let mut b = Vec::new();
            b.extend_from_slice(b"worktree /repo/wt-current\0branch refs/heads/main\0");
            b.push(b'\0');
            b.extend_from_slice(b"worktree /repo/wt-agent\0branch refs/heads/feat\0");
            b
        },
        std::path::Path::new("/repo/wt-current"),
    );

    let result = agent_active(&worktrees, wt_json, ag_json, Some("ws-current"));
    assert_eq!(
        result,
        Some(PathBuf::from("/repo/wt-agent")),
        "a plain (non-agent) pane in our workspace must not count — the real agent's worktree wins"
    );
}

// ---------------------------------------------------------------------------
// agent_statuses: per-row agent status from the herdr overlay (AC-18, AC-19, AC-20)
// ---------------------------------------------------------------------------

use herdr_file_viewer::worktree::agent_statuses;

/// A worktree with a REAL agent (`agent` present) surfaces its `agent_status` at the matching
/// row index; a worktree whose workspace has no agent — and a worktree not mentioned by herdr at
/// all — surfaces `None`. The returned Vec is aligned 1:1 with the `worktrees` slice.
#[test]
fn agent_statuses_maps_real_agent_status_per_row() {
    let worktrees = make_worktrees(); // wt-a, wt-b, wt-c (in that order)
    // wt-a → ws-1 (working agent), wt-b → ws-2 (idle agent), wt-c → ws-3 (no agent).
    let statuses = agent_statuses(
        &worktrees,
        worktree_json_three(),
        agent_json_two_workspaces(),
    );
    assert_eq!(
        statuses,
        vec![Some("working".to_string()), Some("idle".to_string()), None,],
        "per-row status aligns 1:1 with the worktrees slice"
    );
}

/// A herdr `agent list` entry WITHOUT an `agent` field is a plain pane, not a real agent (AC-19):
/// its workspace must yield no status. Here ws-2 hosts a plain pane (no `agent`), so wt-b → None.
#[test]
fn agent_statuses_ignores_non_agent_panes() {
    let worktrees = make_worktrees();
    // ws-2 entry has NO `agent` field → it is a plain pane, not a real agent (real nested shape).
    let agent_json = r#"{"id": 2, "result": {"agents": [
        {"id": "real", "agent": "claude", "agent_status": "working", "workspace_id": "ws-1"},
        {"id": "pane", "agent_status": "unknown", "workspace_id": "ws-2"}
    ]}}"#;
    let statuses = agent_statuses(&worktrees, worktree_json_three(), agent_json);
    assert_eq!(
        statuses,
        vec![Some("working".to_string()), None, None],
        "a non-agent pane (no `agent` field) contributes no status badge"
    );
}

/// Malformed JSON (either argument) degrades to all-`None`, never a panic (AC-15, AC-20).
#[test]
fn agent_statuses_malformed_json_is_all_none() {
    let worktrees = make_worktrees();
    let n = worktrees.len();
    assert_eq!(
        agent_statuses(&worktrees, "not json {{{", agent_json_two_workspaces()),
        vec![None; n],
        "malformed worktree JSON → all None"
    );
    assert_eq!(
        agent_statuses(&worktrees, worktree_json_three(), "also broken"),
        vec![None; n],
        "malformed agent JSON → all None"
    );
    assert_eq!(
        agent_statuses(&worktrees, "bad", "bad"),
        vec![None; n],
        "both malformed → all None"
    );
}

/// An empty worktrees slice yields an empty status Vec (the alignment invariant holds at length 0).
#[test]
fn agent_statuses_empty_worktrees_is_empty_vec() {
    let worktrees: Vec<herdr_file_viewer::worktree::Worktree> = Vec::new();
    assert_eq!(
        agent_statuses(
            &worktrees,
            worktree_json_three(),
            agent_json_two_workspaces()
        ),
        Vec::<Option<String>>::new()
    );
}
