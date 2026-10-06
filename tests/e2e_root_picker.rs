//! e2e (pty): the `--pick-root` popup and the `HERDR_FILE_VIEWER_ROOT` hand-off it drives.
//!
//! Unix-only: see `tests/cli_smoke.rs` for why this `expectrl`-pty e2e suite is not ported to
//! Windows's `conpty` backend (and the picker itself ships unix-only for now).
#![cfg(unix)]

mod common;

use common::{TempDir, canon, process_cwd, viewer_command};
use expectrl::process::unix::WaitStatus;
use expectrl::session::OsSession;
use expectrl::{Eof, Expect, Session};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A fake `herdr` that writes its argv, one per line, to `out`. The picker calls it exactly once
/// (synchronously, before exiting), so reading `out` after EOF needs no wait.
fn fake_herdr(dir: &Path, out: &Path) -> PathBuf {
    let bin = dir.join("fake-herdr");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", out.display()),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Spawn the picker. `config_dir` isolates the plugin config (the split direction comes from its
/// `open_direction`); `placement` is what the launcher would pass as HERDR_FILE_VIEWER_PICK_PLACEMENT.
fn picker_session(
    home: &Path,
    herdr: &Path,
    config_dir: &Path,
    placement: Option<&str>,
) -> OsSession {
    let mut cmd = viewer_command(home);
    cmd.arg("--pick-root")
        .env("HOME", home)
        .env("HERDR_BIN_PATH", herdr)
        .env("HERDR_PLUGIN_CONFIG_DIR", config_dir)
        .env_remove("HERDR_FILE_VIEWER_PICK_PLACEMENT");
    if let Some(p) = placement {
        cmd.env("HERDR_FILE_VIEWER_PICK_PLACEMENT", p);
    }
    let mut s = Session::spawn(cmd).expect("spawn the picker in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));
    // ratatui writes only changed cells and skips blank ones with cursor moves, so expectations
    // match single words / the newly written tail, never a phrase spanning a space.
    s.expect("viewer").expect("the picker draws its prompt");
    s
}

fn assert_clean_exit(s: &mut OsSession) {
    s.expect(Eof).expect("the picker exits (closing the popup)");
    match s.get_process().wait().expect("reap") {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0),
        other => panic!("expected clean exit, got {other:?}"),
    }
}

#[test]
fn tab_completes_and_enter_opens_a_viewer_tab_rooted_there() {
    let home = TempDir::new();
    let tools = TempDir::new();
    std::fs::create_dir_all(home.path().join("project-alpha")).unwrap();
    let out = tools.path().join("argv.txt");
    let herdr = fake_herdr(tools.path(), &out);

    let mut s = picker_session(home.path(), &herdr, tools.path(), Some("tab"));
    s.send("proj\t").unwrap();
    s.expect("alpha/")
        .expect("Tab completes the unique directory");
    s.send("\r").unwrap();
    assert_clean_exit(&mut s);

    let argv = std::fs::read_to_string(&out).expect("herdr was called");
    let root = canon(&home.path().join("project-alpha"));
    let expected = format!(
        "plugin\npane\nopen\n--plugin\nherdr-file-viewer\n--entrypoint\nfile-viewer\n\
         --placement\ntab\n--focus\n--env\nHERDR_FILE_VIEWER_ROOT={}\n",
        root.display()
    );
    assert_eq!(argv, expected);
}

#[test]
fn default_placement_opens_a_split_in_the_configured_direction() {
    let home = TempDir::new();
    let tools = TempDir::new();
    std::fs::create_dir_all(home.path().join("beta")).unwrap();
    std::fs::write(
        tools.path().join("config.toml"),
        "open_direction = \"down\"\n",
    )
    .unwrap();
    let out = tools.path().join("argv.txt");
    let herdr = fake_herdr(tools.path(), &out);

    // No placement env (the `split` launcher arg's default path): a split, `down` per config.
    let mut s = picker_session(home.path(), &herdr, tools.path(), None);
    s.send("beta\r").unwrap();
    assert_clean_exit(&mut s);

    let argv = std::fs::read_to_string(&out).expect("herdr was called");
    let expected = format!(
        "plugin\npane\nopen\n--plugin\nherdr-file-viewer\n--entrypoint\nfile-viewer\n\
         --placement\nsplit\n--direction\ndown\n--focus\n--env\nHERDR_FILE_VIEWER_ROOT={}\n",
        canon(&home.path().join("beta")).display()
    );
    assert_eq!(argv, expected);
}

#[test]
fn esc_cancels_without_calling_herdr() {
    let home = TempDir::new();
    let tools = TempDir::new();
    let out = tools.path().join("argv.txt");
    let herdr = fake_herdr(tools.path(), &out);

    let mut s = picker_session(home.path(), &herdr, tools.path(), None);
    s.send("\x1b").unwrap();
    assert_clean_exit(&mut s);
    assert!(!out.exists(), "Esc must not open anything");
}

#[test]
fn a_missing_directory_is_reported_and_the_popup_stays_open() {
    let home = TempDir::new();
    let tools = TempDir::new();
    let out = tools.path().join("argv.txt");
    let herdr = fake_herdr(tools.path(), &out);

    let mut s = picker_session(home.path(), &herdr, tools.path(), None);
    s.send("nope\r").unwrap();
    s.expect("found").expect("the error is shown in the popup");
    // Ctrl-C (not just Esc) cancels too.
    s.send("\x03").unwrap();
    assert_clean_exit(&mut s);
    assert!(!out.exists(), "nothing opens for a missing directory");
}

#[test]
fn picking_a_file_roots_at_its_directory_and_opens_it() {
    let home = TempDir::new();
    let tools = TempDir::new();
    std::fs::create_dir_all(home.path().join("cfg")).unwrap();
    std::fs::write(home.path().join("cfg/starter-config.toml"), "x = 1\n").unwrap();
    let out = tools.path().join("argv.txt");
    let herdr = fake_herdr(tools.path(), &out);

    let mut s = picker_session(home.path(), &herdr, tools.path(), Some("tab"));
    // Tab completes the file name (no trailing slash), Enter opens it.
    s.send("cfg/sta\t").unwrap();
    s.expect("config.toml").expect("Tab completes the file");
    s.send("\r").unwrap();
    assert_clean_exit(&mut s);

    let argv = std::fs::read_to_string(&out).expect("herdr was called");
    let dir = canon(&home.path().join("cfg"));
    assert!(
        argv.ends_with(&format!(
            "--env\nHERDR_FILE_VIEWER_ROOT={}\n--env\nHERDR_FILE_VIEWER_OPEN={}\n",
            dir.display(),
            dir.join("starter-config.toml").display()
        )),
        "{argv}"
    );
}

#[test]
fn viewer_opens_an_absolute_file_handed_over_with_its_root() {
    // The picker passes the file as an absolute, canonical HERDR_FILE_VIEWER_OPEN under the root.
    let launched_from = TempDir::new();
    let chosen = TempDir::new();
    std::fs::write(chosen.path().join("pick.txt"), "PICKED_FILE_MARKER\n").unwrap();
    let root = canon(chosen.path());

    let mut cmd = viewer_command(launched_from.path());
    cmd.env("HERDR_FILE_VIEWER_ROOT", &root)
        .env("HERDR_FILE_VIEWER_OPEN", root.join("pick.txt"));
    let mut s = Session::spawn(cmd).expect("spawn the viewer");
    s.set_expect_timeout(Some(Duration::from_secs(15)));
    s.expect("PICKED_FILE_MARKER")
        .expect("the handed-over file is open in the content pane");
    s.send("q").unwrap();
    s.expect(Eof).expect("viewer exits");
}

#[test]
fn a_picked_file_in_a_repo_subdirectory_opens_under_the_worktree_root() {
    // The root widens to the worktree top level; the canonical file path must still sit under it
    // (macOS temp dirs are behind the /var -> /private/var symlink, so this checks canonical forms).
    let launched_from = TempDir::new();
    let repo = TempDir::new();
    common::init_repo_with_commit(repo.path());
    std::fs::create_dir_all(repo.path().join("sub")).unwrap();
    std::fs::write(repo.path().join("sub/deep.txt"), "DEEP_FILE_MARKER\n").unwrap();
    let sub = canon(&repo.path().join("sub"));

    let mut cmd = viewer_command(launched_from.path());
    cmd.env("HERDR_FILE_VIEWER_ROOT", &sub)
        .env("HERDR_FILE_VIEWER_OPEN", sub.join("deep.txt"));
    let mut s = Session::spawn(cmd).expect("spawn the viewer");
    s.set_expect_timeout(Some(Duration::from_secs(15)));
    s.expect("DEEP_FILE_MARKER")
        .expect("the file opens even though the root widened to the repo");
    s.send("q").unwrap();
    s.expect(Eof).expect("viewer exits");
}

#[test]
fn renderers_run_from_the_launch_dir_not_the_followed_root() {
    // The viewer's cwd follows the (possibly untrusted) root, but external tools must not:
    // a renderer configured as `ls` lists the directory it runs in, so the launch dir's marker
    // in the content pane proves it ran there, not in the root.
    let launched_from = TempDir::new();
    let root = TempDir::new();
    let config = TempDir::new();
    std::fs::write(launched_from.path().join("LAUNCH_SIDE"), "").unwrap();
    std::fs::write(root.path().join("view.txt"), "x\n").unwrap();
    std::fs::write(config.path().join("config.toml"), "syntax = \"ls\"\n").unwrap();

    let mut cmd = viewer_command(launched_from.path());
    cmd.env("HERDR_FILE_VIEWER_ROOT", canon(root.path()))
        .env("HERDR_FILE_VIEWER_OPEN", "view.txt")
        .env("HERDR_PLUGIN_CONFIG_DIR", config.path());
    let mut s = Session::spawn(cmd).expect("spawn the viewer");
    s.set_expect_timeout(Some(Duration::from_secs(15)));
    s.expect("LAUNCH_SIDE")
        .expect("the renderer ran in the launch directory");
    s.send("q").unwrap();
    s.expect(Eof).expect("viewer exits");
}

#[test]
fn a_file_named_like_a_line_reference_opens_literally() {
    // `notes:12` is a real filename here; it must not be read as `notes` at line 12.
    let launched_from = TempDir::new();
    let root = TempDir::new();
    std::fs::write(root.path().join("notes:12"), "LITERAL_NAME_MARKER\n").unwrap();
    let root = canon(root.path());

    let mut cmd = viewer_command(launched_from.path());
    cmd.env("HERDR_FILE_VIEWER_ROOT", &root)
        .env("HERDR_FILE_VIEWER_OPEN", root.join("notes:12"));
    let mut s = Session::spawn(cmd).expect("spawn the viewer");
    s.set_expect_timeout(Some(Duration::from_secs(15)));
    s.expect("LITERAL_NAME_MARKER")
        .expect("the file named notes:12 is open");
    s.send("q").unwrap();
    s.expect(Eof).expect("viewer exits");
}

#[test]
fn viewer_roots_at_the_handed_over_directory_not_its_cwd() {
    // The viewer herdr launches for the picker starts in the plugin dir with the invoking pane's
    // context; HERDR_FILE_VIEWER_ROOT must win so the tree shows the chosen directory.
    let launched_from = TempDir::new();
    let chosen = TempDir::new();
    std::fs::write(launched_from.path().join("WRONG.txt"), "x\n").unwrap();
    std::fs::write(chosen.path().join("CHOSEN.txt"), "x\n").unwrap();

    let mut cmd = viewer_command(launched_from.path());
    cmd.env("HERDR_FILE_VIEWER_ROOT", chosen.path());
    let mut s = Session::spawn(cmd).expect("spawn the viewer");
    s.set_expect_timeout(Some(Duration::from_secs(15)));
    s.expect("CHOSEN.txt")
        .expect("the tree is rooted at the handed-over directory");
    // The viewer moves its process cwd onto the root before the first frame, which is how the
    // root-aware tab launcher reads a running viewer's root from herdr's `pane list`.
    let pid = s.get_process().pid().as_raw() as u32;
    assert_eq!(process_cwd(pid), canon(chosen.path()));
    s.send("q").unwrap();
    s.expect(Eof).expect("viewer exits");
}
