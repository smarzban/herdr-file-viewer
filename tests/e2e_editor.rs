//! T-22 — e2e (pty): the open-in-editor hand-off launches the configured `$EDITOR` on the
//! selected file and never mutates it (AC-19, AC-N1). The "editor" is a recording shell
//! script that writes the path it was given to a file and exits — so the assertion is a
//! filesystem check, independent of any terminal-screen parsing.
//!
//! The success test also serves as the regression test for the **best-effort post-editor
//! `terminal.clear()`** (`src/app.rs`): the hand-off re-enters the alternate screen and the
//! run loop then calls `terminal.clear()`, which issues a cursor-position (DSR) query. A pty
//! does not answer that query, so a non-best-effort clear would make `run()` return an error
//! and the process exit non-zero — the `exit == 0` assertion guards against that regression.

mod common;

use common::{TempDir, viewer_command};
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

// Skipped on macOS: under an `expectrl` pty on macOS the suspend/resume editor hand-off does
// not engage — the viewer never enters the hand-off (confirmed via CI tracing: the recorder
// script runs fine *directly* on macOS, and the close key works, but `open_in_editor` is never
// driven to completion under the macOS pty). It's an interaction between the e2e pty harness and
// the suspend/resume cycle on macOS, not the product logic: the hand-off is covered
// cross-platform by the `editor.rs` and `controller::open_in_editor` unit tests (which pass on
// macOS), and real-use editor hand-off on macOS is verified manually. The sibling
// `a_missing_editor_*` e2e below still runs on macOS.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "expectrl pty + suspend/resume hand-off doesn't engage on macOS; logic is unit-tested and verified manually"
)]
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
    // shortly after; poll the filesystem for the recorded *content* (not mere existence — the
    // `>` redirect creates the file before the path is written, which would race a content
    // read).
    let deadline = Instant::now() + Duration::from_secs(10);
    let recorded = loop {
        let contents = std::fs::read_to_string(&record).unwrap_or_default();
        if contents.ends_with("edit.txt") {
            break contents;
        }
        assert!(
            Instant::now() < deadline,
            "editor was never invoked on the selected file"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        recorded.ends_with("edit.txt"),
        "editor invoked on the selected file: {recorded:?}"
    );

    // AC-N1: the hand-off must not have modified the file.
    assert_eq!(
        std::fs::read_to_string(p.join("edit.txt")).unwrap(),
        "EDITME\n",
        "file unchanged"
    );

    // The recorder is written before the editor exits, and the viewer re-enables raw mode
    // only after it returns; give that a moment so the close key is read in raw mode.
    std::thread::sleep(Duration::from_millis(150));
    s.send("q").expect("send close");
    s.expect(Eof)
        .expect("the viewer terminates after the close key");
    match s.get_process().wait().expect("reap the viewer") {
        // exit==0 also guards the best-effort post-editor clear (see the module doc).
        WaitStatus::Exited(_, code) => assert_eq!(code, 0, "clean exit after editor hand-off"),
        other => panic!("expected a clean exit, got {other:?}"),
    }
}

#[test]
fn a_missing_editor_is_a_non_fatal_notice_not_a_crash() {
    // With no usable $EDITOR, open-in-editor must surface a notice and keep running (AC-19
    // degradation) — the run loop never crashes (constitution). Asserted via a clean exit.
    let dir = TempDir::new();
    let p = dir.path();
    std::fs::write(p.join("edit.txt"), "EDITME\n").unwrap();

    let mut cmd = viewer_command(p);
    cmd.env("EDITOR", ""); // empty → the hand-off fails before touching the terminal
    let mut s = Session::spawn(cmd).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    s.expect("edit.txt").expect("tree should list the file");
    s.send("e")
        .expect("send open-in-editor with no editor configured");
    // The file must still be intact, and the viewer must still close cleanly.
    s.send("q").expect("send close");
    s.expect(Eof)
        .expect("viewer terminates after a failed hand-off + close");
    assert_eq!(
        std::fs::read_to_string(p.join("edit.txt")).unwrap(),
        "EDITME\n",
        "file unchanged"
    );
    match s.get_process().wait().expect("reap the viewer") {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0, "a missing editor does not crash"),
        other => panic!("expected a clean exit, got {other:?}"),
    }
}
