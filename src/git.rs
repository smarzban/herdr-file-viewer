//! Git Service — read-only answers to git questions (status, baseline, changed-set, diff).
//!
//! Issues **only** read-only `git` subcommands (AC-N2), capturing stdout via
//! `std::process`. Not-a-repo or any git failure degrades to an empty/neutral result so
//! the viewer keeps working as a plain browser (AC-26).
//!
//! The viewer opens *untrusted* repositories (e.g. an agent's worktree, a clone), so
//! every invocation is hardened against repo-controlled code execution: executable filters
//! are neutralized, `--no-ext-diff` + `--no-textconv` refuse diff/textconv programs,
//! `diff.submodule=short` stops Git from spawning a nested diff that would not inherit
//! those flags, `core.fsmonitor` and `core.hooksPath` are neutralized, and
//! `GIT_OPTIONAL_LOCKS=0` keeps status/diff from writing the index (AC-N2). Paths are parsed
//! from NUL-delimited (`-z`) output as raw bytes, so any filename — spaces, control chars,
//! non-ASCII — maps to the real filesystem path. The host's base-branch hint is threaded
//! through every Base query so the baseline used to *decide* Base matches the one used to
//! *compute* it.

use crate::root::Resolved;
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

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
/// Hard cap on how many untracked descendants a directory diff will spawn a `git` process for.
/// The byte cap ([`MAX_DIFF_BYTES`]) alone does not bound process spawns: a directory of many
/// *small* untracked files stays under it while still spawning one process each. The render
/// layer's AC-13 preview cap truncates the visible pane far below this, so a dropped tail is
/// never displayed.
const MAX_UNTRACKED_DIFF_FILES: usize = 512;

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
    status_in(&Operation::new(repo_root))
}

