//! Worktree Provider — data model, `git worktree list --porcelain -z` parser, and live
//! enumeration.
//!
//! [`parse_porcelain`] is a **pure parser**: it performs no filesystem access and spawns no
//! processes. [`list`] is the live entry point that shells out to git and feeds the result to
//! the parser. (AC-1, AC-2, AC-N4)
//!
//! [`agent_active`] resolves the pre-selected worktree when a herdr agent is running (AC-3,
//! AC-4, AC-15).

use crate::git::git_command;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A single git worktree record.
///
/// Bare worktrees are excluded by the parser — they never appear in the returned `Vec`.
#[derive(Debug, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute path to the worktree root.
    pub path: PathBuf,
    /// Branch name with the `refs/heads/` prefix stripped, or `None` when HEAD is detached.
    pub branch: Option<String>,
    /// `true` when HEAD is detached (no branch).
    pub detached: bool,
    /// `true` when this worktree's path equals the `current_root` passed to [`parse_porcelain`].
    pub is_current: bool,
    /// `true` when git reports this worktree as prunable.
    pub is_prunable: bool,
}

/// Enumerate the live worktrees by shelling `git worktree list --porcelain -z` and feeding
/// the output to [`parse_porcelain`].
///
/// `repo_root` is the directory passed to `git -C`; `current_root` is the path that should be
/// marked [`Worktree::is_current`] — it is canonicalized here (symlink-stable) before the
/// comparison inside the pure parser.
///
/// Returns an **empty `Vec`** on any failure (git missing, non-zero exit, spawn error) — the
/// caller is responsible for degrading gracefully (AC-26). Never panics or mutates the repo
/// (AC-N1, AC-N2).
pub fn list(repo_root: &Path, current_root: &Path) -> Vec<Worktree> {
    let canonical_current = current_root
        .canonicalize()
        .unwrap_or_else(|_| current_root.to_path_buf());

    let out = git_command(repo_root, &["worktree", "list", "--porcelain", "-z"])
        .output()
        .ok();

    match out {
        Some(o) if o.status.success() => {
            let mut wts = parse_porcelain(&o.stdout, &canonical_current);
            // `parse_porcelain` flags `is_current` by comparing git's RAW emitted path against
            // `canonical_current`. git can emit a path that differs textually from the canonical
            // current root (a symlinked worktree dir; macOS `/tmp` vs `/private/tmp`), which
            // would mis-flag the current row. Recompute it here — `list` is authoritative for
            // real paths — by canonicalizing each row's own path. A missing/prunable path won't
            // canonicalize → `unwrap_or(false)` → correctly not current (AC-4).
            for wt in &mut wts {
                wt.is_current = wt
                    .path
                    .canonicalize()
                    .map(|p| p == canonical_current)
                    .unwrap_or(false);
            }
            wts
        }
        _ => Vec::new(),
    }
}

/// Parse the raw bytes from `git worktree list --porcelain -z` into a `Vec<Worktree>`.
///
/// With `-z` each attribute line is NUL-terminated, and records are separated by an extra NUL
/// (the `\0\0` boundary). Bare worktrees are silently excluded from the result.
/// `current_root` is the path whose worktree should be marked [`Worktree::is_current`].
pub fn parse_porcelain(bytes: &[u8], current_root: &Path) -> Vec<Worktree> {
    // Split on NUL; empty tokens mark record boundaries (the extra NUL between records).
    let tokens: Vec<&[u8]> = bytes.split(|&b| b == b'\0').collect();

    let mut result = Vec::new();
    let mut record: Vec<&[u8]> = Vec::new();

    for token in &tokens {
        if token.is_empty() {
            // Record boundary — process whatever we accumulated.
            if !record.is_empty() {
                if let Some(w) = parse_record(&record, current_root) {
                    result.push(w);
                }
                record.clear();
            }
        } else {
            record.push(token);
        }
    }
    // Handle a final record that wasn't terminated by an extra NUL.
    if !record.is_empty()
        && let Some(w) = parse_record(&record, current_root)
    {
        result.push(w);
    }

    result
}

// ---------------------------------------------------------------------------
// Agent-active resolution (AC-3, AC-4, AC-15)
// ---------------------------------------------------------------------------

/// Serde-only view of one entry from `herdr worktree list --json`.
///
/// Only the fields needed for agent-active resolution are read; all other fields
/// are ignored. `Option<String>` + `#[serde(default)]` means missing fields
/// degrade to `None` rather than causing a parse error (defensive).
#[derive(Deserialize)]
struct HerdrWorktreeEntry {
    path: Option<String>,
    #[serde(default)]
    open_workspace_id: Option<String>,
}

