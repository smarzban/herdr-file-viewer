use std::io::Read;

use herdr_file_viewer::open_target::{self, CliAction};

fn main() -> std::io::Result<()> {
    // Argv parsing lives in the library (`open_target::parse_args`) so it is unit-tested and so
    // unknown / bare flags degrade instead of killing a herdr-spawned pane.
    match open_target::parse_args(std::env::args().skip(1)) {
        CliAction::LaunchDecision => {
            let mut json = String::new();
            std::io::stdin().read_to_string(&mut json)?;
            // the invocation context herdr hands plugin actions; a programmatic
            // invocation's pane anchors the decision instead of the UI focus
            let ctx = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok();
            println!(
                "{}",
                herdr_file_viewer::launch::launch_decision(&json, ctx.as_deref())
            );
            Ok(())
        }
        CliAction::LaunchDecisionTab => {
            let mut json = String::new();
            std::io::stdin().read_to_string(&mut json)?;
            let ctx = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok();
            println!(
                "{}",
                herdr_file_viewer::launch::launch_decision_tab(&json, ctx.as_deref())
            );
            Ok(())
        }
        CliAction::Run { open } => herdr_file_viewer::run(open),
    }
}
