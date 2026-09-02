//! #160: a git that rejects `--attr-source` (Apple Git 2.39.x) must still activate
//! git awareness. Own integration binary so the process-wide probe cache starts unset
//! and sees the wrapper before any other test in this crate can pin it.
//!
//! Unix-only: the wrapper is a shell script prepended to `PATH`. Windows CI uses a
//! current Git for Windows (2.40+), so this Apple-git path is not the failure mode there.

#![cfg(unix)]

mod common;

use common::{TempDir, canon, git, init_repo_with_commit};
use herdr_file_viewer::context::LaunchContext;
use herdr_file_viewer::git::{Status, current_branch, status};
use herdr_file_viewer::root::resolve;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate the real `git` *before* we prepend a wrapper to `PATH`.
fn real_git_path() -> PathBuf {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("resolve git on PATH");
    assert!(
        out.status.success(),
        "git must be on PATH: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

/// A `git` that fails if `--attr-source` is present (Apple Git 2.39 behaviour) and
/// otherwise execs the real binary. The probe (`git --attr-source=… --version`) hits
/// this first, so the viewer omits the flag and the forwarded commands succeed.
fn install_attr_source_rejecting_wrapper(dir: &Path, real_git: &Path) {
    let wrapper = dir.join("git");
    let script = format!(
        "#!/bin/sh\n\
         for arg in \"$@\"; do\n\
         case \"$arg\" in\n\
         --attr-source=*) exit 129 ;;\n\
         esac\n\
         done\n\
         exec {} \"$@\"\n",
        real_git.display()
    );
    fs::write(&wrapper, script).unwrap();
    let mut perms = fs::metadata(&wrapper).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&wrapper, perms).unwrap();
}

/// Restores `PATH` on drop so a panic mid-test cannot leak the wrapper into later
/// commands in this process (this binary has only one test, but Drop is the honest
/// cleanup).
struct RestorePath(OsString);

impl Drop for RestorePath {
    fn drop(&mut self) {
        // SAFETY: same process-wide PATH mutation as the install below; Drop runs
        // once, on this thread, after the viewer calls have finished.
        unsafe { std::env::set_var("PATH", &self.0) };
    }
}

#[test]
fn apple_git_style_unknown_attr_source_still_activates_git_awareness() {
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    fs::write(repo.path().join("f.py"), "a\n").unwrap();
    git(repo.path(), &["add", "f.py"]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    fs::write(repo.path().join("f.py"), "a\nb\n").unwrap();

    let real_git = real_git_path();
    let bin = TempDir::new();
    install_attr_source_rejecting_wrapper(bin.path(), &real_git);

    // Sanity: the wrapper itself rejects the flag the way Apple Git 2.39 does.
    let probe = Command::new(bin.path().join("git"))
        .args([
            "--attr-source=4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "--version",
        ])
        .output()
        .expect("run wrapper probe");
    assert!(!probe.status.success(), "wrapper must reject --attr-source");

    let orig_path = std::env::var_os("PATH").expect("PATH is set");
    let mut prefixed = std::env::split_paths(&orig_path).collect::<Vec<_>>();
    prefixed.insert(0, bin.path().to_path_buf());
    let new_path = std::env::join_paths(prefixed).expect("join PATH");
    // SAFETY: this integration binary has one test that mutates PATH. `RestorePath`
    // puts the original back on drop. `Command::new("git")` in the viewer reads PATH
    // at spawn time, so the probe and every later query see the wrapper.
    unsafe { std::env::set_var("PATH", &new_path) };
    let _restore = RestorePath(orig_path);

    let resolved = resolve(&LaunchContext {
        cwd: repo.path().to_path_buf(),
        ..Default::default()
    });
    let map = status(repo.path());
    let branch = current_branch(repo.path());

    assert!(
        resolved.is_git_repo,
        "#160: a git that rejects --attr-source must still be detected as a repo"
    );
    assert_eq!(
        resolved.repo_root.as_ref().map(|p| canon(p)),
        Some(canon(repo.path()))
    );
    assert_eq!(
        map.get(&PathBuf::from("f.py")),
        Some(&Status::Modified),
        "#160: status markers must populate"
    );
    assert!(
        branch.is_some(),
        "#160: current branch must resolve for the tree border"
    );
}
