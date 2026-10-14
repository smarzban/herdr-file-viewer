//! Root Resolver: tree root + git-presence detection (AC-1, AC-2, AC-26).

mod common;

use common::{TempDir, canon, git, init_repo_with_commit};
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
fn foreign_core_worktree_does_not_become_the_viewer_root() {
    let opened = TempDir::new();
    let outside = TempDir::new();
    git(opened.path(), &["init", "-q"]);
    git(
        opened.path(),
        &["config", "core.worktree", outside.path().to_str().unwrap()],
    );
    // Git itself reports the foreign directory. The viewer must not adopt it.
    let reported = git(opened.path(), &["rev-parse", "--show-toplevel"]);
    assert_eq!(canon(Path::new(reported.trim())), canon(outside.path()));

    let nested = opened.path().join("nested");
    fs::create_dir_all(&nested).unwrap();
    let resolved = resolve(&ctx(&nested));
    assert_eq!(canon(&resolved.root), canon(&nested));
    assert!(!resolved.is_git_repo);
    assert!(resolved.repo_root.is_none());
    assert_ne!(canon(&resolved.root), canon(outside.path()));
}

#[test]
fn core_worktree_that_matches_the_opened_repo_still_resolves() {
    let dir = TempDir::new();
    git(dir.path(), &["init", "-q"]);
    git(
        dir.path(),
        &["config", "core.worktree", dir.path().to_str().unwrap()],
    );
    let sub = dir.path().join("nested/inner");
    fs::create_dir_all(&sub).unwrap();
    let resolved = resolve(&ctx(&sub));
    assert!(resolved.is_git_repo);
    assert_eq!(canon(&resolved.root), canon(dir.path()));
    assert_eq!(
        resolved.repo_root.as_deref().map(canon),
        Some(canon(dir.path()))
    );
}

#[test]
fn linked_worktree_root_is_the_worktree_and_is_flagged() {
    let main = TempDir::new();
    init_repo_with_commit(main.path());
    // Create a linked worktree on a new branch (path must not pre-exist).
    let wt = main.path().join("wt");
    git(
        main.path(),
        &[
            "worktree",
            "add",
            "-q",
            wt.to_str().unwrap(),
            "-b",
            "feature",
        ],
    );
    let r = resolve(&ctx(&wt));
    assert!(r.is_git_repo);
    assert!(
        r.is_worktree,
        "a linked worktree must be flagged is_worktree"
    ); // AC-1
    assert_eq!(canon(&r.root), canon(&wt));
}