fn status_in(op: &Operation) -> BTreeMap<PathBuf, Status> {
    // -uall lists each untracked file individually (not a collapsed `dir/`); -z gives
    // verbatim NUL-delimited paths (no quoting/escaping to misparse). `status` checks every
    // populated submodule for changes, so it needs the submodule inventory.
    match run_bytes(
        op,
        Reach::Submodules,
        &["status", "--porcelain=v1", "-z", "-uall"],
    ) {
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
    let op = Operation::new(repo);
    match (
        resolve_base_branch(&op, resolved.base_branch.as_deref()),
        current_branch_in(&op),
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
    let op = Operation::new(repo_root);
    match baseline {
        // Uncommitted changes vs HEAD — exactly the working-tree status.
        Baseline::Head => status_in(&op),
        Baseline::Base => {
            // No resolvable base → degrade to a HEAD comparison (consistent with diff()).
            let Some(fork) = base_fork_point(&op, base_hint) else {
                return status_in(&op);
            };
            // `git diff <fork>` compares the fork-point tree to the working tree, so it
            // already includes committed-on-branch AND uncommitted tracked changes, and it checks
            // populated submodules for changes.
            let mut map = BTreeMap::new();
            if let Some(out) = run_bytes(
                &op,
                Reach::Submodules,
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
            for (path, s) in status_in(&op) {
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
    let op = Operation::new(repo_root);
    // The path is appended as a raw OsStr arg (not lossy UTF-8) so non-ASCII / non-UTF-8
    // filenames reach git verbatim and their diffs are not silently empty.
    if is_untracked(&op, path) {
        return diff_untracked(&op, path, unified);
    }
    let against = baseline_commit(&op, baseline, base_hint);
    let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
    args.extend(unified);
    args.push(&against);
    args.push("--");
    // A commit-vs-worktree diff checks a submodule path for changes.
    let mut cmd = op.command(Reach::Submodules, &args);
    cmd.arg(path);
    capture_stdout(cmd)
}

/// The `/dev/null`-vs-file `--no-index` patch for a KNOWN-untracked path (all its content shown
/// as additions). The caller guarantees `path` is untracked, so this skips the `is_untracked`
/// probe that the general [`diff`] must run first — one `git` process, not two. `unified` is the
/// optional full-context window. Read-only, and the same root-confinement guard [`diff`] applies
/// (AC-N5) so a status-derived path can never resolve outside the tree.
fn diff_untracked(op: &Operation, path: &Path, unified: Option<&str>) -> String {
    if !is_within_root(op.repo_root, path) {
        return String::new();
    }
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
    // `--no-index` compares two paths outside any index; it never checks a submodule.
    let mut cmd = op.command(Reach::Repository, &args);
    cmd.arg(path);
    capture_stdout(cmd)
}

/// Raw unified diff for every change under `rel_dir` against `baseline`.
/// `rel_dir` is repo-root-relative; an empty path means the whole tree root (no pathspec).
/// Tracked changes come from one directory-scoped git diff; untracked files are appended using the
/// same `--no-index` path used by the single-file [`diff`] query, so a status-mode directory view
/// includes every `?` descendant as well as modified/staged/deleted tracked files.
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
    let op = Operation::new(repo_root);
    let against = baseline_commit(&op, baseline, base_hint);
    // Pathspec only when scoped to a subdir; empty pathspec = whole tree.
    let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
    args.push(&against);
    args.push("--");
    // A commit-vs-worktree diff checks populated submodules for changes. This operation's
    // submodule inventory then also covers its status and every untracked-file diff below.
    let mut cmd = op.command(Reach::Submodules, &args);
    if !rel_dir.as_os_str().is_empty() {
        cmd.arg(rel_dir);
    }
    let mut output = capture_stdout(cmd);

    // `git diff` intentionally omits untracked paths. Append each untracked descendant as a safe
    // /dev/null-vs-file patch via [`diff_untracked`] — NOT the general [`diff`], which would
    // re-spawn `git ls-files` to re-establish untracked-ness we already have from `status` (a
    // second, wasted process per file).
    //
    // Two independent bounds, because they guard different resources:
    //   • BYTES — cap the running total at [`MAX_DIFF_BYTES`] (like [`capture_stdout`] does for a
    //     single invocation) so the string can't grow oversized; the final admitted patch is
    //     trimmed to the remaining budget at a UTF-8 boundary.
    //   • COUNT — cap the number of spawned processes at [`MAX_UNTRACKED_DIFF_FILES`]; the byte cap
    //     alone doesn't bound spawns when the untracked files are small (many stay under it).
    // The render layer's AC-13 preview cap truncates the visible pane far below either bound, so a
    // dropped tail is never displayed. `status` is a `BTreeMap`, so the paths admitted before a cap
    // are the lexicographically-first ones (deterministic), not an arbitrary subset.
    let cap = MAX_DIFF_BYTES as usize;
    let mut spawned = 0usize;
    for (path, state) in status_in(&op) {
        if cap.saturating_sub(output.len()) == 0 || spawned >= MAX_UNTRACKED_DIFF_FILES {
            break;
        }
        if state != Status::Untracked
            || (!rel_dir.as_os_str().is_empty() && !path.starts_with(rel_dir))
        {
            continue;
        }
        spawned += 1;
        let file_diff = diff_untracked(&op, &path, None);
        if file_diff.is_empty() {
            continue;
        }
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        // Append only what fits in the remaining budget, cut at a char boundary so the String
        // stays valid UTF-8. `floor_char_boundary` returns the largest boundary <= the index.
        let budget = cap.saturating_sub(output.len());
        let take = file_diff.floor_char_boundary(budget.min(file_diff.len()));
        output.push_str(&file_diff[..take]);
    }
    output
}

/// Cached answer to [`probe_attr_source`]. One probe per process; every later query reuses it.
static ATTR_SOURCE_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Whether this `git` accepts `--attr-source` (git ≥ 2.40). Apple's Xcode git is still 2.39.x
/// and treats the unknown flag as a hard error, so passing it unconditionally made every query
/// fail and the viewer degraded to a plain, non-git browser (#160).
fn attr_source_supported() -> bool {
    *ATTR_SOURCE_SUPPORTED.get_or_init(probe_attr_source)
}

/// Probe: `git --attr-source=<empty-tree> --version`. Success means the flag is known; any
/// failure (unknown option, git missing) means omit it. `--version` needs no repository.
fn probe_attr_source() -> bool {
    Command::new("git")
        .arg(format!("--attr-source={EMPTY_TREE}"))
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Turn git's NUL-delimited config-key bytes into an OS argument without changing the driver
/// name. Repository config is untrusted, so lossy decoding would leave a non-UTF-8 driver active.
#[cfg(unix)]
fn os_string_from_git_bytes(bytes: Vec<u8>) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Some(OsString::from_vec(bytes))
}

#[cfg(windows)]
fn os_string_from_git_bytes(bytes: Vec<u8>) -> Option<OsString> {
    String::from_utf8(bytes).ok().map(OsString::from)
}

/// Extract every configured filter driver name from `git config --null` output, with or without
/// values (`key\nvalue` records). A driver subsection may itself contain dots, so split at the
/// final dot, not every dot.
fn configured_filter_driver_names(out: &[u8]) -> BTreeSet<Vec<u8>> {
    let mut drivers = BTreeSet::new();
    for record in out.split(|byte| *byte == 0).filter(|key| !key.is_empty()) {
        // A key never contains a newline; a value may.
        let key = record.split(|byte| *byte == b'\n').next().unwrap_or(record);
        let Some(prefix) = key.get(..7) else { continue };
        if !prefix.eq_ignore_ascii_case(b"filter.") {
            continue;
        }
        let Some(last_dot) = key.iter().rposition(|byte| *byte == b'.') else {
            continue;
        };
        if last_dot < 7 {
            continue;
        }
        let field = &key[last_dot + 1..];
        if [b"clean".as_slice(), b"smudge", b"process", b"required"]
            .iter()
            .any(|candidate| field.eq_ignore_ascii_case(candidate))
        {
            drivers.insert(key[7..last_dot].to_vec());
        }
    }
    drivers
}

/// How long one inventory read may block. An included config path that never returns (for
/// example a fifo) must not freeze every later git query; past this budget the inventory is
/// unavailable and the caller fails closed.
const FILTER_INVENTORY_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a whole recursive inventory may take. Each repository's reads are bounded by
/// [`FILTER_INVENTORY_TIMEOUT`]; this caps their sum. The work grows with the number of populated
/// submodules (about 1.1s for a 200-submodule superproject, so this covers roughly 1800), so a
/// flat per-read budget would refuse a large legitimate superproject. Kept well short of a long
/// stall: every slow-but-successful read a repository plants (even in submodules git itself
/// skips) is paid in full until this budget ends in a refusal.
const SUBMODULE_INVENTORY_TOTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest config-key listing accepted from the inventory. A truncated read is incomplete, so
/// it is refused rather than turned into a partial override list.
const MAX_FILTER_INVENTORY_BYTES: u64 = 1024 * 1024;

/// Deepest submodule nesting the inventory follows. Deeper nesting (or a `.git` that leads back
/// into an enclosing worktree) makes the inventory unavailable, so the query is refused.
const MAX_SUBMODULE_DEPTH: usize = 8;

/// Most populated submodules one inventory visits; more is refused. Far above real
/// superprojects, it bounds the git processes a hostile index can make one query start.
const MAX_INVENTORIED_SUBMODULES: usize = 10_000;

/// Submodule repositories inventoried at once. Each inventory runs two short-lived git processes
/// (three when its worktree needs `rev-parse`), so this bounds concurrent children (and their
/// pipes) for a many-submodule repo. More workers do not help: the walk is bound by process
/// start-up, not by waiting.
const SUBMODULE_INVENTORY_PARALLELISM: usize = 8;

/// How far a git command can reach, which decides whose filter drivers its overrides must cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// The opened repository only, for commands that never check a submodule for changes
    /// (`rev-parse`, `merge-base`, `ls-files --error-unmatch`, `worktree list`,
    /// `diff --no-index`). Their overrides still cover the repository's own drivers: a
    /// `--no-index` diff applies filters selected by `$GIT_DIR/info/attributes`.
    Repository,
    /// The repository and every populated submodule below it, for `status` and for a diff that
    /// compares the worktree (`git diff <commit>`). Both spawn `git status` inside each populated
    /// submodule to detect a dirty one. That child inherits the `-c` overrides through
    /// `GIT_CONFIG_PARAMETERS`, but it reads the submodule's own config and
    /// `$GIT_DIR/info/attributes`, so a driver named only there must be neutralized too.
    Submodules,
}

/// Builds the hardened commands of ONE public git operation (a status, a diff, a root
/// resolution). Each [`Reach`]'s filter inventory runs at most once per operation, on first use,
/// and is dropped with it: nothing is kept across operations, so every user action reads the
/// repository's configuration afresh.
pub(crate) struct Operation<'a> {
    repo_root: &'a Path,
    repository_drivers: OnceCell<Option<BTreeSet<Vec<u8>>>>,
    submodule_drivers: OnceCell<Option<BTreeSet<Vec<u8>>>>,
}

impl<'a> Operation<'a> {
    pub(crate) fn new(repo_root: &'a Path) -> Self {
        Self {
            repo_root,
            repository_drivers: OnceCell::new(),
            submodule_drivers: OnceCell::new(),
        }
    }

    /// A `git -C <repo_root> <args>` command hardened as [`git_command_with`] describes, with
    /// every filter driver `reach` can run neutralized. When that inventory is unavailable the
    /// command is one git refuses before touching the repository.
    pub(crate) fn command(&self, reach: Reach, args: &[&str]) -> Command {
        let drivers = match reach {
            Reach::Submodules => self.submodule_drivers.get_or_init(|| {
                // Reuse this operation's repository read rather than reading that config twice.
                match self.repository_drivers.get() {
                    Some(None) => None,
                    Some(Some(known)) => submodule_filter_drivers(
                        self.repo_root,
                        Some(known.clone()),
                        FILTER_INVENTORY_TIMEOUT,
                        SUBMODULE_INVENTORY_TOTAL_TIMEOUT,
                    ),
                    None => submodule_filter_drivers(
                        self.repo_root,
                        None,
                        FILTER_INVENTORY_TIMEOUT,
                        SUBMODULE_INVENTORY_TOTAL_TIMEOUT,
                    ),
                }
            }),
            // The submodule inventory is a superset, so reuse it when this operation has one.
            Reach::Repository => match self.submodule_drivers.get() {
                Some(all) => all,
                None => self.repository_drivers.get_or_init(|| {
                    repository_filter_drivers(self.repo_root, FILTER_INVENTORY_TIMEOUT)
                }),
            },
        };
        let Some(overrides) = drivers.as_ref().and_then(filter_overrides_from_names) else {
            // A global option that git cannot recognize fails before repository setup or command
            // dispatch. This keeps the existing degrade-to-neutral behavior without risking a
            // query whose executable-filter inventory could not be established.
            let mut refused = base_git_command(self.repo_root);
            refused.arg("--herdr-refuse-unverified-filter-config");
            return refused;
        };
        git_command_with(self.repo_root, args, attr_source_supported(), &overrides)
    }
}

/// Filter driver names configured for `repo_root` itself, read without consulting attributes
/// or invoking filters. Git's documented behavior for a filter without a command is
/// pass-through; in git 2.39's `convert.c`, empty clean/process strings are likewise rejected by
/// the `*cmd` check before any child can spawn.
fn repository_filter_drivers(repo_root: &Path, timeout: Duration) -> Option<BTreeSet<Vec<u8>>> {
    let config = spawn_inventory_reader(repo_root, false, &FILTER_CONFIG_ARGS, read_capped)?;
    let listing = config_listing(config.finish(Instant::now() + timeout)?)?;
    Some(configured_filter_driver_names(&listing))
}

/// Filter driver names configured for `repo_root` and for every populated submodule below it,
/// recursively. `known` is this operation's already-read repository set, which skips re-reading
/// that config. Each repository's reads are bounded by `per_read`, the whole walk by `total`.
fn submodule_filter_drivers(
    repo_root: &Path,
    known: Option<BTreeSet<Vec<u8>>>,
    per_read: Duration,
    total: Duration,
) -> Option<BTreeSet<Vec<u8>>> {
    let total_deadline = Instant::now() + total;
    // The opened repository runs inline: one without submodules starts no worker threads.
    let top = inventory_repo(repo_root, false, known, per_read, total_deadline)?;
    if top.populated_submodules.is_empty() {
        return Some(top.drivers);
    }
    if top.populated_submodules.len() > MAX_INVENTORIED_SUBMODULES {
        return None;
    }
    let queue = Mutex::new(SubmoduleQueue {
        visited: top.populated_submodules.len(),
        pending: top
            .populated_submodules
            .into_iter()
            .map(|checkout| (checkout, 1))
            .collect(),
        in_flight: 0,
        drivers: top.drivers,
        failed: false,
    });
    let wake = Condvar::new();
    let workers_finished = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..SUBMODULE_INVENTORY_PARALLELISM)
            .map(|_| {
                std::thread::Builder::new()
                    .spawn_scoped(scope, || {
                        inventory_worker(&queue, &wake, per_read, total_deadline)
                    })
                    .ok()
            })
            .collect();
        if workers.iter().any(Option::is_none) {
            // Too few workers is not a reason to run short: stop the ones that did start.
            fail_inventory(&queue, &wake);
        }
        // Join every worker before deciding: a panicked worker joined here is a refusal, not a
        // panic out of this scope into the caller.
        let joined: Vec<bool> = workers
            .into_iter()
            .map(|worker| worker.is_some_and(|worker| worker.join().is_ok()))
            .collect();
        joined.into_iter().all(|ok| ok)
    });
    let queue = queue.into_inner().ok()?;
    (workers_finished && !queue.failed).then_some(queue.drivers)
}

