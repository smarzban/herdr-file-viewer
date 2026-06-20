//! herdr-file-viewer — a git-aware, read-only file viewer that runs as a herdr TUI pane.
//!
//! A library crate (the testable components) plus a thin binary (`src/main.rs` → [`run`]).
//! Modules are added by each plan task as it lands.

pub mod context;
pub mod git;
pub mod root;
pub mod view_policy;

/// Entry point invoked by the binary. Wires the components and runs the event loop.
///
/// Stubbed in T-1; assembled in T-20.
pub fn run() -> std::io::Result<()> {
    Ok(())
}
