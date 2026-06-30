//! main wiring smoke test over a real pty (AC-17 launch behavior, AC-20 close).
//! Spawns the *built* binary in a temp dir, asserts it draws the file tree, then presses
//! the close key and asserts a clean exit(0). External renderers (glow/delta/bat) need not
//! be installed — the Content Renderer falls back to plain text, and the tree draws either
//! way.

mod common;

use common::TempDir;
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::process::Command;
use std::time::Duration;

#[test]
fn viewer_draws_a_filename_then_exits_zero_on_close() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("hello.txt"), "hi there\n").unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_herdr-file-viewer"));
    cmd.current_dir(dir.path());
    // Hermetic: disable the `git ls-remote` update check (AC-27/hermetic tests) so the smoke
    // test performs no network I/O. See `src/update/mod.rs` DISABLE_ENV — any value disables it.
    cmd.env("HERDR_FILE_VIEWER_NO_UPDATE_CHECK", "1");

    let mut p = Session::spawn(cmd).expect("spawn the viewer in a pty");
    p.set_expect_timeout(Some(Duration::from_secs(10)));

    // The tree column lists the file in the launch directory (AC-3 display / AC-17 launch).
    p.expect("hello.txt")
        .expect("viewer should draw the file tree");

    // The close key returns control and exits the process (AC-20).
    p.send("q").expect("send the close key");
    p.expect(Eof)
        .expect("process should terminate after the close key");

    match p.get_process().wait().expect("reap the viewer process") {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0, "AC-20: clean exit on close"),
        other => panic!("expected a clean exit, got {other:?}"),
    }
}