/// Mark the inventory incomplete and wake every waiting worker so it stops.
fn fail_inventory(queue: &Mutex<SubmoduleQueue>, wake: &Condvar) {
    queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .failed = true;
    wake.notify_all();
}

/// A checkout a worker has taken from the queue. If the worker unwinds before reporting it, the
/// inventory fails and the other workers are woken, rather than waiting on it forever.
struct ClaimedCheckout<'a> {
    queue: &'a Mutex<SubmoduleQueue>,
    wake: &'a Condvar,
}

impl Drop for ClaimedCheckout<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            fail_inventory(self.queue, self.wake);
        }
    }
}

/// A checkout with this name makes its inventory worker panic, so a test can drive the unwind
/// path that no real repository reaches on demand.
#[cfg(test)]
const PANICKING_CHECKOUT_FOR_TESTS: &str = "hfv-inventory-worker-panics";

/// Shared state of a recursive inventory. A work queue rather than per-level batches, so one
/// slow repository never leaves the other workers idle.
struct SubmoduleQueue {
    /// Checkouts still to inventory, with their nesting depth.
    pending: VecDeque<(PathBuf, usize)>,
    in_flight: usize,
    /// Checkouts queued so far, against [`MAX_INVENTORIED_SUBMODULES`].
    visited: usize,
    drivers: BTreeSet<Vec<u8>>,
    /// Any incomplete repository makes the whole inventory unavailable.
    failed: bool,
}

