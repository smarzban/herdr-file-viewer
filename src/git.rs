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
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// git's well-known empty-tree object — the baseline for an unborn repo's first files.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The host's null-device token: the untracked-file diff base and the `core.hooksPath`
/// neutralization target both need a path that always resolves to "discard" — `/dev/null`
/// on unix, `NUL` on Windows (AC-6).
#[cfg(unix)]
const NULL_DEVICE: &str = "/dev/null";
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";

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
    // -uall lists each untracked file individually (not a collapsed `dir/`); -z gives
    // verbatim NUL-delimited paths (no quoting/escaping to misparse).
    match run_bytes(repo_root, &["status", "--porcelain=v1", "-z", "-uall"]) {
        // Not a repo / git unavailable → empty (AC-26).
        None => BTreeMap::new(),
        Some(out) => parse_porcelain_status(&out),
    }
}

/// Decode one raw path field from git's NUL-delimited (`-z`) output into a [`PathBuf`].
///
/// unix: a lossless byte-for-byte mapping (today's `OsStr::from_bytes`) — any filename
/// (spaces, control chars, non-UTF-8 bytes) maps to the real filesystem path, since unix
/// `OsStr` is an arbitrary byte sequence.
///
/// Windows: git always emits UTF-8 path bytes (Windows paths are UTF-16 internally, so a
/// non-UTF-8 byte sequence from git is essentially unreachable there). Bytes are decoded as
/// UTF-8; a path that somehow fails to decode is dropped (`None`) rather than panicking,
/// consistent with this module's degrade-to-neutral rule (AC-5).
#[cfg(unix)]
fn path_from_git_bytes(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(windows)]
fn path_from_git_bytes(bytes: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(bytes).ok().map(PathBuf::from)
}

/// Parse `git status --porcelain=v1 -z -uall` output into a per-path status map.
///
/// Extracted from [`status`] so the parser's defensive branches (truncated records,
/// unknown XY codes, rename/copy `old -> new` form, empty input) are unit-testable
/// without a live git repo. Pure: bytes in, map out.
fn parse_porcelain_status(out: &[u8]) -> BTreeMap<PathBuf, Status> {
    let mut map = BTreeMap::new();
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
        if let Some(s) = classify(code)
            && let Some(p) = path_from_git_bytes(path)
        {
            map.insert(p, s);
        }
    }
    map
}

