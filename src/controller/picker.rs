//! Worktree picker (`W`) — open, intent handling, the agent-overlay hint, and the draw-model
//! projection. Part of the Session Controller (split out of `controller/mod.rs`, M6).

use super::*;

impl Controller {
    /// The owned picker draw model for the Presenter (AC-1, AC-5), or `None` when the picker is
    /// closed. Maps each worktree row to a [`PickerRowView`] (path + branch + detached + the
    /// current marker, AC-18, + the per-row agent status, AC-19) and carries the cursor; the path
    /// display string is the worktree's full path — informative for choosing among worktrees. The
    /// Presenter sanitizes the strings (AC-27) and renders the detached/current/agent markers.
    pub(super) fn picker_view(&self) -> Option<PickerView> {
        let picker = self.modal.picker()?;
        Some(PickerView {
            rows: picker
                .rows
                .iter()
                .enumerate()
                .map(|(i, w)| PickerRowView {
                    path: w.path.to_string_lossy().into_owned(),
                    branch: w.branch.clone(),
                    detached: w.detached,
                    is_current: w.is_current,
                    // Aligned 1:1 with rows; `.get` is defensive against a future divergence.
                    agent: picker.agent_statuses.get(i).cloned().flatten(),
                })
                .collect(),
            cursor: picker.cursor,
            hscroll: picker.hscroll,
        })
    }

    /// Route an intent while the worktree picker is open (modal). NavUp/NavDown move the
    /// highlight, Expand/Collapse (Right/Left) scroll the overlay rows horizontally so long
    /// worktree paths can be read sideways, Activate confirms (re-root to the selected worktree,
    /// AC-7; re-selecting the current worktree is a no-op via re_root, AC-11), Close cancels (no
    /// state change, AC-6). All other intents are inert.
    pub(super) fn handle_picker_intent(&mut self, intent: Intent) -> Effects {
        match intent {
            Intent::NavUp => {
                if let Some(p) = self.modal.picker_mut()
                    && p.cursor > 0
                {
                    p.cursor -= 1;
                    return Effects::redraw();
                }
                Effects::noop()
            }
            Intent::NavDown => {
                if let Some(p) = self.modal.picker_mut()
                    && p.cursor + 1 < p.rows.len()
                {
                    p.cursor += 1;
                    return Effects::redraw();
                }
                Effects::noop()
            }
            Intent::Expand => {
                // Right (→/l): scroll the overlay rows right so a long path can be read sideways.
                // Monotonic here — the Presenter clamps to the live inner width at draw, so an
                // over-scroll past the widest row is harmless and not surfaced to the controller.
                if let Some(p) = self.modal.picker_mut() {
                    let next = p.hscroll.saturating_add(HSCROLL_STEP);
                    if next != p.hscroll {
                        p.hscroll = next;
                        return Effects::redraw();
                    }
                }
                Effects::noop()
            }
            Intent::Collapse => {
                // Left (←/h): scroll the overlay rows left, clamped at the left edge (0).
                if let Some(p) = self.modal.picker_mut()
                    && p.hscroll > 0
                {
                    p.hscroll = p.hscroll.saturating_sub(HSCROLL_STEP);
                    return Effects::redraw();
                }
                Effects::noop()
            }
            Intent::Activate => {
                // Take the selected target, CLOSE the picker, then re-root. Closing first
                // guarantees the picker closes even if re_root early-returns (e.g. re-selecting
                // the current root is a no-op — AC-11 — and would not reach re_root's own
                // picker-clear). `.get(p.cursor)` is defensive: the picker is never opened with
                // empty rows and the cursor is bounds-clamped, but the invariant is distant —
                // use a local guard so a future change cannot introduce a panic.
                let target = self
                    .modal
                    .picker()
                    .and_then(|p| p.rows.get(p.cursor))
                    .map(|w| w.path.clone());
                self.modal = Modal::None;
                if let Some(target) = target {
                    self.re_root(&target);
                }
                Effects::redraw()
            }
            Intent::Close => {
                // Cancel: close the picker; nothing else changes (AC-6).
                self.modal = Modal::None;
                Effects::redraw()
            }
            // Modal: any other intent is inert while picking.
            _ => Effects::noop(),
        }
    }

    /// Open the worktree picker (AC-1). Gated to a git repo — outside one it is a no-op with a
    /// non-fatal notice and no picker (AC-14). Rows come from the read-only git worktree list; the
    /// pre-select is the agent-active worktree when herdr reports one (AC-3), else the current root
    /// (AC-4). A missing/failing herdr overlay degrades to the git-only list (AC-15).
    pub(super) fn open_worktree_picker(&mut self) -> Effects {
        if !self.is_git_repo {
            self.action_notice =
                Some("worktree switch is only available inside a git repository".into());
            return Effects::redraw();
        }
        let rows = crate::worktree::list(&self.root, &self.root);
        if rows.is_empty() {
            // git failed/no worktrees (shouldn't happen in a repo) — notice, no picker.
            self.action_notice = Some("could not list worktrees".into());
            return Effects::redraw();
        }
        // Fetch the herdr overlay ONCE (the two read-only list queries, AC-20) and feed BOTH the
        // per-row status badges (AC-19) and the agent-active pre-select (AC-3) from it. With no
        // overlay (herdr absent / query failed), rows carry no badge and the cursor falls back to
        // the current root (AC-4, AC-15).
        let current_idx = rows.iter().position(|w| w.is_current).unwrap_or(0);
        let overlay = self.herdr_overlay();
        let agent_statuses = match &overlay {
            Some((wt, ag)) => crate::worktree::agent_statuses(&rows, wt, ag),
            None => vec![None; rows.len()],
        };
        let cursor = overlay
            .as_ref()
            .and_then(|(wt, ag)| {
                crate::worktree::agent_active(&rows, wt, ag, self.our_workspace_id.as_deref())
            })
            .and_then(|active| rows.iter().position(|w| w.path == active))
            .unwrap_or(current_idx);
        self.modal = Modal::Picker(PickerState {
            rows,
            agent_statuses,
            cursor,
            hscroll: 0,
        });
        Effects::redraw()
    }

    /// Fetch the herdr agent overlay — the `worktree list` + `agent list` JSON — with exactly the
    /// two read-only queries (AC-20), or `None` when herdr is absent or either query fails (a
    /// git-only picker, AC-15). herdr's `worktree list` and `agent list` BOTH print JSON by
    /// default; `agent list` REJECTS a `--json` flag (verified live against herdr 0.7.x — it exits
    /// non-zero), so neither subcommand is passed the flag. (A prior `--json` on the agent query
    /// made this overlay silently fail → always fall back to the current root, AC-4/AC-15.)
    ///
    /// This is the single point both the per-row status badges and the agent-active pre-select
    /// derive from, so opening the picker issues exactly two herdr calls.
    fn herdr_overlay(&self) -> Option<(String, String)> {
        let herdr = self.herdr.as_ref()?;
        let wt_json = herdr.run_json(&["worktree", "list"]).ok()?;
        let ag_json = herdr.run_json(&["agent", "list"]).ok()?;
        Some((wt_json, ag_json))
    }
}
