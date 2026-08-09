//! Host Adapter — the herdr boundary: parse the injected launch context (AC-26).
//!
//! `HERDR_PLUGIN_CONTEXT_JSON` is parsed defensively — malformed or missing input degrades
//! to a minimal `{ cwd }` context, never a panic (AC-26).

use crate::context::LaunchContext;
use serde::Deserialize;
use std::path::PathBuf;

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
    parse_context_from(json.as_deref(), cwd, std::env::current_exe().ok())
}

/// Pure parser behind [`from_env`] (testable without touching process env). Missing or
/// malformed JSON yields a minimal `{ cwd: fallback_cwd }` context (AC-26).
pub fn parse_context(json: Option<&str>, fallback_cwd: PathBuf) -> LaunchContext {
    parse_context_from(json, fallback_cwd, None)
}

/// [`parse_context`] plus the viewer's own executable path, used to recognise (and skip) a
/// focused-pane cwd that is the plugin's OWN install directory.
///
/// Why that matters: herdr launches the pane from the plugin root (the manifest command is
/// relative), so a viewer pane's cwd *is* the plugin dir. Open the viewer while a viewer is
/// focused — the natural thing to do, since the plugin's whole point is browsing — and
/// `focused_pane_cwd` reports the plugin's install directory, rooting the new viewer at
/// `~/.config/herdr/plugins/github/herdr-file-viewer-…` instead of the user's project. Falling
/// through to `workspace_cwd` gives the workspace the user is actually working in.
///
/// Only an EXACT match or an ancestor of our own binary is skipped, so a real project that merely
/// sits above the plugin dir is unaffected, and so is any subdirectory the user browses to.
pub fn parse_context_from(
    json: Option<&str>,
    fallback_cwd: PathBuf,
    own_exe: Option<PathBuf>,
) -> LaunchContext {
    let raw: RawContext = json
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let is_own_install_dir = |candidate: &str| {
        own_exe
            .as_ref()
            .is_some_and(|exe| exe.starts_with(PathBuf::from(candidate)))
    };
    // Ignore empty-string fields (a malformed host value) so they fall through to the next
    // candidate / the process-cwd fallback rather than rooting at an empty path.
    let cwd = raw
        .focused_pane_cwd
        .filter(|s| !s.is_empty() && !is_own_install_dir(s))
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

/// Whether `dir` is the plugin's own install directory — i.e. our executable lives inside it.
pub fn is_own_install_dir(dir: &std::path::Path, own_exe: Option<&std::path::Path>) -> bool {
    own_exe.is_some_and(|exe| exe.starts_with(dir))
}

/// Pick a viewed root from the workspace's OTHER panes when the launch context only offers the
/// plugin's own install directory.
///
/// herdr derives both `focused_pane_cwd` and `workspace_cwd` from the focused pane, and a viewer
/// pane's cwd is the plugin root (its command is relative, so herdr launches it from there). Open
/// the viewer while a viewer is focused and BOTH fields therefore name the plugin's install
/// directory — the fallback chain inside [`parse_context_from`] has nothing better to offer, and
/// the tree shows the plugin's own source instead of the user's project.
///
/// This asks herdr for the panes in the same workspace and returns the first cwd that is not
/// inside our install directory. Pure over the JSON so it is testable without a live herdr; the
/// caller supplies the document.
pub fn root_from_sibling_panes(
    panes_json: &str,
    workspace_id: Option<&str>,
    own_exe: Option<&std::path::Path>,
) -> Option<PathBuf> {
    let doc: serde_json::Value = serde_json::from_str(panes_json).ok()?;
    let panes = doc.get("result")?.get("panes")?.as_array()?;
    panes
        .iter()
        .filter(|p| match workspace_id {
            // Same workspace only: another workspace's pane is a different piece of work.
            Some(id) => p.get("workspace_id").and_then(|v| v.as_str()) == Some(id),
            None => true,
        })
        .filter_map(|p| p.get("cwd").and_then(|v| v.as_str()))
        .filter(|cwd| !cwd.is_empty())
        .find(|cwd| own_exe.is_none_or(|exe| !exe.starts_with(PathBuf::from(cwd))))
        .map(PathBuf::from)
}
