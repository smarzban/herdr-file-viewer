//! T-22 — e2e (pty): the open-in-editor hand-off launches the configured `$EDITOR` on the
//! selected file and never mutates it (AC-19, AC-N1). The "editor" is a recording shell
//! script that writes the path it was given to a file and exits — so the assertion is a
//! filesystem check, independent of any terminal-screen parsing.

mod common;

use common::{viewer_command, TempDir};
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

#[test]
fn open_in_editor_invokes_the_editor_on_the_selected_file_without_modifying_it() {
    let dir = TempDir::new();
    let p = dir.path();
    std::fs::write(p.join("edit.txt"), "EDITME\n").unwrap();

    // A recording "editor": writes the file path it receives ($1) to $RECORD, then exits.
    let script = p.join("rec-editor.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf '%s' \"$1\" > \"$RECORD\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let record = p.join("record.out");

    let mut cmd = viewer_command(p);
    cmd.env("EDITOR", &script).env("RECORD", &record);
    let mut s = Session::spawn(cmd).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    s.expect("edit.txt").expect("tree should list the file");

    // The selected file (cursor 0) is edit.txt; hand it off to the editor.
    s.send("e").expect("send open-in-editor");

    // The hand-off is synchronous (the viewer waits for the editor), so the record appears
    // shortly after; poll the filesystem rather than the screen.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !record.exists() {
        assert!(Instant::now() < deadline, "editor was never invoked");
        std::thread::sleep(Duration::from_millis(25));
    }
    let recorded = std::fs::read_to_string(&record).unwrap();
    assert!(
        recorded.ends_with("edit.txt"),
        "the editor was invoked on the selected file (got {recorded:?})"
    );

    // AC-N1: the hand-off must not have modified the file.
    assert_eq!(std::fs::read_to_string(p.join("edit.txt")).unwrap(), "EDITME\n", "file unchanged");

    s.send("q").expect("send close");
    s.expect(Eof).expect("the viewer terminates after the close key");
    match s.get_process().wait().expect("reap the viewer") {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0, "clean exit after editor hand-off"),
        other => panic!("expected a clean exit, got {other:?}"),
    }
}
