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
            println!("{}", herdr_file_viewer::launch::launch_decision_tab(&json));
            Ok(())
        }
        CliAction::OpenDirection => {
            // The launcher scripts cannot parse TOML; this prints the one resolved value they
            // need, already in `plugin pane open --direction` vocabulary. Config loading is the
            // same defensive path the TUI uses, so a malformed config degrades to `right` here
            // exactly as it degrades to defaults there.
            let (config, _) = herdr_file_viewer::config::load_config_from_env();
            let eff = herdr_file_viewer::config::resolve(&config, |k| std::env::var(k).ok());
            println!("{}", eff.open_direction.label());
            Ok(())
        }
        CliAction::Run { open } => herdr_file_viewer::run(open),
    }
}
