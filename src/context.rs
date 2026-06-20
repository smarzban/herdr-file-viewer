//! The normalized launch context the Host Adapter hands to the Root Resolver.
//!
//! Produced at the herdr boundary (T-17) from injected env/JSON; consumed by
//! [`crate::root::resolve`]. Malformed host input degrades to a minimal `{ cwd }`.

use std::path::PathBuf;

/// What herdr tells the viewer about how it was launched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchContext {
    /// The invoking pane's working directory.
    pub cwd: PathBuf,
    /// The worktree root, when herdr launched us inside a managed worktree.
    pub worktree_root: Option<PathBuf>,
    /// A base-branch hint from herdr (the branch a worktree forked from).
    pub base_branch: Option<String>,
    /// Whether herdr says this is a worktree launch.
    pub is_worktree: bool,
}
