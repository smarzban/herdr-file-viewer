//! herdr-file-viewer — a git-aware, read-only file viewer that runs as a herdr TUI pane.
//!
//! A library crate (the testable components) plus a thin binary (`src/main.rs` → [`run`]).
//! Modules are added by each plan task as it lands.

pub mod app;
pub mod context;
pub mod controller;
pub mod editor;
pub mod finder;
pub mod fuzzy;
pub mod git;
pub mod herdr;
pub mod host;
pub mod index;
pub mod input;
pub mod intent;
pub mod launch;
pub mod picker;
pub mod presenter;
pub mod prompt;
pub mod render;
pub mod root;
pub mod tree;
pub mod update;
pub mod view_policy;
pub mod worktree;

/// Entry point invoked by the binary. Wires the components and runs the event loop.
///
/// Delegates to [`app::run`], which assembles the live components and drives the terminal
/// loop until the user closes the viewer (AC-20).
pub fn run() -> std::io::Result<()> {
    app::run()
}
