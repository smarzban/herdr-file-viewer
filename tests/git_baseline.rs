//! T-5 — Git Service: baseline resolution, changed-set, diff (AC-9, AC-14, AC-15, AC-16)
//! plus the read-only guarantee across all query methods (AC-N2).

mod common;

use common::{git, TempDir};
use herdr_file_viewer::git::{changed_set, default_baseline, diff, status, Baseline, Status};
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

    let base_diff = diff(repo.path(), Path::new("seed.txt"), Baseline::Base, None, false);
    assert!(base_diff.contains("@@"), "base diff is a unified diff (AC-9)");
    assert!(base_diff.contains("+2"), "base diff shows the committed addition");

    // No uncommitted change → empty HEAD diff; the two baselines differ (AC-16).
    let head_diff = diff(repo.path(), Path::new("seed.txt"), Baseline::Head, None, false);
    assert!(!head_diff.contains("@@"), "HEAD diff is empty (no uncommitted change)");
    assert_ne!(base_diff, head_diff);
}

#[test]
fn full_context_diff_includes_whole_file_context_the_compact_diff_omits() {
    // PR2 / AC-9: full_context=true asks git for whole-file context, so lines far from the
    // change (outside the default 3-line hunk window) are present — the compact diff omits them.
    let repo = make_repo();
    let mut lines: Vec<String> = Vec::new();
    lines.push("TOP_MARKER".into());
    for n in 1..=20 {
        lines.push(format!("body{n}"));
    }
    lines.push("BOTTOM_MARKER".into());
    let write = |ls: &[String]| fs::write(repo.path().join("big.txt"), format!("{}\n", ls.join("\n"))).unwrap();
    write(&lines);
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "add big"]);
    // Change one middle line; the markers are far from it (> 3 lines away).
    lines[10] = "CHANGED".into();
    write(&lines);

    let compact = diff(repo.path(), Path::new("big.txt"), Baseline::Head, None, false);
    let full = diff(repo.path(), Path::new("big.txt"), Baseline::Head, None, true);

    assert!(compact.contains("CHANGED") && full.contains("CHANGED"), "both show the change");
    assert!(
        !compact.contains("TOP_MARKER") && !compact.contains("BOTTOM_MARKER"),
        "the compact (3-line) hunk omits distant context:\n{compact}"
    );
    assert!(
        full.contains("TOP_MARKER") && full.contains("BOTTOM_MARKER"),
        "the full-context diff carries the whole file as context:\n{full}"
    );
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
    assert!(diff(repo.path(), Path::new("feat.txt"), Baseline::Base, Some("trunk"), false).contains("@@"));

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

    let d = diff(repo.path(), Path::new("brand_new.txt"), Baseline::Head, None, false);
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
fn remote_tracking_base_branch_is_used_when_no_local_base_exists() {
    // A cloned/worktree repo whose base exists only as origin/main, no local main.
    let repo = make_repo(); // on "main" with seed committed
    let main_sha = git(repo.path(), &["rev-parse", "HEAD"]);
    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    fs::write(repo.path().join("feat.txt"), "x\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "feature"]);
    git(repo.path(), &["update-ref", "refs/remotes/origin/main", &main_sha]);
    git(repo.path(), &["branch", "-D", "main"]); // base now only remote-tracking

    assert_eq!(default_baseline(&resolved(repo.path())), Baseline::Base); // AC-14
    let set = changed_set(repo.path(), Baseline::Base, None);
    assert!(set.contains_key(&PathBuf::from("feat.txt")));
}

#[test]
fn option_like_base_branch_hint_is_rejected_not_injected() {
    let repo = make_repo(); // local "main" exists as fallback
    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    fs::write(repo.path().join("feat.txt"), "x\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "feature"]);

    // A hint shaped like a git flag must be ignored (no option injection); resolution
    // falls back to the local base, so committed work is still found.
    let set = changed_set(repo.path(), Baseline::Base, Some("--output=/tmp/pwned"));
    assert!(set.contains_key(&PathBuf::from("feat.txt")));
}

#[test]
fn detached_worktree_defaults_to_base() {
    // A managed worktree checked out in detached HEAD still reviews its body of work vs
    // the base (AC-14), even though there is no current branch name.
    let main = make_repo(); // on "main"
    let wt = main.path().join("wt");
    git(main.path(), &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feature"]);
    git(&wt, &["checkout", "-q", "--detach"]); // detached HEAD inside the worktree

    let mut r = resolved(&wt);
    r.is_worktree = true;
    r.base_branch = Some("main".to_string());
    assert_eq!(default_baseline(&r), Baseline::Base);
}

#[test]
fn non_ascii_filename_round_trips_through_status_and_diff() {
    let repo = make_repo();
    fs::write(repo.path().join("résumé.txt"), "café\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "add"]);
    fs::write(repo.path().join("résumé.txt"), "café\nnoël\n").unwrap();

    let set = changed_set(repo.path(), Baseline::Head, None);
    assert_eq!(
        set.get(&PathBuf::from("résumé.txt")),
        Some(&Status::Modified)
    );
    let d = diff(repo.path(), Path::new("résumé.txt"), Baseline::Head, None, false);
    assert!(d.contains("+noël"), "diff of a non-ASCII path is not empty");
}

#[test]
fn staged_file_in_unborn_repo_diffs_as_added() {
    // No commits yet → HEAD doesn't resolve; a staged file must still diff as added.
    let repo = TempDir::new();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(repo.path(), &["config", "user.email", "t@example.com"]);
    git(repo.path(), &["config", "user.name", "T"]);
    fs::write(repo.path().join("first.txt"), "hello\n").unwrap();
    git(repo.path(), &["add", "first.txt"]);

    let d = diff(repo.path(), Path::new("first.txt"), Baseline::Head, None, false);
    assert!(d.contains("+hello"), "unborn-repo staged file shows an added diff (AC-9)");
}

#[test]
fn diff_refuses_paths_outside_the_root() {
    // No arbitrary file reads; the viewer never resolves above its root (AC-N5).
    let repo = make_repo();
    assert!(diff(repo.path(), Path::new("/etc/hostname"), Baseline::Head, None, false).is_empty());
    assert!(diff(repo.path(), Path::new("../../etc/hostname"), Baseline::Head, None, false).is_empty());
}

#[test]
fn malicious_repo_config_is_not_executed_during_queries() {
    // A planted .git/config must not run programs via fsmonitor/textconv when the viewer
    // opens an untrusted repo.
    let repo = make_repo();
    let marker = repo.path().join("PWNED");
    let script = repo.path().join("payload.sh");
    fs::write(&script, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let s = script.to_str().unwrap();
    git(repo.path(), &["config", "core.fsmonitor", s]);
    // Cover every attribute/config-driven exec vector: textconv, ext-diff, clean/smudge.
    fs::write(repo.path().join(".gitattributes"), "* diff=pwn filter=pwn\n").unwrap();
    git(repo.path(), &["config", "diff.pwn.textconv", s]);
    git(repo.path(), &["config", "filter.pwn.clean", s]);
    git(repo.path(), &["config", "filter.pwn.smudge", s]);
    fs::write(repo.path().join("seed.txt"), "changed\n").unwrap();

    let _ = status(repo.path());
    let _ = changed_set(repo.path(), Baseline::Head, None);
    let _ = diff(repo.path(), Path::new("seed.txt"), Baseline::Head, None, false);

    assert!(
        !marker.exists(),
        "repo-configured fsmonitor/textconv/clean/smudge must not execute"
    );
}

#[test]
fn diff_refuses_symlink_escaping_the_root() {
    // A symlinked intermediate directory must not let a path resolve outside the root.
    let repo = make_repo();
    let outside = TempDir::new();
    fs::write(outside.path().join("secret.txt"), "TOPSECRET\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), repo.path().join("escape")).unwrap();

    let d = diff(repo.path(), Path::new("escape/secret.txt"), Baseline::Head, None, false);
    assert!(d.is_empty(), "must not read files via a symlink escaping the root (AC-N5)");
    assert!(!d.contains("TOPSECRET"));
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
    let _ = diff(repo.path(), Path::new("seed.txt"), Baseline::Base, None, false);
    let _ = diff(repo.path(), Path::new("feat.txt"), Baseline::Head, None, false);

    assert_eq!(before, git(repo.path(), &["status", "--porcelain"]), "AC-N2");
    assert_eq!(head_before, git(repo.path(), &["rev-parse", "HEAD"]), "AC-N2");
}
