//! Session view + session picker (`s` / `S`) — enter/leave the session view, follow the
//! current transcript live, and choose among the root's session transcripts. Part of the
//! Session Controller (a feature submodule, like `picker`).

use super::*;
use crate::presenter::{SessionPickerRowView, SessionPickerView};
use crate::session::{Follower, SessionSet, home_dir, list_sessions, projects_dir, session_title};
use std::time::{Duration, SystemTime};

/// How often the live-follow may stat the followed transcripts. A stat is cheap, but the event
/// loop ticks often — bound the I/O to twice a second.
const SESSION_FOLLOW_INTERVAL: Duration = Duration::from_millis(500);

/// How often auto mode re-scans the project directory for a *newer* session. Deliberately much
/// coarser than the append-follow: with two concurrently active sessions in one root,
/// mtime-newest leadership alternates on every append, and following it at the append cadence
/// would swap the whole member set (and re-ingest the other transcript) twice a second. The
/// design accepts the flap itself — the picker (`S`) is the escape hatch — but bounds its rate.
const SESSION_RESCAN_INTERVAL: Duration = Duration::from_secs(3);

/// The session picker's modal state (rows + cursor), the worktree picker's sibling.
pub(super) struct SessionPickerState {
    pub rows: Vec<SessionPickerRow>,
    pub cursor: usize,
}

/// One transcript row held by the open session picker.
pub(super) struct SessionPickerRow {
    /// The transcript file, confirmed on pick.
    pub path: PathBuf,
    /// The display label: the session's user-given title, else its transcript-id prefix.
    pub label: String,
    /// Humanized last-activity age ("3m ago").
    pub age: String,
    /// Whether this transcript is the one the session view currently presents.
    pub is_current: bool,
}

impl Controller {
    /// Toggle the session view (`s`): present the current Claude Code session's file set, or
    /// restore the full directory tree.
    pub(super) fn toggle_session_view(&mut self) -> Effects {
        if self.tree.session_view() {
            self.leave_session_view();
        } else {
            self.enter_session_view(None);
        }
        self.dispatch_render();
        Effects::redraw()
    }

    /// Leave the session view, restoring the full tree. The follower — and any explicit picker
    /// choice — is kept, so toggling back re-enters the same session (an explicit choice holds
    /// until a worktree switch, another choice, or exit). A no-op when the view is off.
    pub(super) fn leave_session_view(&mut self) {
        if !self.tree.session_view() {
            return;
        }
        self.session_view = false;
        self.tree
            .set_session_view(false, &SessionSet::default(), None);
    }

    /// Apply the `session_view = true` startup config: enter the session view once, before the
    /// first frame. A no-op when already on (defensive for tests).
    pub fn startup_session_view(&mut self) {
        if !self.tree.session_view() {
            self.enter_session_view(None);
            self.dispatch_render();
        }
    }

    /// Enter (or re-enter) the session view. `explicit` carries a picker-chosen transcript;
    /// `None` keeps a previously chosen one (an explicit choice holds until a worktree switch,
    /// another choice, or exit) or auto-follows the newest transcript for the root.
    pub(super) fn enter_session_view(&mut self, explicit: Option<PathBuf>) {
        // Mutually exclusive with the git filters: each replaces what the tree shows.
        self.changed_only = false;
        self.status_mode = false;
        self.tree.set_changed_only(false, &self.changed);

        match explicit {
            Some(path) => {
                self.session_explicit = true;
                // Re-picking the already-followed transcript keeps its read state (no re-parse).
                if self
                    .session_follower
                    .as_ref()
                    .is_none_or(|f| f.path() != path)
                {
                    self.session_follower = Some(Follower::new(path));
                }
            }
            None => {
                if !self.session_explicit || self.session_follower.is_none() {
                    self.session_explicit = false;
                    // Keep the existing follower when it already points at the newest transcript
                    // (or when there is no newer candidate): a `s` toggle re-entry or startup
                    // must not re-ingest a large transcript it has already consumed.
                    match self.newest_transcript() {
                        Some(newest)
                            if self
                                .session_follower
                                .as_ref()
                                .is_none_or(|f| f.path() != newest) =>
                        {
                            self.session_follower = Some(Follower::new(newest));
                        }
                        _ => {}
                    }
                }
            }
        }
        self.session_view = true;
        self.session_checked = None; // next poll re-checks immediately
        self.session_rescanned = None;
        if let Some(f) = &mut self.session_follower {
            f.poll();
        } else {
            self.action_notice = Some("No Claude Code session found for this root".to_string());
        }
        self.apply_session_set(true);
    }