/// Take checkouts from `queue` until it is drained or has failed, queueing the populated
/// submodules each one registers.
fn inventory_worker(
    queue: &Mutex<SubmoduleQueue>,
    wake: &Condvar,
    per_read: Duration,
    total_deadline: Instant,
) {
    loop {
        let (checkout, depth) = {
            let Ok(mut state) = queue.lock() else { return };
            loop {
                if state.failed {
                    return;
                }
                if let Some(next) = state.pending.pop_front() {
                    state.in_flight += 1;
                    break next;
                }
                if state.in_flight == 0 {
                    return;
                }
                // Bounded even if a claimed checkout is never reported: past the total budget
                // the inventory is incomplete.
                let now = Instant::now();
                if now >= total_deadline {
                    state.failed = true;
                    drop(state);
                    wake.notify_all();
                    return;
                }
                state = match wake.wait_timeout(state, total_deadline - now) {
                    Ok((state, _)) => state,
                    Err(_) => return,
                };
            }
        };
        let _claim = ClaimedCheckout { queue, wake };
        #[cfg(test)]
        if checkout.file_name() == Some(std::ffi::OsStr::new(PANICKING_CHECKOUT_FOR_TESTS)) {
            panic!("injected inventory worker panic");
        }
        let inventory = if depth > MAX_SUBMODULE_DEPTH {
            None
        } else {
            inventory_repo(&checkout, true, None, per_read, total_deadline)
        };
        let Ok(mut state) = queue.lock() else { return };
        state.in_flight -= 1;
        match inventory {
            Some(found)
                if state.visited + found.populated_submodules.len()
                    <= MAX_INVENTORIED_SUBMODULES =>
            {
                state.visited += found.populated_submodules.len();
                state.drivers.extend(found.drivers);
                let nested = found.populated_submodules.into_iter();
                state.pending.extend(nested.map(|path| (path, depth + 1)));
            }
            _ => state.failed = true,
        }
        drop(state);
        wake.notify_all();
    }
}

/// One repository's share of the filter inventory.
struct RepoInventory {
    drivers: BTreeSet<Vec<u8>>,
    /// Worktree paths of the populated submodules registered in this repository's index.
    populated_submodules: Vec<PathBuf>,
}

/// `git config` arguments that list every configured filter key and every `core.worktree`
/// value, following includes. Values come too: the submodule worktree check reads them.
const FILTER_CONFIG_ARGS: [&str; 5] = [
    "config",
    "--includes",
    "--null",
    "--get-regexp",
    r"^filter\.|^core\.worktree$",
];

/// Inventory `dir`'s configured filter driver names (unless `known` already holds them) and its
/// populated submodules. Built-in `git config`, `git ls-files` and, only when the listing cannot
/// settle a submodule's worktree, `git rev-parse`: they read configuration and the index, never
/// attributes, and never invoke filter drivers. The children run concurrently, each bounded by
/// `per_read` and all by `total_deadline`.
fn inventory_repo(
    dir: &Path,
    is_submodule: bool,
    known: Option<BTreeSet<Vec<u8>>>,
    per_read: Duration,
    total_deadline: Instant,
) -> Option<RepoInventory> {
    let deadline = (Instant::now() + per_read).min(total_deadline);
    if Instant::now() >= deadline {
        return None;
    }
    let config = match known {
        Some(_) => None,
        None => Some(spawn_inventory_reader(
            dir,
            is_submodule,
            &FILTER_CONFIG_ARGS,
            read_capped,
        )?),
    };
    // The index, not `.gitmodules`, is what git consults: a gitlink with no `.gitmodules` entry
    // is still checked for a dirty worktree.
    let gitlinks = spawn_inventory_reader(
        dir,
        is_submodule,
        &["ls-files", "--stage", "-z"],
        read_gitlinks,
    )?;

    let (drivers, listing) = match (known, config) {
        (Some(known), _) => (known, None),
        (None, Some(config)) => {
            let listing = config_listing(config.finish(deadline)?)?;
            (configured_filter_driver_names(&listing), Some(listing))
        }
        (None, None) => return None,
    };
    let (gitlinks_status, gitlinks) = gitlinks.finish(deadline)?;
    if !gitlinks_status.success() {
        return None;
    }
    // A submodule's own `core.worktree` decides where its nested status runs. Follow it only when
    // it is the checkout the superproject sees; anything else is not a complete inventory.
    if is_submodule
        && !listing
            .as_deref()
            .is_some_and(|listing| worktree_is_checkout(dir, listing))
    {
        let toplevel =
            spawn_inventory_reader(dir, true, &["rev-parse", "--show-toplevel"], read_capped)?;
        let (status, out) = toplevel.finish(deadline)?;
        // Raw bytes, like every other path this module reads from git: a non-UTF-8 checkout
        // path must still compare equal to itself.
        let reported = path_from_git_bytes(out?.strip_suffix(b"\n")?)?;
        if !status.success() || reported.canonicalize().ok()? != dir.canonicalize().ok()? {
            return None;
        }
    }

    // Git checks a gitlink for dirtiness only when `<path>/.git` exists; an unpopulated submodule
    // has no repository whose config could name a filter.
    let populated_submodules = gitlinks?
        .into_iter()
        .map(|path| dir.join(path))
        .filter(|checkout| checkout.join(".git").exists())
        .collect();
    Some(RepoInventory {
        drivers,
        populated_submodules,
    })
}

