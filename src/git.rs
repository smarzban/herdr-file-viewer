//! Git Service — read-only answers to git questions (status, baseline, changed-set, diff).
//!
//! Issues **only** read-only `git` subcommands (AC-N2), capturing stdout via
//! `std::process`. Not-a-repo or any git failure degrades to an empty/neutral result so
//! the viewer keeps working as a plain browser (AC-26).
//!
//! The viewer opens *untrusted* repositories (e.g. an agent's worktree, a clone), so
//! every invocation is hardened against repo-controlled code execution: `--no-ext-diff` +
//! `--no-textconv` refuse repo-configured diff/textconv programs, `core.fsmonitor` and
//! `core.hooksPath` are neutralized, and `GIT_OPTIONAL_LOCKS=0` keeps status/diff from
//! writing the index (AC-N2). Paths are parsed from NUL-delimited (`-z`) output as raw
//! bytes, so any filename — spaces, control chars, non-ASCII — maps to the real
//! filesystem path. The host's base-branch hint is threaded through every Base query so
//! the baseline used to *decide* Base matches the one used to *compute* it.

use crate::root::Resolved;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// git's well-known empty-tree object — the baseline for an unborn repo's first files.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Unified-context window for a full-context (whole-file) diff: a value far larger than any
/// real file, so every unchanged line is emitted as context around the changes. The output is
/// bounded by [`MAX_DIFF_BYTES`] (and the render layer's AC-13 cap), so an enormous file's
/// diff is never buffered whole.
const FULL_CONTEXT: &str = "-U1000000";

