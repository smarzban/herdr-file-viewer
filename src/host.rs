//! Host Adapter — the herdr boundary: parse the injected launch context (AC-26).
//!
//! `HERDR_PLUGIN_CONTEXT_JSON` is parsed defensively — malformed or missing input degrades
//! to a minimal `{ cwd }` context, never a panic (AC-26).
//!
//! It also parses the launcher's own placement marker ([`PLACEMENT_ENV`] → [`Placement`]) — which
//! herdr layout summoned this viewer. That is *not* part of the host's context JSON; see
//! [`PLACEMENT_ENV`] for why we mark our own argv instead.

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

/// The env var every unix launcher script sets on its `plugin pane open` argv
/// (`--env HERDR_FILE_VIEWER_PLACEMENT=split|tab|overlay|popup`) so the viewer knows which herdr
/// layout it was summoned into.
///
/// It is a marker we set ourselves, not something herdr reports: a pane row carries no placement
/// field and no per-pane zoom flag (`$defs.PaneInfo`, verified 2026-08-07 against herdr 0.8.0), and
/// a popup is not a pane at all. The marker is entirely under our control, is a plain string to
/// parse, and is hermetically testable — hence this rather than sniffing `HERDR_PANE_ID`.
///
/// The const lives beside its parser, the same convention `crate::open_target::OPEN_ENV` follows.
pub const PLACEMENT_ENV: &str = "HERDR_FILE_VIEWER_PLACEMENT";

/// Which herdr layout the viewer was summoned into, per the [`PLACEMENT_ENV`] marker.
///
/// The one behaviour that keys off it is the `Z` host zoom (`controller::Controller::host_zoom`),
/// which is skipped for [`Overlay`](Placement::Overlay) and [`Popup`](Placement::Popup):
/// * a **popup** is not a pane, so `pane zoom --current` would resolve to the *underlying* focused
///   pane and zoom someone else's window (and later un-zoom it);
/// * an **overlay** is already covering — herdr implements it as a split whose new half is
///   tab-zoomed — so zooming it is redundant and fights herdr's own restore-on-close.
///
/// [`Unknown`](Placement::Unknown) — the marker absent or unrecognized — means "not launched by one
/// of our unix launchers": a direct `plugin pane open`, or one of the Windows `.ps1` launchers,
/// which spawn a genuine split pane via `pane split` + `pane run` and therefore *want* the host
/// zoom. So `Unknown` deliberately keeps today's behaviour rather than failing closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// `scripts/open-file-viewer.sh` — a split pane beside the current work.
    Split,
    /// `scripts/open-file-viewer-tab.sh` — the viewer in its own tab.
    Tab,
    /// `scripts/open-file-viewer-overlay.sh` — a temporary cover over the active pane.
    Overlay,
    /// `scripts/open-file-viewer-popup.sh` — a floating, session-modal popup (not a pane).
    Popup,
    /// No marker, or one this build doesn't recognize. Keeps the pre-marker behaviour.
    #[default]
    Unknown,
}

/// Pure parser for the [`PLACEMENT_ENV`] marker. Surrounding whitespace is tolerated (a stray
/// newline must not silently re-enable the host zoom inside a popup); anything else unrecognized —
/// including an absent or empty value — is [`Placement::Unknown`].
pub fn parse_placement(raw: Option<&str>) -> Placement {
    match raw.map(str::trim) {
        Some("split") => Placement::Split,
        Some("tab") => Placement::Tab,
        Some("overlay") => Placement::Overlay,
        Some("popup") => Placement::Popup,
        _ => Placement::Unknown,
    }
}

/// Build a `LaunchContext` from the process environment: the injected context JSON, falling
/// back to the process working directory. Never panics (AC-26).
pub fn from_env() -> LaunchContext {
    let json = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok();
    let cwd = std::env::current_dir().unwrap_or_default();
    parse_context(json.as_deref(), cwd)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_placement_maps_every_launcher_marker() {
        // The four markers the unix launchers set on their `--env` flag. Pinned here so a rename in
        // a script (or here) can't silently degrade a popup to `Unknown` — which would let `Z` zoom
        // the pane *underneath* the popup.
        assert_eq!(parse_placement(Some("split")), Placement::Split);
        assert_eq!(parse_placement(Some("tab")), Placement::Tab);
        assert_eq!(parse_placement(Some("overlay")), Placement::Overlay);
        assert_eq!(parse_placement(Some("popup")), Placement::Popup);
    }

    #[test]
    fn parse_placement_tolerates_surrounding_whitespace() {
        // A stray newline around the marker must not cost a popup its host-zoom guard.
        assert_eq!(parse_placement(Some(" popup\n")), Placement::Popup);
        assert_eq!(parse_placement(Some("\toverlay ")), Placement::Overlay);
    }

    #[test]
    fn parse_placement_is_unknown_when_absent_or_unrecognized() {
        // Absent = launched some other way (a direct `plugin pane open`, or a Windows .ps1
        // launcher, which really does open a split pane) — `Unknown` keeps the pre-marker
        // behaviour, host zoom included. Unrecognized values degrade the same way rather than
        // guessing.
        assert_eq!(parse_placement(None), Placement::Unknown);
        assert_eq!(parse_placement(Some("")), Placement::Unknown);
        assert_eq!(parse_placement(Some("   ")), Placement::Unknown);
        assert_eq!(parse_placement(Some("zoomed")), Placement::Unknown);
        assert_eq!(parse_placement(Some("Popup")), Placement::Unknown);
    }

    #[test]
    fn placement_defaults_to_unknown() {
        // The controller's field default: a viewer whose launcher never called the setter behaves
        // exactly as it did before the marker existed.
        assert_eq!(Placement::default(), Placement::Unknown);
    }
}
