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

// Win32 strips trailing spaces from directory names, so the `repo ` fixture cannot exist on the
// Windows job.
#[cfg(unix)]
#[test]
fn core_worktree_differing_only_by_trailing_space_is_refused() {
    let parent = TempDir::new();
    let opened = parent.path().join("repo");
    let foreign = parent.path().join("repo ");
    fs::create_dir_all(&opened).unwrap();
    fs::create_dir_all(&foreign).unwrap();
    fs::write(opened.join("opened.txt"), "opened").unwrap();
    fs::write(foreign.join("foreign.txt"), "foreign").unwrap();
    git(&opened, &["init", "-q"]);
    git(
        &opened,
        &["config", "core.worktree", foreign.to_str().unwrap()],
    );
    // Git itself follows the trailing-space sibling; trimming its output would hide that (the
    // `git` helper trims, so read the raw stdout here).
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&opened)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .unwrap();
    let reported = String::from_utf8_lossy(&out.stdout);
    assert!(reported.ends_with("repo \n"), "{reported:?}");

    let resolved = resolve(&ctx(&opened));
    assert_eq!(canon(&resolved.root), canon(&opened));
    assert!(!resolved.is_git_repo);
    assert!(resolved.repo_root.is_none());
}

#[test]
fn core_worktree_naming_an_ancestor_is_refused() {
    let outer = TempDir::new();
    let repo = outer.path().join("repo");
    let nested = repo.join("nested");
    fs::create_dir_all(&nested).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &["config", "core.worktree", outer.path().to_str().unwrap()],
    );
    let reported = git(&repo, &["rev-parse", "--show-toplevel"]);
    assert_eq!(canon(Path::new(reported.trim())), canon(outer.path()));

    let resolved = resolve(&ctx(&nested));
    assert_eq!(canon(&resolved.root), canon(&nested));
    assert!(!resolved.is_git_repo);
    assert!(resolved.repo_root.is_none());
}

#[test]
fn invalid_git_dir_between_cwd_and_the_repo_does_not_hide_the_repo() {
    let dir = TempDir::new();
    git(dir.path(), &["init", "-q"]);
    // An empty `.git` directory is not a repository; git walks past it, and so must we.
    fs::create_dir_all(dir.path().join("nested/.git")).unwrap();
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
fn submodule_resolves_to_the_submodule_checkout() {
    let lib = TempDir::new();
    init_repo_with_commit(lib.path());
    let sup = TempDir::new();
    init_repo_with_commit(sup.path());
    // A submodule's git dir lives in the superproject and carries its own `core.worktree`
    // pointing back at the checkout, which is a legitimate match.
    git(
        sup.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            lib.path().to_str().unwrap(),
            "sub",
        ],
    );
    let checkout = sup.path().join("sub");
    let inner = checkout.join("inner");
    fs::create_dir_all(&inner).unwrap();
    let resolved = resolve(&ctx(&inner));
    assert!(resolved.is_git_repo);
    assert_eq!(canon(&resolved.root), canon(&checkout));
    assert_eq!(
        resolved.repo_root.as_deref().map(canon),
        Some(canon(&checkout))
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
