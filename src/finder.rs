//! Finder State — the ephemeral state of the go-to-file overlay.
//!
//! [`FinderState`] holds a query buffer ([`PromptInput`]), the full candidate list returned
//! by [`crate::index::build`], the current scored/ranked match indices, and the cursor
//! position within the match list. Construction populates the candidates; the query, matches,
//! and cursor start empty/zero and are driven by later tasks (T-6: typing/nav, T-7: confirm).

use crate::fuzzy;
use crate::prompt::PromptInput;

/// Live state of the go-to-file overlay while it is open (AC-1).
///
/// Created by [`crate::controller::Controller::open_finder`] when the user presses `f` and
/// destroyed when they confirm or cancel (T-7).
pub struct FinderState {
    /// The current query the user has typed.
    prompt: PromptInput,
    /// Every file under the root, as root-relative strings (from [`crate::index::build`]).
    /// Populated once at open time; not refreshed mid-session (YAGNI / T-6 concern).
    candidates: Vec<String>,
    /// Indices into `candidates` that match the current query, ranked best-first.
    /// Empty when the query is empty (no matches until the user types). Driven by T-6.
    matches: Vec<usize>,
    /// Cursor position within `matches`. Driven by T-6.
    cursor: usize,
}

impl FinderState {
    /// Build a new `FinderState` with an empty prompt over the given candidate list.
    pub fn new(candidates: Vec<String>) -> Self {
        Self {
            prompt: PromptInput::default(),
            candidates,
            matches: Vec::new(),
            cursor: 0,
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

    /// Re-run [`fuzzy::match_and_rank`] against the current query and reset the cursor to 0.
    fn recompute(&mut self) {
        self.matches = fuzzy::match_and_rank(self.prompt.query(), &self.candidates);
        self.cursor = 0;
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

    /// The ranked match indices (into `candidates`). Exposed for tests and the Presenter (T-8).
    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    /// The cursor position within the match list. Exposed for tests and confirm (T-7).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The candidate index at the current cursor position within the match list, or `None`
    /// when the match list is empty (zero matches → no selection to confirm).
    pub fn selected_candidate_index(&self) -> Option<usize> {
        self.matches.get(self.cursor).copied()
    }
}
