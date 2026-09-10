//! e2e (pty): the session view end-to-end through the real binary — `s` presents the
//! transcript-derived file set (found via `$HOME/.claude/projects/<slug>`) with the
//! `[session]` title tag and the outside-root divider, and `s` again restores the full tree,
//! exiting cleanly.
//!
//! Unix-only, like the rest of the `expectrl`-pty e2e suite (see `tests/cli_smoke.rs`).
#![cfg(unix)]

mod common;

use common::{TempDir, viewer_command};
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::fs;
use std::path::Path;
use std::time::Duration;

fn create_line(path: &Path) -> String {
    format!(
        "{{\"toolUseResult\":{{\"type\":\"create\",\"filePath\":\"{}\"}}}}\n",
        path.display()
    )
}

#[test]
fn session_view_toggles_end_to_end() {
    let home = TempDir::new();
    let dir = TempDir::new();
    let p = dir.path();
    fs::write(p.join("touched.rs"), "MEMBER\n").unwrap();
    fs::write(p.join("zz_other.rs"), "OTHER\n").unwrap();
    // The binary derives the transcript store from ITS root — the physical cwd (macOS temp
    // dirs live behind /var → /private/var symlinks), so the fixture must slug the
    // canonicalized path or the store lands under a different project slug.
    let physical = p.canonicalize().unwrap();
    let outside = home.path().canonicalize().unwrap().join("outside.txt");
    fs::write(&outside, "OUT\n").unwrap();
    let store = herdr_file_viewer::session::projects_dir(home.path(), &physical);
    fs::create_dir_all(&store).unwrap();
    fs::write(
        store.join("e2e.jsonl"),
        format!(
            "{}{}",
            create_line(&physical.join("touched.rs")),
            create_line(&outside),
        ),
    )
    .unwrap();

    let mut cmd = viewer_command(p);
    cmd.env("HOME", home.path());
    let mut s = Session::spawn(cmd).expect("spawn the viewer in a pty");
    s.set_expect_timeout(Some(Duration::from_secs(15)));

    // The full tree first: both files list.
    s.expect("zz_other.rs").expect("full tree lists every file");

    // `s` enters the session view: the divider and the outside member render. (The `[session]`
    // title tag is asserted in the presenter tests — the pty's narrow tree column truncates
    // this fixture's long temp-dir title, so it is not a reliable stream marker here.)
    s.send("s").expect("send session-view toggle");
    s.expect("outside root")
        .expect("the outside-root divider renders");
    s.expect("outside.txt")
        .expect("the outside-root member renders below the divider");

    // `]` jumps across member file rows — onto the outside member — and its CONTENT must
    // preview (the classifier's containment guard is waived for exactly this row, ADR-0012;
    // without that, every outside member would show the `[binary file]` placeholder).
    s.send("]").expect("send next-session-file jump");
    s.expect("OUT")
        .expect("an outside-root member previews like any file");

    // `s` again restores the full tree (the non-member reappears in a fresh frame).
    s.send("s").expect("send session-view toggle again");
    s.expect("zz_other.rs")
        .expect("leaving the session view restores the full tree");

    s.send("q").expect("send close");
    s.expect(Eof).expect("the viewer terminates");
    match s.get_process().wait().expect("reap the viewer") {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0, "clean exit after toggling"),
        other => panic!("expected a clean exit, got {other:?}"),
    }
}
