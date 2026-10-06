//! Finder State — the ephemeral state of the go-to-file overlay.
//!
//! [`FinderState`] holds a query buffer ([`PromptInput`]), the full candidate list returned
//! by [`crate::index::build`], the current scored/ranked match indices, and the cursor
//! position within the match list. The live overlay builds its fresh index and matches on one
//! worker; input and draw only edit a query or read immutable result snapshots.

use crate::fuzzy;
use crate::prompt::PromptInput;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Shared, immutable paths + ranking. Cloning a draw model never clones the paths, and
/// measuring a frame never scans the full result set. All results remain navigable.
#[derive(Clone, Default)]
pub struct FinderMatches {
    candidates: Arc<Vec<String>>,
    ranked: Arc<Ranking>,
}

#[derive(Default)]
struct Ranking {
    indices: Vec<usize>,
    max_width: usize,
}

impl FinderMatches {
    pub fn len(&self) -> usize {
        self.ranked.indices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranked.indices.is_empty()
    }

    pub fn max_width(&self) -> usize {
        self.ranked.max_width
    }

    /// Only resolve the visible slice, even when the cursor is millions of rows down.
    pub fn window(&self, offset: usize, count: usize) -> impl Iterator<Item = &str> {
        let start = offset.min(self.len());
        let end = start.saturating_add(count).min(self.len());
        self.ranked.indices[start..end]
            .iter()
            .map(|&i| self.candidates[i].as_str())
    }
}

impl From<Vec<String>> for FinderMatches {
    fn from(paths: Vec<String>) -> Self {
        let max_width = paths.iter().map(|p| path_width(p)).max().unwrap_or(0);
        let indices = (0..paths.len()).collect();
        Self {
            candidates: Arc::new(paths),
            ranked: Arc::new(Ranking { indices, max_width }),
        }
    }
}

fn path_width(path: &str) -> usize {
    if path.is_ascii() && !path.bytes().any(|b| b.is_ascii_control()) {
        path.len()
    } else {
        ratatui::text::Line::raw(crate::text_layout::sanitize_control(path)).width()
    }
}

struct Completion {
    seq: u64,
    matches: FinderMatches,
}

#[derive(Default)]
pub(crate) struct Shared {
    cancelled: AtomicBool,
    revision: AtomicU64,
    indexed: AtomicBool,
    count: AtomicUsize,
    query: Mutex<(u64, String)>,
    // One replaceable slot bounds pending results. The UI only takes Arc snapshots.
    completion: Mutex<Option<Completion>>,
    /// Signalled on every published completion, for [`FinderState::settle`].
    ready: Condvar,
}

struct Worker {
    shared: Arc<Shared>,
    wake: mpsc::SyncSender<()>,
    /// Never joined; only asked whether the thread has exited (see [`FinderState::poll`]).
    thread: std::thread::JoinHandle<()>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.cancelled.store(true, Ordering::Relaxed);
        let _ = self.wake.try_send(());
        // Never join on the UI thread. The worker retains the index while live and drains
        // the mailbox on exit (see [`ReleaseOnExit`]), so its millions of strings are freed on
        // the worker thread even when the UI never polled the last completion.
    }
}

/// Takes any unpolled completion out of the mailbox as the worker exits. Without it, a
/// completion published just before cancellation keeps the candidate list alive inside
/// `Shared`; if the worker exited first, the UI would drop the last `Shared` reference and
/// free the whole index synchronously when the finder closes.
struct ReleaseOnExit<'a>(&'a Shared);

impl Drop for ReleaseOnExit<'_> {
    fn drop(&mut self) {
        // Runs on unwind too, so tolerate a poisoned lock rather than panicking twice.
        let pending = self
            .0
            .completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(pending); // freed after the lock is released, so a concurrent poll never waits on it
    }
}

/// The production index job: the real walk, wired to this opening's cancellation flag and
/// progress counter. A separate function so tests can run it against a `Shared` they own.
fn index_walk(
    root: PathBuf,
    is_git_repo: bool,
) -> impl FnOnce(&Shared) -> Option<Vec<String>> + Send + 'static {
    move |shared| {
        crate::index::build_cancellable(
            &root,
            is_git_repo,
            || shared.cancelled.load(Ordering::Relaxed),
            |n| shared.count.store(n, Ordering::Relaxed),
        )
    }
}

