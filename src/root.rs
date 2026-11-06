//! Root Resolver — resolve the tree root and git-presence from a [`LaunchContext`].
//!
//! Root is the working tree's top-level inside a git repo (AC-1) else the cwd (AC-2).
//! Not-a-repo is a normal result (`is_git_repo == false`), never an error (AC-26).
//! Uses only read-only `git` subcommands.

use crate::context::LaunchContext;
use crate::git::{Operation, Reach};
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
    // One operation: the filter inventory runs once for every query below.
    let git = Operation::new(&ctx.cwd);
    match trusted_toplevel(&git, &ctx.cwd) {
        Some(root) => Resolved {
            is_git_repo: true,
            repo_root: Some(root.clone()),
            is_worktree: is_linked_worktree(&git),
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

/// Git's toplevel, only when it is the directory that actually holds the `.git` entry git
/// discovered from `cwd`.
///
/// `core.worktree` in an untrusted repo config can make `rev-parse --show-toplevel` name a
/// different directory than the one the user opened. The filesystem fact is the nearest
/// ancestor of `cwd` whose `.git` entry resolves to the git dir git itself reports; git's
/// toplevel is accepted only when the two canonical paths are the same. A mismatch degrades to
/// a non-repo rooted at `cwd`, so later reads and git queries stay on the opened tree.
fn trusted_toplevel(git: &Operation, cwd: &Path) -> Option<PathBuf> {
    let reported = PathBuf::from(git_output(git, &["rev-parse", "--show-toplevel"])?);
    let git_dir = git_output(git, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
    let discovered = directory_owning_git_dir(cwd, Path::new(&git_dir))?;
    let reported_canon = reported.canonicalize().ok()?;
    // Keep git's own toplevel string when it names that same directory, so a trusted repo
    // resolves exactly as before.
    (reported_canon == discovered).then_some(reported)
}

/// Nearest ancestor of `start`, including `start`, whose `.git` entry resolves to `git_dir`:
/// a `.git` directory that is `git_dir`, or a `.git` file whose `gitdir:` names it (linked
/// worktrees, submodules). Matching git's own discovery, rather than stopping at any `.git`
/// that exists, skips an empty or invalid `.git` that git itself walked past. Canonical.
fn directory_owning_git_dir(start: &Path, git_dir: &Path) -> Option<PathBuf> {
    let git_dir = git_dir.canonicalize().ok()?;
    let mut dir = start.canonicalize().ok()?;
    loop {
        let entry = dir.join(".git");
        let target = if entry.is_file() {
            gitfile_target(&dir, &entry)
        } else {
            entry.canonicalize().ok()
        };
        if target.as_deref() == Some(git_dir.as_path()) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The canonical git dir a `.git` file (`gitdir: <path>`) in `dir` points at. Like git, only
/// trailing line terminators are stripped and a relative path resolves against `dir`.
pub(crate) fn gitfile_target(dir: &Path, gitfile: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(gitfile).ok()?;
    let target = content
        .strip_prefix("gitdir: ")?
        .trim_end_matches(['\n', '\r']);
    dir.join(target).canonicalize().ok()
}

/// Run a read-only `git` query in the operation's directory; `Some(stdout)` minus its line terminator on success,
/// else `None`. Only the final `\n` is dropped, never other whitespace: a path git prints may
/// itself end in a space or `\r`, and trimming it would compare a different directory than
/// the one git will use (`core.worktree = "/repo "` must not pass for `/repo`).
/// Built through the Git Service's shared [`Operation::command`] so the untrusted-repo
/// hardening (no optional index locks, neutralized `core.fsmonitor`/`core.hooksPath`, dropped
/// repo-redirecting env so the *root* resolves against that directory) is applied identically here and
/// in the Git Service — it cannot drift between the two. The shared builder pins
/// `--attr-source` only when the installed git accepts it (git ≥ 2.40); older git (Apple's
/// 2.39.x) omits the flag so root resolution still sees a real repo instead of degrading to a
/// plain browser (#160).
fn git_output(git: &Operation, args: &[&str]) -> Option<String> {
    // Root resolution runs only `rev-parse`, which never checks a submodule for changes.
    let out = git.command(Reach::Repository, args).output().ok()?;
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        Some(stdout.strip_suffix('\n').unwrap_or(&stdout).to_string())
    } else {
        None
    }
}

/// A linked worktree's git-dir (`.git/worktrees/<name>`) differs from the shared
/// common dir (`.git`); the main working tree's are the same.
fn is_linked_worktree(git: &Operation) -> bool {
    let git_dir = git_output(git, &["rev-parse", "--path-format=absolute", "--git-dir"]);
    let common = git_output(
        git,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    match (git_dir, common) {
        (Some(g), Some(c)) => g != c,
        _ => false,
    }
}
