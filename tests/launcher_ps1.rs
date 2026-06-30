//! Parse-only test for the Windows launcher scripts (T-8 — AC-16).
//!
//! `scripts/open-file-viewer.ps1` and `scripts/open-file-viewer-tab.ps1` are thin glue over the
//! already-unit-tested, portable launch-decision logic (`src/launch.rs`). The end-to-end
//! launch-or-focus-or-close toggle needs a live herdr on Windows and is reviewer-checked
//! (AC-16); what we CAN cheaply assert here, hermetically, is that each script is syntactically
//! valid PowerShell — `[ScriptBlock]::Create` parses the script text without executing it, so a
//! syntax error (a typo, an unbalanced brace, …) fails this test immediately instead of only
//! surfacing the first time someone presses the launcher's key on real Windows.

#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

fn script_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
}

/// Parse `script` with `[ScriptBlock]::Create`, asserting it succeeds (exit 0) and reports no
/// error on stderr. Never executes the script's body.
fn assert_parses(name: &str) {
    let path = script_path(name);
    assert!(path.is_file(), "{name} must exist at {}", path.display());

    let cmd = format!(
        "$null = [ScriptBlock]::Create((Get-Content -Raw -LiteralPath '{}'))",
        path.display()
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &cmd])
        .output()
        .expect("run powershell.exe to parse the script");

    assert!(
        output.status.success(),
        "{name} failed to parse: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn open_file_viewer_ps1_parses() {
    assert_parses("open-file-viewer.ps1");
}

#[test]
fn open_file_viewer_tab_ps1_parses() {
    assert_parses("open-file-viewer-tab.ps1");
}

#[test]
fn fetch_or_build_ps1_parses() {
    // Not a launcher, but the same hermetic parse-check is cheap insurance for the other
    // Windows-only script (T-7) — a syntax error there would otherwise only surface on a real
    // Windows install.
    assert_parses("fetch-or-build.ps1");
}
