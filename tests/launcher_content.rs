//! Cross-platform regression guards for the Windows launcher scripts.
//!
//! `launcher_ps1.rs` parse-checks the scripts, but it is `#![cfg(windows)]` and so runs only on
//! the advisory Windows CI job. These content checks are deliberately **not** gated to Windows:
//! they read the launcher text and run on the *required* (Linux/macOS) matrix, so regressions in
//! the spaced-path spawn form (GH #58) or UTF-8 JSON setup fail a blocking check.
//!
//! Why it matters: herdr's `pane run <id> <command>` types `<command>` into the pane's shell
//! (PowerShell on Windows). A bare or plain-quoted path like `pane run $np "$ViewerBin"` splits on
//! a space in the install path (e.g. `C:\Users\First Last\...`), so the viewer never launches —
//! reproduced live on real Windows. The fix runs the viewer via the PowerShell call operator with
//! a quoted absolute path: `pane run $np "& \"$ViewerBin\""`.

use std::path::PathBuf;

fn read_script(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn windows_launchers_spawn_via_call_operator_not_a_bare_path() {
    for name in ["open-file-viewer.ps1", "open-file-viewer-tab.ps1"] {
        let s = read_script(name);

        // The bare form that splits on a space in the install path must be gone.
        assert!(
            !s.contains(r#"pane run $np "$ViewerBin""#),
            "{name} still spawns with the bare `pane run $np \"$ViewerBin\"` form — it splits on a \
             space in the install path (GH #58). Use the call-operator form."
        );

        // The viewer must be spawned via the call operator (`& ...`) so a quoted, spaced path runs.
        assert!(
            s.contains(r#"pane run $np "& "#),
            "{name} must spawn the viewer via the PowerShell call operator: \
             pane run $np \"& \\\"$ViewerBin\\\"\""
        );
    }
}

#[test]
fn windows_json_consumers_force_utf8_before_convert_from_json() {
    for name in ["open-file-viewer.ps1", "open-file-viewer-tab.ps1"] {
        let script = read_script(name);
        assert_utf8_before_json(name, &script);
    }

    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("herdr-plugin.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display()));
    let actions: Vec<&str> = manifest
        .lines()
        .filter(|line| line.contains("ConvertFrom-Json"))
        .collect();
    assert_eq!(actions.len(), 2, "expected both Windows action payloads");
    for action in actions {
        assert_utf8_before_json("manifest Windows action", action);
    }
}

fn assert_utf8_before_json(label: &str, text: &str) {
    let convert = text
        .find("ConvertFrom-Json")
        .unwrap_or_else(|| panic!("{label} must parse herdr JSON"));
    let setup = &text[..convert];
    assert!(
        setup.contains("[Console]::OutputEncoding"),
        "{label} must set the native stdout decoder to UTF-8 before ConvertFrom-Json"
    );
    assert!(
        setup.contains("$OutputEncoding"),
        "{label} must set PowerShell's native-pipeline encoding before ConvertFrom-Json"
    );
    assert!(
        setup.contains("System.Text.UTF8Encoding($false)"),
        "{label} must use BOM-less UTF-8 before ConvertFrom-Json"
    );
}
