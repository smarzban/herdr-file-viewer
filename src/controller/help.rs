//! Help overlay (`?`) — open/close, key + mouse handling, and the draw-model projection.
//! Part of the Session Controller (split out of `controller/mod.rs`, M6).

use super::*;

impl Controller {
    /// The owned help-overlay draw model for the Presenter (AC-5, AC-11), or `None` when the
    /// overlay is closed. Projects [`HelpState`] → [`HelpView`]: the active index, the section
    /// labels, the active body (cloned so the Presenter stays borrow-free) + its scroll, and the
    /// self-operating key-hints footer string (AC-11). The footer is built here so the Presenter
    /// stays mode-agnostic — it shows, at minimum, how to switch sections and how to close.
    pub(super) fn help_view(&self) -> Option<HelpView> {
        let help = self.modal.help()?;
        let active = help.active_index();
        let labels: Vec<String> = help
            .section_labels()
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Center the About section only; What's New stays left-aligned. About is the section whose
        // label is `HelpSection::About::label()` ("About") — matched by label so the projection
        // stays decoupled from the section index (the Vec is the settings seam).
        let center = labels.get(active).map(String::as_str) == Some(HelpSection::About.label());
        Some(HelpView {
            active,
            labels,
            body: help.active_body().clone(),
            scroll: help.sections[active].scroll,
            hint: HELP_FOOTER_HINT.to_string(),
            center,
        })
    }

