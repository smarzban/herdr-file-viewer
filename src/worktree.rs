//! Worktree Provider — data model and `git worktree list --porcelain -z` parser.
//!
//! This module is a **pure parser**: it performs no filesystem access and spawns no processes.
//! Live git shelling is handled by the caller (T-2). (AC-1, AC-2, AC-N4)

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
