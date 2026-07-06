//! Column/tree pointer handling — wheel scroll, press/drag on the divider and scrollbars
//! (click-to-scroll), click vs double-click, and the scrollbar track\u2192offset math. Routed here
//! from `handle_mouse` for the non-modal case. Part of the Session Controller (M6 split).

use super::*;

impl Controller {
    /// Map a mouse event to a state change. Mouse is additive to the keyboard-first design
    /// (AC-18). A `Shift`+mouse event is left untouched so the terminal's own selection/copy
    /// still works (herdr reserves Shift+mouse for exactly that). Selection/activation happen
    /// on button *release*, so a divider drag is never mistaken for a click.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> Effects {
        // Modal gate, exhaustive over `Modal` (so a new overlay variant forces a mouse-routing
        // decision here, mirroring the keyboard gate in `handle`):
        // - Picker / Prompt are keyboard-only — swallow the mouse entirely. Without this a
        //   click/wheel would reach the tree/content beneath and change the selection under the
        //   modal, so a later confirm would act on the WRONG file (or strand an override on a dir).
        // - LineSelect owns the mouse over the content pane: route to its own handler (BEFORE the
        //   column handler) so a click places the marker and never leaks to the columns or
        //   starts a divider/scrollbar drag while the mode is active (AC-8/AC-9/AC-12).
        // - Help / Finder ARE mouse-interactive (wheel scrolls, click selects/switches): route to
        //   their own handler, which consumes every event and never leaks to the columns (AC-21).
        // - None: no modal → the two-column mouse handler below.
        match self.modal {
            Modal::Picker(_) | Modal::Prompt(_) => Effects::noop(),
            Modal::LineSelect(_) => self.handle_line_select_mouse(ev),
            Modal::Help(_) => self.handle_help_mouse(ev),
            Modal::Finder(_) => self.handle_finder_mouse(ev),
            Modal::None => self.handle_column_mouse(ev),
        }
    }

    /// Mouse handling while line-select mode owns the pointer (copy-line-reference). The mode is
    /// keyboard-first, but a click is a natural way to place the marker: a left **release** in the
    /// content pane ALWAYS moves the marker to the clicked source line (AC-8), regardless of the
    /// Shift modifier — herdr and most terminals reserve Shift+mouse for native text selection, so
    /// a shift-click extend can never be relied on to reach the plugin (keyboard `Shift`+`j`/`k`
    /// is the supported way to extend). A double-click copies the reference and closes the mode
    /// (AC-9) — mirroring the keyboard move / Enter. Every other event kind (press / drag / wheel)
    /// is inert, so a click can't start a divider or scrollbar drag while the mode holds the
    /// mouse. A click outside the content region is inert too and clears any pending double-click,
    /// so a stray click can't pair a later one across regions.
    fn handle_line_select_mouse(&mut self, ev: MouseEvent) -> Effects {
        // Act only on a completed left-click (consistent with `handle_click`); swallow the rest so
        // the mode keeps the mouse and no divider/scrollbar drag starts underneath.
        if ev.kind != MouseEventKind::Up(MouseButton::Left) {
            return Effects::noop();
        }
        let (col, row) = (ev.column, ev.row);
        if self.hit_test(col, row) != MouseRegion::Content {
            self.last_click = None; // outside content → inert, and break any pending double-click
            return Effects::noop();
        }
        // Map the clicked screen row to a 1-based source line. The content area's top row is the
        // content rect's `y`, so the clicked line's display-row offset is `content_scroll + (row -
        // content_top)`; `line_at_content_row` maps that offset back to a source line, correctly even
        // when the `w` wrap override is on (a source line then spans several display rows, so the
        // mapping is NOT 1:1). Clamp into `[1, line_count]`. `hit_test == Content` guarantees
        // `content_inner` is `Some` and `row >= content_inner.y`, so the subtraction never underflows.
        let content_top = self.geom.content_inner.map_or(row, |c| c.y);
        let last = self.content.lines.len().max(1);
        let display_row = self.content_scroll as usize + row.saturating_sub(content_top) as usize;
        let line = self.line_at_content_row(display_row).clamp(1, last);

        // Reuse the exact click-detection the columns use: same-row within the window is a
        // double-click, and every non-content click above cleared `last_click`.
        let now = Instant::now();
        let double = is_double_click(self.last_click, (col, row), now);
        self.last_click = Some((col, row, now));

        if double {
            return self.copy_line_reference(); // AC-9: copy + close, same as Enter
        }
        if let Some(state) = self.modal.line_select_mut() {
            // A single click ALWAYS places the marker (collapsing the anchor to the clicked
            // line), regardless of the Shift modifier: herdr and most terminals reserve
            // Shift+mouse for their own native text selection, so a shift-click never reliably
            // reaches the plugin. If one does get through anyway, it harmlessly places the
            // marker rather than claiming an extend we can't guarantee. Keyboard `Shift`+`j`/`k`
            // (and Shift+arrows) is the supported way to extend a multi-line selection.
            state.move_to(line, last); // AC-8: click places the marker
        }
        Effects::redraw()
    }

    /// Handle a mouse event over the two columns, with no modal open (the [`handle_mouse`] gate
    /// routes here only for [`Modal::None`]). Shift+mouse is inert so the terminal can do its own
    /// text selection; otherwise the wheel scrolls the column under the pointer, a left press
    /// starts a divider or scrollbar drag (scrollbars also jump to the pressed position), and a
    /// left release is a click — unless it ended a drag, in which case it's consumed.
    fn handle_column_mouse(&mut self, ev: MouseEvent) -> Effects {
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            return Effects::noop();
        }
        let (col, row) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::ScrollDown => self.scroll_at(col, row, WHEEL_STEP),
            MouseEventKind::ScrollUp => self.scroll_at(col, row, -WHEEL_STEP),
            MouseEventKind::ScrollRight => self.hscroll_at(col, row, HSCROLL_STEP as i32),
            MouseEventKind::ScrollLeft => self.hscroll_at(col, row, -(HSCROLL_STEP as i32)),
            MouseEventKind::Down(MouseButton::Left) => {
                // A press on the divider begins a resize drag; on a scrollbar it begins a scroll
                // drag AND jumps to the pressed position (click-to-scroll). Anything else waits for
                // the release (a click). Always (re)set `drag` from the press — so a stale drag from
                // a release we never saw (e.g. swallowed by a modal) can't keep acting on later moves.
                let region = self.hit_test(col, row);
                self.drag = match region {
                    MouseRegion::Divider => Some(Drag::Divider),
                    MouseRegion::ContentVBar => Some(Drag::ContentV),
                    MouseRegion::ContentHBar => Some(Drag::ContentH),
                    MouseRegion::TreeVBar => Some(Drag::TreeV),
                    MouseRegion::TreeHBar => Some(Drag::TreeH),
                    _ => None,
                };
                match region {
                    MouseRegion::ContentVBar => self.scroll_content_to_row(row),
                    MouseRegion::ContentHBar => self.scroll_content_h_to_col(col),
                    MouseRegion::TreeVBar => self.scroll_tree_to_row(row),
                    MouseRegion::TreeHBar => self.scroll_tree_h_to_col(col),
                    _ => Effects::noop(),
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                Some(Drag::Divider) => self.resize_split_to_col(col),
                Some(Drag::ContentV) => self.scroll_content_to_row(row),
                Some(Drag::ContentH) => self.scroll_content_h_to_col(col),
                Some(Drag::TreeV) => self.scroll_tree_to_row(row),
                Some(Drag::TreeH) => self.scroll_tree_h_to_col(col),
                // The finder is modal: its scrollbar drag is handled in handle_finder_mouse and
                // never reaches this (non-finder) path. Covered here only for exhaustiveness.
                Some(Drag::FinderV) => Effects::noop(),
                None => Effects::noop(),
            },
            MouseEventKind::Up(MouseButton::Left) => {
                if self.drag.take().is_some() {
                    // End of a drag, not a click. Clear the pending-click so a tree-row click made
                    // before the drag can't pair with a later one as a double-click — the drag may
                    // have scrolled the viewport, so the same screen row now maps to a different node.
                    self.last_click = None;
                    return Effects::noop();
                }
                self.handle_click(col, row)
            }
            _ => Effects::noop(),
        }
    }

    /// A completed left-click: select the tree row it landed on (or focus the content pane). A
    /// double-click [`activate`](Self::activate)s the row — a directory toggles expand/collapse,
    /// a file opens in zoom mode (the editor hand-off is the `e` key, not the mouse).
    fn handle_click(&mut self, col: u16, row: u16) -> Effects {
        let region = self.hit_test(col, row);
        let now = Instant::now();
        match region {
            MouseRegion::TreeRow(idx) => {
                if idx >= self.tree.visible_nodes().len() {
                    self.last_click = None; // empty area below the nodes — inert, and breaks any
                    return Effects::noop(); // pending double-click sequence
                }
                // A double-click is two clicks on the SAME tree row within the window. Because
                // every non-tree-row click clears `last_click` (below), AND the finder's
                // open/confirm/Esc paths also clear it, `last_click` only ever holds a prior
                // tree-row click — the column-agnostic same-row match in `is_double_click`
                // cannot be tripped by a click in a different context (another pane or the finder).
                let double = is_double_click(self.last_click, (col, row), now);
                self.last_click = Some((col, row, now));
                self.action_notice = None;
                self.focus = Focus::Tree;
                self.tree.set_cursor(idx);
                self.dispatch_render(); // selection changed → re-render the content pane
                if double {
                    return self.activate(); // folder → expand/collapse, file → zoom mode
                }
                Effects::redraw()
            }
            MouseRegion::Content => {
                self.last_click = None; // a non-tree click breaks any pending double-click
                self.focus = Focus::Content;
                Effects::redraw()
            }
            // Scrollbars are handled on press/drag (above), not as a click; reaching here is inert.
            MouseRegion::Divider
            | MouseRegion::ContentVBar
            | MouseRegion::ContentHBar
            | MouseRegion::TreeVBar
            | MouseRegion::TreeHBar
            | MouseRegion::Outside => {
                self.last_click = None;
                Effects::noop()
            }
        }
    }

    /// Scroll the pane under the cursor: the content pane scrolls vertically; over the tree the
    /// wheel moves the selection (the tree then scrolls to keep it in view, #45).
    fn scroll_at(&mut self, col: u16, row: u16, delta: isize) -> Effects {
        match self.hit_test(col, row) {
            MouseRegion::Content => {
                self.scroll_content(delta);
                Effects::redraw()
            }
            MouseRegion::TreeRow(_) => {
                self.focus = Focus::Tree;
                self.tree.move_cursor(delta.signum());
                self.dispatch_render();
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Horizontal wheel / trackpad swipe scrolls sideways: the content pane (like the `←`/`→`
    /// keys, for unwrapped long lines) or the tree (like the `H`/`L` keys). Each clamps to
    /// `[0, widest − viewport]`, so it is inert when nothing overflows.
    fn hscroll_at(&mut self, col: u16, row: u16, delta: i32) -> Effects {
        match self.hit_test(col, row) {
            MouseRegion::Content => self.scroll_content_h(delta),
            MouseRegion::TreeRow(_) => self.scroll_tree_h(delta),
            _ => Effects::noop(),
        }
    }

    /// Scroll the tree horizontally by `delta` columns, clamped to `[0, widest − tree width]` from
    /// the last drawn frame, so a long / deeply-nested row can be read sideways without ever
    /// over-scrolling past the content.
    fn scroll_tree_h(&mut self, delta: i32) -> Effects {
        let max = self
            .geom
            .tree_inner
            .map_or(0, |t| self.geom.tree_content_width.saturating_sub(t.width));
        let next = (self.tree_hscroll as i32 + delta).clamp(0, max as i32);
        self.tree_hscroll = next as u16;
        Effects::redraw()
    }

    /// The keyboard path for tree horizontal scroll (AC-18): `H`/`L` move `tree_hscroll` by the
    /// same step the mouse wheel uses, clamped to the measured max — mirroring how the content
    /// pane's `←`/`→` scroll `content_hscroll`. Inert unless the tree is focused, so the keys
    /// never fight the content pane's own horizontal scroll when the content is focused.
    pub(super) fn scroll_tree_h_focus(&mut self, delta: i32) -> Effects {
        if self.focus != Focus::Tree {
            return Effects::noop();
        }
        self.scroll_tree_h(delta)
    }

    /// The fraction `[0,1]` of a press/drag along a scrollbar track of `len` cells starting at
    /// `start`, as a rounding numerator/denominator: returns `(rel, span)` so callers stay in
    /// integer math (`offset = round(rel/span * max)`). `span` is 0 for a degenerate 1-cell track.
    pub(super) fn track_fraction(pos: u16, start: u16, len: u16) -> (u32, u32) {
        let rel = pos.saturating_sub(start).min(len.saturating_sub(1)) as u32;
        (rel, len.saturating_sub(1) as u32)
    }

    /// Map a press/drag at `pos` on a scrollbar track `[start, start + len)` onto an offset in
    /// `[0, max]`, rounded to the nearest integer. `None` (the caller no-ops) when the mapping is
    /// degenerate — a 1-cell track (`span == 0`) or a zero range (`max == 0`). The single
    /// track→offset rounding rule every linear scrollbar drag shares: a vertical bar passes the
    /// row + track `y`/`height`, a horizontal bar the col + track `x`/`width`. (The finder maps a
    /// drag to a *match index* and intentionally differs at the degenerate boundary, so it keeps
    /// its own mapping — see `finder_scroll_to_row`.)
    fn track_to_offset(pos: u16, start: u16, len: u16, max: u32) -> Option<u32> {
        let (rel, span) = Self::track_fraction(pos, start, len);
        if span == 0 || max == 0 {
            return None;
        }
        Some((rel * max + span / 2) / span)
    }

    /// Map a vertical press/drag on the content scrollbar track to a content scroll offset. The
    /// track is the fed-back `content_vbar` rect; the fraction maps linearly onto
    /// `[0, max_content_scroll]`, rounded to the nearest line. No-op without overflow.
    fn scroll_content_to_row(&mut self, row: u16) -> Effects {
        let Some(track) = self.geom.content_vbar else {
            return Effects::noop();
        };
        let Some(off) =
            Self::track_to_offset(row, track.y, track.height, self.max_content_scroll() as u32)
        else {
            return Effects::noop();
        };
        self.content_scroll = off as u16;
        Effects::redraw()
    }

    /// Map a horizontal press/drag on the content horizontal scrollbar to a content h-scroll offset.
    fn scroll_content_h_to_col(&mut self, col: u16) -> Effects {
        let Some(track) = self.geom.content_hbar else {
            return Effects::noop();
        };
        let Some(off) =
            Self::track_to_offset(col, track.x, track.width, self.max_content_hscroll() as u32)
        else {
            return Effects::noop();
        };
        self.content_hscroll = off as u16;
        Effects::redraw()
    }

    /// Map a horizontal press/drag on the tree's horizontal scrollbar to a tree h-scroll offset.
    fn scroll_tree_h_to_col(&mut self, col: u16) -> Effects {
        let Some(track) = self.geom.tree_hbar else {
            return Effects::noop();
        };
        let max = self.geom.tree_content_width.saturating_sub(track.width);
        let Some(off) = Self::track_to_offset(col, track.x, track.width, max as u32) else {
            return Effects::noop();
        };
        self.tree_hscroll = off as u16;
        Effects::redraw()
    }

    /// Map a vertical press/drag on the tree's vertical scrollbar to a selection — scrubbing the
    /// cursor through the file list, which scrolls the tree to keep it in view (the tree has no
    /// independent vertical offset; its position follows the selection, #45).
    fn scroll_tree_to_row(&mut self, row: u16) -> Effects {
        let Some(track) = self.geom.tree_vbar else {
            return Effects::noop();
        };
        let len = self.tree.visible_nodes().len();
        // `max = len - 1` (the last index): a 1-cell track or a list of ≤ 1 node yields `None` here,
        // exactly the old `span == 0 || len <= 1` no-op.
        let Some(idx) =
            Self::track_to_offset(row, track.y, track.height, len.saturating_sub(1) as u32)
        else {
            return Effects::noop();
        };
        let idx = idx as usize;
        self.focus = Focus::Tree;
        // A drag fires many events on the same row; only re-select (and re-render the content, an
        // expensive job) when the target actually changes, so a held scrub doesn't re-render the
        // same file every tick.
        if idx == self.tree.cursor() {
            return Effects::redraw();
        }
        self.tree.set_cursor(idx);
        self.dispatch_render();
        Effects::redraw()
    }

    /// During a divider drag, set the split so the divider tracks the cursor column — clamped
    /// like the keyboard resize so neither column can collapse.
    fn resize_split_to_col(&mut self, col: u16) -> Effects {
        if self.geom.area_width == 0 {
            return Effects::noop();
        }
        let tree_w = col.saturating_sub(self.geom.area_x) as i32;
        let pct =
            (tree_w * 100 / self.geom.area_width as i32).clamp(SPLIT_MIN as i32, SPLIT_MAX as i32);
        self.split_pct = pct as u16;
        Effects::redraw()
    }

    /// Which region of the last-drawn frame a cell falls in. The divider is checked first (it
    /// sits between the columns); a tree click maps to a visible node index by its row.
    fn hit_test(&self, col: u16, row: u16) -> MouseRegion {
        if let Some(dx) = self.geom.divider_x
            && (col == dx || col + 1 == dx)
        {
            return MouseRegion::Divider;
        }
        // Scrollbars live INSIDE the panes (a reserved gutter), fed back as 1-cell track rects that
        // are present only when that bar is drawn — so a hit on a `Some` track is a real bar. Check
        // them before the text rects. The tree's vertical bar no longer shares the divider column.
        let pos = Position { x: col, y: row };
        if self.geom.content_vbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::ContentVBar;
        }
        if self.geom.content_hbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::ContentHBar;
        }
        if self.geom.tree_vbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::TreeVBar;
        }
        if self.geom.tree_hbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::TreeHBar;
        }
        if let Some(t) = self.geom.tree_inner
            && t.contains(pos)
        {
            // Map the screen row to the node actually drawn there: the on-screen offset plus the
            // tree's scroll offset (#45), the same value `draw_tree` scrolled by. The row index may
            // still exceed the node count (the empty area below the last node): the click handler
            // treats that as inert, while the wheel still scrolls the column.
            return MouseRegion::TreeRow((row - t.y) as usize + self.geom.tree_scroll as usize);
        }
        if let Some(c) = self.geom.content_inner
            && c.contains(Position { x: col, y: row })
        {
            return MouseRegion::Content;
        }
        MouseRegion::Outside
    }
}

