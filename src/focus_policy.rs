//! Pure focus cycling and pinned-preview action policy.

use crate::intent::Intent;
use crate::presenter::Focus;

/// Destination selected by the focus/action policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionTarget {
    Tree,
    Active,
    Pinned,
    Rejected,
}

/// The mutable preview interaction state an in-file action targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewTarget {
    Active,
    Pinned,
}

/// Cycle keyboard focus through the regions that are currently meaningful.
pub fn next_focus(current: Focus, has_pin: bool, tree_hidden: bool) -> Focus {
    if !has_pin {
        return if tree_hidden {
            Focus::Content
        } else {
            match current {
                Focus::Tree => Focus::Content,
                Focus::Content | Focus::Pinned => Focus::Tree,
            }
        };
    }

    if tree_hidden {
        return match current {
            Focus::Pinned => Focus::Content,
            Focus::Tree | Focus::Content => Focus::Pinned,
        };
    }

    match current {
        Focus::Tree => Focus::Pinned,
        Focus::Pinned => Focus::Content,
        Focus::Content => Focus::Tree,
    }
}

/// Decide whether an action is unavailable from pinned focus. Rejection is deliberately exact:
/// callers consume it with a notice and must never fall through to the active file.
pub fn target_for(focus: Focus, intent: Intent) -> ActionTarget {
    if focus == Focus::Pinned && unavailable_from_pinned(intent) {
        return ActionTarget::Rejected;
    }
    match focus {
        Focus::Tree => ActionTarget::Tree,
        Focus::Content => ActionTarget::Active,
        Focus::Pinned => ActionTarget::Pinned,
    }
}

/// AC-31's complete pinned-only unavailable set.
pub fn unavailable_from_pinned(intent: Intent) -> bool {
    matches!(
        intent,
        Intent::Activate
            | Intent::OpenFullscreen
            | Intent::OpenGoToLine
            | Intent::TreeScrollRight
            | Intent::OpenInEditor
            | Intent::OpenWithApp
            | Intent::RevealInFileManager
            | Intent::AddAnnotation
            | Intent::ShowAnnotations
            | Intent::CycleDiffRender
            | Intent::CycleView
            | Intent::ToggleWrap
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_cycles_match_visible_regions() {
        assert_eq!(next_focus(Focus::Tree, true, false), Focus::Pinned);
        assert_eq!(next_focus(Focus::Pinned, true, false), Focus::Content);
        assert_eq!(next_focus(Focus::Content, true, false), Focus::Tree);
        assert_eq!(next_focus(Focus::Pinned, true, true), Focus::Content);
        assert_eq!(next_focus(Focus::Content, true, true), Focus::Pinned);
        assert_eq!(next_focus(Focus::Tree, false, false), Focus::Content);
        assert_eq!(next_focus(Focus::Content, false, false), Focus::Tree);
    }

    #[test]
    fn target_for_maps_each_focus_to_its_interaction_surface() {
        assert_eq!(target_for(Focus::Tree, Intent::NavDown), ActionTarget::Tree);
        assert_eq!(
            target_for(Focus::Content, Intent::NavDown),
            ActionTarget::Active
        );
        assert_eq!(
            target_for(Focus::Pinned, Intent::NavDown),
            ActionTarget::Pinned
        );
    }

    #[test]
    fn pinned_rejection_set_is_exact() {
        let unavailable = [
            Intent::Activate,
            Intent::OpenFullscreen,
            Intent::OpenGoToLine,
            Intent::TreeScrollRight,
            Intent::OpenInEditor,
            Intent::OpenWithApp,
            Intent::RevealInFileManager,
            Intent::AddAnnotation,
            Intent::ShowAnnotations,
            Intent::CycleDiffRender,
            Intent::CycleView,
            Intent::ToggleWrap,
        ];
        for intent in Intent::ALL {
            assert_eq!(
                unavailable_from_pinned(intent),
                unavailable.contains(&intent),
                "{intent:?}"
            );
        }
    }
}
