//! Intents — the viewer's complete, closed vocabulary of user actions.
//!
//! The Input Dispatcher ([`crate::input::map_key`]) decodes key events into these; the
//! Session Controller consumes them. The set deliberately contains no file/git mutation intent
//! (AC-N3). An annotation action may edit session-only in-memory state, but never repository data.
//! The only file hand-off is [`Intent::OpenInEditor`], which launches an *external* editor (Editor
//! Launcher) — it never modifies a file in-pane.

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
    /// Toggle full-screen reading of the selected **file**. When the pane is not full-screen, open
    /// the file in the in-plugin zoom (content pane fills the frame, focused — like
    /// [`Intent::Activate`] on a file) AND zoom this pane in herdr (`pane zoom --current --on`), so
    /// the file takes over the whole terminal, not just the plugin's split. When the pane is
    /// already full-screen, reverse it: un-zoom the pane (`--off`) and restore the two-column split.
    /// The toggle is driven by the viewer's own record of whether it opened the host zoom, so it
    /// works with or without a live herdr, and every other exit path (`Esc`/`q`, the `z` zoom
    /// toggle, a worktree re-root, quit) releases the host zoom too — the pane never lingers
    /// full-screen behind the split. On a **directory** (only reachable when not full-screen) it
    /// behaves like [`Intent::Activate`] (expand/collapse). Read-only: the herdr pane zoom is a host
    /// **layout** op, never a file/git mutation (AC-N1/N3), and best-effort — it degrades to the
    /// in-plugin zoom when herdr is absent. Bound to `Z` (Shift+`z`) only — no event hook.
    OpenFullscreen,
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
    /// Open the selected entry with the OS default application (`O`). Read-only external
    /// hand-off — launches another process, never modifies the file (AC-1, AC-N1). Non-blocking
    /// (does not suspend the TUI).
    OpenWithApp,
    /// Reveal the selected entry in the OS file manager (`R`). Read-only external hand-off
    /// (AC-2, AC-N1). Non-blocking.
    RevealInFileManager,
    /// Copy the selected node's **repo-relative** path to the clipboard (e.g. `src/app.rs`).
    /// Read-only — it copies a path string, never reads or writes the file's contents (AC-N3).
    CopyRepoPath,
    /// Copy the selected node's **absolute** path to the clipboard. Read-only, like
    /// [`Intent::CopyRepoPath`] — no file contents are touched.
    CopyAbsPath,
    /// Open the annotation editor for the selected file. Saving changes session-only in-memory
    /// annotation state; it never writes a file or mutates git.
    AddAnnotation,
    /// Open the session-only annotation overview. Viewing it never touches files or git.
    ShowAnnotations,
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
    /// Open the in-app help overlay (`?` key) showing the What's New and About sections.
    /// Read-only — touches only in-memory UI state; no file, git, or network access
    /// (AC-1, AC-6, AC-19, AC-N1, AC-N3, AC-N4). Opened only by the explicit `?` key —
    /// no event hook (AC-N4).
    ShowHelp,
    /// Advance to the next match, wrapping at the end of the match list. Read-only navigation —
    /// scroll/highlight only, no mutation (AC-19, AC-N1, AC-N3). A no-op when there is no
    /// committed search with ≥1 match. Bound to `n` only — no event hook (AC-N6).
    NextMatch,
    /// Retreat to the previous match, wrapping at the start of the match list. Read-only
    /// navigation — scroll/highlight only, no mutation (AC-19, AC-N1, AC-N3). A no-op when there
    /// is no committed search with ≥1 match. Bound to `N` only — no event hook (AC-N6).
    PrevMatch,
    /// Scroll the tree pane left by the horizontal step (AC-18: the tree's h-scroll was
    /// mouse-only; this key makes it keyboard-reachable). Read-only navigation — it only
    /// adjusts an in-memory scroll offset; no file or git mutation (AC-N1, AC-N3). Bound to
    /// `H` (Shift+`h`) only — no event hook (AC-N6). Inert unless the tree is focused.
    TreeScrollLeft,
    /// Scroll the tree pane right by the horizontal step (AC-18). Read-only navigation — like
    /// [`Intent::TreeScrollLeft`] it only moves the in-pane scroll; no mutation. Bound to `L`
    /// (Shift+`l`) only — no event hook (AC-N6). Inert unless the tree is focused.
    TreeScrollRight,
    /// Close the viewer and return control to the prior pane (AC-20).
    Close,
}

