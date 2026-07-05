//! Line-reference formatting for the copy-line-reference feature — turns a selected line or
//! line range on a file into the `path:line` / `path:start-end` string the Copy adapter (T-7)
//! puts on the clipboard, plus the line-select modal state and its enter/exit on the Controller.

use super::*;

/// Format `rel_path` plus a 1-based line selection as `"<rel>:<n>"` for a single line
/// (`start == end`) or `"<rel>:<lo>-<hi>"` for a range, normalizing `start`/`end` to ascending
/// order first so a selection dragged either direction reads the same. Pure formatting only —
/// no sanitization of `rel_path` (the Copy adapter, T-7, handles that before this is called).
// #[allow(dead_code)] removed in T-7 when the copy adapter calls this.
#[allow(dead_code)]
pub(crate) fn format_line_reference(rel_path: &str, start: usize, end: usize) -> String {
    let (lo, hi) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if lo == hi {
        format!("{rel_path}:{lo}")
    } else {
        format!("{rel_path}:{lo}-{hi}")
    }
}

/// In-progress line selection on the content pane: `anchor` is where the selection started,
/// `marker` is the current cursor line. Both are 1-based source-line indices. A plain move
/// collapses the selection (anchor follows marker); an extend move holds the anchor so the
/// selection grows/shrinks toward the new marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineSelectState {
    anchor: usize,
    marker: usize,
}

#[allow(dead_code)]
impl LineSelectState {
    /// Start a new selection collapsed onto a single `line`.
    pub(crate) fn new(line: usize) -> Self {
        Self {
            anchor: line,
            marker: line,
        }
    }

    /// Move the marker to `line` (clamped to `[1, last]`) and collapse the selection onto it —
    /// the anchor follows the marker.
    pub(crate) fn move_to(&mut self, line: usize, last: usize) {
        self.marker = Self::clamp(line, last);
        self.anchor = self.marker;
    }

    /// Move the marker to `line` (clamped to `[1, last]`) while holding the anchor fixed,
    /// extending (or shrinking) the selection.
    pub(crate) fn extend_to(&mut self, line: usize, last: usize) {
        self.marker = Self::clamp(line, last);
    }

    /// The current selection as an ascending `(start, end)` pair.
    pub(crate) fn selection(&self) -> (usize, usize) {
        if self.anchor <= self.marker {
            (self.anchor, self.marker)
        } else {
            (self.marker, self.anchor)
        }
    }

    fn clamp(line: usize, last: usize) -> usize {
        line.max(1).min(last.max(1))
    }
}

impl Controller {
    /// Enter line-select mode with the marker on the top *visible* source line — the source-view
    /// case (AC-1). The top visible line is `content_scroll + 1` (1-based), clamped into
    /// `[1, line_count]` so an empty/short file still yields a valid line 1. The selection starts
    /// collapsed (anchor == marker); the user moves/extends it from here (T-5). The
    /// transformed/auto-switch view case is T-6. Read-only: touches only in-memory modal state.
    pub fn enter_line_select_at_top(&mut self) {
        let last = self.content.lines.len().max(1);
        let top = (self.content_scroll as usize + 1).clamp(1, last);
        self.modal = Modal::LineSelect(LineSelectState::new(top));
    }

    /// Leave line-select mode without copying (AC-4): close the modal, touching no clipboard and
    /// leaving the content scroll unchanged. Mirrors the finder/prompt cancel path.
    pub fn exit_line_select(&mut self) {
        self.modal = Modal::None;
    }

    /// Whether line-select mode is currently active. Exposed for the Presenter (T-9) and tests.
    pub fn line_select_active(&self) -> bool {
        self.modal.line_select().is_some()
    }

    /// The current line-select selection as an ascending 1-based `(start, end)` pair, or `None`
    /// when line-select is inactive. Exposed for the Presenter (T-9) and tests.
    pub fn line_selection(&self) -> Option<(usize, usize)> {
        self.modal.line_select().map(|s| s.selection())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_has_no_range_suffix() {
        assert_eq!(
            format_line_reference("src/editor.rs", 50, 50),
            "src/editor.rs:50"
        );
    }

    #[test]
    fn range_normalizes_ascending() {
        assert_eq!(
            format_line_reference("src/editor.rs", 50, 58),
            "src/editor.rs:50-58"
        );
        assert_eq!(
            format_line_reference("src/editor.rs", 58, 50),
            "src/editor.rs:50-58"
        );
    }

    #[test]
    fn move_clamps_to_bounds() {
        let mut state = LineSelectState::new(5);
        state.move_to(0, 10);
        assert_eq!(state.selection(), (1, 1));

        let mut state = LineSelectState::new(5);
        state.move_to(999, 10);
        assert_eq!(state.selection(), (10, 10));
    }

    #[test]
    fn move_keeps_single_line_selection() {
        let mut state = LineSelectState::new(5);
        state.move_to(8, 10);
        assert_eq!(state.selection(), (8, 8));
    }

    #[test]
    fn extend_holds_anchor_and_orders() {
        let mut state = LineSelectState::new(5);
        state.extend_to(2, 10);
        assert_eq!(state.selection(), (2, 5));

        let mut state = LineSelectState::new(5);
        state.extend_to(9, 10);
        assert_eq!(state.selection(), (5, 9));
    }
}
