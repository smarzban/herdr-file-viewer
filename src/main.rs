use std::io::Read;

fn main() -> std::io::Result<()> {
    // Modes that never start the TUI:
    //   `--launch-decision` / `--launch-decision-tab`: read herdr `pane list` JSON on stdin and
    //   print OPEN / FOCUS / CLOSE / SWITCHTAB for the shell launch scripts.
    // Normal run: optional `--open <path>[:line]` (or `--open=<…>`) seeds the launch open target;
    // env `HERDR_FILE_VIEWER_OPEN` is an alternative (flag wins) and is resolved inside the library.
    let mut open_flag: Option<String> = None;
    let mut launch_mode: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--launch-decision" | "--launch-decision-tab" => {
                launch_mode = Some(arg);
            }
            "--open" => match args.next() {
                Some(v) => open_flag = Some(v),
                None => {
                    eprintln!("error: --open requires a path argument (path or path:line)");
                    std::process::exit(2);
                }
            },
            a if a.starts_with("--open=") => {
                open_flag = Some(a["--open=".len()..].to_string());
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                eprintln!(
                    "usage: herdr-file-viewer [--open <path>[:line]]\n       herdr-file-viewer --launch-decision[-tab]"
                );
                std::process::exit(2);
            }
        }
    }

    if let Some(mode) = launch_mode {
        let mut json = String::new();
        std::io::stdin().read_to_string(&mut json)?;
        let decision = if mode == "--launch-decision-tab" {
            herdr_file_viewer::launch::launch_decision_tab(&json)
        } else {
            herdr_file_viewer::launch::launch_decision(&json)
        };
        println!("{decision}");
        return Ok(());
    }

    herdr_file_viewer::run(open_flag)
}
