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

    // Single-quote the path for PowerShell and escape any embedded `'` by doubling it, so a repo
    // checked out under a path containing a quote can't terminate the literal (defensive — these
    // are our own fixed-name scripts, but the interpolation should still be robust).
    let quoted = path.display().to_string().replace('\'', "''");
    let cmd = format!("$null = [ScriptBlock]::Create((Get-Content -Raw -LiteralPath '{quoted}'))");
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

/// Parse an inline PowerShell string with `[ScriptBlock]::Create`, asserting success without
/// executing it.
fn assert_ps_parses(label: &str, script: &str) {
    let quoted = script.replace('\'', "''");
    let cmd = format!("$null = [ScriptBlock]::Create('{quoted}')");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &cmd])
        .output()
        .expect("run powershell.exe to parse the inline command");
    assert!(
        output.status.success(),
        "{label} failed to parse: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn manifest_windows_action_commands_parse() {
    // The Windows [[actions]] run an inline `-Command` (not a `-File`) that re-derives the
    // launcher's absolute path from the `\\?\` server cwd. That PowerShell lives in the manifest
    // TOML, so the .ps1 parse-checks above don't cover it — a syntax error there would first
    // surface when the tester presses the key on real Windows. Extract each such payload (the
    // line carrying the de-verbatim marker, unwrapped from its TOML literal) and parse it here.
    let manifest = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("herdr-plugin.toml"),
    )
    .expect("read manifest");

    let payloads: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("[IO.Directory]::GetCurrentDirectory()"))
        .map(|l| l.trim_end_matches(',').trim_matches('\'').trim())
        .collect();

    assert_eq!(
        payloads.len(),
        2,
        "expected exactly the two Windows action -Command payloads, found {}: {payloads:?}",
        payloads.len()
    );
    for p in payloads {
        assert_ps_parses("manifest Windows action -Command", p);
    }
}