    /// Project the follower's members around the root and hand them to the tree. The selection
    /// is preserved by path — live updates must never move the user's cursor — and the content
    /// pane re-renders only when the selected row actually changed. `select_first` seeds the
    /// cursor at the top instead (entering the view).
    fn apply_session_set(&mut self, select_first: bool) {
        let set = self
            .session_follower
            .as_ref()
            .map(|f| f.set_for(&self.root))
            .unwrap_or_default();
        // Selection identity is (path, kind), not path alone: a member file replaced by a
        // same-named directory holding another member synthesizes BOTH a Dir and a File row for
        // that path (see `file_row`'s note in tree.rs) — a path-only restore would land on the
        // directory row while the content pane kept showing the dead file.
        let before = self.tree.selected().map(|n| (n.path.clone(), n.kind));
        self.tree.set_session_view(true, &set, home_dir());
        if select_first {
            self.tree.set_cursor(0);
        } else if let Some((path, kind)) = &before
            && let Some(idx) = self
                .tree
                .visible_nodes()
                .iter()
                .position(|n| &n.path == path && n.kind == *kind)
        {
            self.tree.set_cursor(idx);
        }
        let after = self.tree.selected().map(|n| (n.path.clone(), n.kind));
        if before != after {
            self.dispatch_render();
        }
    }

    /// The live-follow tick, called from [`poll`](Controller::poll): throttled to
    /// [`SESSION_FOLLOW_INTERVAL`], it picks up transcript appends — and, when auto-following,
    /// a brand-new newest session — and re-projects the set. Returns whether anything visible
    /// changed (the caller's redraw signal).
    pub(super) fn poll_session_follow(&mut self) -> bool {
        if !self.tree.session_view() {
            return false;
        }
        let now = Instant::now();
        if self
            .session_checked
            .is_some_and(|t| now.duration_since(t) < SESSION_FOLLOW_INTERVAL)
        {
            return false;
        }
        self.session_checked = Some(now);
        let mut changed = false;
        // Auto mode follows the newest transcript: a fresh `claude` session in this root takes
        // over the view. An explicit picker choice holds instead. Re-scanned on its own, much
        // coarser cadence ([`SESSION_RESCAN_INTERVAL`]) so two concurrently-live sessions
        // trading mtime leadership can't swap the member set twice a second.
        let rescan_due = self
            .session_rescanned
            .is_none_or(|t| now.duration_since(t) >= SESSION_RESCAN_INTERVAL);
        if !self.session_explicit && rescan_due {
            self.session_rescanned = Some(now);
            if let Some(newest) = self.newest_transcript()
                && self
                    .session_follower
                    .as_ref()
                    .is_none_or(|f| f.path() != newest)
            {
                self.session_follower = Some(Follower::new(newest));
                changed = true;
            }
        }
        if let Some(f) = &mut self.session_follower
            && f.poll()
        {
            changed = true;
        }
        if changed {
            self.apply_session_set(false);
        }
        changed
    }

    /// Reset the session state a worktree switch invalidates: an explicit transcript choice is
    /// root-bound (back to auto-follow), and the follower re-resolves against the new root. The
    /// session view itself is a carried preference (AC-12 spirit) — re-entered when it was on.
    pub(super) fn reset_session_for_root(&mut self) {
        self.session_explicit = false;
        self.session_follower = None;
        self.session_checked = None;
        self.session_rescanned = None;
        if self.session_view {
            self.enter_session_view(None);
        }
    }