/// How many columns one horizontal-scroll step moves the result rows. Mirrors the controller's
/// `HSCROLL_STEP` — defined here so `FinderState` is self-contained and the controller can call
/// `scroll_left`/`scroll_right` without passing a delta.
const HSCROLL_STEP: u16 = 8;

/// The longest a query edit waits for its own match before the frame is drawn. A small index
/// finishes well inside it, so its results paint together with the typed character; a larger
/// one outlasts it and is picked up by a later poll, so a keystroke never blocks for longer.
pub const SETTLE_BUDGET: Duration = Duration::from_millis(10);

/// Indexing progress alone repaints at most this often, so polling a busy finder more often
/// than the idle tick does not redraw the whole frame at the poll rate.
const PROGRESS_REPAINT: Duration = Duration::from_millis(100);

/// Live state of the go-to-file overlay while it is open (AC-1).
///
/// Created by [`crate::controller::Controller::open_finder`] when the user presses `f` and
/// destroyed when they confirm or cancel.
pub struct FinderState {
    /// The current query the user has typed.
    prompt: PromptInput,
    /// Every file under the root, as root-relative strings (from [`crate::index::build`]).
    /// Populated by the worker once per opening; queries replace only the ranking.
    results: FinderMatches,
    worker: Option<Worker>,
    seq: u64,
    applied_seq: u64,
    progress: usize,
    indexed: bool,
    error: Option<String>,
    confirm_seq: Option<u64>,
    progress_painted: Option<Instant>,
    /// Cursor position within `matches`. Driven by the run loop.
    cursor: usize,
    /// Horizontal scroll offset for the result rows, in columns. Monotonic here — the Presenter
    /// clamps to `max_row_width − inner_width` at draw so it can never over-scroll. Reset to 0
    /// in `recompute()` (a new query starts unscrolled). Does NOT affect the query line.
    hscroll: u16,
    /// Test-only: runs inside `settle` while it holds the mailbox lock, just before it waits, so
    /// a test can release the worker knowing its publication must land during the wait.
    #[cfg(test)]
    before_settle_wait: Option<Box<dyn FnMut() + Send>>,
}

impl FinderState {
    /// Build a new `FinderState` with an empty prompt over the given candidate list.
    pub fn new(candidates: Vec<String>) -> Self {
        Self {
            prompt: PromptInput::default(),
            results: FinderMatches {
                candidates: Arc::new(candidates),
                ..Default::default()
            },
            worker: None,
            seq: 0,
            applied_seq: 0,
            progress: 0,
            indexed: true,
            error: None,
            confirm_seq: None,
            progress_painted: None,
            cursor: 0,
            hscroll: 0,
            #[cfg(test)]
            before_settle_wait: None,
        }
    }

    /// Open immediately; each opening owns a fresh index and worker. Closing or re-rooting
    /// drops this state, cancelling the old worker and disconnecting its result slot.
    pub fn start(root: PathBuf, is_git_repo: bool) -> Self {
        Self::start_with(index_walk(root, is_git_repo))
    }

    pub(crate) fn start_with(
        index: impl FnOnce(&Shared) -> Option<Vec<String>> + Send + 'static,
    ) -> Self {
        let mut state = Self::new(Vec::new());
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        let (wake, rx) = mpsc::sync_channel(1);
        let spawn = std::thread::Builder::new()
            .name("file-finder".into())
            .spawn(move || {
                let shared = worker_shared;
                let _release = ReleaseOnExit(&shared);
                let Some(paths) = index(&shared) else { return };
                let paths = Arc::new(paths);
                // Every consumer clamps a row width to u16::MAX, so saturating here loses
                // nothing and keeps this per-path cache at a quarter of a usize.
                let mut widths: Vec<u16> = Vec::with_capacity(paths.len());
                for (i, path) in paths.iter().enumerate() {
                    if i % 64 == 0 && shared.cancelled.load(Ordering::Relaxed) {
                        return;
                    }
                    widths.push(path_width(path).try_into().unwrap_or(u16::MAX));
                }
                shared.count.store(paths.len(), Ordering::Relaxed);
                shared.indexed.store(true, Ordering::Relaxed);
                let mut last_seq = None;
                loop {
                    if shared.cancelled.load(Ordering::Relaxed) {
                        return;
                    }
                    let (seq, query) = shared.query.lock().unwrap().clone();
                    let cancelled = || {
                        shared.cancelled.load(Ordering::Relaxed)
                            || shared.revision.load(Ordering::Relaxed) != seq
                    };
                    // A query counts as done only once its result is published. Marking it before
                    // the match would let a cancelled attempt strand the current query: its next
                    // wake would skip it, and nothing would ever match it.
                    if last_seq != Some(seq)
                        && let Some(indices) =
                            fuzzy::match_and_rank_cancellable(&query, &paths, cancelled)
                    {
                        let mut max_width = 0;
                        for (n, &i) in indices.iter().enumerate() {
                            if n % 64 == 0 && cancelled() {
                                break;
                            }
                            max_width = max_width.max(usize::from(widths[i]));
                        }
                        if !cancelled() {
                            let result = Completion {
                                seq,
                                matches: FinderMatches {
                                    candidates: Arc::clone(&paths),
                                    ranked: Arc::new(Ranking { indices, max_width }),
                                },
                            };
                            // Drop the superseded result after releasing the mailbox lock.
                            let old = shared.completion.lock().unwrap().replace(result);
                            shared.ready.notify_all();
                            drop(old);
                            last_seq = Some(seq);
                        }
                    }
                    if rx.recv().is_err() {
                        return;
                    }
                    while rx.try_recv().is_ok() {}
                }
            });
        match spawn {
            Err(error) => state.error = Some(format!("File indexing unavailable: {error}")),
            Ok(thread) => {
                state.indexed = false;
                state.applied_seq = u64::MAX;
                state.worker = Some(Worker {
                    shared,
                    wake,
                    thread,
                });
            }
        }
        state
    }

