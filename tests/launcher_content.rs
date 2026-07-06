//! Cross-platform regression guard for the Windows launcher spawn form (GH #58).
//!
//! `launcher_ps1.rs` parse-checks the scripts, but it is `#![cfg(windows)]` and so runs only on
//! the advisory Windows CI job. This content check is deliberately **not** gated to Windows: it
//! reads the launcher text and runs on the *required* (Linux/macOS) matrix, so a regression to the
//! bare-path spawn fails a blocking check.
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
