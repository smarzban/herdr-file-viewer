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
        KeyCode::Char('Z') => Some(Intent::OpenFullscreen),
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
        KeyCode::Char('O') => Some(Intent::OpenWithApp),
        KeyCode::Char('R') => Some(Intent::RevealInFileManager),
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

/// One row of the [keybinding registry](REGISTRY): a single [`Intent`] paired with its stable
/// snake_case name, its default key(s), and a human description.
///
/// This is the single source of truth for each global action's default binding. The dispatcher,
/// the help overlay, and the README docs-consistency check all derive from it, so a key, its
/// config name, and its description live in exactly one place. `default_keys` reproduces the
/// current [`map_key`] bindings verbatim; `name` is the public `[keys]` config key.
#[allow(dead_code)] // T-1 foundation: consumed by tests here; the dispatcher/overlay wire it in T-2+.
pub(crate) struct Binding {
    /// The global action this row binds.
    pub intent: Intent,
    /// The stable snake_case identifier, used as the `[keys]` config key (e.g. `nav_up`).
    pub name: &'static str,
    /// The default key(s) that decode to `intent` absent any user config. Non-empty.
    pub default_keys: &'static [KeyCode],
    /// A concise, human-readable one-liner describing the action.
    pub description: &'static str,
}

/// The keybinding registry: one [`Binding`] per [`Intent::ALL`] member, in that order.
///
/// The `default_keys` mirror [`map_key`] exactly (behavior-preserving foundation). Invariants,
/// all test-enforced rather than at runtime: every `Intent::ALL` member appears exactly once
/// (AC-2), no two rows share a `name` (AC-5), and no [`KeyCode`] is a default of two rows (AC-3).
#[allow(dead_code)] // T-1 foundation: consumed by tests here; the dispatcher/overlay wire it in T-2+.
pub(crate) const REGISTRY: &[Binding] = &[
    Binding {
        intent: Intent::NavUp,
        name: "nav_up",
        default_keys: &[KeyCode::Up, KeyCode::Char('k')],
        description: "Move the tree cursor up one row.",
    },
    Binding {
        intent: Intent::NavDown,
        name: "nav_down",
        default_keys: &[KeyCode::Down, KeyCode::Char('j')],
        description: "Move the tree cursor down one row.",
    },
    Binding {
        intent: Intent::Expand,
        name: "expand",
        default_keys: &[KeyCode::Right, KeyCode::Char('l')],
        description: "Expand the selected directory.",
    },
    Binding {
        intent: Intent::Collapse,
        name: "collapse",
        default_keys: &[KeyCode::Left, KeyCode::Char('h')],
        description: "Collapse the selected directory.",
    },
    Binding {
        intent: Intent::Activate,
        name: "activate",
        default_keys: &[KeyCode::Enter],
        description: "Activate the selected node: expand/collapse a directory, or open a file.",
    },
    Binding {
        intent: Intent::OpenFullscreen,
        name: "open_fullscreen",
        default_keys: &[KeyCode::Char('Z')],
        description: "Toggle full-screen reading of the selected file (in-pane plus herdr pane zoom).",
    },
    Binding {
        intent: Intent::ToggleIgnore,
        name: "toggle_ignore",
        default_keys: &[KeyCode::Char('i')],
        description: "Reveal or hide gitignored files.",
    },
    Binding {
        intent: Intent::ToggleHidden,
        name: "toggle_hidden",
        default_keys: &[KeyCode::Char('.')],
        description: "Hide or reveal dot-prefixed (hidden) files and folders.",
    },
    Binding {
        intent: Intent::ToggleChangedOnly,
        name: "toggle_changed_only",
        default_keys: &[KeyCode::Char('c')],
        description: "Restrict the tree to changed files, or restore the full tree.",
    },
    Binding {
        intent: Intent::ToggleBaseline,
        name: "toggle_baseline",
        default_keys: &[KeyCode::Char('b')],
        description: "Switch the diff baseline between base-branch and HEAD.",
    },
    Binding {
        intent: Intent::CycleView,
        name: "cycle_view",
        default_keys: &[KeyCode::Char('v')],
        description: "Cycle the content pane's view mode.",
    },
    Binding {
        intent: Intent::OpenInEditor,
        name: "open_in_editor",
        default_keys: &[KeyCode::Char('e')],
        description: "Hand the selected file off to an external editor.",
    },
    Binding {
        intent: Intent::OpenWithApp,
        name: "open_with_app",
        default_keys: &[KeyCode::Char('O')],
        description: "Open the selected entry with the OS default application.",
    },
    Binding {
        intent: Intent::RevealInFileManager,
        name: "reveal_in_file_manager",
        default_keys: &[KeyCode::Char('R')],
        description: "Reveal the selected entry in the OS file manager.",
    },
    Binding {
        intent: Intent::CopyRepoPath,
        name: "copy_repo_path",
        default_keys: &[KeyCode::Char('y')],
        description: "Copy the selected node's repo-relative path to the clipboard.",
    },
    Binding {
        intent: Intent::CopyAbsPath,
        name: "copy_abs_path",
        default_keys: &[KeyCode::Char('Y')],
        description: "Copy the selected node's absolute path to the clipboard.",
    },
    Binding {
        intent: Intent::ToggleFocus,
        name: "toggle_focus",
        default_keys: &[KeyCode::Tab],
        description: "Move focus between the tree and content columns.",
    },
    Binding {
        intent: Intent::ShrinkTree,
        name: "shrink_tree",
        default_keys: &[KeyCode::Char('<')],
        description: "Narrow the tree column.",
    },
    Binding {
        intent: Intent::GrowTree,
        name: "grow_tree",
        default_keys: &[KeyCode::Char('>')],
        description: "Widen the tree column.",
    },
    Binding {
        intent: Intent::ToggleWrap,
        name: "toggle_wrap",
        default_keys: &[KeyCode::Char('w')],
        description: "Force content-line wrapping on or off.",
    },
    Binding {
        intent: Intent::ToggleZoom,
        name: "toggle_zoom",
        default_keys: &[KeyCode::Char('z')],
        description: "Hide the tree so the content pane fills the frame, or restore the split.",
    },
    Binding {
        intent: Intent::Refresh,
        name: "refresh",
        default_keys: &[KeyCode::Char('r')],
        description: "Re-read git state (status and changed-set) and re-render.",
    },
    Binding {
        intent: Intent::DismissUpdate,
        name: "dismiss_update",
        default_keys: &[KeyCode::Char('u')],
        description: "Dismiss the update-available banner for this session.",
    },
    Binding {
        intent: Intent::SwitchWorktree,
        name: "switch_worktree",
        default_keys: &[KeyCode::Char('W')],
        description: "Open the worktree picker to re-root at another git worktree.",
    },
    Binding {
        intent: Intent::OpenFinder,
        name: "open_finder",
        default_keys: &[KeyCode::Char('f')],
        description: "Open the go-to-file finder to navigate to any file by fuzzy query.",
    },
    Binding {
        intent: Intent::OpenGoToLine,
        name: "open_go_to_line",
        default_keys: &[KeyCode::Char(':')],
        description: "Open the go-to-line prompt to scroll the content pane to a line number.",
    },
    Binding {
        intent: Intent::OpenSearch,
        name: "open_search",
        default_keys: &[KeyCode::Char('/')],
        description: "Open the search prompt at the bottom of the content pane.",
    },
    Binding {
        intent: Intent::NextMatch,
        name: "next_match",
        default_keys: &[KeyCode::Char('n')],
        description: "Advance to the next search match (wraps at the end).",
    },
    Binding {
        intent: Intent::PrevMatch,
        name: "prev_match",
        default_keys: &[KeyCode::Char('N')],
        description: "Retreat to the previous search match (wraps at the start).",
    },
    Binding {
        intent: Intent::TreeScrollLeft,
        name: "tree_scroll_left",
        default_keys: &[KeyCode::Char('H')],
        description: "Scroll the tree pane left.",
    },
    Binding {
        intent: Intent::TreeScrollRight,
        name: "tree_scroll_right",
        default_keys: &[KeyCode::Char('L')],
        description: "Scroll the tree pane right.",
    },
    Binding {
        intent: Intent::ShowHelp,
        name: "show_help",
        default_keys: &[KeyCode::Char('?')],
        description: "Open the in-app help overlay (What's New and About).",
    },
    Binding {
        intent: Intent::Close,
        name: "close",
        default_keys: &[KeyCode::Char('q'), KeyCode::Esc],
        description: "Close the viewer and return to the prior pane.",
    },
];