/// Serde-only view of one entry from `herdr agent list`.
///
/// `herdr agent list` reports NON-agent panes too — entries with no `agent` field and an
/// `agent_status` of `"unknown"`. A REAL agent is one where `agent` is present; only those are
/// counted for pre-selection (AC-3) and per-row status badges (AC-19). Unknown fields are ignored;
/// every field is `#[serde(default)]` so a missing field degrades to `None` rather than failing
/// the parse (defensive — AC-15).
#[derive(Deserialize)]
struct HerdrAgentEntry {
    /// Present only for a DETECTED agent (e.g. `"claude"`); absent for a plain pane.
    #[serde(default)]
    agent: Option<String>,
    /// The agent's reported status (`working`/`idle`/`blocked`/`done`/`unknown`).
    #[serde(default)]
    agent_status: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
}

/// Envelope for `herdr worktree list` — the entries live under `result.worktrees`, not at the
/// top level. herdr wraps every reply as `{"id":…,"result":{…,"worktrees":[…]}}`. Both fields are
/// `#[serde(default)]` so a missing `result` / `worktrees` (or a wholly unexpected shape) degrades
/// to an empty list rather than failing the parse (defensive — AC-15).
#[derive(Deserialize, Default)]
struct WorktreeListEnvelope {
    #[serde(default)]
    result: WorktreeListResult,
}

#[derive(Deserialize, Default)]
struct WorktreeListResult {
    #[serde(default)]
    worktrees: Vec<HerdrWorktreeEntry>,
}

/// Envelope for `herdr agent list` — the entries live under `result.agents`. Same nested
/// `{"id":…,"result":{"agents":[…]}}` shape and the same defensive degradation as the worktree
/// envelope (AC-15).
#[derive(Deserialize, Default)]
struct AgentListEnvelope {
    #[serde(default)]
    result: AgentListResult,
}

#[derive(Deserialize, Default)]
struct AgentListResult {
    #[serde(default)]
    agents: Vec<HerdrAgentEntry>,
}

/// Resolve which worktree an active herdr agent is running in, using the tiered rule:
///
/// 1. Parse `agent_json` → the set of workspace ids that host a running agent.
/// 2. Parse `worktree_json` → entries `(path, open_workspace_id)`.
///    Both are parsed defensively (`serde_json::from_str(...).unwrap_or_default()`);
///    malformed/missing input produces an empty collection rather than a panic (AC-15).
/// 3. A worktree entry *qualifies* if its `open_workspace_id` is present, non-empty,
///    and belongs to the agent workspaces set.
///
/// **Tier 1 — prefer our workspace:** if `our_workspace_id` is `Some(ws)`, `ws` hosts an
/// agent, and exactly one worktree entry has `open_workspace_id == ws` → return that path.
///
/// **Tier 2 — unique agent worktree:** else if exactly one worktree entry qualifies overall
/// → return that path.
///
/// **Tier 3 — None:** zero qualifying entries, or genuinely ambiguous (>1) with no own-
/// workspace winner.
///
/// The returned `PathBuf` is always a path that appears in `worktrees`; if the resolved
/// path cannot be matched there (symlink-stable comparison) `None` is returned so the
/// caller falls back to the current root (AC-4).
pub fn agent_active(
    worktrees: &[Worktree],
    worktree_json: &str,
    agent_json: &str,
    our_workspace_id: Option<&str>,
) -> Option<PathBuf> {
    // Step 1 — parse agent workspaces (defensive). Only entries with a present `agent` field
    // are REAL agents; non-agent panes (no `agent`, `agent_status: "unknown"`) are excluded so
    // an idle pane can never mask the agent-active pre-select (AC-3) — consistent with the
    // per-row status badges (AC-19).
    let agent_workspaces: HashSet<String> = {
        let entries = serde_json::from_str::<AgentListEnvelope>(agent_json)
            .map(|e| e.result.agents)
            .unwrap_or_default();
        entries
            .into_iter()
            .filter(|e| e.agent.is_some())
            .filter_map(|e| e.workspace_id)
            .filter(|ws| !ws.is_empty())
            .collect()
    };

    // Step 2 — parse herdr worktree entries (defensive).
    let wt_entries = serde_json::from_str::<WorktreeListEnvelope>(worktree_json)
        .map(|e| e.result.worktrees)
        .unwrap_or_default();

    // Step 3 — collect qualifying entries (path present, workspace_id ∈ agent_workspaces).
    let qualifying: Vec<(PathBuf, String)> = wt_entries
        .into_iter()
        .filter_map(|e| {
            let path_str = e.path?;
            let ws_id = e.open_workspace_id?;
            if ws_id.is_empty() || !agent_workspaces.contains(&ws_id) {
                return None;
            }
            Some((PathBuf::from(path_str), ws_id))
        })
        .collect();

    // Step 4 — Tier 1: prefer our own workspace if it uniquely matches.
    let chosen_path: PathBuf = if let Some(own_ws) = our_workspace_id {
        if agent_workspaces.contains(own_ws) {
            let own_matches: Vec<&PathBuf> = qualifying
                .iter()
                .filter(|(_, ws)| ws == own_ws)
                .map(|(p, _)| p)
                .collect();
            if own_matches.len() == 1 {
                own_matches[0].clone()
            } else {
                // Tier 2 fallback
                if qualifying.len() == 1 {
                    qualifying[0].0.clone()
                } else {
                    return None;
                }
            }
        } else {
            // Our workspace has no agent — fall to Tier 2.
            if qualifying.len() == 1 {
                qualifying[0].0.clone()
            } else {
                return None;
            }
        }
    } else {
        // No own workspace hint — Tier 2.
        if qualifying.len() == 1 {
            qualifying[0].0.clone()
        } else {
            return None;
        }
    };

    // Step 7 — normalize against the worktrees slice (symlink-stable).
    let canon_chosen = chosen_path
        .canonicalize()
        .unwrap_or_else(|_| chosen_path.clone());

    worktrees
        .iter()
        .find(|w| {
            let canon_w = w.path.canonicalize().unwrap_or_else(|_| w.path.clone());
            canon_w == canon_chosen
        })
        .map(|w| w.path.clone())
}