    /// Open the session picker (`S`): the root's transcripts, newest first, current one
    /// pre-selected. No transcripts → a notice, no picker (mirrors the worktree picker's
    /// degrade).
    pub(super) fn open_session_picker(&mut self) -> Effects {
        let sessions = match self.sessions_dir() {
            Some(dir) => list_sessions(&dir),
            None => Vec::new(),
        };
        if sessions.is_empty() {
            self.action_notice = Some("No Claude Code sessions found for this root".to_string());
            return Effects::redraw();
        }
        let current = self
            .session_follower
            .as_ref()
            .map(|f| f.path().to_path_buf());
        let now = SystemTime::now();
        let rows: Vec<SessionPickerRow> = sessions
            .iter()
            .map(|s| SessionPickerRow {
                path: s.path.clone(),
                label: session_title(&s.path).unwrap_or_else(|| transcript_id(&s.path)),
                age: humanize_age(now, s.modified),
                is_current: current.as_deref() == Some(&s.path),
            })
            .collect();
        let cursor = rows.iter().position(|r| r.is_current).unwrap_or(0);
        self.modal = Modal::SessionPicker(SessionPickerState { rows, cursor });
        Effects::redraw()
    }

    /// Route an intent while the session picker is open (modal): NavUp/NavDown move,
    /// Activate confirms (hold that transcript and show the session view), Close cancels.
    /// Everything else is inert, exactly like the worktree picker.
    pub(super) fn handle_session_picker_intent(&mut self, intent: Intent) -> Effects {
        match intent {
            Intent::NavUp => {
                if let Some(p) = self.modal.session_picker_mut()
                    && p.cursor > 0
                {
                    p.cursor -= 1;
                    return Effects::redraw();
                }
                Effects::noop()
            }
            Intent::NavDown => {
                if let Some(p) = self.modal.session_picker_mut()
                    && p.cursor + 1 < p.rows.len()
                {
                    p.cursor += 1;
                    return Effects::redraw();
                }
                Effects::noop()
            }
            Intent::Activate => {
                let target = self
                    .modal
                    .session_picker()
                    .and_then(|p| p.rows.get(p.cursor))
                    .map(|r| r.path.clone());
                self.modal = Modal::None;
                if let Some(target) = target {
                    self.enter_session_view(Some(target));
                    self.dispatch_render();
                }
                Effects::redraw()
            }
            Intent::Close => {
                self.modal = Modal::None;
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// The owned session-picker draw model for the Presenter, or `None` while it is closed.
    pub(super) fn session_picker_view(&self) -> Option<SessionPickerView> {
        let picker = self.modal.session_picker()?;
        Some(SessionPickerView {
            rows: picker
                .rows
                .iter()
                .map(|r| SessionPickerRowView {
                    label: r.label.clone(),
                    age: r.age.clone(),
                    is_current: r.is_current,
                })
                .collect(),
            cursor: picker.cursor,
        })
    }

    /// Where Claude Code keeps this root's transcripts, or `None` without a home directory.
    fn sessions_dir(&self) -> Option<PathBuf> {
        home_dir().map(|home| projects_dir(&home, &self.root))
    }

    /// The most recently modified transcript for the root, if any.
    fn newest_transcript(&self) -> Option<PathBuf> {
        let dir = self.sessions_dir()?;
        list_sessions(&dir).into_iter().next().map(|s| s.path)
    }
}

/// A transcript's display id: the first 8 characters of its file stem (a session UUID), enough
/// to tell rows apart without a title.
fn transcript_id(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    stem.chars().take(8).collect()
}

/// A coarse human age for a picker row ("just now", "3m ago", "2h ago", "5d ago"). A modified
/// time in the future (clock skew) reads as "just now".
fn humanize_age(now: SystemTime, modified: SystemTime) -> String {
    let secs = now
        .duration_since(modified)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn humanize_age_buckets() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let at = |ago: u64| now - Duration::from_secs(ago);
        assert_eq!(humanize_age(now, at(5)), "just now");
        assert_eq!(humanize_age(now, at(180)), "3m ago");
        assert_eq!(humanize_age(now, at(7200)), "2h ago");
        assert_eq!(humanize_age(now, at(432_000)), "5d ago");
        // Future mtime (skew) degrades to "just now", never panics.
        assert_eq!(humanize_age(now, now + Duration::from_secs(60)), "just now");
    }

    #[test]
    fn transcript_id_is_the_stem_prefix() {
        assert_eq!(
            transcript_id(Path::new("/x/7819f839-922e-49eb-a080-63b3ab7ae5a0.jsonl")),
            "7819f839"
        );
        assert_eq!(transcript_id(Path::new("/x/s.jsonl")), "s");
    }
}
