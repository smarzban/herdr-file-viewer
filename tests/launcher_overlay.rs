//! Content guards for the overlay launchers (`scripts/open-file-viewer-overlay.sh` / `.ps1`).
//!
//! Hermetic, like `launcher_content.rs`: they read the launcher text. An overlay is a split with
//! the tab zoomed onto it, and `pane zoom <id> --off` flattens it back into a plain split — so the
//! overlay launchers' FOCUS branch must be `zoom --on` alone, and the Windows one (which spawns
//! the viewer by absolute path and so can't ask herdr for the placement, GH #58) must zoom the
//! pane it splits. Copy-pasting the split launcher's `--on`/`--off` FOCUS branch would quietly
//! turn this into a second split action; that is what these catch.

use std::path::PathBuf;

fn read_script(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The script with `#` line-comments stripped, so assertions see commands, not prose.
fn code_only(script: &str) -> String {
    script
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn unix_overlay_launcher_requests_the_overlay_placement() {
    let s = code_only(&read_script("open-file-viewer-overlay.sh"));
    assert!(
        s.contains("plugin pane open") && s.contains("--placement overlay"),
        "the unix overlay launcher must open the manifest pane with `--placement overlay`"
    );
    assert!(
        !s.contains("--placement split") && !s.contains("--placement tab"),
        "the unix overlay launcher must not fall back to another placement"
    );
    // Same placement-agnostic decision as the split launcher (tab-scoped: is there a Files pane
    // in the focused pane's tab?), so a viewer opened by any launcher is found, never duplicated.
    assert!(
        s.contains("--launch-decision") && !s.contains("--launch-decision-tab"),
        "the unix overlay launcher must reuse the tab-scoped `--launch-decision` mode"
    );
}

#[test]
fn overlay_launchers_never_unzoom_the_pane() {
    for name in [
        "open-file-viewer-overlay.sh",
        "open-file-viewer-overlay.ps1",
    ] {
        let s = code_only(&read_script(name));
        assert!(
            s.contains("--on"),
            "{name} must bring a viewer back up with `pane zoom <id> --on`"
        );
        assert!(
            !s.contains("--off"),
            "{name} runs `zoom --off`, which flattens the overlay into a plain split"
        );
    }
}

#[test]
fn windows_overlay_launcher_zooms_the_pane_it_splits() {
    // Windows can't ask herdr for `--placement overlay` (absolute-path spawn, GH #58), so the
    // launcher must build the same state itself: split, run the viewer, then zoom onto it.
    let s = code_only(&read_script("open-file-viewer-overlay.ps1"));
    let split = s
        .find("'pane', 'split'")
        .expect("must `pane split` the viewer pane");
    let run = s
        .find("pane run $np")
        .expect("must `pane run` the viewer into it");
    let zoom = s
        .find("pane zoom $np --on")
        .expect("must `pane zoom $np --on` after spawning");
    assert!(
        split < run && run < zoom,
        "the Windows overlay launcher must split, then run the viewer, then zoom onto the pane"
    );
}