/// Borrow the [keybinding registry](REGISTRY) rows: the single source of truth for each global
/// action's default key(s), snake_case name, and description.
#[allow(dead_code)] // T-1 foundation: consumed by tests here; the dispatcher/overlay wire it in T-2+.
pub(crate) fn registry() -> &'static [Binding] {
    REGISTRY
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
        (KeyCode::Char('Z'), Intent::OpenFullscreen),
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
        (KeyCode::Char('O'), Intent::OpenWithApp),
        (KeyCode::Char('R'), Intent::RevealInFileManager),
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
    fn registry_covers_every_intent_exactly_once_with_nonempty_keys() {
        // AC-2: every global action (Intent::ALL member) appears in the registry exactly once
        // with a non-empty default key set, so nothing is unreachable by default.
        let intents: HashSet<Intent> = registry().iter().map(|b| b.intent).collect();
        let all: HashSet<Intent> = Intent::ALL.iter().copied().collect();
        assert_eq!(intents, all, "REGISTRY must cover exactly Intent::ALL");
        assert_eq!(
            registry().len(),
            Intent::ALL.len(),
            "REGISTRY must have one row per Intent::ALL member (no duplicates)"
        );
        for b in registry() {
            assert!(
                !b.default_keys.is_empty(),
                "{:?} ({}) must have >=1 default key",
                b.intent,
                b.name
            );
        }
    }

    #[test]
    fn registry_names_are_unique() {
        // AC-5: each global action carries a unique snake_case intent name (these become the
        // public `[keys]` config keys, so a clash would make one intent unaddressable).
        let names: HashSet<&str> = registry().iter().map(|b| b.name).collect();
        assert_eq!(
            names.len(),
            registry().len(),
            "no two REGISTRY rows may share a name"
        );
    }

    #[test]
    fn registry_default_keys_are_collision_free() {
        // AC-3: no bindable key is in the default key set of two different actions.
        let all_keys: Vec<KeyCode> = registry()
            .iter()
            .flat_map(|b| b.default_keys.iter().copied())
            .collect();
        let unique: HashSet<KeyCode> = all_keys.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all_keys.len(),
            "REGISTRY default keys must be collision-free (no key in two rows)"
        );
    }

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

    #[test]
    fn shift_z_maps_to_open_fullscreen_and_lowercase_z_stays_toggle_zoom() {
        // `Z` (Shift+`z`, reported as `Char('Z')` with or without the Shift bit) opens the
        // selected file full-screen (in-plugin zoom + herdr pane zoom). Lowercase `z` stays the
        // in-plugin-only zoom toggle — no collision. A Ctrl chord on `Z` must fire nothing.
        assert_eq!(map_key(k(KeyCode::Char('Z'))), Some(Intent::OpenFullscreen));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::SHIFT)),
            Some(Intent::OpenFullscreen),
            "Z with the SHIFT bit set still maps to OpenFullscreen"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-Z must not fire an intent"
        );
        assert_eq!(map_key(k(KeyCode::Char('z'))), Some(Intent::ToggleZoom));
    }

    #[test]
    fn shift_o_r_map_to_open_and_reveal_and_lowercase_o_is_unbound() {
        // `O` (Shift+o) opens the selected entry with the OS default app; `R` (Shift+r) reveals
        // it in the OS file manager. Lowercase `o` stays unbound (`r` is Refresh — untouched).
        // A Ctrl chord on either capital key must fire nothing.
        assert_eq!(map_key(k(KeyCode::Char('O'))), Some(Intent::OpenWithApp));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::SHIFT)),
            Some(Intent::OpenWithApp),
            "O with SHIFT bit set still maps to OpenWithApp"
        );
        assert_eq!(
            map_key(k(KeyCode::Char('R'))),
            Some(Intent::RevealInFileManager)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT)),
            Some(Intent::RevealInFileManager),
            "R with SHIFT bit set still maps to RevealInFileManager"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-O must not fire an intent"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-R must not fire an intent"
        );
        // Lowercase `o` stays unbound; `r` stays Refresh (no collision).
        assert_eq!(map_key(k(KeyCode::Char('o'))), None);
        assert_eq!(map_key(k(KeyCode::Char('r'))), Some(Intent::Refresh));
    }
}
