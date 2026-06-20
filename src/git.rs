//! Git Service — read-only answers to git questions (status, baseline, changed-set, diff).
//!
//! Issues **only** read-only `git` subcommands (AC-N2), capturing stdout via
//! `std::process`. Not-a-repo or any git failure degrades to an empty/neutral result so
//! the viewer keeps working as a plain browser (AC-26). Paths are repo-root-relative,
//! matching git's own output; `core.quotePath=false` keeps non-ASCII paths verbatim so
//! the keys match the real filesystem, and `-uall` lists every untracked file (not a
//! collapsed `dir/`). The host's base-branch hint is threaded through every Base query
//! so the baseline used to *decide* Base matches the one used to *compute* it.

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

/// Per-file working-tree status, keyed by repo-root-relative path.
pub fn status(repo_root: &Path) -> BTreeMap<PathBuf, Status> {
    let mut map = BTreeMap::new();
    let Some(out) = run_raw(
        repo_root,
        &[
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain",
            "-uall",
        ],
    ) else {
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
        let path = path_field.rsplit(" -> ").next().unwrap_or(path_field);
        if let Some(s) = classify(code) {
            map.insert(PathBuf::from(path), s);
        }
    }
    map
}

/// The context-smart default baseline: base branch on a feature branch / worktree
/// (AC-14), else HEAD on the base/default branch (AC-15).
pub fn default_baseline(resolved: &Resolved) -> Baseline {
    let Some(repo) = resolved.repo_root.as_deref() else {
        return Baseline::Head;
    };
    match (
        resolve_base_branch(repo, resolved.base_branch.as_deref()),
        current_branch(repo),
    ) {
        // On a branch other than the base/default branch → compare to the base (AC-14).
        (Some(base), Some(cur)) if base != cur => Baseline::Base,
        // On the base branch, detached, or no base info → uncommitted vs HEAD (AC-15).
        _ => Baseline::Head,
    }
}

/// The set of files changed against `baseline`, keyed by repo-root-relative path.
/// `base_hint` is the host-supplied base branch (carried from the launch context); it is
/// used for the Base baseline so the query matches `default_baseline`'s decision.
pub fn changed_set(
    repo_root: &Path,
    baseline: Baseline,
    base_hint: Option<&str>,
) -> BTreeMap<PathBuf, Status> {
    match baseline {
        // Uncommitted changes vs HEAD — exactly the working-tree status.
        Baseline::Head => status(repo_root),
        Baseline::Base => {
            // No resolvable base → degrade to a HEAD comparison (consistent with diff()).
            let Some(fork) = base_fork_point(repo_root, base_hint) else {
                return status(repo_root);
            };
            // `git diff <fork>` compares the fork-point tree to the working tree, so it
            // already includes committed-on-branch AND uncommitted tracked changes.
            let mut map = BTreeMap::new();
            if let Some(out) = run_raw(
                repo_root,
                &["-c", "core.quotePath=false", "diff", "--no-ext-diff", "--name-status", &fork],
            ) {
                for line in out.lines() {
                    let mut fields = line.split('\t');
                    let code = fields.next().unwrap_or("");
                    let path = fields.last().unwrap_or(""); // new path on rename/copy
                    if path.is_empty() {
                        continue;
                    }
                    if let Some(s) = classify_name_status(code) {
                        map.insert(PathBuf::from(path), s);
                    }
                }
            }
            // Untracked files aren't in `git diff` but are part of the body of work.
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
/// An untracked file has no tracked baseline, so we synthesize an added-file diff of its
/// content (via `git diff --no-index`) rather than returning an empty diff.
pub fn diff(repo_root: &Path, path: &Path, baseline: Baseline, base_hint: Option<&str>) -> String {
    let p = path.to_string_lossy();
    // `--no-ext-diff` refuses any external diff helper an untrusted repo's config or
    // .gitattributes might point at — git must not execute repo-controlled programs.
    if is_untracked(repo_root, path) {
        return run_allow_fail(
            repo_root,
            &["diff", "--no-ext-diff", "--no-index", "--no-color", "--", "/dev/null", p.as_ref()],
        );
    }
    let against = match baseline {
        Baseline::Head => "HEAD".to_string(),
        Baseline::Base => base_fork_point(repo_root, base_hint).unwrap_or_else(|| "HEAD".to_string()),
    };
    run_raw(repo_root, &["diff", "--no-ext-diff", &against, "--", p.as_ref()]).unwrap_or_default()
}

/// Build a `git -C <repo> <args>` command hardened for read-only use against an
/// untrusted repository: `GIT_OPTIONAL_LOCKS=0` stops status/diff from opportunistically
/// refreshing (writing) the index, keeping git state truly unchanged (AC-N2).
fn git_command(repo_root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(repo_root)
        .args(args);
    cmd
}

/// Run a read-only `git` command in `repo_root`, returning raw (untrimmed) stdout.
/// `None` if git is missing or exits non-zero (degrade to a plain browser, AC-26).
fn run_raw(repo_root: &Path, args: &[&str]) -> Option<String> {
    let out = git_command(repo_root, args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// Run a read-only `git` command, returning stdout regardless of exit code. Used for
/// `git diff --no-index`, which exits 1 precisely *because* it found differences.
fn run_allow_fail(repo_root: &Path, args: &[&str]) -> String {
    git_command(repo_root, args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
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

/// Whether `path` is untracked (not in the index) but present on disk.
fn is_untracked(repo_root: &Path, path: &Path) -> bool {
    let p = path.to_string_lossy();
    let tracked = run_raw(repo_root, &["ls-files", "--error-unmatch", "--", p.as_ref()]).is_some();
    !tracked && repo_root.join(path).exists()
}

/// Whether a ref resolves to a commit. `--end-of-options` keeps a `-`-prefixed name from
/// being parsed as a flag (defense-in-depth alongside [`is_safe_ref`]).
fn ref_exists(repo_root: &Path, name: &str) -> bool {
    run_raw(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{name}^{{commit}}"),
        ],
    )
    .is_some()
}

/// A host-supplied branch name we are willing to pass to git. Rejects empty and
/// option-like (`-`-prefixed) values so an untrusted hint can't inject a git flag.
fn is_safe_ref(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-')
}

/// The base/default branch: the host's hint if it is safe and resolves, else the
/// conventional fallback. Remote-tracking refs are included so a freshly-cloned repo or
/// worktree whose base exists only as `origin/main` still resolves a base (AC-14).
fn resolve_base_branch(repo_root: &Path, hint: Option<&str>) -> Option<String> {
    if let Some(h) = hint {
        if is_safe_ref(h) && ref_exists(repo_root, h) {
            return Some(h.to_string());
        }
    }
    ["main", "master", "origin/main", "origin/master"]
        .into_iter()
        .find(|c| ref_exists(repo_root, c))
        .map(str::to_string)
}

/// The merge-base of the base branch and HEAD — where the body of work forks off.
fn base_fork_point(repo_root: &Path, hint: Option<&str>) -> Option<String> {
    let base = resolve_base_branch(repo_root, hint)?;
    run_trimmed(repo_root, &["merge-base", &base, "HEAD"])
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

/// Map a `git diff --name-status` code letter to a tree status.
fn classify_name_status(code: &str) -> Option<Status> {
    match code.chars().next() {
        Some('A') => Some(Status::Added),
        Some('D') => Some(Status::Deleted),
        Some('M' | 'T' | 'R' | 'C') => Some(Status::Modified),
        _ => None,
    }
}
