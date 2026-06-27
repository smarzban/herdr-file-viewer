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
    /// Activate the selected node (Enter / double-click): expand/collapse a directory, or open
    /// a file in zoom mode (content pane full-screen). Never an edit — the editor hand-off
    /// stays on [`Intent::OpenInEditor`] (AC-N3).
    Activate,
    /// Reveal/hide gitignored files (AC-5).
    ToggleIgnore,
    /// Hide/reveal dot-prefixed ("hidden") files and folders (#46) — a tree filter, independent
    /// of the gitignore toggle. Read-only.
    ToggleHidden,
    /// Restrict the tree to changed files / restore the full tree (AC-6).
    ToggleChangedOnly,
    /// Switch the diff baseline between base-branch and HEAD (AC-16).
    ToggleBaseline,
    /// Cycle the content pane's view mode over the applicable set (AC-11).
    CycleView,
    /// Hand the selected file off to an external editor (AC-19).
    OpenInEditor,
    /// Copy the selected node's **repo-relative** path to the clipboard (e.g. `src/app.rs`).
    /// Read-only — it copies a path string, never reads or writes the file's contents (AC-N3).
    CopyRepoPath,
    /// Copy the selected node's **absolute** path to the clipboard. Read-only, like
    /// [`Intent::CopyRepoPath`] — no file contents are touched.
    CopyAbsPath,
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
    /// Dismiss the "update available" banner for this session (it returns next launch while
    /// still behind). Read-only — touches only in-memory UI state.
    DismissUpdate,
    /// Open the worktree picker to re-root the viewer at another git worktree of the current
    /// repository (the worktree switch). Read-only — it re-roots the in-pane view; it never
    /// checks out a branch or mutates any worktree (AC-N1/N2). The picker is keyboard-operable
    /// (AC-5); a switch happens ONLY in response to this explicit action (AC-N5).
    SwitchWorktree,
    /// Open the go-to-file finder overlay to navigate to any file in the repository by
    /// typing a fuzzy query. Read-only — it navigates the viewer's selection; it never
    /// modifies any file (AC-1, AC-N1, AC-N3).
    OpenFinder,
    /// Open the go-to-line prompt to scroll the content pane to a source line by number.
    /// Read-only navigation — it only moves the in-pane scroll; no file or git mutation
    /// (AC-1, AC-N1). Opens for any selected **file**, in every view: in a source-mapped
    /// (syntax/content) view the jump is immediate; in a transformed view (rendered-markdown /
    /// diff / full-diff) confirming auto-switches the file to the source-mapped view and jumps
    /// once it re-renders (AC-7, revised). With nothing / a directory selected it shows a notice
    /// and opens nothing. Opened only by the explicit `:` key — no event hook (AC-N6).
    OpenGoToLine,
    /// Open the search prompt at the bottom of the content pane (`/`). Read-only navigation —
    /// search moves highlight/scroll only; no file or git mutation (AC-8, AC-N1, AC-N3).
    /// Unlike `:` (go-to-line) the search prompt opens in **every** view mode — it is not
    /// view-gated. Opened only by the explicit `/` key — no event hook (AC-N6).
    OpenSearch,
    /// Advance to the next match, wrapping at the end of the match list. Read-only navigation —
    /// scroll/highlight only, no mutation (AC-19, AC-N1, AC-N3). A no-op when there is no
    /// committed search with ≥1 match. Bound to `n` only — no event hook (AC-N6).
    NextMatch,
    /// Retreat to the previous match, wrapping at the start of the match list. Read-only
    /// navigation — scroll/highlight only, no mutation (AC-19, AC-N1, AC-N3). A no-op when
    /// there is no committed search with ≥1 match. Bound to `N` only — no event hook (AC-N6).
    PrevMatch,
    /// Close the viewer and return control to the prior pane (AC-20).
    Close,
}

impl Intent {
    /// Every intent variant — lets the dispatcher and tests enumerate the closed set so
    /// keyboard-completeness (AC-18) and the no-edit invariant (AC-N3) stay checkable.
    pub const ALL: [Intent; 27] = [
        Intent::NavUp,
        Intent::NavDown,
        Intent::Expand,
        Intent::Collapse,
        Intent::Activate,
        Intent::ToggleIgnore,
        Intent::ToggleHidden,
        Intent::ToggleChangedOnly,
        Intent::ToggleBaseline,
        Intent::CycleView,
        Intent::OpenInEditor,
        Intent::CopyRepoPath,
        Intent::CopyAbsPath,
        Intent::ToggleFocus,
        Intent::ShrinkTree,
        Intent::GrowTree,
        Intent::ToggleWrap,
        Intent::ToggleZoom,
        Intent::Refresh,
        Intent::DismissUpdate,
        Intent::SwitchWorktree,
        Intent::OpenFinder,
        Intent::OpenGoToLine,
        Intent::OpenSearch,
        Intent::NextMatch,
        Intent::PrevMatch,
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
                | Intent::Activate
                | Intent::ToggleIgnore
                | Intent::ToggleHidden
                | Intent::ToggleChangedOnly
                | Intent::ToggleBaseline
                | Intent::CycleView
                | Intent::OpenInEditor
                | Intent::CopyRepoPath
                | Intent::CopyAbsPath
                | Intent::ToggleFocus
                | Intent::ShrinkTree
                | Intent::GrowTree
                | Intent::ToggleWrap
                | Intent::ToggleZoom
                | Intent::Refresh
                | Intent::DismissUpdate
                | Intent::SwitchWorktree
                | Intent::OpenFinder
                | Intent::OpenGoToLine
                | Intent::OpenSearch
                | Intent::NextMatch
                | Intent::PrevMatch
                | Intent::Close => false,
            };
            assert!(
                !mutates_file,
                "{intent:?} must not mutate file contents (AC-N3)"
            );
        }
    }

    #[test]
    fn all_lists_every_variant_once() {
        let set: HashSet<&Intent> = Intent::ALL.iter().collect();
        assert_eq!(
            set.len(),
            Intent::ALL.len(),
            "Intent::ALL must have no duplicates"
        );
    }

    #[test]
    fn switch_worktree_is_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::SwitchWorktree),
            "Intent::ALL must contain SwitchWorktree"
        );
    }

    #[test]
    fn open_finder_is_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::OpenFinder),
            "Intent::ALL must contain OpenFinder"
        );
    }

    #[test]
    fn open_go_to_line_is_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::OpenGoToLine),
            "Intent::ALL must contain OpenGoToLine"
        );
    }

    #[test]
    fn open_search_is_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::OpenSearch),
            "Intent::ALL must contain OpenSearch"
        );
    }

    #[test]
    fn next_match_is_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::NextMatch),
            "Intent::ALL must contain NextMatch"
        );
    }

    #[test]
    fn prev_match_is_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::PrevMatch),
            "Intent::ALL must contain PrevMatch"
        );
    }

    #[test]
    fn all_length_is_27() {
        assert_eq!(
            Intent::ALL.len(),
            27,
            "Intent::ALL must have exactly 27 variants after adding OpenSearch/NextMatch/PrevMatch"
        );
    }
}