    pub fn busy(&self) -> bool {
        self.worker.is_some() && (!self.indexed || self.applied_seq != self.seq)
    }

    pub fn status(&self) -> Option<String> {
        if let Some(error) = &self.error {
            Some(error.clone())
        } else if !self.indexed {
            Some(format!("Indexing… {} files", self.progress))
        } else if self.busy() {
            Some("Searching…".into())
        } else {
            None
        }
    }

    pub fn snapshot(&self) -> FinderMatches {
        self.results.clone()
    }

    /// A fast type-and-Enter accepts the current query once it is ready, without
    /// blocking input or ever confirming rows from a previous query.
    pub fn defer_confirm(&mut self) -> bool {
        if self.busy() && !self.query().is_empty() {
            self.confirm_seq = Some(self.seq);
            true
        } else {
            false
        }
    }

    pub fn take_ready_confirm(&mut self) -> bool {
        !self.busy() && self.confirm_seq.take().is_some_and(|seq| seq == self.seq)
    }

    /// Give the current query up to `budget` to finish before the next frame is drawn, then
    /// drain it. A small index paints its results with the typed character, as the synchronous
    /// matcher did, instead of a collapsed `Searching…` frame until the next event-loop poll.
    /// Never waits while indexing, which can take far longer than any frame.
    pub fn settle(&mut self, budget: Duration) -> bool {
        if let Some(worker) = &self.worker
            && worker.shared.indexed.load(Ordering::Relaxed)
            && self.applied_seq != self.seq
        {
            let seq = self.seq;
            let slot = worker.shared.completion.lock().unwrap();
            #[cfg(test)]
            if let Some(hook) = &mut self.before_settle_wait {
                hook();
            }
            let _slot = worker
                .shared
                .ready
                .wait_timeout_while(slot, budget, |c| c.as_ref().is_none_or(|c| c.seq != seq))
                .unwrap();
        }
        self.poll()
    }

    /// Nonblocking result/progress drain, called from the existing event-loop poll.
    pub fn poll(&mut self) -> bool {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        // While this state is alive the worker only exits by panicking: cancellation and a
        // disconnected wake channel both come from dropping it. Read this before draining,
        // so a result it published just before exiting is still applied.
        let exited = worker.thread.is_finished();
        let count = worker.shared.count.load(Ordering::Relaxed);
        let indexed = worker.shared.indexed.load(Ordering::Relaxed);
        let progress_due = count != self.progress
            && self
                .progress_painted
                .is_none_or(|at| now.duration_since(at) >= PROGRESS_REPAINT);
        let mut changed = indexed != self.indexed || progress_due;
        if changed {
            self.progress = count;
            self.progress_painted = Some(now);
        }
        self.indexed = indexed;
        let result = worker.shared.completion.lock().unwrap().take();
        if let Some(result) = result
            && result.seq == self.seq
        {
            self.results = result.matches;
            self.applied_seq = result.seq;
            changed = true;
        }
        if exited {
            // Report it rather than showing `Indexing…` forever with a deferred Enter that can
            // never fire. Later edits fall back to matching the candidates already received.
            self.worker = None;
            self.indexed = true;
            self.confirm_seq = None;
            self.error = Some("File search stopped unexpectedly".into());
            changed = true;
        }
        changed
    }

