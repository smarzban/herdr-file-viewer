//! Intents — the viewer's complete, closed vocabulary of user actions.
//!
//! The Input Dispatcher ([`crate::input::map_key`]) decodes key events into these; the
//! Session Controller consumes them. The set is deliberately read-only: there is **no
//! edit/write intent** (AC-N3). The only file hand-off is [`Intent::OpenInEditor`], which
//! launches an *external* editor (Editor Launcher) — it never modifies a file in-pane.

/// A single user action, decoded from a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
    /// Move the tree cursor up one row.
    NavUp,
    /// Move the tree cursor down one row.
    NavDown,
    /// Expand the selected directory (AC-3).
    Expand,
    /// Collapse the selected directory (AC-3).
    Collapse,
    /// Reveal/hide gitignored files (AC-5).
    ToggleIgnore,
    /// Restrict the tree to changed files / restore the full tree (AC-6).
    ToggleChangedOnly,
    /// Switch the diff baseline between base-branch and HEAD (AC-16).
    ToggleBaseline,
    /// Cycle the content pane's view mode over the applicable set (AC-11).
    CycleView,
    /// Hand the selected file off to an external editor / new pane (AC-19).
    OpenInEditor,
    /// Move focus between the tree and content columns (AC-21).
    ToggleFocus,
    /// Narrow the tree column (move the tree/content divider left).
    ShrinkTree,
    /// Widen the tree column (move the tree/content divider right).
    GrowTree,
    /// Force content-line wrapping on/off, overriding the per-mode default (so long lines in
    /// code and diffs can be wrapped on demand instead of truncated).
    ToggleWrap,
    /// Hide the tree so the content pane fills the frame / restore the two-column layout — a
    /// pure layout toggle for reading a file full-screen.
    ToggleZoom,
    /// Re-read git state (working-tree status + changed-set) and re-render, so the viewer picks
    /// up changes made outside it — a merge, pull, or commit in another pane. Read-only.
    Refresh,
    /// Close the viewer and return control to the prior pane (AC-20).
    Close,
}

impl Intent {
    /// Every intent variant — lets the dispatcher and tests enumerate the closed set so
    /// keyboard-completeness (AC-18) and the no-edit invariant (AC-N3) stay checkable.
    pub const ALL: [Intent; 16] = [
        Intent::NavUp,
        Intent::NavDown,
        Intent::Expand,
        Intent::Collapse,
        Intent::ToggleIgnore,
        Intent::ToggleChangedOnly,
        Intent::ToggleBaseline,
        Intent::CycleView,
        Intent::OpenInEditor,
        Intent::ToggleFocus,
        Intent::ShrinkTree,
        Intent::GrowTree,
        Intent::ToggleWrap,
        Intent::ToggleZoom,
        Intent::Refresh,
        Intent::Close,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn no_intent_mutates_file_contents() {
        // AC-N3: the viewer offers no in-pane editing. This exhaustive match fails to
        // compile if a variant is ever added, forcing a conscious read-only/edit decision;
        // every current variant is navigation, a view/filter toggle, an external hand-off,
        // or close — none writes a file's contents.
        for intent in Intent::ALL {
            let mutates_file = match intent {
                Intent::NavUp
                | Intent::NavDown
                | Intent::Expand
                | Intent::Collapse
                | Intent::ToggleIgnore
                | Intent::ToggleChangedOnly
                | Intent::ToggleBaseline
                | Intent::CycleView
                | Intent::OpenInEditor
                | Intent::ToggleFocus
                | Intent::ShrinkTree
                | Intent::GrowTree
                | Intent::ToggleWrap
                | Intent::ToggleZoom
                | Intent::Refresh
                | Intent::Close => false,
            };
            assert!(!mutates_file, "{intent:?} must not mutate file contents (AC-N3)");
        }
    }

    #[test]
    fn all_lists_every_variant_once() {
        let set: HashSet<&Intent> = Intent::ALL.iter().collect();
        assert_eq!(set.len(), Intent::ALL.len(), "Intent::ALL must have no duplicates");
    }
}
