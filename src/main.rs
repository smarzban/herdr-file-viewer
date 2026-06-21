use std::io::Read;

fn main() -> std::io::Result<()> {
    // `--launch-decision`: read a herdr `pane list` JSON on stdin and print the launcher's
    // decision (OPEN / FOCUS <id> / CLOSE <id>) for scripts/open-file-viewer.sh. This does NOT
    // start the TUI, so the launcher can compute its action in-process (no extra runtime dep).
    if std::env::args().nth(1).as_deref() == Some("--launch-decision") {
        let mut json = String::new();
        std::io::stdin().read_to_string(&mut json)?;
        println!("{}", herdr_file_viewer::launch::launch_decision(&json));
        return Ok(());
    }
    herdr_file_viewer::run()
}
