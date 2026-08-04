//! In-file navigation bottom prompt — go-to-line (`:`) and incremental search (`/`, `n`/`N`):
//! open, key handling, match navigation, and the source-line jump. Part of the Session Controller
//! (split out of `controller/mod.rs`, M6).

use super::*;

/// Extract the plain text of displayed lines for an in-file search.
fn plain_lines(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

/// Replace one interaction's search with matches over its displayed lines, returning the first line.
fn refresh_search(
    interaction: &mut PreviewInteractionState,
    lines: &[Line<'_>],
    query: String,
) -> Option<usize> {
    let matches = crate::search::find_matches(&query, &plain_lines(lines));
    let first_line = matches.first().map(|matched| matched.line);
    interaction.search = Some(SearchState {
        query,
        matches,
        current: 0,
    });
    first_line
}

/// Recompute one interaction's committed search against replacement displayed lines.
fn recompute_search(interaction: &mut PreviewInteractionState, lines: &[Line<'_>]) {
    let Some(previous) = interaction.search.take() else {
        return;
    };
    if previous.query.is_empty() {
        return;
    }
    let matches = crate::search::find_matches(&previous.query, &plain_lines(lines));
    interaction.search = Some(SearchState {
        query: previous.query,
        current: previous.current.min(matches.len().saturating_sub(1)),
        matches,
    });
}

/// Advance or retreat one interaction's committed search, returning its new line and wrap flag.
fn navigate_search(
    interaction: &mut PreviewInteractionState,
    forward: bool,
) -> Option<(usize, bool)> {
    let search = interaction.search.as_mut()?;
    if search.matches.is_empty() {
        return None;
    }
    let wrapped = if forward {
        search.current + 1 >= search.matches.len()
    } else {
        search.current == 0
    };
    search.current = if forward {
        (search.current + 1) % search.matches.len()
    } else {
        (search.current + search.matches.len() - 1) % search.matches.len()
    };
    Some((search.matches[search.current].line, wrapped))
}

impl Controller {
    /// Scroll the content pane so 1-based source line `line_1based` is visible, landing the line near
    /// the top of the viewport. The source line is clamped to `[1, source_line_count]` (below 1 →
    /// line 1; above the last → the last line), mapped to its display-row offset, then that offset is
    /// clamped to `[0, max_content_scroll()]` so a near-the-end line shows the last screenful (the
    /// target stays within view). Without wrap a source line maps 1:1 to a display row, so the offset
    /// is `line-1`; with wrap on (the `w` override wraps every mode) earlier long lines occupy several
    /// rows, so the offset is the cumulative wrapped-row count of the lines BEFORE the target — the
    /// same mapping the wrapped-row total uses, so `:N` lands on source line N either way. (AC-3, AC-4)
    pub fn scroll_to_line(&mut self, line_1based: usize) {
        let source_lines = self.active_lines().len();
        let line = line_1based.max(1).min(source_lines.max(1));
        let offset = if self.effective_wrap() {
            self.wrapped_rows_before(line - 1)
        } else {
            line - 1
        };
        self.active_interaction.vertical_scroll =
            (offset.min(u16::MAX as usize) as u16).min(self.max_content_scroll());
    }

    /// Open the go-to-line prompt (AC-1). Opens whenever a **file** is selected, in any view: in a
    /// source-mapped (SyntaxContent) view the confirm jumps directly; in a transformed view
    /// (RenderedMarkdown / Diff / FullDiff) — where a source line has no 1:1 display row — the confirm
    /// switches this file to the source-mapped content view and jumps once it re-renders (AC-7). With
    /// nothing / a directory selected there is no file to address, so emit a one-line notice and open
    /// nothing. Snapshots the current content scroll into the prompt state.
    pub(super) fn open_go_to_line(&mut self) -> Effects {
        self.prompt_target = PreviewTarget::Active;
        if self.selected_view_mode().is_some() {
            self.modal = Modal::Prompt(PromptState {
                mode: PromptMode::GoToLine,
                input: crate::prompt::PromptInput::new(),
                saved_scroll: self.active_interaction.vertical_scroll,
            });
        } else {
            self.action_notice = Some("Go to line: select a file first".into());
        }
        Effects::redraw()
    }

    /// Open the search prompt (AC-8). Search works in every view mode (RenderedMarkdown, Diff,
    /// FullDiff, SyntaxContent) but requires a file to be selected — a directory selection or
    /// nothing selected shows a notice instead (mirrors go-to-line's file-gate, owner UX).
    /// Like other modal openers, it is a no-op while the picker or finder is already open.
    /// Snapshots the current content scroll into the prompt state (for Esc-restore).
    pub(super) fn open_search(&mut self) -> Effects {
        // Modal mutual-exclusion: the picker and finder guards in handle() already prevent this
        // from being reached while those modals are open, but be explicit for clarity and for
        // future direct callers.
        if self.modal.picker().is_some() || self.modal.finder().is_some() {
            return Effects::noop();
        }
        let target = if self.focus == Focus::Pinned {
            if self.pinned_snapshot.is_none() {
                self.action_notice = Some("Search: no pinned preview".into());
                return Effects::redraw();
            }
            PreviewTarget::Pinned
        } else {
            PreviewTarget::Active
        };
        self.prompt_target = target;
        // File-gate applies only to the active tree-selected preview. A pin retains its applied
        // document independently of subsequent tree selection or re-rooting.
        // selected_view_mode() returns Some(mode) iff a file node is currently selected.
        if target == PreviewTarget::Active && self.selected_view_mode().is_none() {
            self.action_notice = Some("Search: select a file first".into());
            return Effects::redraw();
        }
        // Zoom-on-open (7b): if the content pane isn't visible (narrow tree-only layout), zoom the
        // file so the user sees the content they're about to search. Mirrors the go-to-file finder.
        if target == PreviewTarget::Active && self.active_interaction.viewport_width == 0 {
            self.zoomed = true;
            self.focus = Focus::Content;
        }
        // AC-20: opening a new search clears any prior committed SearchState so highlights from
        // the old query are gone before the new prompt opens. Clear first, then snapshot scroll.
        match target {
            PreviewTarget::Active => self.active_interaction.search = None,
            PreviewTarget::Pinned => {
                if let Some(interaction) = self.pinned_interaction_mut() {
                    interaction.search = None;
                }
            }
        }
        self.modal = Modal::Prompt(PromptState {
            mode: PromptMode::Search,
            input: crate::prompt::PromptInput::new(),
            saved_scroll: match target {
                PreviewTarget::Active => self.active_interaction.vertical_scroll,
                PreviewTarget::Pinned => self
                    .pinned_interaction()
                    .map_or(0, |interaction| interaction.vertical_scroll),
            },
        });
        Effects::redraw()
    }

    /// Whether an in-file-nav bottom prompt is currently open.
    pub fn prompt_open(&self) -> bool {
        self.modal.prompt().is_some()
    }

    /// Go-to-line prompt key handling: digits build the line number, non-digit printables are
    /// ignored (AC-2); Backspace deletes; Enter jumps (clamped, AC-3/AC-4) or — when empty —
    /// just closes with no jump (AC-5); Esc closes leaving the scroll unchanged (AC-6). Confirm
    /// and cancel both close the prompt. Go-to-line is not incremental, so the content scroll
    /// only ever moves on a non-empty Enter.
    pub(super) fn go_to_line_key(&mut self, key: KeyEvent) -> Effects {
        match key.code {
            // Only accept ASCII digits with no modifier other than SHIFT (consistent with the
            // finder's printable-char gate).
            KeyCode::Char(c)
                if c.is_ascii_digit()
                    && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                if let Some(p) = self.modal.prompt_mut() {
                    p.input.push(c);
                }
                Effects::redraw()
            }
            // A non-digit printable is ignored — the buffer is unchanged, no repaint. (AC-2)
            KeyCode::Char(_) => Effects::noop(),
            KeyCode::Backspace => {
                if let Some(p) = self.modal.prompt_mut() {
                    p.input.backspace();
                }
                Effects::redraw()
            }
            KeyCode::Enter => {
                let q = self
                    .modal
                    .prompt()
                    .map(|p| p.input.query().to_string())
                    .unwrap_or_default();
                self.modal = Modal::None; // confirm always closes (AC-5 empty also closes)
                // A new confirm supersedes any auto-switch jump still queued from an earlier confirm,
                // so the older line can't overwrite this one when its render lands.
                self.pending_goto = None;
                if !q.is_empty() {
                    // The buffer holds only ASCII digits (non-digits are rejected above), so a
                    // parse failure can only be an overflow → treat as "beyond the last line";
                    // scroll_to_line clamps usize::MAX to the last line (AC-4).
                    let n = q.parse::<usize>().unwrap_or(usize::MAX);
                    let source_mapped = self.selected_view_mode() == Some(ViewMode::SyntaxContent);
                    if source_mapped && self.applied_seq == self.latest_seq {
                        // Source-mapped AND the displayed content is the latest render → the line→row
                        // mapping is valid now, so jump synchronously (AC-3).
                        self.scroll_to_line(n);
                    } else if let Some(path) = self
                        .tree
                        .selected()
                        .filter(|node| node.kind == NodeKind::File)
                        .map(|node| node.path.clone())
                    {
                        // Either a transformed view (a source line has no display row here) or a
                        // source render still in flight (the override reports SyntaxContent before its
                        // render lands — jumping now would clamp against stale content). Queue the jump
                        // for the render that carries the source-mapped content, and only (re)dispatch
                        // when we must actually switch the view mode (AC-7); `poll` applies the queued
                        // jump once the matching render lands.
                        if !source_mapped {
                            self.overrides.insert(path, ViewMode::SyntaxContent);
                            self.dispatch_render();
                        }
                        self.pending_goto = Some((self.latest_seq, n));
                    }
                }
                Effects::redraw()
            }
            KeyCode::Esc => {
                self.modal = Modal::None; // cancel: close, scroll unchanged (AC-6)
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Search prompt key handling. Incremental: every printable char or Backspace re-runs
    /// `find_matches` over the displayed content's plain text and scrolls the first match into
    /// view. Enter commits — closes the prompt and retains the SearchState so n/N can navigate
    /// the committed matches (AC-14). Esc closes the prompt (cancel-restore semantics).
    /// All other keys are ignored.
    pub(super) fn search_prompt_key(&mut self, key: KeyEvent) -> Effects {
        match key.code {
            // Accept any printable char (no modifier beyond SHIFT — consistent with the finder's
            // printable-char gate). Unlike go-to-line, search does NOT restrict to digits.
            KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                if let Some(p) = self.modal.prompt_mut() {
                    p.input.push(c);
                }
                self.refresh_search();
                Effects::redraw()
            }
            KeyCode::Backspace => {
                if let Some(p) = self.modal.prompt_mut() {
                    p.input.backspace();
                }
                self.refresh_search();
                Effects::redraw()
            }
            // Commit — retain the SearchState; Esc-cancel is the path that clears it.
            // Close the prompt but leave self.search intact so n/N can navigate the committed
            // matches (AC-14). The query, matches, and current index persist.
            // Exception: an empty query commits nothing — clear search so no phantom
            // "Search: (no matches)" state persists after the prompt closes.
            KeyCode::Enter => {
                let target = self.prompt_target();
                let empty = self
                    .modal
                    .prompt()
                    .map(|p| p.input.query().is_empty())
                    .unwrap_or(true);
                if empty {
                    match target {
                        PreviewTarget::Active => self.active_interaction.search = None,
                        PreviewTarget::Pinned => {
                            if let Some(interaction) = self.pinned_interaction_mut() {
                                interaction.search = None;
                            }
                        }
                    }
                }
                self.modal = Modal::None;
                Effects::redraw()
            }
            // AC-17: Esc cancels the search — restore the pre-open scroll snapshot and clear
            // the in-progress SearchState (no highlights remain after cancel).
            KeyCode::Esc => {
                let target = self.prompt_target();
                let saved_scroll = self.modal.prompt().map(|p| p.saved_scroll).unwrap_or(0);
                match target {
                    PreviewTarget::Active => {
                        self.active_interaction.vertical_scroll = saved_scroll;
                        self.active_interaction.search = None;
                    }
                    PreviewTarget::Pinned => {
                        if let Some(interaction) = self.pinned_interaction_mut() {
                            interaction.vertical_scroll = saved_scroll;
                            interaction.search = None;
                        }
                    }
                }
                self.modal = Modal::None;
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Advance to the next match in document order (the `n` key, AC-15). Wraps from the last
    /// match back to the first with a notice (AC-16). Inert when there is no committed search
    /// with ≥1 match: no search, a committed search with zero matches, or the prompt still open
    /// (AC-19). Scrolls the new current match into view.
    pub(super) fn next_match(&mut self) -> Effects {
        if self.prompt_open() {
            return Effects::noop();
        }
        let target = self.focus_preview_target();
        let Some((next_line, wrapped)) = self.navigate_target_search(target, true) else {
            return Effects::noop();
        };
        if wrapped {
            self.action_notice = Some("Search: wrapped to first match".into());
        }
        self.scroll_target_to_line(target, next_line + 1);
        Effects::redraw()
    }

    /// Retreat to the previous match in document order (the `N` key, AC-15). Wraps from the
    /// first match back to the last with a notice (AC-16). Inert when there is no committed
    /// search with ≥1 match (AC-19). Scrolls the new current match into view.
    pub(super) fn prev_match(&mut self) -> Effects {
        if self.prompt_open() {
            return Effects::noop();
        }
        let target = self.focus_preview_target();
        let Some((prev_line, wrapped)) = self.navigate_target_search(target, false) else {
            return Effects::noop();
        };
        if wrapped {
            self.action_notice = Some("Search: wrapped to last match".into());
        }
        self.scroll_target_to_line(target, prev_line + 1);
        Effects::redraw()
    }

    /// Incremental search core: read the current prompt query, run `find_matches` over the
    /// displayed content's plain-text lines, store the result in `self.search`, and scroll
    /// the first match into view (AC-9, AC-10, AC-18).
    pub(super) fn refresh_search(&mut self) {
        let q = self
            .modal
            .prompt()
            .map(|p| p.input.query().to_string())
            .unwrap_or_default();
        let target = self.prompt_target();
        let first_line = match target {
            PreviewTarget::Active => refresh_search(
                &mut self.active_interaction,
                &self.active_display.content().lines,
                q,
            ),
            PreviewTarget::Pinned => self.pinned_snapshot.as_mut().and_then(|pin| {
                let (document, interaction) = (&pin.document, &mut pin.interaction);
                refresh_search(interaction, &document.content().lines, q)
            }),
        };
        if let Some(first_line) = first_line {
            // `first_line` is 0-based; scroll_to_line takes 1-based.
            self.scroll_target_to_line(target, first_line + 1);
        }
    }

    /// Re-run a COMMITTED search (the prompt is closed, so [`refresh_search`] — which reads the live
    /// prompt buffer — does not apply) against the freshly-rendered content, keeping the selected
    /// match ordinal where it still exists. Called by `poll` after a width reflow re-renders the
    /// same markdown file: match line indices are computed against the *rendered* lines, which shift
    /// when glow re-lays-out a table at the new width, so stale highlights are recomputed rather than
    /// dropped — a resize must not silently clear an active search. Unlike `refresh_search`, it does
    /// NOT scroll (the reflow preserves the user's position). A no-op when no search is active or the
    /// stored query is empty.
    ///
    /// [`refresh_search`]: Self::refresh_search
    pub(super) fn recompute_committed_search(&mut self) {
        let lines = &self.active_display.content().lines;
        recompute_search(&mut self.active_interaction, lines);
    }

    pub(super) fn focus_preview_target(&self) -> PreviewTarget {
        if self.focus == Focus::Pinned {
            PreviewTarget::Pinned
        } else {
            PreviewTarget::Active
        }
    }

    pub(super) fn prompt_target(&self) -> PreviewTarget {
        self.prompt_target
    }

    fn navigate_target_search(
        &mut self,
        target: PreviewTarget,
        forward: bool,
    ) -> Option<(usize, bool)> {
        match target {
            PreviewTarget::Active => navigate_search(&mut self.active_interaction, forward),
            PreviewTarget::Pinned => self
                .pinned_interaction_mut()
                .and_then(|interaction| navigate_search(interaction, forward)),
        }
    }

    fn scroll_target_to_line(&mut self, target: PreviewTarget, line: usize) {
        match target {
            PreviewTarget::Active => self.scroll_to_line(line),
            PreviewTarget::Pinned => self.scroll_pinned_to_line(line),
        }
    }

    pub(super) fn target_search(&self, target: PreviewTarget) -> Option<&SearchState> {
        match target {
            PreviewTarget::Active => self.active_interaction.search.as_ref(),
            PreviewTarget::Pinned => self.pinned_interaction().and_then(|p| p.search.as_ref()),
        }
    }
}
