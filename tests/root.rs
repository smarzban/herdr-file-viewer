//! T-3 — Root Resolver: tree root + git-presence detection (AC-1, AC-2, AC-26).

mod common;

use common::{canon, git, init_repo_with_commit, TempDir};
use herdr_file_viewer::context::LaunchContext;
use herdr_file_viewer::root::resolve;
use std::fs;
use std::path::Path;

fn ctx(cwd: &Path) -> LaunchContext {
    LaunchContext {
        cwd: cwd.to_path_buf(),
        ..Default::default()
    }
}

#[test]
fn plain_directory_is_its_own_root_and_not_a_git_repo() {
    let dir = TempDir::new();
    let r = resolve(&ctx(dir.path()));
    assert_eq!(canon(&r.root), canon(dir.path())); // AC-2
    assert!(!r.is_git_repo); // AC-26
    assert!(!r.is_worktree);
    assert!(r.repo_root.is_none());
}

#[test]
fn git_repo_root_is_the_repository_toplevel() {
    let dir = TempDir::new();
    git(dir.path(), &["init", "-q"]);
    // Resolve from a nested subdirectory; root must still be the repo top-level.
    let sub = dir.path().join("nested/inner");
    fs::create_dir_all(&sub).unwrap();
    let r = resolve(&ctx(&sub));
    assert!(r.is_git_repo);
    assert_eq!(canon(&r.root), canon(dir.path()));
    assert!(!r.is_worktree); // the main working tree is not a linked worktree
}

#[test]
fn linked_worktree_root_is_the_worktree_and_is_flagged() {
    let main = TempDir::new();
    init_repo_with_commit(main.path());
    // Create a linked worktree on a new branch (path must not pre-exist).
    let wt = main.path().join("wt");
    git(
        main.path(),
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feature"],
    );
    let r = resolve(&ctx(&wt));
    assert!(r.is_git_repo);
    assert!(r.is_worktree, "a linked worktree must be flagged is_worktree"); // AC-1
    assert_eq!(canon(&r.root), canon(&wt));
}
