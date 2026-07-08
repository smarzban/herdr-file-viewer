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

/// The char index within `s` displayed at 0-based display column `col` — i.e. how many chars sit
/// to the left of a caret dropped at that column. Used to turn a mouse column into a character
/// position for click-drag selection. Clamps to the char count when `col` is at/past the end (a
/// caret at end-of-line).
///
/// v1 assumes one display column per char: `bat` expands tabs to spaces before we ever see the
/// text, and code is overwhelmingly single-width, so `col` maps straight to a char index. Precise
/// wide-glyph (CJK / emoji) column accounting is a follow-up; it would replace this body with a
/// width-summing walk without changing the signature.
fn char_index_at_col(s: &str, col: usize) -> usize {
    col.min(s.chars().count())
}

/// In-progress selection on the content pane. `anchor` is where the selection started, `marker`
/// the current cursor; both are 1-based source-line indices. Two granularities share this state:
///
/// - **Line** (keyboard `j`/`k`, Shift-extend): whole source lines. `char_mode` is `false` and the
///   `*_col` carets are ignored — copy takes full lines.
/// - **Character** (mouse click-drag): `anchor_col`/`marker_col` are char carets (0-based char
///   indices into the *displayed* line, gutter included) pairing with `anchor`/`marker`. Set while
///   `char_mode` is `true` — copy takes the exact `anchor..marker` character span.
///
/// A keyboard move reverts to line granularity (`char_mode = false`), so the two never tangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineSelectState {
    anchor: usize,
    marker: usize,
    anchor_col: usize,
    marker_col: usize,
    char_mode: bool,
}

impl LineSelectState {
    /// Start a new selection collapsed onto a single `line` (line granularity).
    pub(crate) fn new(line: usize) -> Self {
        Self {
            anchor: line,
            marker: line,
            anchor_col: 0,
            marker_col: 0,
            char_mode: false,
        }
    }

    /// Begin a character-granular selection collapsed at `(line, col)` — a mouse press. `col` is a
    /// char caret into the displayed line; `line` is clamped to `[1, last]`.
    pub(crate) fn begin_char(&mut self, line: usize, col: usize, last: usize) {
        let l = Self::clamp(line, last);
        self.anchor = l;
        self.marker = l;
        self.anchor_col = col;
        self.marker_col = col;
        self.char_mode = true;
    }

    /// Extend a character-granular selection: move the marker to `(line, col)` while holding the
    /// anchor — a mouse drag. `line` is clamped to `[1, last]`.
    pub(crate) fn drag_char(&mut self, line: usize, col: usize, last: usize) {
        self.marker = Self::clamp(line, last);
        self.marker_col = col;
        self.char_mode = true;
    }

    /// Whether the selection is character-granular (a mouse drag), vs. whole-line (keyboard).
    pub(crate) fn is_char_mode(&self) -> bool {
        self.char_mode
    }

    /// The character selection as an ordered `((lo_line, lo_col), (hi_line, hi_col))` pair —
    /// ascending by line then column, so a drag in either direction reads the same.
    pub(crate) fn char_span(&self) -> ((usize, usize), (usize, usize)) {
        let a = (self.anchor, self.anchor_col);
        let m = (self.marker, self.marker_col);
        if a <= m { (a, m) } else { (m, a) }
    }

    /// Move the marker to `line` (clamped to `[1, last]`) and collapse the selection onto it —
    /// the anchor follows the marker. A keyboard move, so it reverts to line granularity.
    pub(crate) fn move_to(&mut self, line: usize, last: usize) {
        self.marker = Self::clamp(line, last);
        self.anchor = self.marker;
        self.char_mode = false;
    }