    /// Handle a mouse event while the help overlay is open. The help overlay owns all mouse while
    /// open and never leaks events to the tree/content beneath (AC-21) — mirroring
    /// [`handle_finder_mouse`](Self::handle_finder_mouse).
    ///
    /// - `ScrollDown`/`ScrollUp` → `scroll_by(±WHEEL_STEP)` on the active section (AC-8 via mouse).
    ///   No clamp here: [`set_pane_geometry`](Self::set_pane_geometry) re-clamps the stored scroll to
    ///   the live measured body height after the next draw (the same split the keyboard path uses).
    /// - `Down(Left)` whose `(col,row)` lands on a section-tab rect → `select(that index)` (AC-10).
    /// - `Shift`+mouse → inert (terminal selection, same as the main gate).
    /// - everything else → consumed no-op (`Effects::noop()`).
    pub(super) fn handle_help_mouse(&mut self, ev: MouseEvent) -> Effects {
        use ratatui::layout::Position;
        // Shift+mouse: terminal selection — inert, same as the main mouse gate.
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            return Effects::noop();
        }
        match ev.kind {
            MouseEventKind::ScrollDown => self.help_scroll(WHEEL_STEP),
            MouseEventKind::ScrollUp => self.help_scroll(-WHEEL_STEP),
            // A left press on a section tab switches sections (AC-10). Hit-test against the tab rects
            // the Presenter fed back (`geom.help_tabs`), so the click maps to the tab actually drawn.
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = Position {
                    x: ev.column,
                    y: ev.row,
                };
                let hit = self
                    .geom
                    .help_tabs
                    .iter()
                    .find(|(_, r)| r.contains(pos))
                    .map(|(idx, _)| *idx);
                if let Some(idx) = hit
                    && let Some(help) = self.modal.help_mut()
                {
                    help.select(idx);
                    return Effects::redraw();
                }
                // A press off every tab is a consumed no-op (modal — the overlay stays open).
                Effects::noop()
            }
            // Other events (drag, release, right/middle button, Moved): inert, but consumed.
            _ => Effects::noop(),
        }
    }

    /// Scroll the active help section by `delta` rows. A no-op when help is closed. After scrolling
    /// it clamps EAGERLY against the last-known geometry (mirrors the `j`/`Down` key path), so a
    /// wheel-down at the bottom never over-scrolls past the last wrapped row on the shown frame; the
    /// post-draw clamp in [`set_pane_geometry`](Self::set_pane_geometry) stays the resize backstop.
    fn help_scroll(&mut self, delta: isize) -> Effects {
        let (rows, height) = (self.geom.help_body_rows, self.geom.help_body_height);
        if let Some(help) = self.modal.help_mut() {
            help.scroll_by(delta as i32);
            // Eager clamp only once a frame has measured the body (rows > 0) — see handle_help_key.
            if rows > 0 {
                help.clamp_scroll(rows, height);
            }
            Effects::redraw()
        } else {
            Effects::noop()
        }
    }

    /// Open the in-app help overlay (AC-1, AC-6, AC-19). Builds two sections:
    ///
    /// - What's New: the embedded CHANGELOG rendered as markdown via `render::render`
    ///   (AC-14). If the markdown renderer is absent/times out, `render::render` falls back to
    ///   plain text + a notice (AC-15) — no extra handling needed here.
    /// - About: the about_text() string rendered as plain text.
    ///
    /// Sets the active section to 0 (What's New) and returns `Effects::redraw()`.
    pub(super) fn open_help(&mut self) -> Effects {
        let prepared = Prepared::Full {
            // Render from the first version heading onward — the changelog's file-meta preamble
            // (title + Keep-a-Changelog/SemVer paragraph + link refs) doesn't belong in What's New.
            text: crate::help::changelog_display().to_owned(),
        };
        // The render is synchronous on the input thread, so bound it to the help-specific
        // `HELP_RENDER_TIMEOUT` (within the AC-22 budget) rather than the shared 5s `RENDER_TIMEOUT`
        // — a slow/wedged renderer must not freeze input. On timeout `render::render` already falls
        // back to plain text + a notice (AC-15), so no new handling is needed here. This reconciles
        // the design's prerender-at-open with AC-22's responsiveness budget.
        //
        // Wrap the changelog at the help box's fixed body width (NOT the default `-w 0`): glow then
        // wraps with its own hanging indents, and the Presenter's `Paragraph::wrap` becomes a no-op
        // that preserves them. The width is the SAME constant the layout draws the body at
        // (`presenter::help_body_text_width`), so glow's wrapped lines fit exactly — never wider (a
        // wider glow wrap would re-introduce a flat 1-char re-wrap in the Presenter). The box is
        // fixed-width, so there is nothing to re-render on resize.
        let r = Renderers {
            timeout: HELP_RENDER_TIMEOUT,
            markdown: crate::render::with_wrap_width(
                &self.renderers.markdown,
                crate::presenter::help_body_text_width(),
            ),
            ..self.renderers.clone()
        };
        let (whats_new_body, _notice) =
            crate::render::render(&r, &prepared, ViewMode::RenderedMarkdown, None, None);
        let whats_new = HelpSectionState {
            label: HelpSection::WhatsNew.label(),
            body: whats_new_body,
            scroll: 0,
        };
        let about_body = crate::help::about_text(self.update_available);
        let about = HelpSectionState {
            label: HelpSection::About.label(),
            body: crate::render::to_text(&about_body),
            scroll: 0,
        };
        self.modal = Modal::Help(HelpState::new(vec![whats_new, about]));
        // Reset double-click state (mirrors open_finder): a tree click made just before the overlay
        // opened must not pair with a same-row click made just after it closes as a double-click.
        self.last_click = None;
        Effects::redraw()
    }

    /// The current help overlay state, or `None` when closed.
    /// Exposed for tests and the `ViewState` projection.
    pub fn help_state(&self) -> Option<&HelpState> {
        self.modal.help()
    }

    /// Dismiss the help overlay. A no-op when help is not the open modal — so this clears ONLY the
    /// help overlay, never some other modal that happens to be open (matching the old per-field
    /// `self.help = None`, which was inert unless help was actually open). The match guard keeps
    /// that contract now that the four modal slots share one `modal` field.
    pub fn close_help(&mut self) {
        if matches!(self.modal, Modal::Help(_)) {
            self.modal = Modal::None;
        }
    }

    /// Route a key event while the help overlay is open (AC-2, AC-3, AC-7, AC-8, AC-9, AC-20).
    ///
    /// - `?` / `Esc` / `q` → close the overlay (`Effects::redraw()`).
    /// - `Tab` / `Right` → `next()` (advance section, wrapping, AC-7).
    /// - `Shift+Tab` (`BackTab`) / `Left` → `prev()` (retreat section, wrapping, AC-7).
    /// - `'1'..='9'` → `select(n-1)` (jump to section by digit, AC-7).
    /// - `j` / `Down` → `scroll_by(+1)` (AC-8).
    /// - `k` / `Up` → `scroll_by(-1)` (saturates at 0, AC-9 top bound; bottom clamp is enforced against live geometry).
    /// - Any other key → consumed as a no-op (`Effects::noop()`) — nothing leaks to the tree
    ///   or viewer (AC-20).
    ///
    /// When the overlay is not open, all keys are a defensive no-op.
    pub fn handle_help_key(&mut self, key: KeyEvent) -> Effects {
        // Ignore Ctrl/Alt chords (mirrors input::map_key): Shift is allowed (Shift+Tab = BackTab
        // retreats), but a Ctrl+'?' / Alt+1 must NOT close or switch — consume it as a no-op so it
        // neither acts here nor leaks past the modal. (R3 item 3, consistency with map_key.)
        if key.modifiers.difference(KeyModifiers::SHIFT) != KeyModifiers::NONE {
            return Effects::noop();
        }
        // Read the last-known help-body geometry up front (the `self.modal.help_mut()` borrow below
        // is exclusive), so the scroll-down arm can clamp eagerly against it.
        let (help_body_rows, help_body_height) =
            (self.geom.help_body_rows, self.geom.help_body_height);
        let Some(help) = self.modal.help_mut() else {
            return Effects::noop();
        };
        match key.code {
            // Close keys: '?' / Esc / 'q' dismiss the overlay (AC-2, AC-3).
            KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = Modal::None;
                Effects::redraw()
            }
            // Section navigation: Tab / Right → next (AC-7).
            KeyCode::Tab | KeyCode::Right => {
                help.next();
                Effects::redraw()
            }
            // Section navigation: Shift+Tab (BackTab) / Left → prev (AC-7).
            KeyCode::BackTab | KeyCode::Left => {
                help.prev();
                Effects::redraw()
            }
            // Digit keys '1'..='9': direct section select (AC-7).
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize); // '1' → 0, '2' → 1, …
                help.select(idx);
                Effects::redraw()
            }
            // Scroll down: j / Down → scroll_by(+1) (AC-8), then clamp EAGERLY against the
            // last-known geometry so the drawn offset never over-scrolls past the last wrapped row
            // on the shown frame (mirrors scroll_content's max_content_scroll). The post-draw clamp
            // in set_pane_geometry stays as the backstop for resize/width changes. (R3 item 5/AC-9.)
            KeyCode::Char('j') | KeyCode::Down => {
                help.scroll_by(1);
                // Clamp eagerly only once a frame has measured the body (help_body_rows > 0);
                // before the first draw there is no geometry to clamp against and clamping to a
                // zero total would wrongly forbid all scroll. set_pane_geometry remains the
                // per-frame backstop. (R3 item 5.)
                if help_body_rows > 0 {
                    help.clamp_scroll(help_body_rows, help_body_height);
                }
                Effects::redraw()
            }
            // Scroll up: k / Up → scroll_by(-1) (saturates at 0, AC-9 top bound).
            KeyCode::Char('k') | KeyCode::Up => {
                help.scroll_by(-1);
                Effects::redraw()
            }
            // Any other key: consumed as a no-op — does not reach the tree/viewer (AC-20).
            _ => Effects::noop(),
        }
    }
}