impl Intent {
    /// Every intent variant — lets the dispatcher and tests enumerate the closed set so
    /// keyboard-completeness (AC-18) and the no-file/git-mutation invariant (AC-N3) stay checkable.
    pub const ALL: [Intent; 35] = [
        Intent::NavUp,
        Intent::NavDown,
        Intent::Expand,
        Intent::Collapse,
        Intent::Activate,
        Intent::OpenFullscreen,
        Intent::ToggleIgnore,
        Intent::ToggleHidden,
        Intent::ToggleChangedOnly,
        Intent::ToggleBaseline,
        Intent::CycleView,
        Intent::OpenInEditor,
        Intent::OpenWithApp,
        Intent::RevealInFileManager,
        Intent::CopyRepoPath,
        Intent::CopyAbsPath,
        Intent::AddAnnotation,
        Intent::ShowAnnotations,
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
        Intent::TreeScrollLeft,
        Intent::TreeScrollRight,
        Intent::ShowHelp,
        Intent::Close,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn intent_effects_never_mutate_files_or_git_and_classify_annotation_edits() {
        // AC-N3: this exhaustive match fails to compile when a variant is added, forcing an
        // explicit repository-mutation decision. AddAnnotation is intentionally distinguished as
        // a possible session-only annotation edit; it still cannot write files or mutate git.
        for intent in Intent::ALL {
            let (mutates_file_or_git, may_edit_in_memory_annotations) = match intent {
                Intent::AddAnnotation => (false, true),
                Intent::NavUp
                | Intent::NavDown
                | Intent::Expand
                | Intent::Collapse
                | Intent::Activate
                | Intent::OpenFullscreen
                | Intent::ToggleIgnore
                | Intent::ToggleHidden
                | Intent::ToggleChangedOnly
                | Intent::ToggleBaseline
                | Intent::CycleView
                | Intent::OpenInEditor
                | Intent::OpenWithApp
                | Intent::RevealInFileManager
                | Intent::CopyRepoPath
                | Intent::CopyAbsPath
                | Intent::ShowAnnotations
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
                | Intent::TreeScrollLeft
                | Intent::TreeScrollRight
                | Intent::ShowHelp
                | Intent::Close => (false, false),
            };
            assert!(
                !mutates_file_or_git,
                "{intent:?} must not mutate files or git (AC-N3)"
            );
            assert_eq!(
                may_edit_in_memory_annotations,
                intent == Intent::AddAnnotation,
                "only AddAnnotation may edit session-only annotation state"
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
    fn all_length_is_35() {
        assert_eq!(
            Intent::ALL.len(),
            35,
            "Intent::ALL must have exactly 35 variants after adding annotation actions"
        );
    }

    #[test]
    fn open_fullscreen_is_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::OpenFullscreen),
            "Intent::ALL must contain OpenFullscreen"
        );
    }

    #[test]
    fn reveal_open_intents_are_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::OpenWithApp),
            "Intent::ALL must contain OpenWithApp"
        );
        assert!(
            Intent::ALL.contains(&Intent::RevealInFileManager),
            "Intent::ALL must contain RevealInFileManager"
        );
    }

    #[test]
    fn tree_scroll_intents_are_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::TreeScrollLeft),
            "Intent::ALL must contain TreeScrollLeft"
        );
        assert!(
            Intent::ALL.contains(&Intent::TreeScrollRight),
            "Intent::ALL must contain TreeScrollRight"
        );
    }

    #[test]
    fn show_help_is_in_all() {
        assert!(
            Intent::ALL.contains(&Intent::ShowHelp),
            "Intent::ALL must contain ShowHelp"
        );
    }

    #[test]
    fn annotation_intents_are_in_all() {
        assert!(Intent::ALL.contains(&Intent::AddAnnotation));
        assert!(Intent::ALL.contains(&Intent::ShowAnnotations));
    }
}
