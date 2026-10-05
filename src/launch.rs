//! Launcher decision — the "launch-or-focus-or-toggle" logic behind
//! `scripts/open-file-viewer.sh`, kept in Rust (not inline shell) so it is hermetically
//! testable and so pane ids extracted from the host's `pane list` JSON are validated before
//! they reach an argv (option-injection guard).

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct PaneList {
    result: PaneListResult,
}
#[derive(Deserialize)]
struct PaneListResult {
    #[serde(default)]
    panes: Vec<Pane>,
}
#[derive(Deserialize)]
struct Pane {
    pane_id: Option<String>,
    label: Option<String>,
    #[serde(default)]
    focused: bool,
    tab_id: Option<String>,
    /// The pane process's live working directory. A viewer keeps its cwd on its tree root
    /// (`app::follow_root`), so for a `"Files"` pane this names the root it is showing.
    cwd: Option<String>,
}

/// Decide the launcher action from a herdr `pane list` JSON, returning one line: `OPEN`,
/// `FOCUS <pane_id>`, or `CLOSE <pane_id>`.
///
/// - Unparseable JSON, or **no focused pane** (we cannot know which tab is current) → `OPEN`:
///   the safe default is to spawn a fresh viewer, never to act on a pane in an unknown tab.
/// - A `"Files"` pane **in the focused pane's tab**: `CLOSE` it when it *is* the focused pane
///   ("toggle off"), otherwise `FOCUS` it. A Files pane in any other tab is ignored.
/// - A pane id that is not flag-safe is never emitted (→ `OPEN`), so a host-supplied id can
///   never option-inject when the launcher passes it to `herdr pane zoom|close`.
pub fn launch_decision(pane_list_json: &str) -> String {
    let Ok(list) = serde_json::from_str::<PaneList>(pane_list_json) else {
        return "OPEN".to_string();
    };
    let panes = &list.result.panes;
    // No focused pane → we cannot tell which tab is current, so open a fresh viewer rather
    // than risk focusing/closing a Files pane in some other tab.
    let Some(focused) = panes.iter().find(|p| p.focused) else {
        return "OPEN".to_string();
    };
    let tab = focused.tab_id.as_deref();
    let files = panes
        .iter()
        .find(|p| p.label.as_deref() == Some("Files") && p.tab_id.as_deref() == tab);
    let Some(files) = files else {
        return "OPEN".to_string();
    };
    // Never emit a pane id that could option-inject `herdr pane zoom|close <id>`.
    let Some(id) = files.pane_id.as_deref().filter(|id| is_flag_safe(id)) else {
        return "OPEN".to_string();
    };
    if Some(id) == focused.pane_id.as_deref() {
        format!("CLOSE {id}")
    } else {
        format!("FOCUS {id}")
    }
}