    /// The current query string. Exposed for the controller test accessor.
    pub fn query(&self) -> &str {
        self.prompt.query()
    }

    /// The full candidate list. Exposed for the controller test accessor.
    pub fn candidates(&self) -> &[String] {
        &self.results.candidates
    }

    /// Push a printable character and request the current query's fuzzy match (AC-7). The
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

    /// Request matching for the current query and reset cursor/scroll so a new query
    /// starts at the left edge. The synchronous constructor uses the same matcher inline.
    fn recompute(&mut self) {
        self.confirm_seq = None;
        self.cursor = 0;
        self.hscroll = 0;
        if let Some(worker) = &self.worker {
            self.seq += 1;
            // Publish the query and its revision under one lock. A worker that reads this query
            // then always sees its revision, so it never cancels the current query as stale.
            let mut query = worker.shared.query.lock().unwrap();
            *query = (self.seq, self.prompt.query().to_string());
            worker.shared.revision.store(self.seq, Ordering::Relaxed);
            drop(query);
            // Old rows cannot be confirmed against the newly typed query.
            self.results.ranked = Arc::default();
            let _ = worker.wake.try_send(());
        } else {
            let indices = fuzzy::match_and_rank(self.prompt.query(), &self.results.candidates);
            let max_width = indices
                .iter()
                .map(|&i| path_width(&self.results.candidates[i]))
                .max()
                .unwrap_or(0);
            self.results.ranked = Arc::new(Ranking { indices, max_width });
        }
    }

    /// Move the cursor within the match list by `delta` rows, clamped to `[0, matches.len()-1]`
    /// so it never runs off either end. A no-op (cursor stays 0) when the list is empty (AC-8).
    pub fn move_selection(&mut self, delta: isize) {
        if self.matches().is_empty() {
            self.cursor = 0;
            return;
        }
        let max = self.matches().len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// The ranked match indices (into `candidates`). Exposed for tests and the Presenter.
    pub fn matches(&self) -> &[usize] {
        &self.results.ranked.indices
    }

    /// The cursor position within the match list. Exposed for tests and the confirm path.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Set the cursor to `idx`, clamped to `[0, matches.len() - 1]`. A no-op when the match
    /// list is empty (cursor stays 0). Used by the mouse click handler to jump the selection
    /// directly to a result row without bounds-checking at the call site.
    pub fn set_cursor(&mut self, idx: usize) {
        if self.matches().is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor = idx.min(self.matches().len() - 1);
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
        self.matches().get(self.cursor).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn finish(state: &mut FinderState) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.busy() {
            state.poll();
            assert!(Instant::now() < deadline, "worker did not complete");
            std::thread::yield_now();
        }
    }

    #[test]
    fn indexing_does_not_own_input_and_only_the_latest_query_is_matched() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut state = FinderState::start_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(vec!["alpha.rs".into(), "beta.rs".into()])
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // The worker is held at a gate: no sleeps or timing guesses. Input must
        // finish while that gate is closed, including backspace and cancellation.
        state.push('b');
        state.push('x');
        state.backspace();
        state.push('e');
        assert_eq!(state.query(), "be");
        assert!(state.busy());
        assert_eq!(state.status().as_deref(), Some("Indexing… 0 files"));
        assert!(state.selected_candidate_index().is_none());
        release_tx.send(()).unwrap();
        finish(&mut state);
        assert_eq!(
            state.snapshot().window(0, 10).collect::<Vec<_>>(),
            ["beta.rs"]
        );
        assert_eq!(state.selected_candidate_index(), Some(1));
    }

