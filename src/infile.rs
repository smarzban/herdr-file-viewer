//! In-file navigation modal state — the bottom-prompt modal (go-to-line now; search added in a
//! later phase). Ephemeral, in-memory only (AC-N3).

use crate::prompt::PromptInput;

/// Which in-file-nav prompt is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    /// Jump the content pane to a source line by number (`:`).
    GoToLine,
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
