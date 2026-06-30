//! Go-to-file finder (`f`) — open, key + mouse handling, confirm, and the draw-model
//! projection. Part of the Session Controller (split out of `controller/mod.rs`, M6).

use super::*;
// The shared double-click timing rule lives with click handling in the `mouse` submodule.
use super::mouse::is_double_click;

impl Controller {
    /// The owned finder draw model for the Presenter (AC-1, AC-2, AC-5), or `None` when the finder
    /// is closed. Resolves the ranked match indices into owned root-relative path strings so the
    /// Presenter is borrow-free; carries the current query and cursor. The Presenter sanitizes the
    /// path strings (AC-27) and renders the query-input line + placeholder + match rows.
    pub(super) fn finder_view(&self) -> Option<FinderView> {
        let f = self.modal.finder()?;
        Some(FinderView {
            query: f.query().to_string(),
            matches: f
                .matches()
                .iter()
                .map(|&i| f.candidates()[i].clone())
                .collect(),
            cursor: f.cursor(),
            hscroll: f.hscroll(),
        })
    }

    /// Handle a mouse event while the go-to-file finder is open (the finder is mouse-interactive;
    /// it owns all mouse while open and never leaks events to the tree/content beneath).
    ///
    /// - `ScrollDown`/`ScrollUp` → move the finder selection by `WHEEL_STEP`, clamped.
    ///   Position-independent (the finder is the active modal).
    /// - `Up(Left)` → click on a result row (select; double-click confirms).
    /// - `Down`/`Drag`/other → inert no-op (no drag in the finder).
    /// - `Shift`+mouse → inert (terminal selection, same as the main gate).
    pub(super) fn handle_finder_mouse(&mut self, ev: MouseEvent) -> Effects {
        use ratatui::layout::Position;
        // Shift+mouse: terminal selection — inert, same as the main mouse gate.
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            return Effects::noop();
        }
        match ev.kind {
            MouseEventKind::ScrollDown => self.finder_move_selection(WHEEL_STEP),
            MouseEventKind::ScrollUp => self.finder_move_selection(-WHEEL_STEP),
            // Horizontal wheel: scroll the result rows sideways, mirroring the vertical-wheel
            // handling above. Additive to the keyboard ←/→ scroll (AC-18 keyboard-first).
            MouseEventKind::ScrollRight => {
                if let Some(f) = self.modal.finder_mut() {
                    f.scroll_right();
                    Effects::redraw()
                } else {
                    Effects::noop()
                }
            }
            MouseEventKind::ScrollLeft => {
                if let Some(f) = self.modal.finder_mut() {
                    f.scroll_left();
                    Effects::redraw()
                } else {
                    Effects::noop()
                }
            }
            // A press on the finder's vertical scrollbar starts a scroll-drag AND jumps the
            // selection to the pressed position (click-to-scroll), mirroring the content/tree bars.
            // Any other press waits for the release (a click on a result row). Always (re)set `drag`
            // from the press so a stale drag can't keep acting on later moves.
            MouseEventKind::Down(MouseButton::Left) => {
                self.drag = if self.geom.finder_vbar.is_some_and(|t| {
                    t.contains(Position {
                        x: ev.column,
                        y: ev.row,
                    })
                }) {
                    Some(Drag::FinderV)
                } else {
                    None
                };
                if matches!(self.drag, Some(Drag::FinderV)) {
                    self.finder_scroll_to_row(ev.row)
                } else {
                    Effects::noop()
                }
            }
            // Continue a scrollbar drag: map the row to a selection position.
            MouseEventKind::Drag(MouseButton::Left) if matches!(self.drag, Some(Drag::FinderV)) => {
                self.finder_scroll_to_row(ev.row)
            }
            // A release ends a scrollbar drag (not a row click); otherwise it is a click on a row.
            MouseEventKind::Up(MouseButton::Left) => {
                if self.drag.take().is_some() {
                    self.last_click = None; // a drag-release is not a click; break double-click pairing
                    Effects::noop()
                } else {
                    self.handle_finder_click(ev.column, ev.row)
                }
            }
            // Other events (right/middle button, Moved, a Drag with no active finder drag): inert.
            _ => Effects::noop(),
        }
    }

    /// Map a vertical press/drag on the finder's scrollbar track (`geom.finder_vbar`) to a
    /// selection position: the fraction along the track maps linearly onto the match list and moves
    /// the cursor (the finder window follows the cursor, and the scrollbar thumb tracks it). No-op
    /// without a drawn bar or any matches.
    fn finder_scroll_to_row(&mut self, row: u16) -> Effects {
        let Some(track) = self.geom.finder_vbar else {
            return Effects::noop();
        };
        let (rel, span) = Self::track_fraction(row, track.y, track.height);
        let Some(finder) = self.modal.finder_mut() else {
            return Effects::noop();
        };
        let total = finder.matches().len();
        if span == 0 || total == 0 {
            return Effects::noop();
        }
        // Its own mapping, not the shared `track_to_offset`: with a single match the finder maps a
        // drag to index 0 and selects it, whereas `track_to_offset` no-ops at a zero range — a
        // deliberate difference for the modal list, so the rounding stays inline here.
        let max = (total - 1) as u32;
        let idx = ((rel * max + span / 2) / span) as usize;
        finder.set_cursor(idx);
        Effects::redraw()
    }

    /// Move the finder selection by `delta` rows (positive = down, negative = up), clamped. A
    /// no-op when the finder is closed or the match list is empty.
    fn finder_move_selection(&mut self, delta: isize) -> Effects {
        if let Some(f) = self.modal.finder_mut() {
            f.move_selection(delta);
            Effects::redraw()
        } else {
            Effects::noop()
        }
    }

    /// Handle a left-button release while the finder is open. Maps the screen cell `(col, row)`
    /// to a result-row index via `self.geom.finder_rows` + `self.geom.finder_scroll`. A click
    /// inside the rows area selects that row (double-click confirms); a click anywhere else is a
    /// modal no-op (the finder stays open — Esc cancels, not an outside click).
    fn handle_finder_click(&mut self, col: u16, row: u16) -> Effects {
        use ratatui::layout::Position;
        let Some(rows_rect) = self.geom.finder_rows else {
            // No rows area (empty query or zero matches) — click is inert but modal.
            self.last_click = None;
            return Effects::noop();
        };
        if !rows_rect.contains(Position { x: col, y: row }) {
            // Click outside the rows area (on the border, query line, etc.) — inert, modal.
            self.last_click = None;
            return Effects::noop();
        }
        // Map screen row → absolute match-list index.
        let idx = self.geom.finder_scroll as usize + (row - rows_rect.y) as usize;
        let Some(finder) = self.modal.finder() else {
            return Effects::noop();
        };
        if idx >= finder.matches().len() {
            // Click landed in the empty area below the last result row — inert.
            self.last_click = None;
            return Effects::noop();
        }
        let now = Instant::now();
        let double = is_double_click(self.last_click, (col, row), now);
        self.last_click = Some((col, row, now));
        // Set the finder cursor to the clicked row.
        if let Some(f) = self.modal.finder_mut() {
            f.set_cursor(idx);
        }
        if double {
            // Double-click: confirm (same as Enter — reveal + render + close).
            return self.confirm_finder();
        }
        Effects::redraw()
    }

    /// Open the go-to-file finder (AC-1). Builds the file index for the current root, then
    /// installs a fresh `FinderState` with an empty query and the full candidate list.
    /// Returns [`Effects::redraw`] so the run loop paints the overlay on the next tick.
    ///
    /// Modal mutual-exclusion (finder inert while the picker is open) holds BY CONSTRUCTION:
    /// `handle()` routes to `handle_picker_intent()` while `self.modal.picker().is_some()`, and its
    /// catch-all `_ => Effects::noop()` swallows `OpenFinder`. No extra guard is needed here.
    pub(super) fn open_finder(&mut self) -> Effects {
        let candidates = crate::index::build(&self.root);
        self.modal = Modal::Finder(FinderState::new(candidates));
        self.last_click = None; // opening the finder resets double-click state so a prior tree
        // click cannot pair with the first finder click as a double-click
        Effects::redraw()
    }

    /// Route a key event while the finder overlay is open.
    ///
    /// - A printable `Char(c)` with no modifier other than `SHIFT` pushes the character,
    ///   re-runs [`fuzzy::match_and_rank`] over the candidates, and resets the selection
    ///   to 0 (AC-7).
    /// - `Backspace` deletes the last character and re-matches (AC-7).
    /// - `Up`/`Down` move the selection within the current match list, clamped at both ends
    ///   (AC-8).
    /// - `Enter` confirms the selection — reveal + render, or a non-fatal notice on a vanished
    ///   target, or a no-op that keeps the finder open when there are no matches (AC-6, AC-10,
    ///   AC-11, AC-20). `Esc` discards the finder, leaving the prior state intact (AC-9).
    ///
    /// When the finder is not open, all keys are a no-op (defensive guard).
    pub fn handle_finder_key(&mut self, key: KeyEvent) -> Effects {
        let Some(finder) = self.modal.finder_mut() else {
            return Effects::noop();
        };
        let effects = match key.code {
            KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                finder.push(c);
                Effects::redraw()
            }
            KeyCode::Backspace => {
                finder.backspace();
                Effects::redraw()
            }
            KeyCode::Up => {
                finder.move_selection(-1);
                Effects::redraw()
            }
            KeyCode::Down => {
                finder.move_selection(1);
                Effects::redraw()
            }
            // Left/Right: horizontal scroll of the result rows. The prompt is append-only so the
            // arrow keys are free — exactly as the picker uses ←/→ for hscroll. The Presenter
            // clamps to `max_row_width − inner_width` at draw, so over-scrolling is harmless here.
            KeyCode::Left => {
                finder.scroll_left();
                Effects::redraw()
            }
            KeyCode::Right => {
                finder.scroll_right();
                Effects::redraw()
            }
            // Enter/Esc dismiss or confirm; both already reset `last_click` (confirm_finder, and
            // the Esc arm) and return early, so they never reach the reset below.
            KeyCode::Enter => return self.confirm_finder(),
            KeyCode::Esc => {
                self.modal = Modal::None;
                self.last_click = None; // closing the finder resets double-click state so a
                // finder click cannot pair with the next tree click
                return Effects::redraw();
            }
            _ => Effects::noop(),
        };
        // A query edit, selection move, or scroll resets a PENDING mouse
        // double-click. Without this, a finder click → keystroke/nav → click on the SAME screen
        // row within the double-click window would be misread as a double-click (confirm), opening
        // a file the user only single-clicked — often a different one, since typing changed the
        // match list. Mirrors the open/Esc/confirm `last_click` clears for the keystroke/nav vector.
        self.last_click = None;
        effects
    }

    /// Confirm the current finder selection: take the selected candidate's root-relative path,
    /// join with the root, and call [`TreeModel::reveal`]. On success re-sync the controller's
    /// filter mirrors (reveal may have relaxed `changed_only`/`hide_hidden` in the tree),
    /// dispatch a render for the newly-selected file, close the finder, and return a redraw.
    ///
    /// - Zero matches (empty list) → no-op; finder stays open (AC-6).
    /// - Reveal returns `false` (target missing/removed since open) → close the finder, set a
    ///   non-fatal `action_notice`, leave the tree selection unchanged (AC-20).
    fn confirm_finder(&mut self) -> Effects {
        let Some(finder) = self.modal.finder() else {
            return Effects::noop();
        };
        let Some(cand_idx) = finder.selected_candidate_index() else {
            return Effects::noop(); // zero matches → no-op, finder stays open (AC-6)
        };
        let rel = finder.candidates()[cand_idx].clone();
        let abs = self.root.join(&rel);
        self.modal = Modal::None; // confirm dismisses the modal regardless of reveal outcome
        self.last_click = None; // closing the finder resets double-click state
        if self.tree.reveal(&abs) {
            // reveal() may have relaxed the tree's changed_only/hide_hidden fields — re-sync
            // the controller's mirror fields so a later `c`/`.` toggle stays consistent
            // (the mirrors at controller.rs:166-168 drive those toggles).
            self.changed_only = self.tree.changed_only();
            self.hide_hidden = self.tree.hide_hidden();
            // If the content pane isn't currently visible — the narrow, tree-only layout where the
            // last frame drew no content column (`content_width == 0`) — open the jumped-to file in
            // zoom mode so the user actually SEES the file they jumped to, instead of landing on a
            // tree row with the file hidden off-screen. This mirrors the tree's Enter/activate on a
            // file (content full-screen). When the content is already visible (the wide two-column
            // layout, or already zoomed), the layout is left untouched and the file just renders.
            if self.content_width == 0 {
                self.zoomed = true;
                self.focus = Focus::Content;
            }
            self.dispatch_render();
            Effects::redraw()
        } else {
            // Target has disappeared since the finder was opened — non-fatal notice (AC-20).
            self.action_notice = Some(format!("Could not open {rel}"));
            Effects::redraw()
        }
    }
}