/// Per-worktree agent status from the herdr overlay, aligned 1:1 with `worktrees` (same order
/// and length). `Some(status)` when that worktree's `open_workspace_id` hosts a REAL agent
/// (`agent` present) — the agent's `agent_status`; `None` otherwise. Defensive: malformed JSON →
/// all `None`. Spawns no process and makes no herdr call, but DOES `canonicalize()` each path
/// (a filesystem stat) for symlink-stable matching, exactly like [`list`].
///
/// This reuses the *same* `worktree list` + `agent list` JSON the picker already fetched for the
/// agent-active pre-select, so it adds no extra subprocess cost (AC-20). Path matching uses the
/// same canonicalize-with-fallback idiom as [`list`]'s `is_current` fix, so a symlinked worktree
/// dir (or macOS `/tmp` vs `/private/tmp`) still matches.
pub fn agent_statuses(
    worktrees: &[Worktree],
    worktree_json: &str,
    agent_json: &str,
) -> Vec<Option<String>> {
    // workspace_id → agent_status, for REAL agents only (an entry with a present `agent`). A
    // non-agent pane (no `agent`) is skipped, so it contributes no badge (AC-19).
    let mut status_by_ws: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let agents = serde_json::from_str::<AgentListEnvelope>(agent_json)
        .map(|e| e.result.agents)
        .unwrap_or_default();
    for e in agents {
        if e.agent.is_none() {
            continue; // plain pane, not a real agent
        }
        if let Some(ws) = e.workspace_id.filter(|ws| !ws.is_empty()) {
            status_by_ws.insert(ws, e.agent_status);
        }
    }

    // path (canonicalized) → open_workspace_id, from the herdr worktree list.
    let wt_entries = serde_json::from_str::<WorktreeListEnvelope>(worktree_json)
        .map(|e| e.result.worktrees)
        .unwrap_or_default();
    let mut ws_by_canon_path: std::collections::HashMap<PathBuf, String> =
        std::collections::HashMap::new();
    for e in wt_entries {
        let (Some(path), Some(ws)) = (e.path, e.open_workspace_id) else {
            continue;
        };
        if ws.is_empty() {
            continue;
        }
        let pb = PathBuf::from(path);
        let canon = pb.canonicalize().unwrap_or(pb);
        ws_by_canon_path.insert(canon, ws);
    }

    worktrees
        .iter()
        .map(|w| {
            let canon_w = w.path.canonicalize().unwrap_or_else(|_| w.path.clone());
            let ws = ws_by_canon_path.get(&canon_w)?;
            // The workspace hosts a real agent → its status (falling back to "unknown" only if
            // the agent reported no status, which shouldn't happen in practice).
            status_by_ws
                .get(ws)
                .map(|s| s.clone().unwrap_or_else(|| "unknown".to_string()))
        })
        .collect()
}

/// Parse a single record (the set of attribute lines for one worktree).
/// Returns `None` for bare worktrees.
fn parse_record(lines: &[&[u8]], current_root: &Path) -> Option<Worktree> {
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    let mut detached = false;
    let mut bare = false;
    let mut is_prunable = false;

    for line in lines {
        let s = std::str::from_utf8(line).unwrap_or("").trim_end();
        if let Some(rest) = s.strip_prefix("worktree ") {
            path = Some(PathBuf::from(rest));
        } else if let Some(rest) = s.strip_prefix("branch ") {
            let name = rest.strip_prefix("refs/heads/").unwrap_or(rest);
            branch = Some(name.to_string());
        } else if s == "detached" {
            detached = true;
        } else if s == "bare" {
            bare = true;
        } else if s.starts_with("prunable") {
            is_prunable = true;
        }
        // HEAD, locked, and other attributes are intentionally ignored.
    }

    if bare {
        return None;
    }

    let path = path?;
    let is_current = path == current_root;

    Some(Worktree {
        path,
        branch,
        detached,
        is_current,
        is_prunable,
    })
}
