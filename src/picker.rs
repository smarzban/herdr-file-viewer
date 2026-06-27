//! Worktree Picker — modal selection state for the worktree switch (AC-1, AC-2, AC-5).
use crate::worktree::Worktree;

/// The open picker's state: the worktree rows and the highlighted cursor. Absent (`None` on the
/// controller) when the picker is closed.
pub struct PickerState {
    pub rows: Vec<Worktree>,
    /// Per-row agent status, aligned 1:1 with `rows` (`Some` when that worktree's herdr workspace
    /// hosts a real agent — its `agent_status` — `None` otherwise). Built once when the picker
    /// opens, from the same herdr overlay used for the preselect (no extra subprocess cost,
    /// AC-20). All `None` when herdr is absent (AC-15).
    pub agent_statuses: Vec<Option<String>>,
    pub cursor: usize,
    /// Horizontal scroll offset (columns) for the overlay rows, so long worktree paths can be
    /// read sideways when the box caps at a narrow pane. A raw monotonic value driven by
    /// Expand/Collapse; the Presenter clamps it to the live inner width at draw. 0 when the
    /// picker opens (reset each time) and a no-op while every row fits.
    pub hscroll: u16,
}

impl PickerState {
    /// Clamp the stored horizontal scroll to `max` columns — the widest row minus the visible
    /// inner width, which the Presenter measures and feeds back each frame. Expand's `scroll_right`
    /// is monotonic (it can't know the row widths), so without this the offset drifts past the real
    /// maximum on over-scroll and a subsequent Collapse has to burn the overshoot down before the
    /// view visibly moves. Mirrors [`crate::finder::FinderState::clamp_hscroll`] (SMA-229).
    pub fn clamp_hscroll(&mut self, max: u16) {
        self.hscroll = self.hscroll.min(max);
    }
}
