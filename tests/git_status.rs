//! T-4 — Git Service: per-file status (AC-7) + read-only guarantee (AC-N2).

mod common;

use common::{git, init_repo_with_commit, TempDir};
use herdr_file_viewer::git::{status, Status};
use std::fs;
use std::path::PathBuf;

#[test]
fn reports_modified_added_deleted_and_untracked() {
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    // Commit two files we will later modify / delete.
    fs::write(repo.path().join("mod.txt"), "one\n").unwrap();
    fs::write(repo.path().join("del.txt"), "bye\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "base"]);

    // Produce all four states.
    fs::write(repo.path().join("mod.txt"), "two\n").unwrap(); // modified
    fs::remove_file(repo.path().join("del.txt")).unwrap(); // deleted
    fs::write(repo.path().join("added.txt"), "n\n").unwrap();
    git(repo.path(), &["add", "added.txt"]); // staged new → added
    fs::write(repo.path().join("untracked.txt"), "u\n").unwrap(); // untracked

    let map = status(repo.path());

    assert_eq!(map.get(&PathBuf::from("mod.txt")), Some(&Status::Modified));
    assert_eq!(map.get(&PathBuf::from("del.txt")), Some(&Status::Deleted));
    assert_eq!(map.get(&PathBuf::from("added.txt")), Some(&Status::Added));
    assert_eq!(
        map.get(&PathBuf::from("untracked.txt")),
        Some(&Status::Untracked)
    );
    // A committed, unchanged file is absent from the status map.
    assert_eq!(map.get(&PathBuf::from("seed.txt")), None);
}

#[test]
fn status_does_not_mutate_the_repo() {
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    fs::write(repo.path().join("seed.txt"), "changed\n").unwrap(); // modified
    fs::write(repo.path().join("new.txt"), "x\n").unwrap(); // untracked

    let before = git(repo.path(), &["status", "--porcelain"]);
    let head_before = git(repo.path(), &["rev-parse", "HEAD"]);

    let _ = status(repo.path());

    let after = git(repo.path(), &["status", "--porcelain"]);
    let head_after = git(repo.path(), &["rev-parse", "HEAD"]);
    assert_eq!(before, after, "AC-N2: working state unchanged after status()");
    assert_eq!(head_before, head_after, "AC-N2: HEAD unchanged after status()");
}

#[test]
fn non_repo_directory_yields_an_empty_status_map() {
    let dir = TempDir::new();
    fs::write(dir.path().join("loose.txt"), "x\n").unwrap();
    assert!(status(dir.path()).is_empty()); // AC-26 degradation
}

#[test]
fn untracked_directory_is_listed_per_file_not_collapsed() {
    // git's default porcelain collapses an untracked dir to `dir/`; -uall must expand it
    // so nested untracked files get status markers and pass the changed-only filter.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    fs::create_dir_all(repo.path().join("newdir/sub")).unwrap();
    fs::write(repo.path().join("newdir/a.txt"), "a\n").unwrap();
    fs::write(repo.path().join("newdir/sub/b.txt"), "b\n").unwrap();

    let map = status(repo.path());
    assert_eq!(
        map.get(&PathBuf::from("newdir/a.txt")),
        Some(&Status::Untracked)
    );
    assert_eq!(
        map.get(&PathBuf::from("newdir/sub/b.txt")),
        Some(&Status::Untracked)
    );
    // The collapsed directory form must NOT be present.
    assert!(map.get(&PathBuf::from("newdir")).is_none());
    assert!(map.get(&PathBuf::from("newdir/")).is_none());
}
