//! e2e (pty): the built viewer reports the file it shows as the `file_viewer_open` herdr pane
//! token, and clears it on quit. `HERDR_BIN_PATH` points at a fake herdr script that appends each
//! argv to a log, so no real herdr is touched.
//!
//! Unix-only: see `tests/cli_smoke.rs` for why this `expectrl`-pty e2e suite is not ported to
//! Windows's `conpty` backend.
#![cfg(unix)]

mod common;

use common::{TempDir, viewer_command};
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const SET: &str = "pane report-metadata test:p1 --source herdr-file-viewer --token file_viewer_open=nested/target.txt";
const CLEAR: &str =
    "pane report-metadata test:p1 --source herdr-file-viewer --clear-token file_viewer_open";

/// A fake herdr that appends its argv (space-joined, one call per line) to `log`.
fn fake_herdr(tools: &Path, log: &Path) -> PathBuf {
    let script = tools.join("herdr");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn report_lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("report-metadata"))
        .map(String::from)
        .collect()
}

/// A viewer opened at `nested/target.txt`, inside a fake herdr pane `test:p1`, with its config dir
/// in `tools` (so a developer's own config cannot change the outcome).
fn viewer(root: &Path, tools: &Path, log: &Path) -> Command {
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/target.txt"), "REPORT_TARGET_MARKER\n").unwrap();
    let mut cmd = viewer_command(root);
    cmd.arg("--open").arg("nested/target.txt");
    cmd.env(herdr_file_viewer::open_report::PANE_ENV, "test:p1");
    cmd.env("HERDR_BIN_PATH", fake_herdr(tools, log));
    cmd.env("HERDR_PLUGIN_CONFIG_DIR", tools.join("config"));
    cmd
}

/// Press `q` and require a clean exit.
macro_rules! quit {
    ($s:expr) => {{
        $s.send("q").expect("send close");
        $s.expect(Eof).expect("viewer terminates after close");
        match $s.get_process().wait().expect("reap") {
            WaitStatus::Exited(_, code) => assert_eq!(code, 0),
            other => panic!("expected clean exit, got {other:?}"),
        }
    }};
}

#[test]
fn reports_the_open_file_and_clears_it_on_quit() {
    let (root, tools) = (TempDir::new(), TempDir::new());
    let log = tools.path().join("herdr.log");
    let mut s = Session::spawn(viewer(root.path(), tools.path(), &log)).expect("spawn");
    s.set_expect_timeout(Some(Duration::from_secs(15)));
    s.expect("REPORT_TARGET_MARKER")
        .expect("the opened file is shown");

    let deadline = Instant::now() + Duration::from_secs(15);
    while !report_lines(&log).iter().any(|l| l == SET) {
        assert!(
            Instant::now() < deadline,
            "the shown file was never reported: {:?}",
            report_lines(&log)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    quit!(s);

    let lines = report_lines(&log);
    assert_eq!(lines.first().map(String::as_str), Some(SET), "{lines:?}");
    assert_eq!(lines.last().map(String::as_str), Some(CLEAR), "{lines:?}");
}

/// The negative is observed at exit, not by sleeping: a live reporter drains its queue (set, then
/// clear) before the process exits, so an ignored switch would leave lines in the log.
#[test]
fn report_open_file_false_sends_nothing() {
    let (root, tools) = (TempDir::new(), TempDir::new());
    let log = tools.path().join("herdr.log");
    std::fs::create_dir_all(tools.path().join("config")).unwrap();
    std::fs::write(
        tools.path().join("config/config.toml"),
        "report_open_file = false\n",
    )
    .unwrap();
    let mut s = Session::spawn(viewer(root.path(), tools.path(), &log)).expect("spawn");
    s.set_expect_timeout(Some(Duration::from_secs(15)));
    s.expect("REPORT_TARGET_MARKER")
        .expect("the opened file is shown");
    quit!(s);

    assert_eq!(report_lines(&log), Vec::<String>::new());
}
