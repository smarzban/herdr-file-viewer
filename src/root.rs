//! Root Resolver — resolve the tree root and git-presence from a [`LaunchContext`].
//!
//! Root is the working tree's top-level inside a git repo (AC-1) else the cwd (AC-2).
//! Not-a-repo is a normal result (`is_git_repo == false`), never an error (AC-26).
//! Uses only read-only `git` subcommands.

use crate::context::LaunchContext;
use std::path::{Path, PathBuf};

/// The resolved root and git facts the Session Controller initializes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The directory the tree is rooted at.
    pub root: PathBuf,
    /// Whether `root` is inside a git working tree.
    pub is_git_repo: bool,
    /// The repository top-level (where git commands run), when a repo.
    pub repo_root: Option<PathBuf>,
    /// Whether this is a *linked* worktree (vs. the main working tree).
    pub is_worktree: bool,
    /// The base branch, carried through from the launch context.
    pub base_branch: Option<String>,
}

/// Resolve the tree root and git-presence for a launch context.
pub fn resolve(ctx: &LaunchContext) -> Resolved {
    match git_output(&ctx.cwd, &["rev-parse", "--show-toplevel"]) {
        Some(toplevel) => {
            let root = PathBuf::from(toplevel);
            Resolved {
                is_git_repo: true,
                repo_root: Some(root.clone()),
                is_worktree: is_linked_worktree(&ctx.cwd),
                base_branch: ctx.base_branch.clone(),
                root,
            }
        }
        // Not a git repo (or git unavailable): a plain browser rooted at the cwd (AC-2, AC-26).
        None => Resolved {
            root: ctx.cwd.clone(),
            is_git_repo: false,
            repo_root: None,
            is_worktree: false,
            base_branch: None,
        },
    }
}

/// Run a read-only `git` query in `dir`; `Some(trimmed stdout)` on success, else `None`.
/// Built through the Git Service's shared [`crate::git::git_command`] so the untrusted-repo
/// hardening (no optional index locks, neutralized `core.fsmonitor`/`core.hooksPath`, dropped
/// repo-redirecting env so the *root* resolves against `dir`) is applied identically here and
/// in the Git Service — it cannot drift between the two. The shared builder also pins
/// `--attr-source`, so root resolution (like the rest of the Git Service) needs git ≥ 2.40;
/// an older git makes these queries fail and the directory degrades to a plain, non-git
/// browser (AC-26) rather than a half-working repo view.
fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
    let out = crate::git::git_command(dir, args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// A linked worktree's git-dir (`.git/worktrees/<name>`) differs from the shared
/// common dir (`.git`); the main working tree's are the same.
fn is_linked_worktree(dir: &Path) -> bool {
    let git_dir = git_output(dir, &["rev-parse", "--path-format=absolute", "--git-dir"]);
    let common = git_output(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    match (git_dir, common) {
        (Some(g), Some(c)) => g != c,
        _ => false,
    }
}