/// Upper bound on bytes captured from a `git diff`, so a full-context diff of a huge file (or
/// an untracked whole-file diff) cannot buffer the entire file into memory before the render
/// layer's display cap (AC-13) runs. Comfortably above that display cap (render's ~1 MB), so
/// it never reduces what the user sees — only the transient buffer and git's work.
const MAX_DIFF_BYTES: u64 = 4 * 1024 * 1024; // 4 MB

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
    // -uall lists each untracked file individually (not a collapsed `dir/`); -z gives
    // verbatim NUL-delimited paths (no quoting/escaping to misparse).
    let Some(out) = run_bytes(repo_root, &["status", "--porcelain=v1", "-z", "-uall"]) else {
        return map; // not a repo / git unavailable → empty (AC-26)
    };
    let mut fields = out.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(rec) = fields.next() {
        // Porcelain v1 record: two status chars, a space, then the path.
        if rec.len() < 3 {
            continue;
        }
        let code = std::str::from_utf8(&rec[..2]).unwrap_or("");
        let path = &rec[3..];
        // A rename/copy is followed by a separate NUL field with the original path.
        if code.contains('R') || code.contains('C') {
            fields.next();
        }
        if let Some(s) = classify(code) {
            map.insert(PathBuf::from(OsStr::from_bytes(path)), s);
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
        // A detached managed worktree is still a body of work to review vs the base.
        (Some(_), None) if resolved.is_worktree => Baseline::Base,
        // On the base branch, plain detached HEAD, or no base info → vs HEAD (AC-15).
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
            if let Some(out) = run_bytes(
                repo_root,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--name-status",
                    "-z",
                    &fork,
                ],
            ) {
                let mut fields = out.split(|&b| b == 0).filter(|f| !f.is_empty());
                while let Some(code_f) = fields.next() {
                    let code = std::str::from_utf8(code_f).unwrap_or("");
                    // Rename/copy emits code, old, new; everything else code, path.
                    let path = if matches!(code.chars().next(), Some('R' | 'C')) {
                        fields.next(); // old
                        fields.next() // new
                    } else {
                        fields.next()
                    };
                    let Some(path) = path else { break };
                    if let Some(s) = classify_name_status(code) {
                        map.insert(PathBuf::from(OsStr::from_bytes(path)), s);
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
/// An untracked file (or any file in an unborn repo) is diffed against the empty tree so
/// AC-9 still shows the new file's content rather than an empty pane.
pub fn diff(
    repo_root: &Path,
    path: &Path,
    baseline: Baseline,
    base_hint: Option<&str>,
    full_context: bool,
) -> String {
    // Never resolve a path outside the root — no arbitrary file reads, and the viewer
    // does not navigate above its root (AC-N5).
    if !is_within_root(repo_root, path) {
        return String::new();
    }
    // For the full-context (whole-file) diff, ask git for a very large unified-context window
    // so every unchanged line is emitted as context around the changes; the default (3 lines)
    // gives the compact hunks-only diff. The render layer still bounds the result (AC-13).
    let unified = full_context.then_some(FULL_CONTEXT);
    // The path is appended as a raw OsStr arg (not lossy UTF-8) so non-ASCII / non-UTF-8
    // filenames reach git verbatim and their diffs are not silently empty.
    if is_untracked(repo_root, path) {
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-index",
            "--no-color",
        ];
        args.extend(unified);
        args.push("--");
        args.push("/dev/null");
        let mut cmd = git_command(repo_root, &args);
        cmd.arg(path);
        return capture_stdout(cmd);
    }
    let against = match baseline {
        Baseline::Head => head_or_empty_tree(repo_root),
        Baseline::Base => {
            base_fork_point(repo_root, base_hint).unwrap_or_else(|| head_or_empty_tree(repo_root))
        }
    };
    let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
    args.extend(unified);
    args.push(&against);
    args.push("--");
    let mut cmd = git_command(repo_root, &args);
    cmd.arg(path);
    capture_stdout(cmd)
}

/// Build a `git -C <dir> <args>` command hardened for read-only use against an **untrusted**
/// repository: `GIT_OPTIONAL_LOCKS=0` stops status/diff from writing the index (AC-N2);
/// `core.fsmonitor` / `core.hooksPath` are neutralized so a planted `.git/config` can't run a
/// program during a query; and inherited repo-redirecting env (`GIT_DIR`/`GIT_WORK_TREE`/…) is
/// dropped so queries resolve against `-C <dir>`, not a repository the viewer was launched
/// against. **This is the single source of that hardening** — the Root Resolver
/// ([`crate::root`]) builds its queries through this same function, so the guards cannot drift
/// between the two.
pub(crate) fn git_command(repo_root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        // Drop inherited repo-redirecting env so queries resolve against `-C <repo>`, not
        // a GIT_DIR/GIT_WORK_TREE the viewer happened to be launched with.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .arg("-C")
        .arg(repo_root)
        // Read attributes from the empty tree, not the worktree `.gitattributes`, so a
        // repo-planted `filter=<driver>` (clean/smudge) or `diff=<driver>` (textconv)
        // cannot run a configured program during a read-only query.
        .arg(format!("--attr-source={EMPTY_TREE}"))
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args);
    cmd
}

/// Capture a git command's stdout (lossy) regardless of exit code, bounded to
/// [`MAX_DIFF_BYTES`]. `git diff` exits 1 under `--no-index` *because* it found differences,
/// so we cannot gate on success.
///
/// The read is bounded so a full-context diff (`-U1000000`) of a huge file — or an untracked
/// whole-file diff — cannot buffer the entire file into memory before the render layer's cap
/// (AC-13) runs: we read at most `MAX_DIFF_BYTES`, then kill git (which is otherwise blocked
/// writing to the now-unread pipe). The render layer still truncates the visible diff and
/// shows its notice; this bound is comfortably above that display cap, so it only limits the
/// transient buffer (and git's work), never what the user sees.
fn capture_stdout(mut cmd: Command) -> String {
    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let mut buf = Vec::new();
    if let Some(out) = child.stdout.take() {
        let _ = out.take(MAX_DIFF_BYTES).read_to_end(&mut buf);
    }
    // We may have stopped reading before git finished (output exceeded the cap); kill it so a
    // git blocked on the full pipe is released, and reap it to avoid a zombie. The exit status
    // is irrelevant here (see above), so it is ignored.
    let _ = child.kill();
    let _ = child.wait();
    String::from_utf8_lossy(&buf).into_owned()
}

/// Run a read-only `git` command, returning raw stdout bytes (for `-z` parsing).
/// `None` if git is missing or exits non-zero (degrade to a plain browser, AC-26).
fn run_bytes(repo_root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = git_command(repo_root, args).output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// Run a read-only `git` command, returning stdout as a (lossy) string. `None` on
/// failure. Used where the output is not a list of paths.
fn run_raw(repo_root: &Path, args: &[&str]) -> Option<String> {
    run_bytes(repo_root, args).map(|b| String::from_utf8_lossy(&b).into_owned())
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

/// `HEAD` when it resolves, else git's empty-tree object so an unborn repo's first
/// (staged) files still diff as additions instead of failing on `bad revision 'HEAD'`.
fn head_or_empty_tree(repo_root: &Path) -> String {
    if run_raw(repo_root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_some() {
        "HEAD".to_string()
    } else {
        EMPTY_TREE.to_string()
    }
}

/// A path that stays within the root: relative, free of parent-dir (`..`) components, and
/// — once resolved — not escaping the root via a symlinked intermediate directory.
fn is_within_root(repo_root: &Path, path: &Path) -> bool {
    if path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    // If the target resolves (exists), ensure symlinks didn't lead outside the root.
    match (
        repo_root.join(path).canonicalize(),
        repo_root.canonicalize(),
    ) {
        (Ok(full), Ok(root)) => full.starts_with(root),
        // Non-existent target (e.g. a deleted file): the lexical checks already bound it.
        _ => true,
    }
}

/// Whether `path` is untracked (not in the index) but present on disk. The path is passed
/// as a raw OsStr arg so non-UTF-8 names match the index correctly.
fn is_untracked(repo_root: &Path, path: &Path) -> bool {
    let mut cmd = git_command(repo_root, &["ls-files", "--error-unmatch", "--"]);
    cmd.arg(path);
    let tracked = cmd.output().map(|o| o.status.success()).unwrap_or(false);
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
    if let Some(h) = hint
        && is_safe_ref(h)
        && ref_exists(repo_root, h)
    {
        return Some(h.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared hardened builder must apply *every* untrusted-repo guard. This is the
    /// regression guard that keeps the Git Service and the Root Resolver — which now build
    /// their queries through this one function — from silently dropping a protection (AC-N2).
    #[test]
    fn git_command_applies_every_untrusted_repo_guard() {
        let cmd = git_command(Path::new("/some/repo"), &["status"]);

        // CLI guards: -C <dir>, neutralized fsmonitor/hooks, attr-source pinned to empty tree.
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|a| a == "-C"),
            "runs with -C <dir>: {args:?}"
        );
        assert!(
            args.contains(&"core.fsmonitor=false".to_string()),
            "fsmonitor neutralized: {args:?}"
        );
        assert!(
            args.contains(&"core.hooksPath=/dev/null".to_string()),
            "hooks neutralized: {args:?}"
        );
        assert!(
            args.iter().any(|a| a.starts_with("--attr-source=")),
            "attr-source pinned to the empty tree: {args:?}"
        );

        // GIT_OPTIONAL_LOCKS=0 is set; the repo-redirecting vars are scrubbed (env value None).
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(
            envs.iter()
                .any(|(k, v)| k == "GIT_OPTIONAL_LOCKS" && v.as_deref() == Some("0")),
            "optional locks disabled: {envs:?}"
        );
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
        ] {
            assert!(
                envs.iter().any(|(k, v)| k == var && v.is_none()),
                "{var} is scrubbed from the child env: {envs:?}"
            );
        }
    }
}