    /// Move the marker to `line` (clamped to `[1, last]`) while holding the anchor fixed,
    /// extending (or shrinking) the selection. A keyboard extend, so it reverts to line granularity.
    pub(crate) fn extend_to(&mut self, line: usize, last: usize) {
        self.marker = Self::clamp(line, last);
        self.char_mode = false;
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
        // Entering L mode drops any ambient content-pane selection so the two overlays never
        // coexist — L mode gets a fresh line marker rather than inheriting the mouse char span.
        self.content_selection = None;
        // Also drop any in-flight drag: a still-held ambient press left `drag = Some(ContentSelect)`,
        // and both mouse paths key their drag arm off that variant — without this reset the next
        // mouse-move would extend the freshly-opened L-mode selection from a press the user never
        // made in L mode. Clearing it keeps the first L-mode drag inert until a genuine L-mode press.
        self.drag = None;
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
                let plain = self.filtered_display_line(n);
                strip_line_gutter(&plain, n).to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The plain text of 1-based source line `n` (its spans joined), with residual control chars
    /// dropped but `\t` kept. Includes the syntax renderer's gutter — callers strip it with
    /// [`strip_line_gutter`]. Caller guarantees `n` is in `[1, content.lines.len()]`.
    fn filtered_display_line(&self, n: usize) -> String {
        self.content.lines[n - 1]
            .spans
            .iter()
            .flat_map(|s| s.content.chars())
            .filter(|c| !c.is_control() || *c == '\t')
            .collect()
    }

    /// Map a screen `(col, row)` to a `(source line, char caret)` for mouse selection — used by both
    /// the L-mode drag and the ambient content-pane drag. The row maps to a source line exactly as
    /// [`handle_line_select_mouse`](Self::handle_line_select_mouse) does; the column subtracts the
    /// pane origin and whatever leading glyph columns the ACTIVE overlay prepends
    /// ([`content_overlay_glyph_cols`](Self::content_overlay_glyph_cols) — 1 in L mode, 0 ambient),
    /// adds any horizontal scroll, then resolves to a char caret in the displayed line. Clamped to
    /// `[1, line_count]` for the line and `[0, line_len]` for the caret.
    ///
    /// The char caret is an index into the *displayed* line (gutter included); the copy path strips
    /// the gutter afterward. Column accounting assumes the unwrapped view (the common case for a
    /// mouse drag); precise mapping under the `w` wrap override is a follow-up.
    pub(crate) fn char_at_content_col(&self, col: u16, row: u16) -> (usize, usize) {
        if self.content.lines.is_empty() {
            return (1, 0); // no content to index (line-select can't open on an empty pane anyway)
        }
        let last = self.content.lines.len();
        let top = self.geom.content_inner.map_or(row, |c| c.y);
        let x = self.geom.content_inner.map_or(0, |c| c.x);
        let display_row = self.content_scroll as usize + row.saturating_sub(top) as usize;
        let line = self.line_at_content_row(display_row).clamp(1, last);
        // The active overlay may prepend leading glyph column(s), so real content begins that many
        // columns right of the pane origin; add horizontal scroll, then drop those glyph columns to
        // index the displayed line. L mode prepends a 1-col ▶/│ gutter glyph; the ambient overlay
        // prepends none (see `content_overlay_glyph_cols`).
        let within = (col.saturating_sub(x)) as usize + self.content_hscroll as usize;
        let disp_col = within.saturating_sub(self.content_overlay_glyph_cols());
        let caret = char_index_at_col(&self.filtered_display_line(line), disp_col);
        (line, caret)
    }

    /// How many leading display columns the ACTIVE content overlay prepends before the real text, so
    /// a mouse column can be mapped back to a character caret in [`char_at_content_col`]. L
    /// line-select mode draws a 1-column gutter glyph (▶ marker / │ bar) ahead of every line, so its
    /// caret math subtracts 1; the ambient selection overlay draws no glyph, so it subtracts 0. The
    /// L-mode overlay is the only one that prepends a glyph, so this is exactly `line_select_active()`.
    fn content_overlay_glyph_cols(&self) -> usize {
        if self.line_select_active() { 1 } else { 0 }
    }

    /// Auto-copy an ambient content-pane selection to the clipboard on the drag's release, leaving
    /// the highlight standing for feedback (unlike the L-mode confirm, which closes the mode). Reads
    /// the char span from [`content_selection`](Controller::content_selection) and builds the text
    /// with [`char_selection_text`](Self::char_selection_text) — the same per-line gutter strip +
    /// control-byte scrub (tabs kept) the L-mode copy uses, so code copies clean. Guards the
    /// no-real-file case like [`copy_line_content`](Self::copy_line_content): a directory / empty
    /// tree shows first-party guidance ("Directory —" / "No files") with no file node selected, so
    /// copy nothing then; a collapsed/empty span also copies nothing. (The other placeholder,
    /// "Rendering…", is shown while a file IS selected, so the file-node guard does NOT catch it —
    /// the press-time `content_rendering` check in `handle_column_mouse` prevents a selection ever
    /// being seeded over it.) Read-only: touches only the clipboard and the transient action notice.
    pub(super) fn copy_content_selection(&mut self) -> Effects {
        let Some((lo, hi)) = self.content_selection.as_ref().map(|s| s.char_span()) else {
            return Effects::redraw();
        };
        let is_file = self
            .tree
            .selected()
            .is_some_and(|node| node.kind == NodeKind::File);
        if !is_file {
            return Effects::redraw();
        }
        let text = self.char_selection_text(lo, hi);
        if text.is_empty() {
            return Effects::redraw();
        }
        self.action_notice = Some(match self.clipboard.copy(&text) {
            Ok(()) => "Copied selection".to_string(),
            Err(e) => format!("Could not copy selection: {e}"),
        });
        Effects::redraw()
    }

    /// The number of leading chars the syntax renderer's line-number gutter occupies on 1-based
    /// source `line` — constant across the source-mapped view, so any line in it measures the same.
    /// `0` when there is no gutter (plain-text fallback / test stubs) or empty content. Both content
    /// overlays use it so a character selection's highlight never paints the gutter on continuation
    /// lines: the L-mode overlay measures it off the marker line, the ambient overlay off its start
    /// line. `line` is clamped into `[1, line_count]`.
    pub(crate) fn content_gutter_len(&self, line: usize) -> usize {
        let total = self.content.lines.len();
        if total == 0 {
            return 0;
        }
        let n = line.clamp(1, total);
        let displayed = self.filtered_display_line(n);
        let code = strip_line_gutter(&displayed, n);
        displayed.chars().count() - code.chars().count()
    }

    /// The gutter width on the L-mode marker line — [`content_gutter_len`](Self::content_gutter_len)
    /// measured off the active line-select marker. `0` when line-select is inactive or content is
    /// empty. The Presenter uses it for the L-mode character-selection highlight.
    pub(crate) fn selection_gutter_len(&self) -> usize {
        let Some(s) = self.modal.line_select() else {
            return 0;
        };
        self.content_gutter_len(s.marker())
    }

    /// The text of a character-granular selection from `(lo_line, lo_col)` to `(hi_line, hi_col)`
    /// (both ordered ascending, char carets into the displayed line). The gutter is stripped per
    /// line and the carets are re-based into the ungutter'd code, so the copy is source text alone:
    /// the tail of the first line, whole middle lines, and the head of the last, joined by `\n`.
    /// A collapsed selection (same line and caret) yields the empty string.
    fn char_selection_text(
        &self,
        (lo_line, lo_col): (usize, usize),
        (hi_line, hi_col): (usize, usize),
    ) -> String {
        let total = self.content.lines.len();
        if total == 0 {
            return String::new();
        }
        let lo_line = lo_line.clamp(1, total);
        let hi_line = hi_line.clamp(1, total);
        (lo_line..=hi_line)
            .map(|n| {
                let displayed = self.filtered_display_line(n);
                let code = strip_line_gutter(&displayed, n);
                // Chars the gutter occupies (constant across the view, but derive it per line so a
                // gutter-less renderer yields 0), so a display-char caret maps into the code.
                let gutter = displayed.chars().count() - code.chars().count();
                let chars: Vec<char> = code.chars().collect();
                let start = if n == lo_line {
                    lo_col.saturating_sub(gutter)
                } else {
                    0
                };
                let end = if n == hi_line {
                    hi_col.saturating_sub(gutter)
                } else {
                    chars.len()
                };
                let start = start.min(chars.len());
                let end = end.min(chars.len()).max(start);
                chars[start..end].iter().collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Copy the selected content to the clipboard and close the mode (the `Enter` / `y` / `Y`
    /// confirm). Whole lines for a keyboard (line-granular) selection via
    /// [`selected_lines_text`](Self::selected_lines_text), or the exact character span for a mouse
    /// drag via [`char_selection_text`](Self::char_selection_text). Surfaces a concise outcome
    /// notice — the line range or "selection", NOT the content itself, which may be many lines
    /// ("Copied line 5" / "Copied lines 5-8" / "Copied selection" on `Ok`; a failure message on
    /// `Err`, AC-10/AC-11).
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

        // Character granularity (a mouse drag) copies the exact selected span; line granularity
        // (keyboard) copies whole lines. A collapsed character selection (a plain click, nothing
        // dragged) falls back to copying the clicked line, so click-then-Enter still yields a line.
        let char_span = self
            .modal
            .line_select()
            .filter(|s| s.is_char_mode())
            .map(|s| s.char_span());
        let (text, label) = match char_span {
            Some((lo, hi)) => {
                let text = self.char_selection_text(lo, hi);
                if text.is_empty() {
                    let marker = self.line_selection().map_or(start, |(_, e)| e);
                    (
                        self.selected_lines_text(marker, marker),
                        format!("line {marker}"),
                    )
                } else {
                    (text, "selection".to_string())
                }
            }
            None => {
                let text = self.selected_lines_text(start, end);
                let label = if start == end {
                    format!("line {start}")
                } else {
                    format!("lines {start}-{end}")
                };
                (text, label)
            }
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
    /// stays visible (AC-7). `Esc` exits without copying (AC-4); `Enter` copies the selected
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
            // Enter / y / Y all confirm: copy the selection, then close the mode. `y`/`Y` reach here
            // because line-select routes every key through this handler, so the normal copy-path
            // bindings would otherwise be inert; wiring them matches the app's `y`/`Y`=copy idiom
            // (and vim's yank). `Y` arrives as the uppercase char + SHIFT, which the modifier guard
            // above permits — accepted so holding Shift never makes copy silently fail.
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
        assert_eq!(
            strip_line_gutter("   1 # Architecture", 1),
            "# Architecture"
        );
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
    fn char_index_at_col_maps_column_to_char_and_clamps() {
        assert_eq!(char_index_at_col("hello", 0), 0);
        assert_eq!(char_index_at_col("hello", 3), 3);
        // Past the end clamps to a caret at end-of-line.
        assert_eq!(char_index_at_col("hello", 99), 5);
        assert_eq!(char_index_at_col("", 4), 0);
    }

    #[test]
    fn char_span_orders_by_line_then_column() {
        // Anchor after marker on the same line → ordered ascending by column.
        let mut s = LineSelectState::new(3);
        s.begin_char(3, 7, 10);
        s.drag_char(3, 2, 10);
        assert_eq!(s.char_span(), ((3, 2), (3, 7)));
        assert!(s.is_char_mode());

        // Anchor on a later line than the marker → ordered ascending by line.
        let mut s = LineSelectState::new(5);
        s.begin_char(5, 1, 10);
        s.drag_char(2, 9, 10);
        assert_eq!(s.char_span(), ((2, 9), (5, 1)));
    }

    #[test]
    fn keyboard_move_reverts_to_line_mode() {
        let mut s = LineSelectState::new(1);
        s.begin_char(1, 4, 10);
        assert!(s.is_char_mode(), "a mouse press is character-granular");
        s.move_to(3, 10);
        assert!(
            !s.is_char_mode(),
            "a keyboard move reverts to line granularity"
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
