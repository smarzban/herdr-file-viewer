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
    match trusted_toplevel(&ctx.cwd) {
        Some(root) => Resolved {
            is_git_repo: true,
            repo_root: Some(root.clone()),
            is_worktree: is_linked_worktree(&ctx.cwd),
            base_branch: ctx.base_branch.clone(),
            root,
        },
        // Not a git repo, git unavailable, or a toplevel that is not the opened tree:
        // a plain browser rooted at the cwd (AC-2, AC-26).
        None => Resolved {
            root: ctx.cwd.clone(),
            is_git_repo: false,
            repo_root: None,
            is_worktree: false,
            base_branch: None,
        },
    }
}

/// Git's toplevel, only when it is the directory that actually holds the `.git` entry above
/// `cwd`.
///
/// `core.worktree` in an untrusted repo config can make `rev-parse --show-toplevel` name a
/// different directory than the one the user opened. Walking from `cwd` to the nearest `.git`
/// file or directory is the filesystem fact; git's answer is accepted only when the two
/// canonical paths are the same. A mismatch degrades to a non-repo rooted at `cwd`, so later
/// reads and git queries stay on the opened tree.
fn trusted_toplevel(cwd: &Path) -> Option<PathBuf> {
    let reported = PathBuf::from(git_output(cwd, &["rev-parse", "--show-toplevel"])?);
    let discovered = directory_containing_git_entry(cwd)?;
    let reported_canon = reported.canonicalize().ok()?;
    let discovered_canon = discovered.canonicalize().ok()?;
    // Keep git's own toplevel string when it names that same directory, so a trusted repo
    // resolves exactly as before.
    (reported_canon == discovered_canon).then_some(reported)
}

/// Nearest ancestor of `start`, including `start`, that contains a `.git` file or directory.
fn directory_containing_git_entry(start: &Path) -> Option<PathBuf> {
    let mut dir = start.canonicalize().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Run a read-only `git` query in `dir`; `Some(trimmed stdout)` on success, else `None`.
/// Built through the Git Service's shared [`crate::git::git_command`] so the untrusted-repo
/// hardening (no optional index locks, neutralized `core.fsmonitor`/`core.hooksPath`, dropped
/// repo-redirecting env so the *root* resolves against `dir`) is applied identically here and
/// in the Git Service — it cannot drift between the two. The shared builder pins
/// `--attr-source` only when the installed git accepts it (git ≥ 2.40); older git (Apple's
/// 2.39.x) omits the flag so root resolution still sees a real repo instead of degrading to a
/// plain browser (#160).
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
