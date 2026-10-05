//! Host Adapter — the herdr boundary: parse the injected launch context (AC-26).
//!
//! `HERDR_PLUGIN_CONTEXT_JSON` is parsed defensively — malformed or missing input degrades
//! to a minimal `{ cwd }` context, never a panic (AC-26).

use crate::context::LaunchContext;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Env var the root picker (`--pick-root`) sets through herdr's `plugin pane open --env` to root
/// the viewer at an explicitly chosen directory instead of the invoking pane's cwd. It is the one
/// sanctioned exception to "the root comes from the focused pane": a separate, explicit launch,
/// never a flag on the default summon (the manifest pane command is relative, so `--cwd` would
/// break the spawn — #139).
pub const ROOT_ENV: &str = "HERDR_FILE_VIEWER_ROOT";

/// The shape of `HERDR_PLUGIN_CONTEXT_JSON`. Every field is optional so a partial or absent
/// object degrades gracefully rather than failing to parse; unknown fields are ignored.
#[derive(Deserialize, Default)]
struct RawContext {
    /// herdr 0.7.0 reports the invoking pane's directory as `focused_pane_cwd` and the
    /// workspace root as `workspace_cwd`; a plain `cwd` is accepted as a fallback. The viewer
    /// roots at the most specific of these so the tree shows the directory the user is in — not
    /// the plugin's own install dir, where the pane process is actually started (the pane
    /// command is a relative path, so herdr launches it from the plugin root).
    focused_pane_cwd: Option<String>,
    workspace_cwd: Option<String>,
    cwd: Option<String>,
    base_branch: Option<String>,
    workspace_id: Option<String>,
}

/// Build a `LaunchContext` from the process environment: the injected context JSON, falling
/// back to the process working directory. Never panics (AC-26).
pub fn from_env() -> LaunchContext {
    let json = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok();
    let cwd = std::env::current_dir().unwrap_or_default();
    let ctx = parse_context(json.as_deref(), cwd);
    let root = std::env::var(ROOT_ENV).ok();
    apply_root_override(ctx, root.as_deref(), Path::is_dir)
}

/// Layer an explicit [`ROOT_ENV`] root over the host context. A blank, relative, or
/// non-directory value is ignored (the host context stands), so a bad hand-off degrades to the
/// normal summon rather than an empty tree. When the override applies, the host's `base_branch`
/// hint is dropped: it describes the *invoking* pane's worktree, not the chosen directory.
/// `is_dir` is injected so the precedence is testable without touching the filesystem.
pub fn apply_root_override(
    mut ctx: LaunchContext,
    root: Option<&str>,
    is_dir: impl Fn(&Path) -> bool,
) -> LaunchContext {
    // Blank means "not set", but a real value is used verbatim: a directory name may end in a
    // space, and trimming it would root at a different (or missing) directory.
    let Some(root) = root.filter(|s| !s.trim().is_empty()) else {
        return ctx;
    };
    let root = PathBuf::from(root);
    if root.is_absolute() && is_dir(&root) {
        ctx.cwd = root;
        ctx.base_branch = None;
    }
    ctx
}

/// Pure parser behind [`from_env`] (testable without touching process env). Missing or
/// malformed JSON yields a minimal `{ cwd: fallback_cwd }` context (AC-26).
pub fn parse_context(json: Option<&str>, fallback_cwd: PathBuf) -> LaunchContext {
    let raw: RawContext = json
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    // Ignore empty-string fields (a malformed host value) so they fall through to the next
    // candidate / the process-cwd fallback rather than rooting at an empty path.
    let cwd = raw
        .focused_pane_cwd
        .filter(|s| !s.is_empty())
        .or(raw.workspace_cwd.filter(|s| !s.is_empty()))
        .or(raw.cwd.filter(|s| !s.is_empty()))
        .map(PathBuf::from)
        .unwrap_or(fallback_cwd);
    LaunchContext {
        cwd,
        base_branch: raw.base_branch,
        workspace_id: raw.workspace_id.filter(|s| !s.is_empty()),
    }
}