/// Decide the launcher action for the **tab** variant (`scripts/open-file-viewer-tab.sh`),
/// returning one line: `OPEN`, `SWITCHTAB <tab_id>`, `FOCUS <pane_id>`, or `CLOSE <pane_id>`.
///
/// Like [`launch_decision`] but tab-scoped: a `"Files"` pane in *another tab of the same
/// workspace* is **switched to** (`herdr tab focus <tab_id>`) rather than duplicated — the
/// idempotency that makes a single keystroke reach the one viewer in this workspace.
///
/// **Root-aware.** `resolve_root` maps a pane cwd to its tree root (worktree top level, else the
/// directory itself), canonicalized so both sides compare equal. Only a viewer showing the
/// **focused pane's root** is switched to, so a viewer opened elsewhere by the root picker
/// (`--pick-root`) is not mistaken for this repo's viewer. When the focused pane's root cannot be
/// resolved (no `cwd` reported, or a path that no longer exists) the decision stays root-agnostic,
/// as it was before roots were tracked.
///
/// - Unparseable JSON, or no focused pane (current tab unknown) → `OPEN`.
/// - A `"Files"` pane in the **focused** tab: `CLOSE` it when it *is* the focused pane (toggle
///   off — herdr auto-closes the emptied tab), otherwise `FOCUS` it in place. This is the toggle,
///   so it is deliberately not root-scoped: a viewer tab holds only that viewer.
/// - Else a `"Files"` pane **showing the focused root** in **another tab of the focused pane's
///   workspace**: `SWITCHTAB` to it. A viewer there showing some other root does not count.
/// - Else `OPEN`. In particular a viewer that lives only in a **different workspace** is left
///   alone and a fresh viewer is opened here — switching to it would yank the user across
///   workspaces (the launcher is meant to reach *this* workspace's viewer, not teleport away).
/// - A pane/tab id that is not flag-safe is never emitted (→ `OPEN`), so a host-supplied id can
///   never option-inject when the launcher passes it to `herdr pane`/`herdr tab`.
pub fn launch_decision_tab(
    pane_list_json: &str,
    resolve_root: impl Fn(&Path) -> Option<PathBuf>,
) -> String {
    let Ok(list) = serde_json::from_str::<PaneList>(pane_list_json) else {
        return "OPEN".to_string();
    };
    let panes = &list.result.panes;
    let Some(focused) = panes.iter().find(|p| p.focused) else {
        return "OPEN".to_string();
    };
    let is_viewer = |p: &&Pane| p.label.as_deref() == Some("Files");
    let root_of = |p: &Pane| {
        p.cwd
            .as_deref()
            .filter(|c| !c.is_empty())
            .and_then(|c| resolve_root(Path::new(c)))
    };

    // Prefer a viewer in the focused tab (toggle/focus in place) over one elsewhere.
    if let Some(here) = panes
        .iter()
        .find(|p| is_viewer(p) && p.tab_id.as_deref() == focused.tab_id.as_deref())
    {
        let Some(id) = here.pane_id.as_deref().filter(|id| is_flag_safe(id)) else {
            return "OPEN".to_string();
        };
        return if Some(id) == focused.pane_id.as_deref() {
            format!("CLOSE {id}")
        } else {
            format!("FOCUS {id}")
        };
    }

    // Otherwise switch to a viewer living in another tab OF THE SAME WORKSPACE, by its
    // (validated) tab id. A viewer in a different workspace is deliberately ignored: switching
    // to it would pull the user out of their current workspace, so we OPEN a fresh viewer here
    // instead. If the focused pane's workspace is unknown, we can't scope safely → OPEN.
    let focused_ws = workspace_of(focused);
    let focused_root = root_of(focused);
    let shows_focused_root = |p: &Pane| match &focused_root {
        Some(want) => root_of(p).as_ref() == Some(want),
        None => true,
    };
    if focused_ws.is_some()
        && let Some(elsewhere) = panes
            .iter()
            .find(|p| is_viewer(p) && workspace_of(p) == focused_ws && shows_focused_root(p))
        && let Some(tab) = elsewhere.tab_id.as_deref().filter(|t| is_flag_safe(t))
    {
        return format!("SWITCHTAB {tab}");
    }
    "OPEN".to_string()
}

/// The workspace a pane belongs to, taken from the prefix of the id we actually act on — its
/// `tab_id` (what `SWITCHTAB` emits), falling back to its `pane_id`. herdr ids are `workspace:…`
/// tokens (e.g. `w19:tB`), so the segment before the first `:` is the workspace. Deriving the
/// scope from the acted-upon id — rather than the separate `workspace_id` field, which a
/// malformed `pane list` could set to a value that disagrees with the tab we would switch to —
/// keeps the scope check and the emitted target self-consistent: we compare and switch on the
/// same id. `None` when the id carries no `workspace:` prefix (an unqualified id is treated as
/// unknown, never as a bare workspace), so the caller degrades to `OPEN` rather than guess.
fn workspace_of(p: &Pane) -> Option<&str> {
    p.tab_id
        .as_deref()
        .and_then(|t| t.split_once(':'))
        .or_else(|| p.pane_id.as_deref().and_then(|t| t.split_once(':')))
        .map(|(ws, _)| ws)
        .filter(|w| !w.is_empty())
}

