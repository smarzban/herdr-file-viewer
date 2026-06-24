//! Worktree Provider — data model, `git worktree list --porcelain -z` parser, and live
//! enumeration.
//!
//! [`parse_porcelain`] is a **pure parser**: it performs no filesystem access and spawns no
//! processes. [`list`] is the live entry point that shells out to git and feeds the result to
//! the parser. (AC-1, AC-2, AC-N4)

use crate::git::git_command;
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
        Some(o) if o.status.success() => parse_porcelain(&o.stdout, &canonical_current),
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
