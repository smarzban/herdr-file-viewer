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
        KeyCode::Char('c') => Some(Intent::ToggleChangedOnly),
        KeyCode::Char('b') => Some(Intent::ToggleBaseline),
        KeyCode::Char('v') => Some(Intent::CycleView),
        KeyCode::Char('e') => Some(Intent::OpenInEditor),
        KeyCode::Tab => Some(Intent::ToggleFocus),
        KeyCode::Char('<') => Some(Intent::ShrinkTree),
        KeyCode::Char('>') => Some(Intent::GrowTree),
        KeyCode::Char('w') => Some(Intent::ToggleWrap),
        KeyCode::Char('z') => Some(Intent::ToggleZoom),
        KeyCode::Char('r') => Some(Intent::Refresh),
        KeyCode::Char('u') => Some(Intent::DismissUpdate),
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
        (KeyCode::Char('c'), Intent::ToggleChangedOnly),
        (KeyCode::Char('b'), Intent::ToggleBaseline),
        (KeyCode::Char('v'), Intent::CycleView),
        (KeyCode::Char('e'), Intent::OpenInEditor),
        (KeyCode::Tab, Intent::ToggleFocus),
        (KeyCode::Char('<'), Intent::ShrinkTree),
        (KeyCode::Char('>'), Intent::GrowTree),
        (KeyCode::Char('w'), Intent::ToggleWrap),
        (KeyCode::Char('z'), Intent::ToggleZoom),
        (KeyCode::Char('r'), Intent::Refresh),
        (KeyCode::Char('u'), Intent::DismissUpdate),
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
}
