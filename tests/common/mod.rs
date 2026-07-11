//! Shared test helpers. Dependency-free on purpose: the techstack pins only `insta`
//! and `expectrl` as dev-deps, so we roll a tiny temp-dir + git runner over `std`
//! rather than pull in `tempfile`.

#![allow(dead_code)] // not every integration test uses every helper

use herdr_file_viewer::controller::Clipboard;
use herdr_file_viewer::root::Resolved;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, SystemTime};

static COUNTER: AtomicU64 = AtomicU64::new(0);
static SWEEP: Once = Once::new();

/// Prefix every [`TempDir`] path shares, so the leak sweep can recognize its own orphans.
const TEMP_PREFIX: &str = "herdr-fv-test-";

/// Best-effort removal of `herdr-fv-test-*` dirs in `base` whose mtime predates `cutoff`.
///
/// Split out of [`sweep_stale_once`] with an injectable `base` + `cutoff` so it is testable
/// without backdating a directory's mtime. Every step is best-effort: an unreadable base, a
/// racing sweeper from a concurrent run, or a dir that vanishes mid-loop is simply skipped.
pub fn sweep_stale_in(base: &Path, cutoff: SystemTime) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(TEMP_PREFIX) {
            continue;
        }
        // Only sweep dirs old enough that no live run (including a concurrent one) could own
        // them; a dir whose mtime we can't read is left alone rather than risk a live removal.
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|mtime| mtime < cutoff)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// One-time, best-effort sweep of temp dirs leaked by previous, *killed* test runs.
///
/// [`TempDir`] removes itself on `Drop`, but `Drop` never runs when a test process is killed
/// (a timeout, `SIGKILL`, Ctrl-C, `panic = "abort"`): those orphans then pile up in the system
/// temp dir and can exhaust a small `/tmp` tmpfs. Reclaim them once per process, at first
/// `TempDir::new()`, taking only entries older than an hour so no live run's dirs are touched.
fn sweep_stale_once() {
    SWEEP.call_once(|| {
        if let Some(cutoff) = SystemTime::now().checked_sub(Duration::from_secs(3600)) {
            sweep_stale_in(&std::env::temp_dir(), cutoff);
        }
    });
}

/// A unique temporary directory removed on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> Self {
        sweep_stale_once();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "{TEMP_PREFIX}{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A Clipboard stub that records every copied string instead of touching a real clipboard,
/// so a test can assert what the `y`/`Y` keys would have copied. The `copied` log is shared
/// (`Arc<Mutex<_>>`) so the test keeps a handle to read back after handing the stub to the
/// controller.
#[derive(Default, Clone)]
pub struct RecordingClipboard {
    pub copied: Arc<Mutex<Vec<String>>>,
}

impl Clipboard for RecordingClipboard {
    fn copy(&mut self, text: &str) -> std::io::Result<()> {
        self.copied.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

/// Build a [`Resolved`] for tests: `repo_root` mirrors `root` when it is a git repo (the tests
/// never exercise a *linked* worktree, so `is_worktree` is false and there is no separate
/// top-level). This is the value `Controller::new` now consumes (ADR-0004), and the factory
/// reads it to build the root-bound providers.
pub fn resolved(root: PathBuf, is_git_repo: bool) -> Resolved {
    Resolved {
        repo_root: is_git_repo.then(|| root.clone()),
        root,
        is_git_repo,
        is_worktree: false,
        base_branch: None,
    }
}

/// Run a read/setup `git` command in `dir`, asserting success; returns trimmed stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("failed to run git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `git init` a repo with a deterministic identity and an initial commit.
pub fn init_repo_with_commit(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("seed.txt"), "seed\n").expect("write seed");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

/// Canonicalize a path for symlink-stable comparisons (e.g. /tmp on macOS).
pub fn canon(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    // On Windows, `fs::canonicalize` returns an extended-length `\\?\` *verbatim* path, whose
    // `Prefix` (VerbatimDisk) does NOT compare equal to the ordinary `Disk` prefix on the
    // non-canonicalized paths the tree builds from a raw root — so `node.path.starts_with(canon)`
    // (and `== canon`) would spuriously fail. Strip the `\\?\` prefix so canon'd paths line up with
    // the tree's. No-op on unix and on already-non-verbatim paths.
    #[cfg(windows)]
    if let Some(rest) = c.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        return PathBuf::from(rest);
    }
    c
}

/// A `Command` that runs the built viewer binary with its cwd set to `dir`. The e2e tests
/// wrap this in an `expectrl` pty session; callers add env (e.g. `EDITOR`) before spawning.
pub fn viewer_command(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_herdr-file-viewer"));
    cmd.current_dir(dir);
    // Tests must never reach the network: disable the once-a-day update check in every spawned
    // viewer (it would otherwise run `git ls-remote` against the real repo).
    cmd.env("HERDR_FILE_VIEWER_NO_UPDATE_CHECK", "1");
    cmd
}