    #[test]
    fn closing_a_gated_index_cancels_without_waiting_for_the_worker() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (exited_tx, exited_rx) = mpsc::channel();
        let state = FinderState::start_with(move |shared| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            exited_tx
                .send(shared.cancelled.load(Ordering::Relaxed))
                .unwrap();
            None
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let shared = Arc::clone(&state.worker.as_ref().unwrap().shared);
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            drop(state);
            dropped_tx.send(()).unwrap();
        });
        let dropped = dropped_rx.recv_timeout(Duration::from_secs(10));
        // Always release the worker, including if a regression blocked Drop on a join.
        release_tx.send(()).unwrap();
        closer.join().unwrap();
        dropped.expect("closing must complete while the indexer is still gated");
        assert!(shared.cancelled.load(Ordering::Relaxed));
        assert!(exited_rx.recv_timeout(Duration::from_secs(10)).unwrap());
    }

    #[test]
    fn a_completion_for_an_old_query_cannot_restore_or_confirm_old_rows() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut state = FinderState::start_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(vec!["alpha.rs".into(), "beta.rs".into()])
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let old = Completion {
            seq: state.seq,
            matches: vec!["alpha.rs".into()].into(),
        };
        state.push('a');
        state.push('z');
        assert!(state.selected_candidate_index().is_none());
        // The gate prevents the real completion from racing this deliberately stale one.
        *state
            .worker
            .as_ref()
            .unwrap()
            .shared
            .completion
            .lock()
            .unwrap() = Some(old);
        state.poll();
        assert!(state.selected_candidate_index().is_none());
        release_tx.send(()).unwrap();
        finish(&mut state);
        assert!(state.matches().is_empty());
        assert_eq!(state.query(), "az");
    }

    #[test]
    fn a_closed_finders_results_cannot_reach_a_new_opening() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let old = FinderState::start_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(vec!["old.rs".into()])
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let old_shared = Arc::clone(&old.worker.as_ref().unwrap().shared);
        drop(old);
        let mut new = FinderState::start_with(|_| Some(vec!["new.rs".into()]));
        new.push('n');
        finish(&mut new);
        // Inject an old completion after the replacement is already ready.
        *old_shared.completion.lock().unwrap() = Some(Completion {
            seq: new.seq,
            matches: vec!["old.rs".into()].into(),
        });
        new.poll();
        assert_eq!(new.snapshot().window(0, 10).collect::<Vec<_>>(), ["new.rs"]);
        release_tx.send(()).unwrap();
    }

    #[test]
    fn snapshots_share_storage_and_resolve_a_window_at_any_offset() {
        let matches = FinderMatches::from(
            (0..100_000)
                .map(|i| format!("file_{i:06}.rs"))
                .collect::<Vec<_>>(),
        );
        let snapshot = matches.clone();
        assert!(Arc::ptr_eq(&matches.candidates, &snapshot.candidates));
        assert!(Arc::ptr_eq(&matches.ranked, &snapshot.ranked));
        assert_eq!(
            snapshot.window(99_998, 20).collect::<Vec<_>>(),
            ["file_099998.rs", "file_099999.rs"]
        );
        assert!(snapshot.window(usize::MAX, 10).next().is_none());
    }

    #[test]
    fn cached_width_matches_the_sanitized_presenter_width_for_unicode_and_controls() {
        for path in [
            "ascii.rs",
            "日本語.rs",
            "e\u{301}.rs",
            "emoji_🦀.rs",
            "bad\x1b\n\t.rs",
        ] {
            let expected =
                ratatui::text::Line::raw(crate::text_layout::sanitize_control(path)).width();
            assert_eq!(path_width(path), expected, "{path:?}");
        }
    }

    #[test]
    fn enter_while_indexing_waits_only_for_its_query_and_is_cancelled_by_an_edit() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut state = FinderState::start_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(vec!["alpha.rs".into(), "beta.rs".into()])
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            !state.defer_confirm(),
            "an empty query still confirms nothing"
        );
        state.push('a');
        assert!(state.defer_confirm());
        assert!(!state.take_ready_confirm(), "the worker is still gated");
        state.backspace();
        state.push('b');
        assert!(state.confirm_seq.is_none(), "editing cancels the old Enter");
        assert!(state.defer_confirm());
        release_tx.send(()).unwrap();
        finish(&mut state);
        assert!(state.take_ready_confirm());
        assert!(!state.take_ready_confirm(), "confirm only once");
        assert_eq!(
            state.snapshot().window(0, 10).collect::<Vec<_>>(),
            ["beta.rs"]
        );
    }

    /// A stall long enough that waiting on it is unmistakable, bounded far below it.
    const STALL: Duration = Duration::from_secs(60);
    const STALL_BOUND: Duration = Duration::from_secs(2);

    #[test]
    fn settle_budget_is_one_frame() {
        // Clock-free half of the settle claim: a keystroke on a huge index may block for at
        // most this long. The behavioural tests below bound the wait, not this value.
        assert_eq!(SETTLE_BUDGET, Duration::from_millis(10));
    }

    #[test]
    fn settle_applies_the_current_querys_result_before_the_next_frame() {
        let mut state =
            FinderState::start_with(|_| Some(vec!["alpha.rs".into(), "beta.rs".into()]));
        finish(&mut state);
        state.push('b');
        let started = Instant::now();
        assert!(state.settle(STALL));
        // Applied by settle itself, with no event-loop poll in between.
        assert_eq!(
            state.snapshot().window(0, 10).collect::<Vec<_>>(),
            ["beta.rs"]
        );
        assert!(!state.busy());
        assert_eq!(state.status(), None);
        // Either interleaving may happen here; the forced one, where the result can only land
        // while settle waits, is `settle_is_woken_by_a_result_published_during_its_wait`.
        assert!(
            started.elapsed() < STALL_BOUND,
            "settle waited for its budget"
        );
    }

    #[test]
    fn settle_never_waits_for_an_index_still_being_built() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut state = FinderState::start_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(vec!["alpha.rs".into()])
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        state.push('a');
        let started = Instant::now();
        state.settle(STALL);
        assert!(started.elapsed() < STALL_BOUND, "settle waited on indexing");
        assert!(state.busy());
        release_tx.send(()).unwrap();
        finish(&mut state);
        assert_eq!(
            state.snapshot().window(0, 10).collect::<Vec<_>>(),
            ["alpha.rs"]
        );
    }

    #[test]
    fn settle_gives_up_after_its_budget_when_the_match_has_not_arrived() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut state = FinderState::start_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(vec!["alpha.rs".into()])
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // Indexed, but the worker is held at the gate, so no match can be published: this is a
        // huge index whose match outlasts the budget.
        let shared = Arc::clone(&state.worker.as_ref().unwrap().shared);
        shared.indexed.store(true, Ordering::Relaxed);
        state.push('a');
        let started = Instant::now();
        state.settle(SETTLE_BUDGET);
        assert!(started.elapsed() < STALL_BOUND, "settle ignored its budget");
        assert!(state.busy());
        assert_eq!(state.status().as_deref(), Some("Searching…"));
        release_tx.send(()).unwrap();
        finish(&mut state);
        assert_eq!(state.selected_candidate_index(), Some(0));
    }

    #[test]
    fn indexing_progress_alone_repaints_at_most_every_100ms() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut state = FinderState::start_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(vec!["alpha.rs".into()])
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let shared = Arc::clone(&state.worker.as_ref().unwrap().shared);
        // Synthetic instants: the throttle is asserted without a clock.
        let t0 = Instant::now();
        shared.count.store(128, Ordering::Relaxed);
        assert!(state.poll_at(t0));
        shared.count.store(256, Ordering::Relaxed);
        assert!(!state.poll_at(t0 + Duration::from_millis(50)));
        assert_eq!(state.status().as_deref(), Some("Indexing… 128 files"));
        assert!(state.poll_at(t0 + PROGRESS_REPAINT));
        assert_eq!(state.status().as_deref(), Some("Indexing… 256 files"));
        release_tx.send(()).unwrap();
        // Finishing the index repaints even inside the throttle window.
        let inside = t0 + PROGRESS_REPAINT + Duration::from_millis(1);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let changed = state.poll_at(inside);
            if state.indexed {
                assert!(changed, "the poll that sees the index finish must repaint");
                break;
            }
            assert!(Instant::now() < deadline, "worker did not complete");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_match_cancelled_by_a_stale_revision_is_retried_not_marked_done() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut state = FinderState::start_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(vec!["alpha.rs".into(), "beta.rs".into()])
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // Publish through the real path. Its wake stays queued while the worker is gated.
        state.push('b');
        let (shared, wake) = {
            let worker = state.worker.as_ref().unwrap();
            (Arc::clone(&worker.shared), worker.wake.clone())
        };
        // Recreate the window a worker could once observe, between `recompute` publishing the
        // query and its revision: the query is current, the revision still the previous one,
        // so the worker's first attempt cancels itself as if it had been superseded.
        shared.revision.store(state.seq - 1, Ordering::Relaxed);
        release_tx.send(()).unwrap();
        // The queued wake is first consumed by the worker's `recv`, which only follows its
        // first, cancelled attempt; a successful send therefore proves that attempt happened.
        let deadline = Instant::now() + Duration::from_secs(10);
        while wake.try_send(()).is_err() {
            assert!(Instant::now() < deadline, "worker never consumed its wake");
            std::thread::yield_now();
        }
        // Complete the publication as `recompute` does: the revision, then a wake. The query
        // was never matched, so the worker must match it now rather than skip it as done.
        shared.revision.store(state.seq, Ordering::Relaxed);
        let _ = wake.try_send(());
        finish(&mut state);
        assert_eq!(
            state.snapshot().window(0, 10).collect::<Vec<_>>(),
            ["beta.rs"]
        );
    }

    #[test]
    fn a_worker_that_stops_unexpectedly_reports_an_error_instead_of_indexing_forever() {
        // Returning without being cancelled stands in for a panic: either way the thread exits
        // while its finder is still open.
        let mut state = FinderState::start_with(|_| None);
        finish(&mut state);
        assert_eq!(
            state.status().as_deref(),
            Some("File search stopped unexpectedly")
        );
        state.push('a');
        assert!(!state.busy());
        assert!(!state.defer_confirm(), "no Enter is held for a dead worker");
        assert_eq!(state.query(), "a");
    }

    /// A fresh directory under the system temp dir, unique to this test.
    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hfv-finder-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn an_unpolled_completion_is_released_by_the_exiting_worker_not_the_closing_ui() {
        let state = FinderState::start_with(|_| Some(vec!["alpha.rs".into()]));
        let shared = Arc::clone(&state.worker.as_ref().unwrap().shared);
        // The initial empty-query completion sits in the mailbox; nothing polls it.
        let deadline = Instant::now() + Duration::from_secs(10);
        while shared.completion.lock().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the worker never published");
            std::thread::yield_now();
        }
        drop(state);
        // Left in the mailbox, the candidate list would be freed by whoever drops `Shared`
        // last, which can be the UI thread closing the finder.
        let deadline = Instant::now() + Duration::from_secs(10);
        while shared.completion.lock().unwrap().is_some() {
            assert!(
                Instant::now() < deadline,
                "the exiting worker left its completion for the UI to free"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn settle_is_woken_by_a_result_published_during_its_wait() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut state = FinderState::start_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(vec!["alpha.rs".into(), "beta.rs".into()])
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let shared = Arc::clone(&state.worker.as_ref().unwrap().shared);
        state.push('b');
        // Hold the query lock so the indexed worker cannot read `b`, let alone publish its
        // match, until the settle hook below lets it go.
        let (locked_tx, locked_rx) = mpsc::channel();
        let (unlock_tx, unlock_rx) = mpsc::channel::<()>();
        let holder_shared = Arc::clone(&shared);
        let holder = std::thread::spawn(move || {
            let _query = holder_shared.query.lock().unwrap();
            locked_tx.send(()).unwrap();
            let _ = unlock_rx.recv();
        });
        locked_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !shared.indexed.load(Ordering::Relaxed) {
            assert!(
                Instant::now() < deadline,
                "the worker never finished indexing"
            );
            std::thread::yield_now();
        }
        // The hook runs while settle holds the mailbox lock, so the worker can only publish
        // once settle has released it by waiting: the result must arrive via the notify.
        state.before_settle_wait = Some(Box::new(move || {
            let _ = unlock_tx.send(());
        }));
        let started = Instant::now();
        assert!(state.settle(STALL));
        assert!(
            started.elapsed() < STALL_BOUND,
            "settle missed the wake-up and waited out its budget"
        );
        assert_eq!(
            state.snapshot().window(0, 10).collect::<Vec<_>>(),
            ["beta.rs"]
        );
        holder.join().unwrap();
    }

    #[test]
    fn the_real_index_job_reports_progress_and_honours_this_openings_cancel_flag() {
        let root = temp_root("walk");
        for name in ["a.rs", "b.rs", "c.rs"] {
            std::fs::write(root.join(name), "x").unwrap();
        }
        let live = Shared::default();
        let paths = index_walk(root.clone(), false)(&live).expect("an uncancelled walk completes");
        assert_eq!(paths.len(), 3);
        assert_eq!(
            live.count.load(Ordering::Relaxed),
            3,
            "progress reaches the counter"
        );

        let closed = Shared::default();
        closed.cancelled.store(true, Ordering::Relaxed);
        assert!(
            index_walk(root.clone(), false)(&closed).is_none(),
            "a closed finder's walk stops instead of indexing the whole root"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
