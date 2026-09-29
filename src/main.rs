use std::io::Read;

use herdr_file_viewer::open_target::{self, CliAction};

fn main() -> std::io::Result<()> {
    // Argv parsing lives in the library (`open_target::parse_args`) so it is unit-tested and so
    // unknown / bare flags degrade instead of killing a herdr-spawned pane.
    match open_target::parse_args(std::env::args().skip(1)) {
        CliAction::LaunchDecision => {
            let mut json = String::new();
            std::io::stdin().read_to_string(&mut json)?;
            println!("{}", herdr_file_viewer::launch::launch_decision(&json));
            Ok(())
        }
        CliAction::LaunchDecisionTab => {
            let mut json = String::new();
            std::io::stdin().read_to_string(&mut json)?;
            // Root-aware: map each pane cwd to its tree root the same way the viewer roots itself
            // (worktree top level, else the directory), canonicalized so herdr's reported cwd
            // (e.g. `/private/tmp`) and the user's path (`/tmp`) compare equal.
            let resolve_root = |cwd: &std::path::Path| {
                let ctx = herdr_file_viewer::context::LaunchContext {
                    cwd: cwd.to_path_buf(),
                    ..Default::default()
                };
                herdr_file_viewer::root::resolve(&ctx)
                    .root
                    .canonicalize()
                    .ok()
            };
            println!(
                "{}",
                herdr_file_viewer::launch::launch_decision_tab(&json, resolve_root)
            );
            Ok(())
        }
        CliAction::PrintOpenDirection => {
            // The launcher's one question: which way should herdr split? Resolving it here rather
            // than parsing TOML in bash/PowerShell keeps the lenient-value rules (trim, case,
            // `bottom`) in the one tested place. A missing or malformed config resolves to the
            // default `right`, so the launch is never blocked by a config problem.
            let (config, _) = herdr_file_viewer::config::load_config_from_env();
            let eff = herdr_file_viewer::config::resolve(&config, |k| std::env::var(k).ok());
            println!("{}", eff.open_direction.label());
            Ok(())
        }
        CliAction::PickRoot => herdr_file_viewer::root_picker::run(),
        CliAction::Run { open } => herdr_file_viewer::run(open),
    }
}
