use std::io::Read;

fn main() -> std::io::Result<()> {
    // `--launch-decision`: read a herdr `pane list` JSON on stdin and print the launcher's
    // decision (OPEN / FOCUS <id> / CLOSE <id>) for scripts/open-file-viewer.sh. This does NOT
    // start the TUI, so the launcher can compute its action in-process (no extra runtime dep).
    // `--launch-decision-tab` is the same, for the tab launcher: it may also emit
    // `SWITCHTAB <tab_id>` so a viewer in another tab is switched to, not duplicated.
    let mode = std::env::args().nth(1);
    if matches!(
        mode.as_deref(),
        Some("--launch-decision") | Some("--launch-decision-tab")
    ) {
        let mut json = String::new();
        std::io::stdin().read_to_string(&mut json)?;
        let decision = if mode.as_deref() == Some("--launch-decision-tab") {
            herdr_file_viewer::launch::launch_decision_tab(&json)
        } else {
            herdr_file_viewer::launch::launch_decision(&json)
        };
        println!("{decision}");
        return Ok(());
    }
    herdr_file_viewer::run()
}