/// Parse `git diff --name-status -z` output into a per-path status map.
///
/// Extracted from [`changed_set`] so the parser's defensive branches (rename/copy
/// `code, old, new` triples, truncated records, unknown codes, empty input) are
/// unit-testable without a live git repo. Pure: bytes in, map out.
fn parse_name_status(out: &[u8]) -> BTreeMap<PathBuf, Status> {
    let mut map = BTreeMap::new();
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
        if let Some(s) = classify_name_status(code)
            && let Some(p) = path_from_git_bytes(path)
        {
            map.insert(p, s);
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
                map = parse_name_status(&out);
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
        args.push(NULL_DEVICE);
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

/// Raw unified diff for every tracked change under `rel_dir` against `baseline`.
/// `rel_dir` is repo-root-relative; an empty path means the whole tree root (no pathspec).
/// Untracked files are not included — `git diff` only sees the index/worktree for tracked paths
/// (file-level untracked still uses the single-file [`diff`] `--no-index` path).
pub fn diff_directory(
    repo_root: &Path,
    rel_dir: &Path,
    baseline: Baseline,
    base_hint: Option<&str>,
) -> String {
    // Same root-bound check as [`diff`]: reject absolute paths and any `..` component so a
    // pathspec cannot escape the tree root (AC-N5). Pass the *relative* path — joining first
    // would make `is_within_root` see an absolute path and always reject it.
    if !rel_dir.as_os_str().is_empty() && !is_within_root(repo_root, rel_dir) {
        return String::new();
    }
    let against = match baseline {
        Baseline::Head => head_or_empty_tree(repo_root),
        Baseline::Base => {
            base_fork_point(repo_root, base_hint).unwrap_or_else(|| head_or_empty_tree(repo_root))
        }
    };
    // Pathspec only when scoped to a subdir; empty pathspec = whole tree.
    let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
    args.push(&against);
    args.push("--");
    let mut cmd = git_command(repo_root, &args);
    if !rel_dir.as_os_str().is_empty() {
        cmd.arg(rel_dir);
    }
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
        .args(["-c", "core.fsmonitor=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={NULL_DEVICE}"))
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

/// The current branch name, or `None` when detached. `pub` so the Session Controller can cache
/// it for the tree's bottom-border title (it is computed once per (re-)root, never per-frame).
pub fn current_branch(repo_root: &Path) -> Option<String> {
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

    #[test]
    fn diff_directory_rejects_parent_dir_and_absolute_pathspecs() {
        // AC-N5: pathspecs must stay inside the tree root. `..` and absolute paths are rejected
        // before any git invocation (is_within_root rejects both).
        let root = Path::new("/tmp/some-repo");
        assert_eq!(
            diff_directory(root, Path::new(".."), Baseline::Head, None),
            "",
            "parent-dir pathspec must not run git"
        );
        assert_eq!(
            diff_directory(root, Path::new("../outside"), Baseline::Head, None),
            "",
            "escaped relative pathspec must not run git"
        );
        assert_eq!(
            diff_directory(root, Path::new("/etc"), Baseline::Head, None),
            "",
            "absolute pathspec must not run git"
        );
    }

    // ---- path_from_git_bytes: platform path-decode seam (AC-5, T-1) ------------

    /// unix: a byte sequence with spaces and non-ASCII (but valid UTF-8) bytes maps
    /// losslessly to the equivalent `PathBuf`, matching today's `OsStr::from_bytes` behaviour.
    #[cfg(unix)]
    #[test]
    fn path_from_git_bytes_unix_maps_arbitrary_bytes_losslessly() {
        let bytes = "my résumé file.txt".as_bytes();
        assert_eq!(
            path_from_git_bytes(bytes),
            Some(PathBuf::from("my résumé file.txt"))
        );
    }

    /// unix: even non-UTF-8 bytes (which a real OS filename can legally contain) decode
    /// without loss — `OsStr` is an arbitrary byte sequence on unix.
    #[cfg(unix)]
    #[test]
    fn path_from_git_bytes_unix_preserves_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let bytes = [b'a', 0xFF, b'b'];
        let decoded = path_from_git_bytes(&bytes).expect("non-UTF-8 bytes still decode on unix");
        assert_eq!(decoded.as_os_str().as_bytes(), &bytes);
    }

    /// Windows: git emits UTF-8 path bytes; a UTF-8 byte path decodes to the equivalent
    /// `PathBuf` (the contract this seam exists to satisfy — AC-5).
    #[cfg(windows)]
    #[test]
    fn path_from_git_bytes_windows_decodes_utf8_paths() {
        let bytes = "my résumé file.txt".as_bytes();
        assert_eq!(
            path_from_git_bytes(bytes),
            Some(PathBuf::from("my résumé file.txt"))
        );
    }

    /// Windows: a non-UTF-8 byte sequence (essentially unreachable from real git output)
    /// is dropped (`None`), never a panic — the module's degrade-to-neutral rule.
    #[cfg(windows)]
    #[test]
    fn path_from_git_bytes_windows_drops_invalid_utf8_without_panic() {
        let bytes = [0xFFu8, 0xFE, 0xFD];
        assert_eq!(path_from_git_bytes(&bytes), None);
    }

    // ---- NULL_DEVICE: platform null-device seam (AC-6, T-2) --------------------

    /// unix: the null-device token is `/dev/null`.
    #[cfg(unix)]
    #[test]
    fn null_device_is_dev_null_on_unix() {
        assert_eq!(NULL_DEVICE, "/dev/null");
    }

    /// Windows: the null-device token is `NUL`.
    #[cfg(windows)]
    #[test]
    fn null_device_is_nul_on_windows() {
        assert_eq!(NULL_DEVICE, "NUL");
    }

    /// `git_command`'s `core.hooksPath` hardening uses the same host null-device token as the
    /// untracked-diff base (both derive from [`NULL_DEVICE`]), so the two callers cannot drift.
    #[test]
    fn hooks_path_hardening_uses_the_null_device_constant() {
        let cmd = git_command(Path::new("/some/repo"), &["status"]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.contains(&format!("core.hooksPath={NULL_DEVICE}")),
            "hooksPath uses the platform null-device constant: {args:?}"
        );
    }

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
            args.contains(&format!("core.hooksPath={NULL_DEVICE}")),
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

    // ---- classify: every porcelain XY code → Status -----------------------------

    /// Table-driven coverage for [`classify`]: each row is a (code, expected) pair
    /// hitting a distinct branch — untracked, deleted, added, the catch-all modified,
    /// the empty/ignored `None` fallback, and an unknown-but-nonempty code.
    #[test]
    fn classify_maps_every_status_code_branch() {
        let cases: &[(&str, Option<Status>)] = &[
            // Untracked — the `??` branch.
            ("??", Some(Status::Untracked)),
            // Deleted — any code containing `D` (staged, unstaged, both).
            (" D", Some(Status::Deleted)),
            ("D ", Some(Status::Deleted)),
            ("DD", Some(Status::Deleted)),
            ("MD", Some(Status::Deleted)), // `D` wins over `M`
            ("AD", Some(Status::Deleted)), // `D` wins over `A`
            // Added — any code containing `A` and NOT `D`.
            ("A ", Some(Status::Added)),
            (" A", Some(Status::Added)),
            ("AM", Some(Status::Added)), // `A` wins over `M`
            // Modified — the catch-all for non-empty, non-untracked, non-D/A codes.
            (" M", Some(Status::Modified)),
            ("M ", Some(Status::Modified)),
            ("MM", Some(Status::Modified)),
            ("MR", Some(Status::Modified)),
            ("MC", Some(Status::Modified)),
            ("MT", Some(Status::Modified)),
            ("RM", Some(Status::Modified)),
            ("CM", Some(Status::Modified)),
            ("TM", Some(Status::Modified)),
            // Empty / whitespace-only → None (unmodified / ignored branch).
            ("", None),
            ("  ", None),
            // Unknown but non-empty code that is neither D/A/M/R/C/T/U → the catch-all
            // maps it to Modified (the trailing `else`). `!` is git's ignored marker,
            // which also falls here: `!`.trim().is_empty() is false → Modified.
            ("!!", Some(Status::Modified)),
            ("UU", Some(Status::Modified)), // unmerged — still mapped to Modified
            ("XY", Some(Status::Modified)), // unknown letters → Modified
        ];
        for (code, expected) in cases {
            assert_eq!(
                classify(code),
                *expected,
                "classify({code:?}) — branch coverage"
            );
        }
    }

    // ---- classify_name_status: every --name-status letter → Status -------------

    /// Table-driven coverage for [`classify_name_status`]: every recognized letter
    /// plus the unknown-letter `None` fallback.
    #[test]
    fn classify_name_status_maps_every_letter() {
        let cases: &[(&str, Option<Status>)] = &[
            ("A", Some(Status::Added)),
            ("D", Some(Status::Deleted)),
            ("M", Some(Status::Modified)),
            ("T", Some(Status::Modified)), // type change → Modified
            ("R", Some(Status::Modified)), // rename → Modified
            ("C", Some(Status::Modified)), // copy → Modified
            // Unknown letters / empty / garbage → None.
            ("X", None),
            ("", None),
            ("Z", None),
            ("?", None),
        ];
        for (code, expected) in cases {
            assert_eq!(
                classify_name_status(code),
                *expected,
                "classify_name_status({code:?}) — letter coverage"
            );
        }
    }

    // ---- parse_porcelain_status: malformed / truncated / edge inputs ----------

    /// Helper: build a `-z` porcelain byte stream by NUL-joining the given field
    /// slices and trailing a NUL terminator (as git emits).
    fn porcelain_bytes(fields: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for f in fields {
            out.extend_from_slice(f);
            out.push(0);
        }
        out
    }

    /// Empty input (git produced no output) → empty map. Covers the no-records branch.
    #[test]
    fn parse_porcelain_status_empty_input() {
        assert!(parse_porcelain_status(b"").is_empty());
        assert!(parse_porcelain_status(&porcelain_bytes(&[])).is_empty());
    }

    /// A record shorter than 3 bytes (`<XY SP path>` minimum) is skipped, not a panic.
    /// Covers the `rec.len() < 3` defensive branch.
    #[test]
    fn parse_porcelain_status_skips_truncated_records() {
        // 0-byte, 1-byte, and 2-byte (exactly the code, no space/path) records.
        let out = porcelain_bytes(&[b"", b"X", b"XY", b" M mod.txt"]);
        let map = parse_porcelain_status(&out);
        assert_eq!(map.len(), 1, "only the well-formed record survives");
        assert_eq!(map.get(&PathBuf::from("mod.txt")), Some(&Status::Modified));
    }

    /// A record whose first two bytes are invalid UTF-8 still parses (the `unwrap_or("")`
    /// branch): `from_utf8` fails, code becomes "", `classify("")` → None, so the
    /// record is dropped rather than crashing.
    #[test]
    fn parse_porcelain_status_invalid_utf8_code_is_dropped() {
        // 0xFF 0xFF is not valid UTF-8; code resolves to "" → classify → None → dropped.
        let bad_code = [0xFFu8, 0xFF, b' ', b'p'];
        let out = porcelain_bytes(&[&bad_code, b" M ok.txt"]);
        let map = parse_porcelain_status(&out);
        assert_eq!(
            map.len(),
            1,
            "invalid-utf8 code is dropped, the next record still parses"
        );
        assert_eq!(map.get(&PathBuf::from("ok.txt")), Some(&Status::Modified));
    }

    /// A rename record (`R…`) is followed by a separate NUL field with the old path;
    /// the parser must consume that extra field so the NEW path is keyed and the next
    /// record is not mis-parsed. This is the most fragile porcelain parse case.
    #[test]
    fn parse_porcelain_status_rename_consumes_old_path_field() {
        // "RM new.txt\0old.txt\0 M after.txt" — R record + old path + trailing record.
        let out = porcelain_bytes(&[b"RM new.txt", b"old.txt", b" M after.txt"]);
        let map = parse_porcelain_status(&out);
        assert_eq!(
            map.get(&PathBuf::from("new.txt")),
            Some(&Status::Modified),
            "rename's NEW path is keyed (R → Modified)"
        );
        assert!(
            !map.contains_key(&PathBuf::from("old.txt")),
            "rename's old path is consumed as the extra field, not keyed"
        );
        assert_eq!(
            map.get(&PathBuf::from("after.txt")),
            Some(&Status::Modified),
            "the record after the rename parses correctly (no desync)"
        );
    }

    /// A copy record (`C…`) likewise consumes its old-path field.
    #[test]
    fn parse_porcelain_status_copy_consumes_old_path_field() {
        let out = porcelain_bytes(&[b"CM c.txt", b"orig.txt", b"?? untracked.txt"]);
        let map = parse_porcelain_status(&out);
        assert_eq!(map.get(&PathBuf::from("c.txt")), Some(&Status::Modified));
        assert!(!map.contains_key(&PathBuf::from("orig.txt")));
        assert_eq!(
            map.get(&PathBuf::from("untracked.txt")),
            Some(&Status::Untracked)
        );
    }

    /// A rename record whose trailing old-path field is MISSING (truncated stream)
    /// must not panic: `fields.next()` returns None, the record's new path is still
    /// keyed, and the loop ends cleanly.
    #[test]
    fn parse_porcelain_status_rename_with_missing_old_field_does_not_panic() {
        let out = porcelain_bytes(&[b"RM only.txt"]); // no old-path field follows
        let map = parse_porcelain_status(&out);
        assert_eq!(map.get(&PathBuf::from("only.txt")), Some(&Status::Modified));
    }

    /// NUL-delimited edge cases: paths containing bytes that look like the porcelain
    /// separator (space) or high-bit bytes pass through verbatim because the parser
    /// slices on NUL, not on spaces.
    #[test]
    fn parse_porcelain_status_preserves_paths_with_spaces_and_high_bytes() {
        // Path "my file.txt" (with a space) and a non-ASCII path.
        let out = porcelain_bytes(&[b"?? my file.txt", "?? résumé.txt".as_bytes()]);
        let map = parse_porcelain_status(&out);
        assert_eq!(
            map.get(&PathBuf::from("my file.txt")),
            Some(&Status::Untracked)
        );
        assert_eq!(
            map.get(&PathBuf::from("résumé.txt")),
            Some(&Status::Untracked)
        );
    }

    /// An unknown XY code (letters git never emits in porcelain v1) still maps through
    /// the catch-all → Modified, so the parser never silently drops a recognizable
    /// change because of an unfamiliar code.
    #[test]
    fn parse_porcelain_status_unknown_xy_code_maps_to_modified() {
        let out = porcelain_bytes(&[b"ZZ weird.txt"]);
        let map = parse_porcelain_status(&out);
        assert_eq!(
            map.get(&PathBuf::from("weird.txt")),
            Some(&Status::Modified),
            "unknown XY code falls through classify → Modified"
        );
    }

    // ---- parse_name_status: malformed / truncated / edge inputs ----------------

    /// Helper: build a `-z` name-status byte stream by NUL-joining fields + terminator.
    fn name_status_bytes(fields: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for f in fields {
            out.extend_from_slice(f);
            out.push(0);
        }
        out
    }

    /// Empty input → empty map.
    #[test]
    fn parse_name_status_empty_input() {
        assert!(parse_name_status(b"").is_empty());
        assert!(parse_name_status(&name_status_bytes(&[])).is_empty());
    }

    /// Rename/copy triples (`code, old, new`) key the NEW path and consume the old.
    #[test]
    fn parse_name_status_rename_triple_keys_new_path() {
        let out = name_status_bytes(&[b"R100", b"old.txt", b"new.txt", b"M", b"mod.txt"]);
        let map = parse_name_status(&out);
        assert_eq!(map.get(&PathBuf::from("new.txt")), Some(&Status::Modified));
        assert!(!map.contains_key(&PathBuf::from("old.txt")));
        assert_eq!(map.get(&PathBuf::from("mod.txt")), Some(&Status::Modified));
    }

    #[test]
    fn parse_name_status_copy_triple_keys_new_path() {
        let out = name_status_bytes(&[b"C100", b"src.txt", b"dst.txt", b"A", b"add.txt"]);
        let map = parse_name_status(&out);
        assert_eq!(map.get(&PathBuf::from("dst.txt")), Some(&Status::Modified));
        assert!(!map.contains_key(&PathBuf::from("src.txt")));
        assert_eq!(map.get(&PathBuf::from("add.txt")), Some(&Status::Added));
    }

    /// A rename record missing its trailing new-path field (truncated stream) must
    /// not panic: the loop breaks on `None` from `fields.next()`.
    #[test]
    fn parse_name_status_rename_with_missing_new_field_breaks_cleanly() {
        let out = name_status_bytes(&[b"R100", b"old.txt"]); // no new-path field
        let map = parse_name_status(&out);
        assert!(map.is_empty(), "truncated rename triple yields no entry");
    }

    /// A code whose first byte is invalid UTF-8 resolves to "" via `unwrap_or("")`,
    /// so `classify_name_status("")` → None and the record is dropped (no panic).
    #[test]
    fn parse_name_status_invalid_utf8_code_is_dropped() {
        let bad_code = [0xFFu8, 0xFF];
        let out = name_status_bytes(&[&bad_code, b"p.txt", b"M", b"ok.txt"]);
        let map = parse_name_status(&out);
        assert_eq!(
            map.len(),
            1,
            "invalid-utf8 code dropped, next record parses"
        );
        assert_eq!(map.get(&PathBuf::from("ok.txt")), Some(&Status::Modified));
    }

    /// An unknown code letter (not A/D/M/T/R/C) → None → the record is dropped.
    #[test]
    fn parse_name_status_unknown_code_letter_is_dropped() {
        let out = name_status_bytes(&[b"X", b"weird.txt", b"M", b"ok.txt"]);
        let map = parse_name_status(&out);
        assert_eq!(map.len(), 1, "unknown letter dropped");
        assert_eq!(map.get(&PathBuf::from("ok.txt")), Some(&Status::Modified));
    }

    /// A code field with trailing content beyond the first char is still mapped by its
    /// leading letter (e.g. `R100`, `C75`, `M100`), matching git's name-status format.
    #[test]
    fn parse_name_status_maps_by_leading_letter_ignoring_trailing_digits() {
        let out = name_status_bytes(&[b"A100", b"a.txt", b"D100", b"d.txt", b"M100", b"m.txt"]);
        let map = parse_name_status(&out);
        assert_eq!(map.get(&PathBuf::from("a.txt")), Some(&Status::Added));
        assert_eq!(map.get(&PathBuf::from("d.txt")), Some(&Status::Deleted));
        assert_eq!(map.get(&PathBuf::from("m.txt")), Some(&Status::Modified));
    }

    /// Paths with spaces and high-bit bytes round-trip verbatim (NUL-delimited parse).
    #[test]
    fn parse_name_status_preserves_paths_with_spaces_and_high_bytes() {
        let out = name_status_bytes(&[b"M", b"my file.txt", b"A", "résumé.txt".as_bytes()]);
        let map = parse_name_status(&out);
        assert_eq!(
            map.get(&PathBuf::from("my file.txt")),
            Some(&Status::Modified)
        );
        assert_eq!(map.get(&PathBuf::from("résumé.txt")), Some(&Status::Added));
    }
}
