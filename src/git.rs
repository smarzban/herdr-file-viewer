//! Git Service — read-only answers to git questions (status, baseline, changed-set, diff).
//!
//! Issues **only** read-only `git` subcommands (AC-N2), capturing stdout via
//! `std::process`. Not-a-repo or any git failure degrades to an empty/neutral result so
//! the viewer keeps working as a plain browser (AC-26). Paths are repo-root-relative,
//! matching git's own output.

use crate::root::Resolved;
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

/// What a diff and the meaning of "changed" compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Baseline {
    /// Uncommitted changes only (vs HEAD).
    Head,
    /// The full body of work since forking from the base branch.
    Base,
}

/// The context-smart default baseline: base branch on a feature branch / worktree
/// (AC-14), else HEAD on the base/default branch (AC-15).
pub fn default_baseline(resolved: &Resolved) -> Baseline {
    let Some(repo) = resolved.repo_root.as_deref() else {
        return Baseline::Head;
    };
    match (
        base_branch(repo, resolved.base_branch.as_deref()),
        current_branch(repo),
    ) {
        // On a branch other than the base/default branch → compare to the base (AC-14).
        (Some(base), Some(cur)) if base != cur => Baseline::Base,
        // On the base branch, detached, or no base info → uncommitted vs HEAD (AC-15).
        _ => Baseline::Head,
    }
}

/// The set of files changed against `baseline`, keyed by repo-root-relative path.
pub fn changed_set(repo_root: &Path, baseline: Baseline) -> BTreeMap<PathBuf, Status> {
    match baseline {
        // Uncommitted changes vs HEAD — exactly the working-tree status.
        Baseline::Head => status(repo_root),
        // The full body of work since the fork point: committed + uncommitted tracked
        // changes, plus any untracked files.
        Baseline::Base => {
            let mut map = BTreeMap::new();
            if let Some(fork) = base_fork_point(repo_root) {
                if let Some(out) = run_raw(repo_root, &["diff", "--name-status", &fork]) {
                    for line in out.lines() {
                        let mut fields = line.split('\t');
                        let code = fields.next().unwrap_or("");
                        let path = fields.last().unwrap_or(""); // new path on rename/copy
                        if path.is_empty() {
                            continue;
                        }
                        if let Some(s) = classify_name_status(code) {
                            map.insert(PathBuf::from(unquote(path)), s);
                        }
                    }
                }
            }
            for (path, s) in status(repo_root) {
                if s == Status::Untracked {
                    map.entry(path).or_insert(Status::Untracked);
                }
            }
            map
        }
    }
}

/// Raw unified diff text for one file against `baseline` (AC-9). Empty if unavailable.
pub fn diff(repo_root: &Path, path: &Path, baseline: Baseline) -> String {
    let against = match baseline {
        Baseline::Head => "HEAD".to_string(),
        Baseline::Base => base_fork_point(repo_root).unwrap_or_else(|| "HEAD".to_string()),
    };
    let p = path.to_string_lossy();
    run_raw(repo_root, &["diff", &against, "--", p.as_ref()]).unwrap_or_default()
}

/// Run a read-only `git` command and trim the stdout (for branch names / hashes).
fn run_trimmed(repo_root: &Path, args: &[&str]) -> Option<String> {
    run_raw(repo_root, args).map(|s| s.trim().to_string())
}

/// The current branch name, or `None` when detached.
fn current_branch(repo_root: &Path) -> Option<String> {
    match run_trimmed(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Some(b) if b != "HEAD" => Some(b),
        _ => None,
    }
}

/// Whether a ref resolves to a commit.
fn ref_exists(repo_root: &Path, name: &str) -> bool {
    run_raw(
        repo_root,
        &["rev-parse", "--verify", "--quiet", &format!("{name}^{{commit}}")],
    )
    .is_some()
}

/// The base/default branch: the host's hint if it resolves, else the conventional
/// `main`/`master` fallback (the plan threads the hint only through `default_baseline`).
fn base_branch(repo_root: &Path, hint: Option<&str>) -> Option<String> {
    if let Some(h) = hint {
        if ref_exists(repo_root, h) {
            return Some(h.to_string());
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|c| ref_exists(repo_root, c))
        .map(str::to_string)
}

/// The merge-base of the base branch and HEAD — where the body of work forks off.
fn base_fork_point(repo_root: &Path) -> Option<String> {
    let base = base_branch(repo_root, None)?;
    run_trimmed(repo_root, &["merge-base", &base, "HEAD"])
}

/// Map a `git diff --name-status` code letter to a tree status.
fn classify_name_status(code: &str) -> Option<Status> {
    match code.chars().next() {
        Some('A') => Some(Status::Added),
        Some('D') => Some(Status::Deleted),
        Some('M' | 'T' | 'R' | 'C') => Some(Status::Modified),
        _ => None,
    }
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
