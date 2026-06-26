//! Finder State — the ephemeral state of the go-to-file overlay.
//!
//! [`FinderState`] holds a query buffer ([`PromptInput`]), the full candidate list returned
//! by [`crate::index::build`], the current scored/ranked match indices, and the cursor
//! position within the match list. Construction populates the candidates; the query, matches,
//! and cursor start empty/zero and are driven by later tasks (T-6: typing/nav, T-7: confirm).

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
    #[allow(dead_code)] // consumed by T-6 (nav) and T-8 (overlay draw)
    matches: Vec<usize>,
    /// Cursor position within `matches`. Driven by T-6.
    #[allow(dead_code)] // consumed by T-6 (nav) and T-7 (confirm)
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
}
