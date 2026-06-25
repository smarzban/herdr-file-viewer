//! Worktree Picker — modal selection state for the worktree switch (AC-1, AC-2, AC-5).
use crate::worktree::Worktree;

/// The open picker's state: the worktree rows and the highlighted cursor. Absent (`None` on the
/// controller) when the picker is closed.
pub struct PickerState {
    pub rows: Vec<Worktree>,
    pub cursor: usize,
}
