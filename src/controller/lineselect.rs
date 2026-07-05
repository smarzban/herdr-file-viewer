//! Line-reference formatting for the copy-line-reference feature — turns a selected line or
//! line range on a file into the `path:line` / `path:start-end` string the Copy adapter (T-7)
//! puts on the clipboard, plus the line-select modal state and its enter/exit on the Controller.

use super::*;

/// Format `rel_path` plus a 1-based line selection as `"<rel>:<n>"` for a single line
/// (`start == end`) or `"<rel>:<lo>-<hi>"` for a range, normalizing `start`/`end` to ascending
/// order first so a selection dragged either direction reads the same. Pure formatting only —
/// no sanitization of `rel_path` (the Copy adapter, T-7, handles that before this is called).
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
    /// Enter line-select mode with the marker on the top *visible* source line (AC-1, AC-15).
    ///
    /// - **Source view, up to date** (`SyntaxContent` AND `applied_seq == latest_seq`): the
    ///   line→row mapping is valid now, so open the modal synchronously on `content_scroll + 1`
    ///   (1-based), clamped into `[1, line_count]` so an empty/short file still yields a valid line 1.
    ///   The selection starts collapsed (anchor == marker); the user moves/extends it from here (T-5).
    /// - **Transformed view, or a source render still in flight**: a source line has no display row in
    ///   a transformed view (RenderedMarkdown / Diff / FullDiff), and a still-in-flight source render
    ///   holds stale content — either way the marker can't be placed now. If the view is transformed,
    ///   switch this file to the source-mapped content view (`SyntaxContent` override + re-render);
    ///   then queue the entry against the dispatched render's seq. [`poll`](Controller::poll) opens the
    ///   modal at the top visible source line once that render lands (the same seq-guard `pending_goto`
    ///   uses). This faithfully mirrors the proven go-to-line auto-switch machinery.
    ///
    /// Read-only: touches only in-memory modal / view-override / scroll state.
    pub fn enter_line_select_at_top(&mut self) {
        // Reset double-click state (mirrors open_finder/open_help): a tree click made just before
        // entry must not pair with the FIRST content click inside the mode as a double-click —
        // `is_double_click` only compares the row, not the column/pane, so a stale prior-context
        // click could otherwise fire `copy_line_reference` (copy + close) before the marker is
        // ever placed. Cleared here, at the top of entry, so BOTH the synchronous path below and
        // the deferred (T-6 auto-switch) path are covered — the clear happens when entry begins,
        // not when the (possibly-deferred) modal actually opens.
        self.last_click = None;
        let source_mapped = self.selected_view_mode() == Some(ViewMode::SyntaxContent);
        if source_mapped && self.applied_seq == self.latest_seq {
            // Source-mapped AND the displayed content is the latest render → the line→row mapping is
            // valid now, so open synchronously (the T-3 path, unchanged).
            let last = self.content.lines.len().max(1);
            let top = (self.content_scroll as usize + 1).clamp(1, last);
            self.modal = Modal::LineSelect(LineSelectState::new(top));
        } else if let Some(path) = self
            .tree
            .selected()
            .filter(|node| node.kind == NodeKind::File)
            .map(|node| node.path.clone())
        {
            // Either a transformed view (no 1:1 source→display row) or a source render still in flight
            // (the override reports SyntaxContent before its render lands). Switch the view only when we
            // must (a transformed view), then queue the entry against the dispatched render's seq;
            // `poll` opens the modal once the matching source-mapped render lands (AC-15). `dispatch_render`
            // bumps `latest_seq`, so read it AFTER the (re)dispatch — mirrors `pending_goto`.
            if !source_mapped {
                self.overrides.insert(path, ViewMode::SyntaxContent);
                self.dispatch_render();
            }
            self.pending_line_select = Some(self.latest_seq);
        }
    }

    /// Copy the current line reference for the selected file to the clipboard and close the mode
    /// (the `Enter` / double-click confirm, T-7). Mirrors [`copy_path`](Controller::copy_path):
    /// build the `path:line` / `path:start-end` reference, run it through
    /// [`sanitize_control`](crate::text_layout::sanitize_control) **before** it reaches the
    /// clipboard *or* the notice (AC-16 — a crafted file name may carry ESC/newline), then surface
    /// the outcome as a transient notice ("Copied …" on `Ok` / a failure message on `Err`,
    /// AC-10/AC-11).
    ///
    /// Guards the no-real-file case: the mode can be open without a selected *file* node (the T-4
    /// guidance screen is non-empty), so if there is no active selection or the selected node is
    /// not a file, copy nothing and return [`Effects::noop`] rather than fabricate a reference.
    /// Read-only (AC-17): touches only the clipboard and in-memory notice / modal state.
    ///
    /// The mode closes on **both** `Ok` and `Err` (via [`exit_line_select`](Self::exit_line_select))
    /// so `Enter` is a completed action that returns to normal navigation — consistent with the
    /// finder/picker/prompt confirm paths; the notice conveys the outcome.
    pub fn copy_line_reference(&mut self) -> Effects {
        // Both an active selection AND a selected file node are required; otherwise there is no
        // reference to copy — do not fabricate one.
        let Some((start, end)) = self.line_selection() else {
            return Effects::noop();
        };
        let Some(rel_path) = self
            .tree
            .selected()
            .filter(|node| node.kind == NodeKind::File)
            .map(|node| {
                self.rel(&node.path)
                    .unwrap_or_else(|| node.path.clone())
                    .to_string_lossy()
                    .into_owned()
            })
        else {
            return Effects::noop();
        };

        let raw = format_line_reference(&rel_path, start, end);
        // Sanitize the WHOLE reference before it reaches the clipboard OR the notice (AC-16) — the
        // path segment is untrusted, exactly as in `copy_path`.
        let text = crate::text_layout::sanitize_control(&raw);
        self.action_notice = Some(match self.clipboard.copy(&text) {
            Ok(()) => format!("Copied {text}"),
            Err(e) => format!("Could not copy line reference: {e}"),
        });
        self.exit_line_select();
        Effects::redraw()
    }

    /// Leave line-select mode without copying (AC-4): close the modal, touching no clipboard and
    /// leaving the content scroll unchanged. Mirrors the finder/prompt cancel path. Also drops any
    /// still-queued deferred entry (AC-15), so a superseded/abandoned auto-switch can't reopen the
    /// modal when its render lands.
    pub fn exit_line_select(&mut self) {
        self.modal = Modal::None;
        self.pending_line_select = None;
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

    /// Route a key event while line-select mode is active. The run loop calls this instead of
    /// the normal key→intent map while `line_select_active()` — so `j`/`k`/arrows move the
    /// marker instead of firing viewer intents (mirrors the finder/prompt/help routing).
    ///
    /// `j`/`Down` move the marker down one line, `k`/`Up` up one — a plain (collapsing) move
    /// (AC-5); with Shift they extend from the held anchor instead (AC-12). This codebase
    /// reports Shift+letter as the **uppercase char** (`J`/`K`) and Shift+arrow as the arrow key
    /// plus `KeyModifiers::SHIFT`, so both spellings are accepted. `move_to`/`extend_to` clamp
    /// the target to `[1, last]` (AC-6). After any move the content pane scrolls so the marker
    /// stays visible (AC-7). `Esc` exits without copying (AC-4); `Enter` copies the reference and
    /// closes the mode ([`copy_line_reference`](Self::copy_line_reference), AC-9). Any other key
    /// is inert. Ctrl/Alt chords are rejected up
    /// front so a reserved combo never moves the marker; Shift is meaningful (extend / shifted
    /// arrows) and is allowed.
    pub fn handle_line_select_key(&mut self, key: KeyEvent) -> Effects {
        // Only Shift is a meaningful modifier here (extend / shifted arrows); reject Ctrl/Alt/…
        // chords so a reserved combo never drives the marker.
        if key.modifiers.difference(KeyModifiers::SHIFT) != KeyModifiers::NONE {
            return Effects::noop();
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Esc => {
                self.exit_line_select(); // AC-4: close, copy nothing, scroll unchanged
                return Effects::redraw();
            }
            KeyCode::Enter => {
                return self.copy_line_reference(); // AC-9: copy the reference, then close the mode
            }
            _ => {}
        }

        let Some(marker) = self.modal.line_select().map(|s| s.marker) else {
            return Effects::noop();
        };
        let last = self.content.lines.len();

        // Classify the key into a marker target + whether it extends the selection. Shift+letter
        // arrives as the uppercase char (`J`/`K`); Shift+arrow as the arrow + the SHIFT bit.
        let (target, extend) = match key.code {
            KeyCode::Char('j') => (marker + 1, false),
            KeyCode::Char('k') => (marker.saturating_sub(1), false),
            KeyCode::Char('J') => (marker + 1, true),
            KeyCode::Char('K') => (marker.saturating_sub(1), true),
            KeyCode::Down => (marker + 1, shift),
            KeyCode::Up => (marker.saturating_sub(1), shift),
            _ => return Effects::noop(),
        };

        if let Some(state) = self.modal.line_select_mut() {
            if extend {
                state.extend_to(target, last);
            } else {
                state.move_to(target, last);
            }
        }

        // AC-7: keep the marker's source row within the viewport. The marker is 1-based, the
        // scroll offset is 0-based display rows, so the marker row is `marker - 1` (a non-wrap
        // 1:1 line→row mapping, as in T-3). If it fell above the top, pin the top to it; if it
        // fell below the bottom, pin the bottom to it; then clamp to the last screenful.
        if let Some(marker) = self.modal.line_select().map(|s| s.marker) {
            let row = marker.saturating_sub(1);
            let scroll = self.content_scroll as usize;
            let height = self.content_height as usize;
            let new_scroll = if row < scroll {
                row
            } else if height > 0 && row >= scroll + height {
                marker.saturating_sub(height)
            } else {
                scroll
            };
            self.content_scroll =
                (new_scroll.min(u16::MAX as usize) as u16).min(self.max_content_scroll());
        }

        Effects::redraw()
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
