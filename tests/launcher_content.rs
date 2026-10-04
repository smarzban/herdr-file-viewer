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

#[test]
fn windows_launchers_pass_the_plugin_config_dir() {
    // herdr injects HERDR_PLUGIN_CONFIG_DIR only into a pane it spawns from the manifest. These
    // launchers spawn the viewer themselves (by absolute path, GH #58), so unless they pass the
    // variable on, the viewer looks for config.toml under $XDG_CONFIG_HOME / $HOME — neither of
    // which Windows sets — resolves a RELATIVE path, and refuses to read it. The user's config
    // file is then ignored with no error, which reads as "the setting does nothing".
    for name in ["open-file-viewer.ps1", "open-file-viewer-tab.ps1"] {
        let s = read_script(name);
        assert!(
            s.contains("plugin config-dir herdr-file-viewer"),
            "{name} must ask herdr for the plugin config directory"
        );
        assert!(
            s.contains("HERDR_PLUGIN_CONFIG_DIR="),
            "{name} must pass HERDR_PLUGIN_CONFIG_DIR to the pane it spawns, or config.toml is \
             silently ignored on Windows"
        );
        assert!(
            s.contains("--env"),
            "{name} must pass it via herdr's --env flag on the pane/tab it creates"
        );
    }
}

#[test]
fn root_picker_launcher_opens_the_picker_as_a_popup() {
    // The `open-file-viewer-at[-tab]` actions only open the manifest's `root-picker` pane as a popup;
    // the picker itself hands the chosen root to the new viewer by `--env`, never `--cwd` (#139).
    let s = read_script("open-file-viewer-at.sh");
    assert!(s.contains("--entrypoint root-picker"), "{s}");
    assert!(s.contains("--placement popup"), "{s}");
    assert!(s.contains("${HERDR_BIN_PATH:-herdr}"), "{s}");
    // The placement reaches the picker through the popup's env (verified live on herdr 0.9.1).
    assert!(
        s.contains(r#"--env "HERDR_FILE_VIEWER_PICK_PLACEMENT=$placement""#),
        "{s}"
    );
    let code: String = s
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!code.contains("--cwd"), "{code}");
}

/// Run `scripts/open-file-viewer-at.sh` with `arg` against a fake herdr that records its argv,
/// returning that argv (one element per line).
#[cfg(unix)]
fn run_root_picker_launcher(arg: Option<&str>) -> String {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!(
        "hfv-at-launcher-{}-{}",
        std::process::id(),
        arg.unwrap_or("none")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("argv.txt");
    let fake = dir.join("fake-herdr");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", out.display()),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/open-file-viewer-at.sh");
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script).env("HERDR_BIN_PATH", &fake);
    if let Some(a) = arg {
        cmd.arg(a);
    }
    assert!(cmd.status().unwrap().success());
    let argv = std::fs::read_to_string(&out).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    argv
}

#[cfg(unix)]
#[test]
fn root_picker_launcher_maps_its_argument_to_the_popup_placement() {
    // Executed, not grepped: the action's `split`/`tab` argument must reach the picker as
    // HERDR_FILE_VIEWER_PICK_PLACEMENT, or the tab action would silently open a split.
    let popup = |placement: &str| {
        format!(
            "plugin\npane\nopen\n--plugin\nherdr-file-viewer\n--entrypoint\nroot-picker\n\
             --placement\npopup\n--env\nHERDR_FILE_VIEWER_PICK_PLACEMENT={placement}\n"
        )
    };
    assert_eq!(run_root_picker_launcher(Some("tab")), popup("tab"));
    assert_eq!(run_root_picker_launcher(Some("split")), popup("split"));
    assert_eq!(run_root_picker_launcher(None), popup("split"));
    assert_eq!(run_root_picker_launcher(Some("bogus")), popup("split"));
}