/// Whether the config listing alone proves that git's worktree for the submodule at `dir` is
/// `dir`, which saves a `rev-parse` per submodule. Git runs a submodule's status with
/// `GIT_DIR=.git` from `dir`, so its worktree is `dir` unless `core.worktree` is set, and a
/// relative value resolves against the real git dir (git `chdir`s there, then to the value).
/// The listing holds every `core.worktree` git could read, so when each one resolves to `dir`,
/// git's choice does too. Anything else (an unreadable `.git`, a valueless or foreign entry) is
/// `false` and left to `rev-parse`.
fn worktree_is_checkout(dir: &Path, listing: &[u8]) -> bool {
    let mut values = listing
        .split(|byte| *byte == 0)
        .filter(|record| record.starts_with(b"core.worktree"))
        .peekable();
    if values.peek().is_none() {
        return true;
    }
    let Ok(checkout) = dir.canonicalize() else {
        return false;
    };
    let dot_git = dir.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git.canonicalize().ok()
    } else {
        crate::root::gitfile_target(dir, &dot_git)
    };
    let Some(git_dir) = git_dir else {
        return false;
    };
    values.all(|record| {
        record
            .strip_prefix(b"core.worktree\n")
            .and_then(path_from_git_bytes)
            .and_then(|value| git_dir.join(value).canonicalize().ok())
            .is_some_and(|worktree| worktree == checkout)
    })
}

/// The output of a finished `git config --get-regexp` read. It returns 1 for no matches. Every
/// other failure is unsafe to interpret as "no filters": fail closed rather than running the
/// requested repo query with an incomplete inventory.
fn config_listing(
    (status, output): (std::process::ExitStatus, Option<Vec<u8>>),
) -> Option<Vec<u8>> {
    let output = output?;
    let no_matches = status.code() == Some(1) && output.is_empty();
    (status.success() || no_matches).then_some(output)
}

/// A spawned inventory child whose stdout is consumed on its own thread.
struct InventoryReader<T> {
    child: std::process::Child,
    output: mpsc::Receiver<Option<T>>,
}

/// `as_submodule` selects the repository the way git does when it checks a submodule for changes:
/// `GIT_DIR=.git` from the checkout, never discovery. A checkout whose `.git` is not a repository
/// then fails here instead of reading an enclosing repository.
fn spawn_inventory_reader<T: Send + 'static>(
    dir: &Path,
    as_submodule: bool,
    args: &[&str],
    read: fn(std::process::ChildStdout) -> Option<T>,
) -> Option<InventoryReader<T>> {
    let mut cmd = base_git_command(dir);
    if as_submodule {
        cmd.env("GIT_DIR", ".git");
    }
    let mut child = cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = crate::proc::terminate_and_reap(&mut child);
        return None;
    };
    let (sender, output) = mpsc::channel();
    // A thread the OS cannot create is an unavailable inventory, not a panic.
    let reader = std::thread::Builder::new().spawn(move || {
        let _ = sender.send(read(stdout));
    });
    if reader.is_err() {
        let _ = crate::proc::terminate_and_reap(&mut child);
        return None;
    }
    Some(InventoryReader { child, output })
}

