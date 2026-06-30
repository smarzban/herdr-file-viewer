//! In-file navigation modal state — the bottom-prompt modal (go-to-line and search).
//! Ephemeral, in-memory only (AC-N3).

use crate::prompt::PromptInput;
use crate::search::Match;

/// Which in-file-nav prompt is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    /// Jump the content pane to a source line by number (`:`).
    GoToLine,
    /// Substring search across the content pane's lines (`/`). Read-only navigation —
    /// moves highlight/scroll only, never mutates a file (AC-N1/N3, AC-8).
    Search,
}

/// State for an open search session: the committed query, the matches it produced, and
/// which match is currently active (the `current` index into `matches`).
///
/// Fields are `pub` so the controller can read and update them without
/// needing extra accessors, and to avoid dead-code warnings before all paths land.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    pub matches: Vec<Match>,
    /// Index of the currently-selected match in `matches`.
    pub current: usize,
}

/// State for the open bottom-prompt modal: the mode, the editable buffer, and the content scroll
/// snapshot taken when the prompt opened (for Esc-restore in the incremental search added later;
/// go-to-line never scrolls while typing, so its Esc simply closes).
#[derive(Debug)]
pub struct PromptState {
    pub mode: PromptMode,
    pub input: PromptInput,
    pub saved_scroll: u16,
}
