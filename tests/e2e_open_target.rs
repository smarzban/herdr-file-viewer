//! e2e (pty): launch open target via `--open` and `HERDR_FILE_VIEWER_OPEN`.
//!
//! Unix-only: see `tests/cli_smoke.rs` for why this `expectrl`-pty e2e suite is not ported to
//! Windows's `conpty` backend in this feature.
#![cfg(unix)]

mod common;

use common::{TempDir, viewer_command};
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::time::Duration;

#[test]
fn open_flag_shows_nested_file_and_line_marker() {
    let dir = TempDir::new();
    let p = dir.path();
    std::fs::create_dir_all(p.join("nested")).unwrap();
    // Unique markers so we know the right file rendered (plain-text fallback is fine).
    std::fs::write(
        p.join("nested/target.txt"),
        "LINE1_AAA\nLINE2_BBB\nLINE3_OPEN_TARGET_MARKER\nLINE4_DDD\n",
    )
    .unwrap();
    std::fs::write(p.join("other.txt"), "OTHER_FILE\n").unwrap();

    let mut cmd = viewer_command(p);
    cmd.arg("--open").arg("nested/target.txt:3");

    let mut s = Session::spawn(cmd).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    // Nested basename in the tree (reveal expanded parents) + unique body line prove
    // --open path:line selected the right file. (Notice text is covered by the env/flag tests;
    // in some pty geometries the content-column notice strip is easy to miss while matching.)
    s.expect("target.txt")
        .expect("tree should show the opened file");
    s.expect("LINE3_OPEN_TARGET_MARKER")
        .expect("content of the opened file (line 3) must be visible");

    s.send("q").expect("send close");
    s.expect(Eof).expect("viewer terminates after close");
    match s.get_process().wait().expect("reap") {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0),
        other => panic!("expected clean exit, got {other:?}"),
    }
}

#[test]
fn open_env_shows_file() {
    let dir = TempDir::new();
    let p = dir.path();
    std::fs::write(p.join("from_env.txt"), "ENV_OPEN_MARKER\n").unwrap();

    let mut cmd = viewer_command(p);
    cmd.env("HERDR_FILE_VIEWER_OPEN", "from_env.txt");

    let mut s = Session::spawn(cmd).expect("spawn");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    s.expect("Opened from_env.txt")
        .expect("notice for env open target");
    s.expect("ENV_OPEN_MARKER")
        .expect("env-selected file content");

    s.send("q").unwrap();
    s.expect(Eof).unwrap();
    match s.get_process().wait().unwrap() {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn open_flag_wins_over_env() {
    let dir = TempDir::new();
    let p = dir.path();
    std::fs::write(p.join("from_env.txt"), "ENV_SHOULD_NOT_WIN\n").unwrap();
    std::fs::write(p.join("from_flag.txt"), "FLAG_WINS_MARKER\n").unwrap();

    let mut cmd = viewer_command(p);
    cmd.env("HERDR_FILE_VIEWER_OPEN", "from_env.txt");
    cmd.arg("--open").arg("from_flag.txt");

    let mut s = Session::spawn(cmd).expect("spawn");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    s.expect("Opened from_flag.txt")
        .expect("flag open target notice");
    s.expect("FLAG_WINS_MARKER")
        .expect("flag path content wins over env");

    s.send("q").unwrap();
    s.expect(Eof).unwrap();
    match s.get_process().wait().unwrap() {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn unknown_arg_still_starts_viewer() {
    let dir = TempDir::new();
    let p = dir.path();
    std::fs::write(p.join("alive.txt"), "STILL_STARTS\n").unwrap();

    let mut cmd = viewer_command(p);
    cmd.arg("--herdr-future-arg");
    cmd.arg("--open"); // bare --open must not exit 2

    let mut s = Session::spawn(cmd).expect("spawn despite unknown/bare flags");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    s.expect("alive.txt")
        .expect("viewer must start with unknown args ignored");
    s.expect("STILL_STARTS")
        .expect("default selection still renders");

    s.send("q").unwrap();
    s.expect(Eof).unwrap();
    match s.get_process().wait().unwrap() {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0),
        other => panic!("{other:?}"),
    }
}
