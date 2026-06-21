//! Launcher decision — the "launch-or-focus-or-toggle" logic behind
//! `scripts/open-file-viewer.sh`, kept in Rust (not inline shell) so it is hermetically
//! testable and so pane ids extracted from the host's `pane list` JSON are validated before
//! they reach an argv (option-injection guard), mirroring the editor hand-off's discipline in
//! [`crate::host`].

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

/// A pane id is safe to place in an argv iff it is a non-empty token of `[A-Za-z0-9_:.-]` that
/// does not start with `-` (which would option-inject). Mirrors `host::is_safe_pane_id`'s
/// anti-option-injection intent, but also allows `:` and `.` because herdr pane ids are
/// `workspace:pane` tokens (e.g. `wE:pD`).
fn is_flag_safe(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, label: &str, focused: bool, tab: &str) -> String {
        format!(r#"{{"pane_id":"{id}","label":"{label}","focused":{focused},"tab_id":"{tab}"}}"#)
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
        let j = list(&[pane("wE:p1", "", false, "wE:t1"), pane("wE:pD", "Files", true, "wE:t1")]);
        assert_eq!(launch_decision(&j), "CLOSE wE:pD");
    }

    #[test]
    fn files_pane_unfocused_in_current_tab_is_focused() {
        let j = list(&[pane("wE:p1", "", true, "wE:t1"), pane("wE:pD", "Files", false, "wE:t1")]);
        assert_eq!(launch_decision(&j), "FOCUS wE:pD");
    }

    #[test]
    fn files_pane_in_another_tab_is_ignored() {
        // The focused pane is in tab wE:t1; a Files pane in wC:t1 must not be touched.
        let j = list(&[pane("wE:p1", "", true, "wE:t1"), pane("wC:pD", "Files", false, "wC:t1")]);
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
        let j = list(&[pane("wE:p1", "", true, "wE:t1"), pane("-rf", "Files", false, "wE:t1")]);
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
}