/// Two left-clicks at the same cell within this window are a double-click (a folder toggles
/// expand/collapse; a file opens in zoom mode — the editor hand-off is the `e` key).
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Two left-clicks on the same **row** within [`DOUBLE_CLICK`] are a double-click. The column
/// is ignored on purpose: a tree row is a single node end-to-end, so a click anywhere along it
/// targets that node, and a touchpad double-tap commonly lands a column or two apart between
/// taps — requiring the exact cell would silently drop those. (The column still matters for
/// *which* node a click selects; that is the caller's hit-test, not this timing rule.) Pure over
/// its timestamps so the timing rule is unit-testable without sleeping.
pub(super) fn is_double_click(
    prev: Option<(u16, u16, Instant)>,
    pos: (u16, u16),
    now: Instant,
) -> bool {
    matches!(prev, Some((_px, py, t)) if py == pos.1 && now.saturating_duration_since(t) <= DOUBLE_CLICK)
}

#[cfg(test)]
mod tests {
    use super::{DOUBLE_CLICK, is_double_click};
    use std::time::Instant;

    #[test]
    fn is_double_click_requires_the_same_row_within_the_window() {
        let t0 = Instant::now();
        let within = t0 + DOUBLE_CLICK / 2;
        let after = t0 + DOUBLE_CLICK * 2;
        // Same cell, inside the window → double-click.
        assert!(is_double_click(Some((5, 5, t0)), (5, 5), within));
        // Same ROW, different column, inside the window → still a double-click. A tree row is
        // one node end-to-end, and a touchpad double-tap often lands a column or two apart, so
        // requiring the exact cell would drop legitimate double-taps.
        assert!(is_double_click(Some((5, 5, t0)), (40, 5), within));
        // Too slow → not a double-click.
        assert!(!is_double_click(Some((5, 5, t0)), (5, 5), after));
        // A different ROW → not a double-click (it would target a different node).
        assert!(!is_double_click(Some((5, 5, t0)), (5, 6), within));
        // No previous click → never a double-click.
        assert!(!is_double_click(None, (5, 5), within));
    }
}