impl<T> InventoryReader<T> {
    /// The child's exit status and parsed stdout (`None` when the reader refused it). On overrun
    /// the child is killed and reaped and nothing is returned: a partial listing is never parsed.
    fn finish(mut self, deadline: Instant) -> Option<(std::process::ExitStatus, Option<T>)> {
        // Waiting for end-of-output first returns as soon as the child is done, rather than at
        // the next exit poll. A missed deadline still kills and reaps the child below.
        let output = self
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        if output.is_ok() {
            // A child that closed its output is exiting: poll finely so a large inventory does
            // not pay the shared helper's 10ms poll interval per read.
            while Instant::now() < deadline {
                if let Ok(Some(status)) = self.child.try_wait() {
                    return Some((status, output.ok()?));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let status = crate::proc::wait_until(&mut self.child, deadline)?;
        Some((status, output.ok()?))
    }
}

impl<T> Drop for InventoryReader<T> {
    /// A reader abandoned early (a sibling failed to spawn) must not leave its child running.
    /// After [`InventoryReader::finish`] the status is already reaped, so this is a no-op.
    fn drop(&mut self) {
        let _ = crate::proc::terminate_and_reap(&mut self.child);
    }
}

/// Read a whole listing, refusing one that reaches the byte cap (it may be truncated).
fn read_capped(stdout: std::process::ChildStdout) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    stdout
        .take(MAX_FILTER_INVENTORY_BYTES)
        .read_to_end(&mut buf)
        .ok()?;
    ((buf.len() as u64) < MAX_FILTER_INVENTORY_BYTES).then_some(buf)
}

/// Stream `git ls-files --stage -z` and keep only gitlink (mode 160000) paths. The full listing
/// is proportional to the tracked-file count and is never buffered; only the retained gitlink
/// paths count against the byte cap.
fn read_gitlinks(stdout: std::process::ChildStdout) -> Option<Vec<PathBuf>> {
    parse_gitlinks(std::io::BufReader::with_capacity(64 * 1024, stdout))
}

fn parse_gitlinks(mut listing: impl std::io::BufRead) -> Option<Vec<PathBuf>> {
    let mut gitlinks = Vec::new();
    let mut retained = 0u64;
    let mut record = Vec::new();
    loop {
        record.clear();
        if listing.read_until(0, &mut record).ok()? == 0 {
            // A conflicted gitlink has one record per stage; inventory its checkout once.
            gitlinks.dedup();
            return Some(gitlinks);
        }
        if !record.starts_with(b"160000 ") {
            continue;
        }
        // `<mode> <object> <stage>\t<path>\0`
        let record = record.strip_suffix(b"\0")?;
        let tab = record.iter().position(|byte| *byte == b'\t')?;
        let path = &record[tab + 1..];
        retained += path.len() as u64;
        if path.is_empty() || retained >= MAX_FILTER_INVENTORY_BYTES {
            return None;
        }
        gitlinks.push(path_from_git_bytes(path)?);
    }
}

#[cfg(test)]
fn filter_overrides_from_config(output: &[u8]) -> Option<Vec<OsString>> {
    filter_overrides_from_names(&configured_filter_driver_names(output))
}

fn filter_overrides_from_names(drivers: &BTreeSet<Vec<u8>>) -> Option<Vec<OsString>> {
    drivers
        .iter()
        .flat_map(|driver| {
            ["clean", "smudge", "process", "required"]
                .into_iter()
                .map(move |field| {
                    // Git splits `-c key=value` at the first equals sign.
                    if driver.contains(&b'=') {
                        return None;
                    }
                    let mut arg = b"filter.".to_vec();
                    arg.extend_from_slice(driver);
                    arg.push(b'.');
                    arg.extend_from_slice(field.as_bytes());
                    arg.push(b'=');
                    if field == "required" {
                        arg.extend_from_slice(b"false");
                    }
                    os_string_from_git_bytes(arg)
                })
        })
        .collect()
}

/// Start a git command with the environment and repository selection shared by the safe filter
/// inventory and every viewer query. The inventory runs only the built-ins `git config`,
/// `git ls-files --stage` and `git rev-parse --show-toplevel`, which read configuration and the
/// index but never attributes, and never invoke filter drivers.
fn base_git_command(repo_root: &Path) -> Command {
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
        .args(["-c", "core.fsmonitor=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={NULL_DEVICE}"))
        // `--no-ext-diff` / `--no-textconv` are flags of this process only. A repository
        // `diff.submodule=diff` makes Git spawn another `git diff` inside a populated
        // submodule, and that child does not receive those flags, so the submodule's
        // `diff.external` (and, without `--attr-source`, its textconv) would run.
        // Command-line `-c` overrides the repo value and is inherited by that child, and
        // `short` never spawns the content diff.
        .args(["-c", "diff.submodule=short"]);
    cmd
}

/// A `git -C <dir> <args>` command for a caller outside this module, as
/// [`Operation::command`] builds it. Use an [`Operation`] instead when one action runs several
/// commands, so the filter inventory runs once.
pub(crate) fn git_command(repo_root: &Path, reach: Reach, args: &[&str]) -> Command {
    Operation::new(repo_root).command(reach, args)
}

/// Build a `git -C <dir> <args>` command hardened for read-only use against an **untrusted**
/// repository: `GIT_OPTIONAL_LOCKS=0` stops status/diff from writing the index (AC-N2);
/// `core.fsmonitor` / `core.hooksPath` are neutralized; `diff.submodule=short` keeps a nested
/// repository from running its own external diff; every configured clean/smudge/process
/// filter is replaced by an empty non-required command; and inherited repo-redirecting env is
/// dropped. **This is the single source of that hardening** for the Git Service and Root Resolver.
///
/// `--attr-source` still pins worktree attributes to the empty tree when available. Older git
/// omits that unsupported flag. Filter overrides also cover attribute sources the flag does not
/// replace, including `$GIT_DIR/info/attributes`. Explicit feature and filter inputs keep unit
/// tests independent of the runner's git version and repository configuration.
fn git_command_with(
    repo_root: &Path,
    args: &[&str],
    pin_attr_source: bool,
    filter_overrides: &[OsString],
) -> Command {
    let mut cmd = base_git_command(repo_root);
    if pin_attr_source {
        cmd.arg(format!("--attr-source={EMPTY_TREE}"));
    }
    for filter_override in filter_overrides {
        cmd.arg("-c").arg(filter_override);
    }
    cmd.args(args);
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
fn run_bytes(op: &Operation, reach: Reach, args: &[&str]) -> Option<Vec<u8>> {
    let out = op.command(reach, args).output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// Run a read-only `git` command, returning stdout as a (lossy) string. `None` on
/// failure. Used where the output is not a list of paths.
fn run_raw(op: &Operation, reach: Reach, args: &[&str]) -> Option<String> {
    run_bytes(op, reach, args).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Run a read-only `git` command and trim the stdout (for branch names / hashes).
fn run_trimmed(op: &Operation, reach: Reach, args: &[&str]) -> Option<String> {
    run_raw(op, reach, args).map(|s| s.trim().to_string())
}

/// The current branch name, or `None` when detached. `pub` so the Session Controller can cache
/// it for the tree's bottom-border title (it is computed once per (re-)root, never per-frame).
pub fn current_branch(repo_root: &Path) -> Option<String> {
    current_branch_in(&Operation::new(repo_root))
}

fn current_branch_in(op: &Operation) -> Option<String> {
    // `rev-parse` reads refs only; it never checks a submodule for changes.
    match run_trimmed(
        op,
        Reach::Repository,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    ) {
        Some(b) if b != "HEAD" => Some(b),
        _ => None,
    }
}

/// The current branch (as [`current_branch`]) and HEAD's commit id, from ONE `git rev-parse`.
/// Both `None` outside a repo or before the first commit (an unborn HEAD fails the call).
pub fn head_state(repo_root: &Path) -> (Option<String>, Option<String>) {
    // `rev-parse` reads refs only; it never checks a submodule for changes.
    let Some(out) = run_trimmed(
        &Operation::new(repo_root),
        Reach::Repository,
        &["rev-parse", "HEAD", "--abbrev-ref", "HEAD"],
    ) else {
        return (None, None);
    };
    let mut lines = out.lines();
    let oid = lines.next().map(str::to_owned);
    let branch = lines.next().filter(|b| *b != "HEAD").map(str::to_owned);
    (branch, oid)
}

/// The commit a diff against `baseline` compares with: the fork point for Base when one resolves,
/// else [`head_or_empty_tree`].
fn baseline_commit(op: &Operation, baseline: Baseline, base_hint: Option<&str>) -> String {
    match baseline {
        Baseline::Head => head_or_empty_tree(op),
        Baseline::Base => base_fork_point(op, base_hint).unwrap_or_else(|| head_or_empty_tree(op)),
    }
}

/// `HEAD` when it resolves, else git's empty-tree object so an unborn repo's first
/// (staged) files still diff as additions instead of failing on `bad revision 'HEAD'`.
fn head_or_empty_tree(op: &Operation) -> String {
    // `rev-parse` reads refs only; it never checks a submodule for changes.
    if run_raw(
        op,
        Reach::Repository,
        &["rev-parse", "--verify", "--quiet", "HEAD"],
    )
    .is_some()
    {
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
fn is_untracked(op: &Operation, path: &Path) -> bool {
    // `ls-files --error-unmatch` matches index entries only; it never checks a submodule.
    let mut cmd = op.command(Reach::Repository, &["ls-files", "--error-unmatch", "--"]);
    cmd.arg(path);
    let tracked = cmd.output().map(|o| o.status.success()).unwrap_or(false);
    !tracked && op.repo_root.join(path).exists()
}

/// Whether a ref resolves to a commit. `--end-of-options` keeps a `-`-prefixed name from
/// being parsed as a flag (defense-in-depth alongside [`is_safe_ref`]).
fn ref_exists(op: &Operation, name: &str) -> bool {
    // `rev-parse` reads refs only; it never checks a submodule for changes.
    run_raw(
        op,
        Reach::Repository,
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
fn resolve_base_branch(op: &Operation, hint: Option<&str>) -> Option<String> {
    if let Some(h) = hint
        && is_safe_ref(h)
        && ref_exists(op, h)
    {
        return Some(h.to_string());
    }
    ["main", "master", "origin/main", "origin/master"]
        .into_iter()
        .find(|c| ref_exists(op, c))
        .map(str::to_string)
}

/// The merge-base of the base branch and HEAD — where the body of work forks off.
fn base_fork_point(op: &Operation, hint: Option<&str>) -> Option<String> {
    let base = resolve_base_branch(op, hint)?;
    // `merge-base` reads commits only; it never checks a submodule for changes.
    run_trimmed(op, Reach::Repository, &["merge-base", &base, "HEAD"])
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
        let cmd = git_command(Path::new("/some/repo"), Reach::Repository, &["status"]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.contains(&format!("core.hooksPath={NULL_DEVICE}")),
            "hooksPath uses the platform null-device constant: {args:?}"
        );
    }

    /// Shared assertions for the untrusted-repo guards that never depend on git's version.
    fn assert_version_independent_guards(cmd: &Command) {
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
            args.contains(&"diff.submodule=short".to_string()),
            "submodule content diffs are not spawned: {args:?}"
        );

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

    fn command_args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn configured_filter_keys_extract_exact_driver_names() {
        let names = configured_filter_driver_names(
            b"filter.simple.clean\0filter.with.dots.process\0filter.Case.smudge\0\
              filter.required-only.required\0diff.not-a-filter.command\0filter.no-command.delay\0",
        );
        assert_eq!(
            names,
            BTreeSet::from([
                b"Case".to_vec(),
                b"required-only".to_vec(),
                b"simple".to_vec(),
                b"with.dots".to_vec(),
            ])
        );
    }

    #[test]
    fn filter_names_are_read_from_a_listing_with_values() {
        let names = configured_filter_driver_names(
            b"filter.lfs.clean\ngit-lfs clean -- %f\0filter.a.b.process\nx.y\nz.clean\0\
              core.worktree\n../../sub\0filter.bare.required\0",
        );
        assert_eq!(
            names,
            BTreeSet::from([b"lfs".to_vec(), b"a.b".to_vec(), b"bare".to_vec()])
        );
    }

    /// The listing settles a submodule's worktree only when every `core.worktree` it holds
    /// resolves, against the real git dir, to the checkout itself.
    #[test]
    fn worktree_listing_proves_only_the_checkout_itself() {
        let root = std::env::temp_dir().join(format!(
            "hfv-worktree-proof-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let embedded = root.join("embedded");
        let absorbed = root.join("absorbed");
        std::fs::create_dir_all(embedded.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("modules/absorbed")).unwrap();
        std::fs::create_dir_all(&absorbed).unwrap();
        std::fs::write(absorbed.join(".git"), "gitdir: ../modules/absorbed\n").unwrap();
        let absolute = embedded.canonicalize().unwrap();
        let absolute = absolute.to_str().unwrap();

        assert!(worktree_is_checkout(&embedded, b"filter.x.clean\nc\0"));
        assert!(worktree_is_checkout(&embedded, b"core.worktree\n..\0"));
        assert!(worktree_is_checkout(
            &embedded,
            format!("core.worktree\n{absolute}\0").as_bytes()
        ));
        assert!(worktree_is_checkout(
            &absorbed,
            b"core.worktree\n../../absorbed\0"
        ));
        assert!(!worktree_is_checkout(&embedded, b"core.worktree\n../..\0"));
        assert!(!worktree_is_checkout(&embedded, b"core.worktree\0"));
        assert!(!worktree_is_checkout(
            &embedded,
            b"core.worktree\n..\0core.worktree\n../..\0"
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_filter_driver_is_neutralized() {
        let overrides = filter_overrides_from_config(b"filter..clean\0filter..process\0").unwrap();
        assert_eq!(
            overrides,
            [
                "filter..clean=",
                "filter..smudge=",
                "filter..process=",
                "filter..required=false"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn equals_in_filter_driver_refuses_entire_inventory() {
        assert!(
            filter_overrides_from_config(b"filter.normal.clean\0filter.probe=x.clean\0").is_none()
        );
    }

    #[test]
    fn filter_inventory_of_a_normal_repo_still_completes() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(
            repository_filter_drivers(repo, FILTER_INVENTORY_TIMEOUT).is_some(),
            "a readable repository config is still a complete inventory"
        );
        assert!(
            submodule_filter_drivers(
                repo,
                None,
                FILTER_INVENTORY_TIMEOUT,
                SUBMODULE_INVENTORY_TOTAL_TIMEOUT
            )
            .is_some(),
            "a readable repository and index are still a complete recursive inventory"
        );
    }

    /// The per-read budget bounds each repository; the total budget bounds the whole walk. A
    /// spent total is an incomplete inventory, whatever the per-read budget allows.
    #[test]
    fn spent_total_budget_refuses_the_recursive_inventory() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(
            submodule_filter_drivers(repo, None, FILTER_INVENTORY_TIMEOUT, Duration::ZERO)
                .is_none()
        );
    }

    /// A repo-local `include.path` that never yields bytes used to block `git config` forever,
    /// and every viewer query waits on that inventory first. The budget must win, and the result
    /// must be "unavailable" so the query is refused rather than run with an unknown filter set.
    #[cfg(unix)]
    #[test]
    fn filter_inventory_times_out_instead_of_blocking_on_an_included_fifo() {
        let root = std::env::temp_dir().join(format!(
            "hfv-filter-fifo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .expect("git init");
        assert!(init.success(), "git init");
        let fifo = root.join("block.fifo");
        let made = Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
        assert!(made.success(), "mkfifo");
        let configured = Command::new("git")
            .args(["config", "include.path"])
            .arg(&fifo)
            .current_dir(&root)
            .status()
            .expect("git config");
        assert!(
            configured.success(),
            "writing include.path must not read the included path"
        );

        let started = std::time::Instant::now();
        let budget = Duration::from_millis(200);
        assert!(
            repository_filter_drivers(&root, budget).is_none(),
            "a stalled include is not a complete filter inventory"
        );
        assert!(
            submodule_filter_drivers(&root, None, budget, SUBMODULE_INVENTORY_TOTAL_TIMEOUT)
                .is_none(),
            "the per-read budget, not the much larger total, bounds a stalled include"
        );
        // The included fifo never returns. 2s is 5x the two 200ms reads — enough for a loaded
        // runner, still far below an unbounded wait or the 10s total budget. See AGENTS.md, "Tests prove things
        // deterministically".
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "inventory must return rather than wait on the included fifo: {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gitlink_scan_keeps_only_mode_160000_paths() {
        let listing = b"100644 0123 0\tsrc/main.rs\0\
                        160000 4567 0\tvendor/lib\0\
                        120000 89ab 0\tlink\0\
                        160000 cdef 1\twith space/sub\0\
                        160000 cdef 2\twith space/sub\0\
                        160000 cdef 3\twith space/sub\0";
        assert_eq!(
            parse_gitlinks(&listing[..]),
            Some(vec![
                PathBuf::from("vendor/lib"),
                PathBuf::from("with space/sub")
            ])
        );
        assert_eq!(parse_gitlinks(&b""[..]), Some(Vec::new()));
    }

    /// A gitlink record the scan cannot read is not a complete inventory.
    #[test]
    fn malformed_gitlink_record_is_refused() {
        assert_eq!(parse_gitlinks(&b"160000 4567 0 no-tab\0"[..]), None);
        assert_eq!(parse_gitlinks(&b"160000 4567 0\t\0"[..]), None);
    }

    /// A worker that panics after taking a checkout from the queue never reports it. The others
    /// must not wait on it forever, and the inventory must come back incomplete (a refusal), not
    /// hang the operation or unwind into it.
    #[test]
    fn panicking_inventory_worker_refuses_instead_of_hanging() {
        let root = std::env::temp_dir().join(format!(
            "hfv-worker-panic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let git = |dir: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.email=t@example.com", "-c", "user.name=T"])
                .args(["-c", "protocol.file.allow=always"])
                .args(args)
                .output()
                .expect("git")
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        let source = root.join("source");
        let superproject = root.join("super");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&superproject).unwrap();
        git(&source, &["init", "-q"]);
        std::fs::write(source.join("file.txt"), "hello\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-q", "-m", "init"]);
        git(&superproject, &["init", "-q"]);
        let source = source.to_str().unwrap();
        // Several healthy submodules keep other workers busy, then idle and waiting, while the
        // panicking one is in flight.
        for name in ["a", "b", "c", PANICKING_CHECKOUT_FOR_TESTS] {
            git(&superproject, &["submodule", "add", "-q", source, name]);
        }

        let (sender, receiver) = mpsc::channel();
        let target = superproject.clone();
        std::thread::spawn(move || {
            let _ = sender.send(submodule_filter_drivers(
                &target,
                None,
                FILTER_INVENTORY_TIMEOUT,
                SUBMODULE_INVENTORY_TOTAL_TIMEOUT,
            ));
        });
        // Without the guard the remaining workers wait forever. 30s is 3x the total budget, so
        // even the bounded-wait fallback alone fits; the guard returns at once.
        let got = receiver.recv_timeout(Duration::from_secs(30));
        assert!(
            matches!(got, Ok(None)),
            "a panicked worker is an incomplete inventory, returned promptly: {got:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn invalid_utf8_filter_inventory_is_refused() {
        assert!(filter_overrides_from_config(b"filter.\xff.clean\0").is_none());
    }

    fn sample_filter_overrides() -> Vec<OsString> {
        [
            "filter.hostile.clean=",
            "filter.hostile.smudge=",
            "filter.hostile.process=",
            "filter.hostile.required=false",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    /// The shared hardened builder must apply *every* untrusted-repo guard that does not
    /// depend on git's version. This is the regression guard that keeps the Git Service and
    /// the Root Resolver from silently dropping a protection (AC-N2). `--attr-source` is
    /// version-gated (#160), while executable filters are neutralized on both paths.
    #[test]
    fn git_command_applies_every_untrusted_repo_guard() {
        let overrides = sample_filter_overrides();
        for pin_attr_source in [true, false] {
            let cmd = git_command_with(
                Path::new("/some/repo"),
                &["status"],
                pin_attr_source,
                &overrides,
            );
            assert_version_independent_guards(&cmd);
            let args = command_args(&cmd);
            for filter_override in &overrides {
                assert!(
                    args.contains(&filter_override.to_string_lossy().into_owned()),
                    "filter command/required setting neutralized: {args:?}"
                );
            }
        }
    }

    /// git ≥ 2.40: `--attr-source` is pinned to the empty tree so worktree attributes are
    /// ignored. Filter neutralization remains an independent guard for other attr sources.
    #[test]
    fn git_command_pins_attr_source_when_supported() {
        let cmd = git_command_with(Path::new("/some/repo"), &["status"], true, &[]);
        let args = command_args(&cmd);
        assert!(
            args.iter().any(|a| a.starts_with("--attr-source=")),
            "attr-source pinned to the empty tree: {args:?}"
        );
        assert!(
            args.contains(&format!("--attr-source={EMPTY_TREE}")),
            "attr-source uses the empty-tree object: {args:?}"
        );
    }

    /// git < 2.40 (Apple Git 2.39.x): omit `--attr-source` so queries still succeed. The
    /// other hardenings stay on — this is the #160 path, not a security-off switch.
    #[test]
    fn git_command_omits_attr_source_when_unsupported() {
        let cmd = git_command_with(Path::new("/some/repo"), &["status"], false, &[]);
        let args = command_args(&cmd);
        assert!(
            args.iter().all(|a| !a.starts_with("--attr-source=")),
            "attr-source must be omitted on older git: {args:?}"
        );
        assert_version_independent_guards(&cmd);
    }

    /// The live builder's `--attr-source` decision must match the probe, so a runner's git
    /// version cannot silently desync the two.
    #[test]
    fn live_git_command_matches_attr_source_probe() {
        let cmd = git_command(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            Reach::Submodules,
            &["status"],
        );
        let args = command_args(&cmd);
        let pinned = args.iter().any(|a| a.starts_with("--attr-source="));
        assert_eq!(
            pinned,
            attr_source_supported(),
            "live git_command attr-source pin must match the probe: {args:?}"
        );
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
