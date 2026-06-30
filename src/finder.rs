//! Finder State — the ephemeral state of the go-to-file overlay.
//!
//! [`FinderState`] holds a query buffer ([`PromptInput`]), the full candidate list returned
//! by [`crate::index::build`], the current scored/ranked match indices, and the cursor
//! position within the match list. Construction populates the candidates; the query, matches,
//! and cursor start empty/zero and are driven by the run loop (typing/nav and confirm).

use crate::fuzzy;
use crate::prompt::PromptInput;

/// How many columns one horizontal-scroll step moves the result rows. Mirrors the controller's
/// `HSCROLL_STEP` — defined here so `FinderState` is self-contained and the controller can call
/// `scroll_left`/`scroll_right` without passing a delta.
const HSCROLL_STEP: u16 = 8;

/// Live state of the go-to-file overlay while it is open (AC-1).
///
/// Created by [`crate::controller::Controller::open_finder`] when the user presses `f` and
/// destroyed when they confirm or cancel.
pub struct FinderState {
    /// The current query the user has typed.
    prompt: PromptInput,
    /// Every file under the root, as root-relative strings (from [`crate::index::build`]).
    /// Populated once at open time; not refreshed mid-session (YAGNI).
    candidates: Vec<String>,
    /// Indices into `candidates` that match the current query, ranked best-first.
    /// Empty when the query is empty (no matches until the user types). Driven by the run loop.
    matches: Vec<usize>,
    /// Cursor position within `matches`. Driven by the run loop.
    cursor: usize,
    /// Horizontal scroll offset for the result rows, in columns. Monotonic here — the Presenter
    /// clamps to `max_row_width − inner_width` at draw so it can never over-scroll. Reset to 0
    /// in `recompute()` (a new query starts unscrolled). Does NOT affect the query line.
    hscroll: u16,
}

impl FinderState {
    /// Build a new `FinderState` with an empty prompt over the given candidate list.
    pub fn new(candidates: Vec<String>) -> Self {
        Self {
            prompt: PromptInput::default(),
            candidates,
            matches: Vec::new(),
            cursor: 0,
            hscroll: 0,
        }
    }

    /// The current query string. Exposed for the controller test accessor.
    pub fn query(&self) -> &str {
        self.prompt.query()
    }

    /// The full candidate list. Exposed for the controller test accessor.
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    /// Push a printable character onto the query and re-run the fuzzy match (AC-7). The
    /// selection is reset to 0 so the old cursor position (into the previous match list) is
    /// never surfaced.
    pub fn push(&mut self, c: char) {
        self.prompt.push(c);
        self.recompute();
    }

    /// Remove the last character from the query and re-run the fuzzy match (AC-7). If the
    /// prompt is already empty this is a no-op (apart from the recompute, which is trivial).
    pub fn backspace(&mut self) {
        self.prompt.backspace();
        self.recompute();
    }

    /// Re-run [`fuzzy::match_and_rank`] against the current query and reset the cursor and
    /// horizontal scroll to 0 so a new query starts at the left edge of the result rows.
    fn recompute(&mut self) {
        self.matches = fuzzy::match_and_rank(self.prompt.query(), &self.candidates);
        self.cursor = 0;
        self.hscroll = 0;
    }

    /// Move the cursor within the match list by `delta` rows, clamped to `[0, matches.len()-1]`
    /// so it never runs off either end. A no-op (cursor stays 0) when the list is empty (AC-8).
    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.cursor = 0;
            return;
        }
        let max = self.matches.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// The ranked match indices (into `candidates`). Exposed for tests and the Presenter.
    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    /// The cursor position within the match list. Exposed for tests and the confirm path.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Set the cursor to `idx`, clamped to `[0, matches.len() - 1]`. A no-op when the match
    /// list is empty (cursor stays 0). Used by the mouse click handler to jump the selection
    /// directly to a result row without bounds-checking at the call site.
    pub fn set_cursor(&mut self, idx: usize) {
        if self.matches.is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor = idx.min(self.matches.len() - 1);
    }

    /// The horizontal scroll offset for the result rows (columns). The Presenter clamps it to
    /// `max_row_width − inner_width` at draw, so it can never over-scroll past the widest row.
    pub fn hscroll(&self) -> u16 {
        self.hscroll
    }

    /// Scroll the result rows right by one step (saturating — the Presenter clamps at draw).
    pub fn scroll_right(&mut self) {
        self.hscroll = self.hscroll.saturating_add(HSCROLL_STEP);
    }

    /// Scroll the result rows left by one step, clamped at 0.
    pub fn scroll_left(&mut self) {
        self.hscroll = self.hscroll.saturating_sub(HSCROLL_STEP);
    }

    /// Clamp the stored horizontal scroll to `max` columns — the widest match row minus the visible
    /// width, which the Presenter measures and feeds back each frame. `scroll_right` is monotonic
    /// (it can't know the row widths), so without this the offset drifts past the real maximum on
    /// over-scroll and a subsequent `scroll_left` has to burn the overshoot down before the view
    /// visibly moves. Called from the controller's geometry feedback, mirroring `content_hscroll`.
    pub fn clamp_hscroll(&mut self, max: u16) {
        self.hscroll = self.hscroll.min(max);
    }

    /// The candidate index at the current cursor position within the match list, or `None`
    /// when the match list is empty (zero matches → no selection to confirm).
    pub fn selected_candidate_index(&self) -> Option<usize> {
        self.matches.get(self.cursor).copied()
    }
}
