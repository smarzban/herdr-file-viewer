//! Launcher decision — the "launch-or-focus-or-toggle" logic behind
//! `scripts/open-file-viewer.sh`, kept in Rust (not inline shell) so it is hermetically
//! testable and so pane ids extracted from the host's `pane list` JSON are validated before
//! they reach an argv (option-injection guard).

use serde::Deserialize;

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
    workspace_id: Option<String>,
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
/// - Unparseable JSON, or no focused pane (current tab unknown) → `OPEN`.
/// - A `"Files"` pane in the **focused** tab: `CLOSE` it when it *is* the focused pane (toggle
///   off — herdr auto-closes the emptied tab), otherwise `FOCUS` it in place.
/// - Else a `"Files"` pane in **another tab of the focused pane's workspace**: `SWITCHTAB` to it.
/// - Else `OPEN`. In particular a viewer that lives only in a **different workspace** is left
///   alone and a fresh viewer is opened here — switching to it would yank the user across
///   workspaces (the launcher is meant to reach *this* workspace's viewer, not teleport away).
/// - A pane/tab id that is not flag-safe is never emitted (→ `OPEN`), so a host-supplied id can
///   never option-inject when the launcher passes it to `herdr pane`/`herdr tab`.
pub fn launch_decision_tab(pane_list_json: &str) -> String {
    let Ok(list) = serde_json::from_str::<PaneList>(pane_list_json) else {
        return "OPEN".to_string();
    };
    let panes = &list.result.panes;
    let Some(focused) = panes.iter().find(|p| p.focused) else {
        return "OPEN".to_string();
    };
    let is_viewer = |p: &&Pane| p.label.as_deref() == Some("Files");

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
    if focused_ws.is_some()
        && let Some(elsewhere) = panes
            .iter()
            .find(|p| is_viewer(p) && workspace_of(p) == focused_ws)
        && let Some(tab) = elsewhere.tab_id.as_deref().filter(|t| is_flag_safe(t))
    {
        return format!("SWITCHTAB {tab}");
    }
    "OPEN".to_string()
}

/// The workspace a pane belongs to: herdr's explicit `workspace_id` when present, else the
/// prefix of its `tab_id`/`pane_id` (herdr ids are `workspace:...` tokens, e.g. `w19:tB`).
/// `None` when nothing identifies the workspace, so the caller degrades to `OPEN` rather than
/// guess.
fn workspace_of(p: &Pane) -> Option<&str> {
    p.workspace_id
        .as_deref()
        .or_else(|| p.tab_id.as_deref().and_then(|t| t.split(':').next()))
        .or_else(|| p.pane_id.as_deref().and_then(|t| t.split(':').next()))
        .filter(|w| !w.is_empty())
}

/// A pane id is safe to place in an argv iff it is a non-empty token of `[A-Za-z0-9_:.-]` that
/// does not start with `-` (which would option-inject). `:` and `.` are allowed because herdr
/// pane ids are `workspace:pane` tokens (e.g. `wE:pD`).
fn is_flag_safe(id: &str) -> bool {
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
        // Derive workspace_id from the id prefix (herdr ids are `workspace:...`), mirroring the
        // real `pane list` payload, so fixtures exercise the workspace-scoping path.
        let ws = tab.split(':').next().unwrap_or("");
        format!(
            r#"{{"pane_id":"{id}","label":"{label}","focused":{focused},"tab_id":"{tab}","workspace_id":"{ws}"}}"#
        )
    }
    fn list(panes: &[String]) -> String {
        format!(r#"{{"result":{{"panes":[{}]}}}}"#, panes.join(","))
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
        assert_eq!(launch_decision_tab(&j), "OPEN");
    }

    #[test]
    fn tab_viewer_focused_closes() {
        // On the viewer's own tab with it focused → toggle off (close the pane; herdr auto-
        // closes the now-empty tab).
        let j = list(&[
            pane("wE:p1", "", false, "wE:t1"),
            pane("wE:pD", "Files", true, "wE:t4"),
        ]);
        assert_eq!(launch_decision_tab(&j), "CLOSE wE:pD");
    }

    #[test]
    fn tab_viewer_in_another_tab_switches_to_that_tab() {
        // THE key difference from the pane launcher: a viewer in a different tab is switched to
        // (by tab id), not duplicated.
        let j = list(&[
            pane("wE:p1", "", true, "wE:t1"),
            pane("wE:pD", "Files", false, "wE:t4"),
        ]);
        assert_eq!(launch_decision_tab(&j), "SWITCHTAB wE:t4");
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
        assert_eq!(launch_decision_tab(&j), "OPEN");
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
        assert_eq!(launch_decision_tab(&j), "SWITCHTAB wQ:tE");
    }

    #[test]
    fn tab_viewer_in_current_tab_unfocused_is_focused() {
        // Edge: the viewer was split into the current tab and isn't focused → focus it in place.
        let j = list(&[
            pane("wE:p1", "", true, "wE:t1"),
            pane("wE:pD", "Files", false, "wE:t1"),
        ]);
        assert_eq!(launch_decision_tab(&j), "FOCUS wE:pD");
    }

    #[test]
    fn tab_no_focused_pane_opens() {
        let j = list(&[pane("wE:pD", "Files", false, "wE:t4")]);
        assert_eq!(launch_decision_tab(&j), "OPEN");
    }

    #[test]
    fn tab_unsafe_tab_id_is_never_emitted() {
        // A tab id that could option-inject `herdr tab focus <id>` must degrade to OPEN.
        let j = list(&[
            pane("wE:p1", "", true, "wE:t1"),
            pane("wE:pD", "Files", false, "-rf"),
        ]);
        assert_eq!(launch_decision_tab(&j), "OPEN");
    }

    #[test]
    fn tab_unsafe_pane_id_is_never_emitted() {
        let j = list(&[
            pane("wE:p1", "", false, "wE:t4"),
            pane("-rf", "Files", true, "wE:t4"),
        ]);
        assert_eq!(launch_decision_tab(&j), "OPEN");
    }

    #[test]
    fn tab_garbage_json_opens() {
        assert_eq!(launch_decision_tab("not json"), "OPEN");
        assert_eq!(launch_decision_tab(""), "OPEN");
    }
}
