//! Git Service — read-only answers to git questions (status, baseline, changed-set, diff).
//!
//! Issues **only** read-only `git` subcommands (AC-N2), capturing stdout via
//! `std::process`. Not-a-repo or any git failure degrades to an empty/neutral result so
//! the viewer keeps working as a plain browser (AC-26). Paths are repo-root-relative,
//! matching git's own output.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A file's git status against the working tree (AC-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Modified,
    Added,
    Deleted,
    Untracked,
}

/// Per-file working-tree status, keyed by repo-root-relative path.
pub fn status(repo_root: &Path) -> BTreeMap<PathBuf, Status> {
    let mut map = BTreeMap::new();
    let Some(out) = run_raw(repo_root, &["status", "--porcelain"]) else {
        return map; // not a repo / git unavailable → empty (AC-26)
    };
    for line in out.lines() {
        // Porcelain v1 line: two status chars, a space, then the path.
        if line.len() < 4 {
            continue;
        }
        let code = &line[..2];
        let path_field = &line[3..];
        // A rename/copy reports "orig -> new"; take the new path.
        let path_str = path_field.rsplit(" -> ").next().unwrap_or(path_field);
        if let Some(s) = classify(code) {
            map.insert(PathBuf::from(unquote(path_str)), s);
        }
    }
    map
}

/// Map a 2-char porcelain code to one of the four tree statuses (AC-7).
/// Precedence: untracked, then deleted, then added, then any other change → modified.
fn classify(code: &str) -> Option<Status> {
    if code == "??" {
        Some(Status::Untracked)
    } else if code.contains('D') {
        Some(Status::Deleted)
    } else if code.contains('A') {
        Some(Status::Added)
    } else if code.trim().is_empty() {
        None // unmodified / ignored
    } else {
        Some(Status::Modified) // M, R, C, T, …
    }
}

/// Strip the surrounding quotes git adds to paths with special characters.
fn unquote(path: &str) -> &str {
    path.strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .unwrap_or(path)
}

/// Run a read-only `git` command in `repo_root`, returning raw (untrimmed) stdout.
/// `None` if git is missing or exits non-zero (degrade to a plain browser, AC-26).
fn run_raw(repo_root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}
