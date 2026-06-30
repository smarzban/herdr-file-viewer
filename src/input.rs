//! Input Dispatcher — map raw key events to [`Intent`]s (AC-18).
//!
//! Keyboard-complete: every viewer function has at least one key. Unbound keys are a no-op
//! (`None`). Char bindings fire only with no active modifier, so a chord like Ctrl+C (the
//! terminal interrupt) never trips an intent. No key yields an editing action — the closed
//! [`Intent`] set has none (AC-N3).

use crate::intent::Intent;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Decode a key event into an [`Intent`], or `None` if the key is unbound.
///
/// Control-style chords (Ctrl/Alt/Super/…) never fire an intent so reserved combos (e.g.
/// Ctrl+C) stay clear; Shift is allowed, because shifted characters (`<` / `>`) are ordinary
/// typing — not a chord — and some terminals report them with the Shift bit set.
pub fn map_key(key: KeyEvent) -> Option<Intent> {
    if key.modifiers.difference(KeyModifiers::SHIFT) != KeyModifiers::NONE {
        return None;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => Some(Intent::NavUp),
        KeyCode::Down | KeyCode::Char('j') => Some(Intent::NavDown),
        KeyCode::Right | KeyCode::Char('l') => Some(Intent::Expand),
        KeyCode::Left | KeyCode::Char('h') => Some(Intent::Collapse),
        KeyCode::Enter => Some(Intent::Activate),
        KeyCode::Char('i') => Some(Intent::ToggleIgnore),
        KeyCode::Char('.') => Some(Intent::ToggleHidden),
        KeyCode::Char('c') => Some(Intent::ToggleChangedOnly),
        KeyCode::Char('b') => Some(Intent::ToggleBaseline),
        KeyCode::Char('v') => Some(Intent::CycleView),
        KeyCode::Char('e') => Some(Intent::OpenInEditor),
        KeyCode::Char('f') => Some(Intent::OpenFinder),
        KeyCode::Char(':') => Some(Intent::OpenGoToLine),
        KeyCode::Char('/') => Some(Intent::OpenSearch),
        KeyCode::Char('n') => Some(Intent::NextMatch),
        KeyCode::Char('N') => Some(Intent::PrevMatch),
        KeyCode::Char('H') => Some(Intent::TreeScrollLeft),
        KeyCode::Char('L') => Some(Intent::TreeScrollRight),
        KeyCode::Char('y') => Some(Intent::CopyRepoPath),
        KeyCode::Char('Y') => Some(Intent::CopyAbsPath),
        KeyCode::Char('W') => Some(Intent::SwitchWorktree),
        KeyCode::Tab => Some(Intent::ToggleFocus),
        KeyCode::Char('<') => Some(Intent::ShrinkTree),
        KeyCode::Char('>') => Some(Intent::GrowTree),
        KeyCode::Char('w') => Some(Intent::ToggleWrap),
        KeyCode::Char('z') => Some(Intent::ToggleZoom),
        KeyCode::Char('r') => Some(Intent::Refresh),
        KeyCode::Char('u') => Some(Intent::DismissUpdate),
        KeyCode::Char('?') => Some(Intent::ShowHelp),
        KeyCode::Char('q') | KeyCode::Esc => Some(Intent::Close),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The canonical key bindings, also used to prove keyboard-completeness.
    const BINDINGS: &[(KeyCode, Intent)] = &[
        (KeyCode::Up, Intent::NavUp),
        (KeyCode::Char('k'), Intent::NavUp),
        (KeyCode::Down, Intent::NavDown),
        (KeyCode::Char('j'), Intent::NavDown),
        (KeyCode::Right, Intent::Expand),
        (KeyCode::Char('l'), Intent::Expand),
        (KeyCode::Left, Intent::Collapse),
        (KeyCode::Char('h'), Intent::Collapse),
        (KeyCode::Enter, Intent::Activate),
        (KeyCode::Char('i'), Intent::ToggleIgnore),
        (KeyCode::Char('.'), Intent::ToggleHidden),
        (KeyCode::Char('c'), Intent::ToggleChangedOnly),
        (KeyCode::Char('b'), Intent::ToggleBaseline),
        (KeyCode::Char('v'), Intent::CycleView),
        (KeyCode::Char('e'), Intent::OpenInEditor),
        (KeyCode::Char('f'), Intent::OpenFinder),
        (KeyCode::Char(':'), Intent::OpenGoToLine),
        (KeyCode::Char('/'), Intent::OpenSearch),
        (KeyCode::Char('n'), Intent::NextMatch),
        (KeyCode::Char('N'), Intent::PrevMatch),
        (KeyCode::Char('H'), Intent::TreeScrollLeft),
        (KeyCode::Char('L'), Intent::TreeScrollRight),
        (KeyCode::Char('y'), Intent::CopyRepoPath),
        (KeyCode::Char('Y'), Intent::CopyAbsPath),
        (KeyCode::Char('W'), Intent::SwitchWorktree),
        (KeyCode::Tab, Intent::ToggleFocus),
        (KeyCode::Char('<'), Intent::ShrinkTree),
        (KeyCode::Char('>'), Intent::GrowTree),
        (KeyCode::Char('w'), Intent::ToggleWrap),
        (KeyCode::Char('z'), Intent::ToggleZoom),
        (KeyCode::Char('r'), Intent::Refresh),
        (KeyCode::Char('u'), Intent::DismissUpdate),
        (KeyCode::Char('?'), Intent::ShowHelp),
        (KeyCode::Char('q'), Intent::Close),
        (KeyCode::Esc, Intent::Close),
    ];

    #[test]
    fn bound_keys_map_to_their_intents() {
        for &(code, want) in BINDINGS {
            assert_eq!(
                map_key(k(code)),
                Some(want),
                "{code:?} should map to {want:?}"
            );
        }
    }

    #[test]
    fn every_intent_has_at_least_one_key() {
        // AC-18: keyboard-complete — no viewer function is unreachable from the keyboard.
        let reachable: HashSet<Intent> = BINDINGS
            .iter()
            .filter_map(|&(c, _)| map_key(k(c)))
            .collect();
        for intent in Intent::ALL {
            assert!(
                reachable.contains(&intent),
                "{intent:?} has no bound key (AC-18)"
            );
        }
    }

    #[test]
    fn unmapped_keys_are_a_noop() {
        assert_eq!(map_key(k(KeyCode::Char('g'))), None);
        assert_eq!(map_key(k(KeyCode::Char('x'))), None);
        assert_eq!(map_key(k(KeyCode::F(1))), None);
        assert_eq!(map_key(k(KeyCode::Backspace)), None);
    }

    #[test]
    fn modified_char_keys_do_not_trigger_intents() {
        // Ctrl+C is the terminal interrupt, not "changed-only"; Alt-chords are unbound too.
        // Accidental or reserved chords must not fire a viewer action.
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn shift_is_allowed_for_shifted_characters() {
        // '<' / '>' are typed with Shift; the resize keys must fire whether or not the
        // terminal reports the Shift bit — but a Ctrl chord on the same key must not.
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT)),
            Some(Intent::ShrinkTree)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE)),
            Some(Intent::GrowTree)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn shift_w_maps_to_switch_worktree_and_lowercase_w_stays_toggle_wrap() {
        // `W` (Shift+w, Char('W')) summons the worktree picker (AC-5/AC-N5).
        // `w` (ToggleWrap) must be unaffected — no collision.
        // A Ctrl chord on `W` must fire nothing.
        assert_eq!(map_key(k(KeyCode::Char('W'))), Some(Intent::SwitchWorktree));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT)),
            Some(Intent::SwitchWorktree)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(map_key(k(KeyCode::Char('w'))), Some(Intent::ToggleWrap));
    }

    #[test]
    fn f_maps_to_open_finder_and_modifier_chords_are_inert() {
        // `f` opens the go-to-file finder (AC-1, AC-N6). Ctrl-f / Alt-f must not fire an intent
        // (reserved terminal chords / alt-text-entry paths must stay clear).
        assert_eq!(map_key(k(KeyCode::Char('f'))), Some(Intent::OpenFinder));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT)),
            None
        );
    }

    #[test]
    fn question_mark_maps_to_show_help_and_modifier_chords_are_inert() {
        // `?` (Shift+/) opens the help overlay (AC-1, AC-N4). Ctrl-? / Alt-? must not fire an
        // intent. `?` is a shifted character — SHIFT bit set must still map.
        assert_eq!(
            map_key(k(KeyCode::Char('?'))),
            Some(Intent::ShowHelp),
            "'?' must map to ShowHelp"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT)),
            Some(Intent::ShowHelp),
            "'?' with SHIFT must still map to ShowHelp"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-? must not fire an intent"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::ALT)),
            None,
            "Alt-? must not fire an intent"
        );
    }

    #[test]
    fn colon_maps_to_open_go_to_line_and_modifier_chords_are_inert() {
        // `:` opens the go-to-line prompt (AC-1, AC-N6). Ctrl-: / Alt-: must not fire an intent.
        // (Shift is allowed — `:` is a shifted char on many layouts — so do not assert Shift is None.)
        assert_eq!(map_key(k(KeyCode::Char(':'))), Some(Intent::OpenGoToLine));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::ALT)),
            None
        );
    }

    #[test]
    fn search_keys_map_correctly_and_modifier_chords_are_inert() {
        // AC-8, AC-N6: `/` → OpenSearch, `n` → NextMatch, `N` → PrevMatch.
        // Ctrl/Alt chords on these keys must NOT fire an intent (AC-N6).
        // `N` is a shifted character (Char('N') with SHIFT) — must still map.
        assert_eq!(map_key(k(KeyCode::Char('/'))), Some(Intent::OpenSearch));
        assert_eq!(map_key(k(KeyCode::Char('n'))), Some(Intent::NextMatch));
        assert_eq!(map_key(k(KeyCode::Char('N'))), Some(Intent::PrevMatch));

        // `N` with SHIFT bit set (as some terminals report it) still maps:
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT)),
            Some(Intent::PrevMatch)
        );

        // Ctrl/Alt chords are inert (AC-N6):
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT)),
            None
        );
    }

    #[test]
    fn copy_path_keys_are_distinct_and_shift_capital_y_is_the_absolute_path() {
        // `y` copies the repo-relative path; `Y` (Shift+y, reported as `Char('Y')` with the
        // Shift bit set) copies the absolute path. A Ctrl chord on the same key fires neither.
        assert_eq!(map_key(k(KeyCode::Char('y'))), Some(Intent::CopyRepoPath));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT)),
            Some(Intent::CopyAbsPath)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn shift_h_l_map_to_tree_horizontal_scroll_and_ctrl_chords_are_inert() {
        // `H` (Shift+h) and `L` (Shift+l) scroll the tree pane left/right (AC-18 — the tree's
        // h-scroll was mouse-only). The lowercase `h`/`l` stay Collapse/Expand; no collision.
        // A Ctrl chord on the same key fires nothing.
        assert_eq!(map_key(k(KeyCode::Char('H'))), Some(Intent::TreeScrollLeft));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT)),
            Some(Intent::TreeScrollLeft),
            "H with SHIFT bit set still maps to TreeScrollLeft"
        );
        assert_eq!(
            map_key(k(KeyCode::Char('L'))),
            Some(Intent::TreeScrollRight)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT)),
            Some(Intent::TreeScrollRight),
            "L with SHIFT bit set still maps to TreeScrollRight"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-H must not fire an intent"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-L must not fire an intent"
        );
        // Lowercase h/l stay Collapse/Expand (no collision).
        assert_eq!(map_key(k(KeyCode::Char('h'))), Some(Intent::Collapse));
        assert_eq!(map_key(k(KeyCode::Char('l'))), Some(Intent::Expand));
    }
}
