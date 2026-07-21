//! Cross-platform regression guards for the Windows launcher scripts.
//!
//! `launcher_ps1.rs` parse-checks the scripts, but it is `#![cfg(windows)]` and so runs only on
//! the advisory Windows CI job. These content checks are deliberately **not** gated to Windows:
//! they read the launcher text and run on the *required* (Linux/macOS) matrix, so regressions in
//! the spaced-path spawn form (GH #58), dual-shell handling, shim/glow assets, or UTF-8 JSON setup
//! fail a blocking check.
//!
//! Why it matters: herdr's `pane run <id> <command>` types `<command>` into the pane's shell
//! (`terminal.default_shell`, else PowerShell on Windows). A bare or plain-quoted path like
//! `pane run $np "$ViewerBin"` splits on a space in the install path (e.g. `C:\Users\First Last\...`),
//! so the viewer never launches — reproduced live on real Windows. PowerShell needs the call
//! operator (`& "path"`); Git Bash/zsh need a plain quoted path. The launchers detect
//! `default_shell` and emit the matching form via a short `%USERPROFILE%\bin\hfv.exe` shim.

use std::path::PathBuf;

fn read_script(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn windows_launchers_avoid_bare_viewerbin_pane_run() {
    for name in ["open-file-viewer.ps1", "open-file-viewer-tab.ps1"] {
        let s = read_script(name);

        // The bare form that splits on a space in the install path must be gone.
        assert!(
            !s.contains(r#"pane run $np "$ViewerBin""#),
            "{name} still spawns with the bare `pane run $np \"$ViewerBin\"` form — it splits on a \
             space in the install path (GH #58)."
        );
    }
}

#[test]
fn windows_launchers_support_powershell_and_unix_like_pane_shells() {
    for name in ["open-file-viewer.ps1", "open-file-viewer-tab.ps1"] {
        let s = read_script(name);

        assert!(
            s.contains("Get-HerdrDefaultShell") && s.contains("Test-UnixLikeShell"),
            "{name} must detect herdr terminal.default_shell so pane run matches PowerShell vs bash/zsh"
        );
        assert!(
            s.contains("Invoke-ViewerInPane"),
            "{name} must centralize pane-run command selection in Invoke-ViewerInPane"
        );
        // PowerShell path still uses the call operator (GH #58).
        assert!(
            s.contains(r#"pane run $paneId "& "#) || s.contains(r#"pane run $paneId "& `""#),
            "{name} must keep the PowerShell call-operator form for non-unix shells"
        );
        // Unix-like path uses a plain quoted forward-slash path (no `&`).
        assert!(
            s.contains(r#"pane run $paneId "`"$fwd`"""#),
            "{name} must pane-run a plain quoted path for Git Bash/zsh panes"
        );
    }
}

#[test]
fn windows_launchers_shim_viewer_and_glow_style() {
    for name in ["open-file-viewer.ps1", "open-file-viewer-tab.ps1"] {
        let s = read_script(name);
        assert!(
            s.contains("Ensure-ViewerShim") && s.contains("hfv.exe"),
            "{name} must install a short %USERPROFILE%\\bin\\hfv.exe shim"
        );
        assert!(
            s.contains("markdown-style.json"),
            "{name} must mirror assets/markdown-style.json beside the shim for glow"
        );
    }
}

#[test]
fn windows_split_launcher_opens_on_the_left() {
    let s = read_script("open-file-viewer.ps1");
    assert!(
        s.contains("pane split --direction right") && s.contains("pane swap --direction left"),
        "open-file-viewer.ps1 must split right then swap left (herdr has no left split)"
    );
}

#[test]
fn unix_split_launcher_opens_on_the_left() {
    let s = read_script("open-file-viewer.sh");
    assert!(
        s.contains("--direction right") && s.contains("pane swap --direction left"),
        "open-file-viewer.sh must open right then swap left so Files lands on the left"
    );
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
