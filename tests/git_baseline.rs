//! T-5 — Git Service: baseline resolution, changed-set, diff (AC-9, AC-14, AC-15, AC-16)
//! plus the read-only guarantee across all query methods (AC-N2).

mod common;

use common::{git, TempDir};
use herdr_file_viewer::git::{changed_set, default_baseline, diff, Baseline, Status};
use herdr_file_viewer::root::Resolved;
use std::fs;
use std::path::{Path, PathBuf};

fn make_repo() -> TempDir {
    let repo = TempDir::new();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(repo.path(), &["config", "user.email", "t@example.com"]);
    git(repo.path(), &["config", "user.name", "T"]);
    fs::write(repo.path().join("seed.txt"), "1\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    repo
}

fn resolved(repo: &Path) -> Resolved {
    Resolved {
        root: repo.to_path_buf(),
        is_git_repo: true,
        repo_root: Some(repo.to_path_buf()),
        is_worktree: false,
        base_branch: None,
    }
}

#[test]
fn default_branch_uses_head_baseline_and_uncommitted_changed_set() {
    let repo = make_repo();
    fs::write(repo.path().join("seed.txt"), "2\n").unwrap(); // uncommitted edit

    assert_eq!(default_baseline(&resolved(repo.path())), Baseline::Head); // AC-15

    let set = changed_set(repo.path(), Baseline::Head, None);
    assert_eq!(set.get(&PathBuf::from("seed.txt")), Some(&Status::Modified));
}

#[test]
fn feature_branch_uses_base_baseline_including_committed_and_uncommitted_work() {
    let repo = make_repo();
    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    fs::write(repo.path().join("feat.txt"), "new\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "feature work"]);
    // An uncommitted edit on top of the committed work.
    fs::write(repo.path().join("seed.txt"), "1\nuncommitted\n").unwrap();

    assert_eq!(default_baseline(&resolved(repo.path())), Baseline::Base); // AC-14

    let base_set = changed_set(repo.path(), Baseline::Base, None);
    assert!(
        base_set.contains_key(&PathBuf::from("feat.txt")),
        "committed feature work must appear in the base changed-set"
    );
    assert!(
        base_set.contains_key(&PathBuf::from("seed.txt")),
        "uncommitted tracked changes must also appear against the base baseline"
    );
    // The committed file is clean vs HEAD, so toggling baseline changes the set (AC-16).
    let head_set = changed_set(repo.path(), Baseline::Head, None);
    assert!(
        !head_set.contains_key(&PathBuf::from("feat.txt")),
        "committed file is clean vs HEAD"
    );
}

#[test]
fn diff_returns_unified_text_and_varies_with_baseline() {
    let repo = make_repo();
    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    fs::write(repo.path().join("seed.txt"), "1\n2\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "extend seed"]);

    let base_diff = diff(repo.path(), Path::new("seed.txt"), Baseline::Base, None);
    assert!(base_diff.contains("@@"), "base diff is a unified diff (AC-9)");
    assert!(base_diff.contains("+2"), "base diff shows the committed addition");

    // No uncommitted change → empty HEAD diff; the two baselines differ (AC-16).
    let head_diff = diff(repo.path(), Path::new("seed.txt"), Baseline::Head, None);
    assert!(!head_diff.contains("@@"), "HEAD diff is empty (no uncommitted change)");
    assert_ne!(base_diff, head_diff);
}

#[test]
fn base_branch_hint_drives_base_queries_for_non_main_master_repos() {
    // A repo whose base branch is "trunk" (no main/master), exactly the case the
    // herdr-supplied hint exists for (AC-14).
    let repo = TempDir::new();
    git(repo.path(), &["init", "-q", "-b", "trunk"]);
    git(repo.path(), &["config", "user.email", "t@example.com"]);
    git(repo.path(), &["config", "user.name", "T"]);
    fs::write(repo.path().join("seed.txt"), "1\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    fs::write(repo.path().join("feat.txt"), "x\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "feature"]);

    let mut r = resolved(repo.path());
    r.base_branch = Some("trunk".to_string());
    assert_eq!(default_baseline(&r), Baseline::Base);

    // With the hint, committed feature work is in the base set...
    let hinted = changed_set(repo.path(), Baseline::Base, Some("trunk"));
    assert!(hinted.contains_key(&PathBuf::from("feat.txt")));
    assert!(diff(repo.path(), Path::new("feat.txt"), Baseline::Base, Some("trunk")).contains("@@"));

    // ...without it (and with no main/master fallback), the Base query degrades to a
    // HEAD comparison, where the committed file is clean — proving the hint is honored.
    let unhinted = changed_set(repo.path(), Baseline::Base, None);
    assert!(!unhinted.contains_key(&PathBuf::from("feat.txt")));
}

#[test]
fn untracked_file_diff_shows_added_content() {
    // AC-9: an untracked (hence "changed") file must show a diff, not an empty pane.
    let repo = make_repo();
    fs::write(repo.path().join("brand_new.txt"), "hello\nworld\n").unwrap();

    let d = diff(repo.path(), Path::new("brand_new.txt"), Baseline::Head, None);
    assert!(d.contains("+hello"), "untracked file diff shows its content as added");
    assert!(d.contains("+world"));
}

#[test]
fn base_queries_on_non_repo_degrade_to_empty() {
    let dir = TempDir::new();
    fs::write(dir.path().join("loose.txt"), "x\n").unwrap();
    assert!(changed_set(dir.path(), Baseline::Base, None).is_empty()); // AC-26
    assert!(changed_set(dir.path(), Baseline::Head, None).is_empty());
}

#[test]
fn baseline_queries_do_not_mutate_the_repo() {
    let repo = make_repo();
    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    fs::write(repo.path().join("feat.txt"), "x\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "w"]);
    fs::write(repo.path().join("seed.txt"), "z\n").unwrap();

    let before = git(repo.path(), &["status", "--porcelain"]);
    let head_before = git(repo.path(), &["rev-parse", "HEAD"]);

    let _ = default_baseline(&resolved(repo.path()));
    let _ = changed_set(repo.path(), Baseline::Base, None);
    let _ = changed_set(repo.path(), Baseline::Head, None);
    let _ = diff(repo.path(), Path::new("seed.txt"), Baseline::Base, None);
    let _ = diff(repo.path(), Path::new("feat.txt"), Baseline::Head, None);

    assert_eq!(before, git(repo.path(), &["status", "--porcelain"]), "AC-N2");
    assert_eq!(head_before, git(repo.path(), &["rev-parse", "HEAD"]), "AC-N2");
}