/// A pane id is safe to place in an argv iff it is a non-empty token of `[A-Za-z0-9_:.-]` that
/// does not start with `-` (which would option-inject). `:` and `.` are allowed because herdr
/// pane ids are `workspace:pane` tokens (e.g. `wE:pD`).
pub(crate) fn is_flag_safe(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, label: &str, focused: bool, tab: &str) -> String {
        // Mirror the real `pane list` payload, including the `workspace_id` field herdr emits
        // (derived here from the id prefix) — the decision logic deliberately ignores it and
        // scopes off the `tab_id` prefix instead, so fixtures carry it to prove it is unused.
        let ws = tab.split(':').next().unwrap_or("");
        pane_ws(id, label, focused, tab, ws)
    }
    // Like `pane`, but with an explicit `workspace_id` so a test can make it DISAGREE with the
    // `tab_id` prefix (a malformed/hostile `pane list`).
    fn pane_ws(id: &str, label: &str, focused: bool, tab: &str, ws: &str) -> String {
        format!(
            r#"{{"pane_id":"{id}","label":"{label}","focused":{focused},"tab_id":"{tab}","workspace_id":"{ws}"}}"#
        )
    }
    fn list(panes: &[String]) -> String {
        format!(r#"{{"result":{{"panes":[{}]}}}}"#, panes.join(","))
    }
    // Like `pane`, plus the pane's live `cwd` (a viewer's cwd is its tree root).
    fn pane_cwd(id: &str, label: &str, focused: bool, tab: &str, cwd: &str) -> String {
        let ws = tab.split(':').next().unwrap_or("");
        format!(
            r#"{{"pane_id":"{id}","label":"{label}","focused":{focused},"tab_id":"{tab}","workspace_id":"{ws}","cwd":"{cwd}"}}"#
        )
    }
    // A resolver that knows no roots: the root-agnostic decision (fixtures without `cwd`).
    fn no_roots(_: &Path) -> Option<PathBuf> {
        None
    }
    // A canned resolver standing in for `root::resolve` + canonicalize: anything under `/repo`
    // resolves to `/repo`, under `/other` to `/other`; `/gone` no longer exists.
    fn roots(p: &Path) -> Option<PathBuf> {
        ["/repo", "/other"]
            .iter()
            .find(|r| p.starts_with(r))
            .map(PathBuf::from)
            .or_else(|| (!p.starts_with("/gone")).then(|| p.to_path_buf()))
    }

    #[test]
    fn no_files_pane_opens() {
        let j = list(&[pane("wE:p1", "", true, "wE:t1")]);
        assert_eq!(launch_decision(&j), "OPEN");
    }

    #[test]
    fn files_pane_focused_closes() {
        let j = list(&[
            pane("wE:p1", "", false, "wE:t1"),
            pane("wE:pD", "Files", true, "wE:t1"),
        ]);
        assert_eq!(launch_decision(&j), "CLOSE wE:pD");
    }

    #[test]
    fn files_pane_unfocused_in_current_tab_is_focused() {
        let j = list(&[
            pane("wE:p1", "", true, "wE:t1"),
            pane("wE:pD", "Files", false, "wE:t1"),
        ]);
        assert_eq!(launch_decision(&j), "FOCUS wE:pD");
    }

    #[test]
    fn files_pane_in_another_tab_is_ignored() {
        // The focused pane is in tab wE:t1; a Files pane in wC:t1 must not be touched.
        let j = list(&[
            pane("wE:p1", "", true, "wE:t1"),
            pane("wC:pD", "Files", false, "wC:t1"),
        ]);
        assert_eq!(launch_decision(&j), "OPEN");
    }

    #[test]
    fn no_focused_pane_opens_rather_than_touching_an_unknown_tab() {
        let j = list(&[pane("wE:pD", "Files", false, "wE:t1")]);
        assert_eq!(launch_decision(&j), "OPEN");
    }

    #[test]
    fn unsafe_pane_id_is_never_emitted() {
        // A pane id beginning with '-' could option-inject `herdr pane zoom <id>`; it must
        // degrade to OPEN, never FOCUS/CLOSE.
        let j = list(&[
            pane("wE:p1", "", true, "wE:t1"),
            pane("-rf", "Files", false, "wE:t1"),
        ]);
        assert_eq!(launch_decision(&j), "OPEN");
    }

    #[test]
    fn garbage_json_opens() {
        assert_eq!(launch_decision("not json"), "OPEN");
        assert_eq!(launch_decision(""), "OPEN");
    }

    #[test]
    fn flag_safe_accepts_real_colon_ids_and_rejects_dangerous_ones() {
        assert!(is_flag_safe("wE:pD"));
        assert!(!is_flag_safe("-rf"));
        assert!(!is_flag_safe(""));
        assert!(!is_flag_safe("a b"));
    }

    // ---- tab launcher (`launch_decision_tab`) -----------------------------------------

    #[test]
    fn tab_no_files_anywhere_opens() {
        let j = list(&[pane("wE:p1", "", true, "wE:t1")]);
        assert_eq!(launch_decision_tab(&j, no_roots), "OPEN");
    }

    #[test]
    fn tab_viewer_focused_closes() {
        // On the viewer's own tab with it focused → toggle off (close the pane; herdr auto-
        // closes the now-empty tab).
        let j = list(&[
            pane("wE:p1", "", false, "wE:t1"),
            pane("wE:pD", "Files", true, "wE:t4"),
        ]);
        assert_eq!(launch_decision_tab(&j, no_roots), "CLOSE wE:pD");
    }

    #[test]
    fn tab_viewer_in_another_tab_switches_to_that_tab() {
        // THE key difference from the pane launcher: a viewer in a different tab is switched to
        // (by tab id), not duplicated.
        let j = list(&[
            pane("wE:p1", "", true, "wE:t1"),
            pane("wE:pD", "Files", false, "wE:t4"),
        ]);
        assert_eq!(launch_decision_tab(&j, no_roots), "SWITCHTAB wE:t4");
    }

    #[test]
    fn tab_switches_only_to_a_viewer_showing_the_focused_root() {
        // A shell in a subdirectory of /repo: the /other viewer (opened by the root picker) is
        // skipped, the /repo viewer is switched to even though it is listed second.
        let j = list(&[
            pane_cwd("wE:p1", "", true, "wE:t1", "/repo/src/deep"),
            pane_cwd("wE:pA", "Files", false, "wE:t2", "/other"),
            pane_cwd("wE:pB", "Files", false, "wE:t3", "/repo"),
        ]);
        assert_eq!(launch_decision_tab(&j, roots), "SWITCHTAB wE:t3");
    }

    #[test]
    fn tab_opens_when_the_only_other_viewer_shows_a_different_root() {
        // The picker-opened viewer at /other must not swallow this repo's summon.
        let j = list(&[
            pane_cwd("wE:p1", "", true, "wE:t1", "/repo"),
            pane_cwd("wE:pA", "Files", false, "wE:t2", "/other"),
        ]);
        assert_eq!(launch_decision_tab(&j, roots), "OPEN");
    }

    #[test]
    fn tab_viewer_without_a_known_root_is_not_switched_to_when_the_focused_root_is_known() {
        // A viewer whose cwd is missing (or no longer exists) cannot prove it shows this root.
        let j = list(&[
            pane_cwd("wE:p1", "", true, "wE:t1", "/repo"),
            pane("wE:pA", "Files", false, "wE:t2"),
            pane_cwd("wE:pB", "Files", false, "wE:t3", "/gone/x"),
        ]);
        assert_eq!(launch_decision_tab(&j, roots), "OPEN");
    }

    #[test]
    fn tab_unresolvable_focused_root_stays_root_agnostic() {
        // No usable focused cwd: behave exactly as before roots were tracked.
        let j = list(&[
            pane_cwd("wE:p1", "", true, "wE:t1", "/gone/x"),
            pane_cwd("wE:pA", "Files", false, "wE:t2", "/other"),
        ]);
        assert_eq!(launch_decision_tab(&j, roots), "SWITCHTAB wE:t2");
    }

    #[test]
    fn tab_same_tab_toggle_is_not_root_scoped() {
        // The focused-tab FOCUS/CLOSE is the toggle: pressing it ON a picker-opened viewer
        // (root /other) while the resolver maps it elsewhere still closes it.
        let j = list(&[pane_cwd("wE:pA", "Files", true, "wE:t2", "/other")]);
        assert_eq!(launch_decision_tab(&j, roots), "CLOSE wE:pA");
    }

    #[test]
    fn tab_viewer_only_in_another_workspace_opens_here() {
        // Regression (cross-workspace jump): the focused pane is in workspace wQ; the only Files
        // viewer lives in workspace w19. Switching to it would yank the user out of wQ, so the
        // launcher must OPEN a fresh viewer in the current workspace instead.
        let j = list(&[
            pane("wQ:p2K", "", true, "wQ:tH"),
            pane("w19:pT", "Files", false, "w19:tB"),
        ]);
        assert_eq!(launch_decision_tab(&j, no_roots), "OPEN");
    }

    #[test]
    fn tab_prefers_a_same_workspace_viewer_over_one_in_another_workspace() {
        // A viewer exists both in another tab of THIS workspace and in a different workspace →
        // switch to the one in this workspace, never the foreign one.
        let j = list(&[
            pane("wQ:p2K", "", true, "wQ:tH"),
            pane("wQ:pV", "Files", false, "wQ:tE"),
            pane("w19:pT", "Files", false, "w19:tB"),
        ]);
        assert_eq!(launch_decision_tab(&j, no_roots), "SWITCHTAB wQ:tE");
    }

    #[test]
    fn tab_spoofed_workspace_id_cannot_force_a_cross_workspace_switch() {
        // Hostile/malformed `pane list`: the viewer's `workspace_id` is spoofed to the focused
        // workspace (wQ) while its `tab_id` still names another workspace (w19:tB). Because the
        // scope is derived from the `tab_id` prefix we actually switch on — not the separate
        // `workspace_id` field — the mismatch is caught and we OPEN here, never SWITCHTAB across.
        let j = list(&[
            pane("wQ:p2K", "", true, "wQ:tH"),
            pane_ws("w19:pT", "Files", false, "w19:tB", "wQ"),
        ]);
        assert_eq!(launch_decision_tab(&j, no_roots), "OPEN");
    }

    #[test]
    fn tab_id_without_a_workspace_prefix_opens_rather_than_guessing() {
        // An id with no `workspace:` delimiter has an unknown workspace; it must not be treated as
        // a bare workspace that could spuriously match. Focused id `p2K` (no `:`) → OPEN.
        let j = list(&[
            pane_ws("p2K", "", true, "tH", ""),
            pane("w19:pT", "Files", false, "w19:tB"),
        ]);
        assert_eq!(launch_decision_tab(&j, no_roots), "OPEN");
    }

    #[test]
    fn tab_viewer_in_current_tab_unfocused_is_focused() {
        // Edge: the viewer was split into the current tab and isn't focused → focus it in place.
        let j = list(&[
            pane("wE:p1", "", true, "wE:t1"),
            pane("wE:pD", "Files", false, "wE:t1"),
        ]);
        assert_eq!(launch_decision_tab(&j, no_roots), "FOCUS wE:pD");
    }

    #[test]
    fn tab_no_focused_pane_opens() {
        let j = list(&[pane("wE:pD", "Files", false, "wE:t4")]);
        assert_eq!(launch_decision_tab(&j, no_roots), "OPEN");
    }

    #[test]
    fn tab_unsafe_tab_id_is_never_emitted() {
        // A tab id that could option-inject `herdr tab focus <id>` must degrade to OPEN.
        let j = list(&[
            pane("wE:p1", "", true, "wE:t1"),
            pane("wE:pD", "Files", false, "-rf"),
        ]);
        assert_eq!(launch_decision_tab(&j, no_roots), "OPEN");
    }

    #[test]
    fn tab_unsafe_pane_id_is_never_emitted() {
        let j = list(&[
            pane("wE:p1", "", false, "wE:t4"),
            pane("-rf", "Files", true, "wE:t4"),
        ]);
        assert_eq!(launch_decision_tab(&j, no_roots), "OPEN");
    }

    #[test]
    fn tab_garbage_json_opens() {
        assert_eq!(launch_decision_tab("not json", no_roots), "OPEN");
        assert_eq!(launch_decision_tab("", no_roots), "OPEN");
    }
}
