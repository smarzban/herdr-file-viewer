//! The line-select modal: its in-progress selection state, the enter/exit transitions on the
//! Controller, and the confirm path that copies the selected lines' CONTENT (the actual file
//! text, not a `path:line` reference) to the clipboard.

use super::*;

/// Strip a leading line-number gutter (as the syntax renderer adds — `bat --style=numbers` prints
/// a right-aligned number then a separator before each source line) from one rendered line's plain
/// text, given the 1-based source line number `n` that line displays.
///
/// Self-validating: it only strips when the digits immediately after the right-align padding EQUAL
/// `n` **and** are followed by a real separator (a space, and/or a box-drawing `│`/`|` with its
/// spaces). So a renderer that adds no gutter (the plain-text fallback, or the test stubs), or a
/// code line that merely happens to start with some other digits, is returned unchanged — and the
/// code's OWN leading indentation, which sits after the single separator, is preserved intact.
fn strip_line_gutter(plain: &str, n: usize) -> &str {
    let after_pad = plain.trim_start_matches(' ');
    let Some(rest) = after_pad.strip_prefix(&n.to_string()) else {
        return plain;
    };
    // The gutter number must be followed by a separator; otherwise this isn't a gutter.
    let mut rest = rest;
    let mut saw_sep = false;
    if let Some(r) = rest.strip_prefix(' ') {
        rest = r;
        saw_sep = true;
    }
    if let Some(r) = rest.strip_prefix('│').or_else(|| rest.strip_prefix('|')) {
        rest = r.strip_prefix(' ').unwrap_or(r);
        saw_sep = true;
    }
    if saw_sep { rest } else { plain }
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

    /// The marker (cursor) line — 1-based. Exposed for the Presenter's line-select overlay (T-9),
    /// which draws the marker emphasis distinct from the rest of the selection range.
    pub(crate) fn marker(&self) -> usize {
        self.marker
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
        // click could otherwise fire `copy_line_content` (copy + close) before the marker is
        // ever placed. Cleared here, at the top of entry, so BOTH the synchronous path below and
        // the deferred (T-6 auto-switch) path are covered — the clear happens when entry begins,
        // not when the (possibly-deferred) modal actually opens.
        self.last_click = None;
        let source_mapped = self.selected_view_mode() == Some(ViewMode::SyntaxContent);
        if source_mapped && self.applied_seq == self.latest_seq {
            // Source-mapped AND the displayed content is the latest render → the line→row mapping is
            // valid now, so open synchronously. The top visible source line is mapped from the scroll
            // offset through `line_at_content_row`, so it is correct even when the `w` wrap override is
            // on (under wrap `content_scroll` is a wrapped display-row offset, NOT a source-line index).
            let last = self.content.lines.len().max(1);
            let top = self
                .line_at_content_row(self.content_scroll as usize)
                .clamp(1, last);
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

    /// Join the plain text of the currently-selected 1-based source lines `[start, end]` with `\n`.
    ///
    /// Line-select forces the source-mapped content view (`SyntaxContent`), so each entry in
    /// `content.lines` is exactly one source line — the selection therefore indexes straight into
    /// it (`line - 1`). For each selected line we (1) join its spans' text, (2) drop any residual
    /// control char **while keeping `\t`** so indentation survives, and (3) strip the syntax
    /// renderer's line-number gutter via [`strip_line_gutter`] so the copy is the source text
    /// alone — not `bat`'s `   1 …` decoration. The `\n` joins are added only *after* that
    /// per-line work so line structure survives (a whole-string `sanitize_control` would eat the
    /// newlines/tabs — hence the per-line approach). Bounds are clamped into `[1, line_count]`; an
    /// empty body yields an empty string.
    fn selected_lines_text(&self, start: usize, end: usize) -> String {
        let total = self.content.lines.len();
        if total == 0 {
            return String::new();
        }
        let lo = start.max(1).min(total);
        let hi = end.max(1).min(total);
        (lo..=hi)
            .map(|n| {
                let plain: String = self.content.lines[n - 1]
                    .spans
                    .iter()
                    .flat_map(|s| s.content.chars())
                    .filter(|c| !c.is_control() || *c == '\t')
                    .collect();
                strip_line_gutter(&plain, n).to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Copy the CONTENT of the selected lines to the clipboard and close the mode (the `Enter` /
    /// `y` / `Y` / double-click confirm). Builds the joined line text via
    /// [`selected_lines_text`](Self::selected_lines_text), copies it, then surfaces a concise
    /// outcome notice naming the line range — NOT the content itself, which may be many lines
    /// ("Copied line 5" / "Copied lines 5-8" on `Ok`, a failure message on `Err`, AC-10/AC-11).
    ///
    /// Guards the no-real-file case: the mode can be open without a selected *file* node (the T-4
    /// guidance screen is non-empty), so if there is no active selection or the selected node is
    /// not a file, copy nothing and return [`Effects::noop`] rather than copy the guidance text.
    /// Read-only (AC-17): touches only the clipboard and in-memory notice / modal state.
    ///
    /// The mode closes on **both** `Ok` and `Err` (via [`exit_line_select`](Self::exit_line_select))
    /// so the confirm is a completed action that returns to normal navigation — consistent with the
    /// finder/picker/prompt confirm paths; the notice conveys the outcome.
    pub fn copy_line_content(&mut self) -> Effects {
        // Both an active selection AND a selected file node are required; otherwise there is no
        // file content to copy — do not copy the non-file guidance screen (T-4).
        let Some((start, end)) = self.line_selection() else {
            return Effects::noop();
        };
        let is_file = self
            .tree
            .selected()
            .is_some_and(|node| node.kind == NodeKind::File);
        if !is_file {
            return Effects::noop();
        }

        let text = self.selected_lines_text(start, end);
        let label = if start == end {
            format!("line {start}")
        } else {
            format!("lines {start}-{end}")
        };
        self.action_notice = Some(match self.clipboard.copy(&text) {
            Ok(()) => format!("Copied {label}"),
            Err(e) => format!("Could not copy {label}: {e}"),
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
    /// stays visible (AC-7). `Esc` exits without copying (AC-4); `Enter` copies the selected lines'
    /// content and closes the mode; `y`/`Y` do the same (the familiar copy keys)
    /// ([`copy_line_content`](Self::copy_line_content), AC-9). Any other key
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
            // Enter / y / Y all confirm: copy the selected lines' content, then close the mode.
            // `y`/`Y` reach here because line-select routes every key through this handler, so the
            // normal copy-path bindings would otherwise be inert; wiring them here matches the
            // muscle memory of "y copies". `Y` arrives as the uppercase char + SHIFT, which the
            // modifier guard above permits.
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                return self.copy_line_content(); // AC-9
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

        // AC-7: keep the marker's source row within the viewport. The marker is a 1-based source
        // line; `content_row_of_line` maps it to its 0-based display-row offset — `marker - 1` when
        // unwrapped, or the cumulative wrapped-row count when the `w` override wraps the view (the
        // same mapping `scroll_to_line` uses, so the two agree). If it fell above the top, pin the
        // top to it; if it fell below the bottom, pin the bottom row to it; then clamp to the last
        // screenful.
        if let Some(marker) = self.modal.line_select().map(|s| s.marker) {
            let row = self.content_row_of_line(marker);
            let scroll = self.content_scroll as usize;
            let height = self.content_height as usize;
            let new_scroll = if row < scroll {
                row
            } else if height > 0 && row >= scroll + height {
                row + 1 - height
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
    fn strip_gutter_removes_number_and_space_keeps_code() {
        // `bat --style=numbers`-style: right-aligned number + one space, then the source line.
        assert_eq!(strip_line_gutter("   1 # Architecture", 1), "# Architecture");
        assert_eq!(strip_line_gutter("  42 fn main() {", 42), "fn main() {");
    }

    #[test]
    fn strip_gutter_preserves_code_indentation() {
        // Only the single separator space is consumed; the code's own leading indentation stays.
        assert_eq!(strip_line_gutter("  2     let x = 5;", 2), "    let x = 5;");
    }

    #[test]
    fn strip_gutter_handles_box_drawing_separator() {
        // Some `bat` styles put a `│` between the number and the code.
        assert_eq!(strip_line_gutter("  1 │ code", 1), "code");
        assert_eq!(strip_line_gutter("  1 | code", 1), "code");
    }

    #[test]
    fn strip_gutter_blank_line_becomes_empty() {
        assert_eq!(strip_line_gutter("   2 ", 2), "");
    }

    #[test]
    fn strip_gutter_leaves_ungutter_content_untouched() {
        // No matching leading number (the test stubs render "line4" etc.) → unchanged.
        assert_eq!(strip_line_gutter("line4", 5), "line4");
        // Digits present but NOT equal to the line number → not a gutter, unchanged.
        assert_eq!(strip_line_gutter("42 is the answer", 7), "42 is the answer");
        // Leading digits equal to `n` but with no separator (code that just starts with them) →
        // unchanged, so we never eat real content.
        assert_eq!(strip_line_gutter("5000 loops", 5), "5000 loops");
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
