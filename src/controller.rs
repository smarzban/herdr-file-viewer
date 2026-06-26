//! Session Controller — orchestrate intents into coordinated state changes.
//!
//! Holds the ephemeral session state (root, git-presence, baseline, filter flags, per-file
//! view overrides, cursor/expansion via the Tree Model, focus, observed width) and turns
//! each [`Intent`] into state mutations plus a set of [`Effects`] the run loop acts on
//! (redraw, quit, force-clear). The side-effecting components — Git Service, Content
//! Renderer, Editor Launcher — are reached through injected traits so the controller is
//! unit-testable with stubs; the Tree Model is the one read-only component it drives
//! directly. No intent edits a file (AC-N3); git-only intents are inert when not in a repo
//! (AC-26); a component failure becomes a non-fatal notice, never a crash.
//!
//! **Rendering is off the input thread (AC-23).** Selecting a file *dispatches* a render
//! job to a worker thread that owns the Content Renderer; `handle()` returns immediately so
//! input never blocks on a slow external renderer. The finished text arrives later and is
//! drained by [`Controller::poll`], which the run loop calls each tick. Jobs carry a
//! monotonic sequence so a slow render for a file the user has since left is dropped rather
//! than clobbering the current selection.

use crate::finder::FinderState;
use crate::git::{Baseline, Status};
use crate::herdr::HerdrCli;
use crate::intent::Intent;
use crate::picker::PickerState;
use crate::presenter::{FinderView, Focus, PaneGeometry, PickerRowView, PickerView, ViewState};
use crate::root::Resolved;
use crate::tree::{Node, NodeKind, TreeModel};
use crate::update::{self, UpdateState, Version};
use crate::view_policy::{FileDescriptor, ViewMode, applicable_modes, default_mode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use ratatui::text::Text;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Tree-column width as a percentage of the pane: its default and the bounds the resize keys
/// clamp to, so neither column can be squeezed to nothing.
const SPLIT_DEFAULT: u16 = 40;
const SPLIT_MIN: u16 = 20;
const SPLIT_MAX: u16 = 80;
/// How many percentage points one resize keypress moves the divider.
const SPLIT_STEP: u16 = 5;
/// How many columns one horizontal-scroll keypress moves the content pane.
const HSCROLL_STEP: u16 = 8;
/// How many content lines one mouse-wheel notch scrolls (matches herdr's default).
const WHEEL_STEP: isize = 3;
/// Two left-clicks at the same cell within this window are a double-click (a folder toggles
/// expand/collapse; a file opens in zoom mode — the editor hand-off is the `e` key).
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Read-only git queries the controller coordinates. Behind a trait so tests stub it and
/// the run loop injects an implementation bound to the real repository. `Send + Sync` so the
/// diff query can run on the render worker thread, off the input path (AC-23).
pub trait GitService: Send + Sync {
    /// Working-tree status per repo-root-relative path (drives tree markers, AC-7).
    fn status(&self) -> BTreeMap<PathBuf, Status>;
    /// The set of files changed against `baseline` (drives the changed-only filter, AC-6,
    /// and is recomputed when the baseline toggles, AC-16).
    fn changed_set(&self, baseline: Baseline) -> BTreeMap<PathBuf, Status>;
    /// Raw unified diff text for one repo-root-relative path against `baseline` (AC-9). With
    /// `full_context`, git emits the whole file as context (for the full-file diff view);
    /// otherwise it returns the compact hunks-only diff.
    fn diff(&self, rel_path: &Path, baseline: Baseline, full_context: bool) -> String;
}

/// The rendered content pane for one file: ingested text plus any non-fatal notices
/// (truncation AC-13, renderer fallback AC-25).
pub struct RenderResult {
    pub content: Text<'static>,
    pub notices: Vec<String>,
}

/// Produce the content-pane text for `(file, mode)`. `Send` so a later task can run it on a
/// worker thread (AC-23). Behind a trait so tests stub it instead of spawning glow/delta/bat.
pub trait ContentProvider: Send {
    fn render(&self, path: &Path, mode: ViewMode, raw_diff: Option<&str>) -> RenderResult;
}

/// Hand the selected file to an external editor (AC-19). Returns `Ok(true)` when the
/// hand-off took over the terminal (the run loop must force a full repaint afterwards),
/// `Ok(false)` if it did not. Behind a trait so the controller never edits or even spawns
/// directly — and tests launch nothing.
pub trait EditorHandoff {
    fn open(&mut self, file: &Path) -> io::Result<bool>;
}

/// Copy a string to the system clipboard (the `y` / `Y` path-copy keys). Behind a trait so
/// the controller never touches the terminal directly — the live implementation emits an
/// OSC 52 sequence — and tests record the copied text instead of writing a real clipboard.
/// Read-only with respect to files: it only ever copies a path string (AC-N3).
pub trait Clipboard {
    fn copy(&mut self, text: &str) -> io::Result<()>;
}

/// The root-bound providers rebuilt on every (re-)root. Editor/clipboard are NOT here — they
/// survive a re-root unchanged, so they live on [`Components`] directly. ADR-0004.
pub struct RootProviders {
    /// Shared (`Arc`) because both the controller (status / changed-set) and the render
    /// worker (diff, off the input thread) query git.
    pub git: Arc<dyn GitService>,
    pub content: Box<dyn ContentProvider>,
}

/// The injected components the controller orchestrates.
pub struct Components {
    /// Builds the root-bound providers for a given [`Resolved`]. Called once at launch, and
    /// again per re-root (T-7/T-8). `Fn` (not `FnOnce`) because a re-root re-invokes it.
    /// ADR-0004.
    pub providers: Box<dyn Fn(&Resolved) -> RootProviders>,
    pub editor: Box<dyn EditorHandoff>,
    pub clipboard: Box<dyn Clipboard>,
}

/// What the run loop should do after an intent is handled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Effects {
    /// Repaint the frame.
    pub redraw: bool,
    /// Tear down and exit (AC-20).
    pub quit: bool,
    /// Force a full clear before repainting — set after an external program (an editor)
    /// has drawn over the screen, so the diffing renderer can't leave stale cells.
    pub clear: bool,
}

impl Effects {
    fn redraw() -> Self {
        Effects {
            redraw: true,
            ..Default::default()
        }
    }
    fn noop() -> Self {
        Effects::default()
    }
}

/// A unit of off-thread rendering work sent to the worker. `seq` orders jobs so a stale
/// result (one whose selection has been superseded) is discarded on arrival. The diff itself
/// is *not* carried here — the worker fetches it from git so nothing git-related (and no
/// unbounded diff text) touches the input thread (AC-23).
struct RenderJob {
    seq: u64,
    path: PathBuf,
    /// Repo-root-relative path for the git diff query (`None` if outside the root).
    rel: Option<PathBuf>,
    mode: ViewMode,
    baseline: Baseline,
    is_git: bool,
}

/// A re-root's off-thread git result: the working-tree status (tree markers, AC-7) and the
/// changed-set against the active baseline (the changed-only filter, AC-6), both keyed by
/// repo-root-relative path. Carried over a one-shot channel from the worker `re_root` spawns to
/// the `poll` that applies them.
type StatusResult = (BTreeMap<PathBuf, Status>, BTreeMap<PathBuf, Status>);

/// The interaction orchestrator and the ephemeral session state.
pub struct Controller {
    root: PathBuf,
    is_git_repo: bool,
    baseline: Baseline,
    show_ignored: bool,
    hide_hidden: bool,
    changed_only: bool,
    /// The tree's horizontal scroll offset (columns), for reading long / deeply-nested rows. Like
    /// the cursor it is navigation state: reset on a re-root (AC-13), not carried.
    tree_hscroll: u16,
    focus: Focus,
    /// The pane width the run loop last observed (session state for the narrow-split flag,
    /// AC-21); the Presenter still lays out from the live frame, never this.
    width: u16,
    /// Vertical scroll offset of the content pane, in lines. Reset to the top whenever a new
    /// render is dispatched (a new file / mode / baseline).
    content_scroll: u16,
    /// Horizontal scroll offset of the content pane, in columns (only used when not wrapping).
    /// Reset to the left edge on a new render.
    content_hscroll: u16,
    /// The content viewport `(width, height)` the Presenter last drew into. Used to clamp
    /// `content_scroll` so the user cannot scroll past the last screenful.
    content_width: u16,
    content_height: u16,
    /// The tree column's share of the width, as a percentage (the rest is the content pane).
    /// Adjustable from the keyboard since the viewer owns both columns (ADR-0002).
    split_pct: u16,
    /// User override forcing content wrap on regardless of view mode (the `w` toggle), so long
    /// lines in code/diffs can be wrapped on demand. `false` ⇒ the per-mode default applies.
    wrap_override: bool,
    /// Hide the tree so the content pane fills the frame (the `z` zoom toggle). Pure layout
    /// state — the selection and rendered content are unchanged.
    zoomed: bool,
    tree: TreeModel,
    /// Changed-set vs the active baseline, cached; recomputed on a baseline toggle (AC-16).
    changed: BTreeMap<PathBuf, Status>,
    /// Per-file view-mode override set by cycling (AC-11); absent ⇒ the policy default.
    overrides: HashMap<PathBuf, ViewMode>,
    /// The content pane's current text and its notices (truncation/fallback).
    content: Text<'static>,
    content_notices: Vec<String>,
    /// A transient notice from the last action (e.g. an editor-launch failure); shown until
    /// the next intent is handled.
    action_notice: Option<String>,
    git: Arc<dyn GitService>,
    editor: Box<dyn EditorHandoff>,
    clipboard: Box<dyn Clipboard>,
    /// The provider factory (ADR-0004), kept so a re-root can rebuild the root-bound providers
    /// (Git Service + Content Renderer) against the new root.
    providers: Box<dyn Fn(&Resolved) -> RootProviders>,
    /// Render dispatch to the worker thread (AC-23). `latest_seq` is the most recently
    /// dispatched job; a `poll`ed result with a smaller seq is stale and dropped.
    job_tx: mpsc::Sender<RenderJob>,
    result_rx: mpsc::Receiver<(u64, RenderResult)>,
    latest_seq: u64,
    /// Hit-test geometry from the last drawn frame (fed back by the Presenter), so a mouse
    /// event can be mapped to a tree row / the content pane / the divider.
    geom: PaneGeometry,
    /// The previous left-click `(col, row, time)`, for double-click detection.
    last_click: Option<(u16, u16, Instant)>,
    /// What the held left button is dragging (divider resize or a scrollbar), so the release is
    /// treated as the end of the drag, not a click. `None` ⇒ no drag in progress.
    drag: Option<Drag>,
    /// The newer version to advertise, if any (set from the cached value at startup and
    /// refreshed by the background check). `None` ⇒ up-to-date / unknown.
    update_available: Option<Version>,
    /// Hides the banner for the rest of this session (the `u` key). Not persisted — it returns
    /// next launch while still behind.
    update_dismissed: bool,
    /// One-shot receiver for the background update check's result (`None` when no check ran).
    update_rx: Option<mpsc::Receiver<Option<Version>>>,
    /// One-shot receiver for a re-root's off-thread status/changed-set computation (AC-17).
    /// `Some` between a re-root and the tick that applies the result; `None` otherwise.
    status_rx: Option<mpsc::Receiver<StatusResult>>,
    /// The open worktree picker's state, or `None` when closed (AC-1). A re-root closes it
    /// (AC-13); the switch itself is wired in later tasks.
    picker: Option<PickerState>,
    /// The open go-to-file finder's state, or `None` when closed (AC-1). Opened by the `f` key
    /// (OpenFinder intent); confirm/cancel wired in T-7.
    finder: Option<FinderState>,
    /// The herdr query channel for the agent-active overlay (AC-3), injected post-construction
    /// via [`set_host`](Self::set_host). `None` until then ⇒ a git-only picker (AC-15).
    /// Session-level — survives a re-root unchanged.
    herdr: Option<Box<dyn HerdrCli>>,
    /// The viewer's own herdr workspace id (the agent-overlay's Tier-1 hint). Session-level —
    /// survives a re-root unchanged.
    our_workspace_id: Option<String>,
    /// The launch base-branch hint (the branch a worktree forked from), carried into a re-root's
    /// re-resolution so the post-switch Base-mode baseline can recover the common shared-base case
    /// (review-gate R1, F). Session-level — survives a re-root unchanged: the herdr per-worktree
    /// hint isn't available cross-worktree, so the launch hint is the best shared-base recovery.
    base_branch: Option<String>,
}

impl Controller {
    /// Build the controller for the resolved root. The root-bound providers (Git Service +
    /// Content Renderer) are built by the factory in `components` for this `resolved` (ADR-0004),
    /// the seam a later re-root re-invokes. When `resolved.is_git_repo`, the initial working-tree
    /// status (tree markers, AC-7) and the changed-set against `baseline` are loaded from git;
    /// otherwise the viewer is a plain browser (AC-26). The initial selection's content is
    /// rendered so the first frame is populated.
    pub fn new(resolved: Resolved, baseline: Baseline, components: Components) -> Self {
        let Components {
            providers,
            editor,
            clipboard,
        } = components;
        let RootProviders { git, content } = providers(&resolved);
        let root = resolved.root.clone();
        let is_git_repo = resolved.is_git_repo;
        // The launch base-branch hint is session-level — recorded once here and carried across
        // re-roots (F). It is `None` outside a repo / when herdr gave no hint.
        let base_branch = resolved.base_branch.clone();
        // The Content Renderer (and the diff query it needs) live on a worker thread; the
        // controller talks to it over a job channel and reads finished renders off a result
        // channel (AC-23). The worker exits when the job sender (held by the controller) is
        // dropped — which is also how a re-root retires the old worker.
        let (job_tx, result_rx) = Self::spawn_worker(Arc::clone(&git), content);

        let mut ctrl = Controller {
            tree: TreeModel::new(root.clone()),
            root,
            is_git_repo,
            baseline,
            show_ignored: false,
            hide_hidden: false,
            tree_hscroll: 0,
            changed_only: false,
            focus: Focus::Tree,
            width: 0,
            content_scroll: 0,
            content_hscroll: 0,
            content_width: 0,
            content_height: 0,
            split_pct: SPLIT_DEFAULT,
            wrap_override: false,
            zoomed: false,
            changed: BTreeMap::new(),
            overrides: HashMap::new(),
            content: Text::raw(""),
            content_notices: Vec::new(),
            action_notice: None,
            git,
            editor,
            clipboard,
            providers,
            job_tx,
            result_rx,
            latest_seq: 0,
            geom: PaneGeometry::default(),
            last_click: None,
            drag: None,
            update_available: None,
            update_dismissed: false,
            update_rx: None,
            status_rx: None,
            picker: None,
            finder: None,
            herdr: None,
            our_workspace_id: None,
            base_branch,
        };
        ctrl.refresh_git_state();
        ctrl.dispatch_render();
        ctrl
    }

    /// Spawn the off-thread render worker that owns `git` (for the diff query) and `content`
    /// (the Content Renderer), returning the job sender and result receiver the controller keeps
    /// (AC-23). The worker runs until the job sender is dropped — so `new` spawns it once, and a
    /// re-root spawns a fresh one and drops the old sender to retire the old worker. The loop
    /// body is the same one `new` used inline before T-7 extracted it; behavior is unchanged.
    fn spawn_worker(
        git: Arc<dyn GitService>,
        content: Box<dyn ContentProvider>,
    ) -> (mpsc::Sender<RenderJob>, mpsc::Receiver<(u64, RenderResult)>) {
        let (job_tx, job_rx) = mpsc::channel::<RenderJob>();
        let (result_tx, result_rx) = mpsc::channel::<(u64, RenderResult)>();
        std::thread::spawn(move || {
            while let Ok(mut job) = job_rx.recv() {
                // Collapse any backlog: under rapid navigation only the most recent selection
                // matters, so skip superseded jobs rather than render each in turn.
                while let Ok(newer) = job_rx.try_recv() {
                    job = newer;
                }
                // The diff is read here, off the input thread, so a large/slow diff never
                // blocks input (AC-23). Other modes don't need git. The full-file diff view
                // asks git for whole-file context; the compact diff uses git's default.
                let raw_diff =
                    if matches!(job.mode, ViewMode::Diff | ViewMode::FullDiff) && job.is_git {
                        let full = job.mode == ViewMode::FullDiff;
                        job.rel
                            .as_deref()
                            .map(|rel| git.diff(rel, job.baseline, full))
                    } else {
                        None
                    };
                // Contain a renderer panic so the worker survives — otherwise the thread would
                // die and rendering would stop for the rest of the session. The unwind is caught
                // here and a placeholder is surfaced in place of the failed render.
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    content.render(&job.path, job.mode, raw_diff.as_deref())
                }))
                .unwrap_or_else(|_| RenderResult {
                    content: Text::raw("[content unavailable — renderer error]"),
                    notices: vec!["the renderer failed unexpectedly; showing a placeholder".into()],
                });
                if result_tx.send((job.seq, result)).is_err() {
                    break; // controller gone
                }
            }
        });
        (job_tx, result_rx)
    }

    /// Re-root the running session to `target`: re-resolve it through the same Root Resolver used
    /// at launch, rebuild the root-bound providers (Git Service + Content Renderer) via the stored
    /// factory (ADR-0004), and respawn the render worker — overwriting `job_tx`/`result_rx` drops
    /// the old sender, so the previous worker (which owns the old providers) exits. A fresh
    /// [`TreeModel`] and reset navigation/view state follow (AC-13), while the user's *preferences*
    /// — `show_ignored`, `hide_hidden`, `changed_only`, `split_pct`, `wrap_override`, `baseline` —
    /// are carried across unchanged (AC-12). The structural re-root (resolve + fresh tree + worker respawn +
    /// carried prefs + nav reset) is **synchronous**, so the tree is immediately navigable; the
    /// heavier git status + changed-set fills in **asynchronously**, applied by [`poll`] (AC-17),
    /// so input is never blocked. Finally the first frame is rendered. A missing or
    /// non-directory `target` produces a non-fatal notice and leaves all state unchanged
    /// (AC-16); re-selecting the current root is a silent no-op (AC-11).
    pub fn re_root(&mut self, target: &Path) {
        // AC-16: a missing or non-directory target aborts with a non-fatal notice. No state
        // change — the viewer stays on its current root with all state intact.
        if !target.is_dir() {
            self.action_notice = Some(format!(
                "cannot switch worktree: {} is not an accessible directory",
                target.display()
            ));
            return;
        }
        // Carry the launch base-branch hint into the re-resolution (review-gate R1, F): herdr's
        // per-worktree hint isn't available cross-worktree, so the launch hint is the best
        // shared-base recovery. `resolve_base_branch` re-validates it against the target repo's
        // refs (worktrees share the repo's refs), recovering a shared base; otherwise it falls
        // back to main/master as before.
        let resolved = crate::root::resolve(&crate::context::LaunchContext {
            cwd: target.to_path_buf(),
            base_branch: self.base_branch.clone(),
            ..Default::default()
        });
        // AC-11: re-selecting the worktree we're already rooted at is a clean no-op — no
        // rebuild, no notice, no state change (canonicalize so /tmp vs /private/tmp matches).
        let target_canon = resolved
            .root
            .canonicalize()
            .unwrap_or_else(|_| resolved.root.clone());
        let current_canon = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        if target_canon == current_canon {
            return;
        }

        // Rebuild the root-bound providers for the new root and respawn the worker. Overwriting
        // `job_tx` drops the old sender, so the old worker (holding the old git Arc + content)
        // exits; the new worker owns the new providers.
        let RootProviders { git, content } = (self.providers)(&resolved);
        let (job_tx, result_rx) = Self::spawn_worker(Arc::clone(&git), content);
        self.git = git;
        self.job_tx = job_tx;
        self.result_rx = result_rx;

        // New root + fresh tree (this alone clears the cursor + expansions).
        self.root = resolved.root.clone();
        self.is_git_repo = resolved.is_git_repo;
        self.tree = TreeModel::new(resolved.root.clone());

        // Reset navigation/view state (AC-13). The picker is closed on a switch (AC-13 "picker
        // is closed"); `herdr`/`our_workspace_id` are session-level and deliberately left intact.
        self.focus = Focus::Tree;
        self.zoomed = false;
        self.content_scroll = 0;
        self.content_hscroll = 0;
        self.tree_hscroll = 0;
        self.overrides.clear();
        self.action_notice = None;
        self.changed = BTreeMap::new();
        self.picker = None;

        // PREFERENCES ARE CARRIED (AC-12) — deliberately NOT reset: show_ignored, hide_hidden,
        // changed_only, split_pct, wrap_override, baseline keep their current values. The fresh
        // TreeModel starts with default filter flags. `show_ignored` and `hide_hidden` are
        // git-independent, so apply them now. The changed-only *filter* is NOT applied here: it
        // must be applied against the REAL changed-set, which `dispatch_status_refresh` computes
        // off-thread — applying it now would filter against the just-cleared empty set. `poll`
        // applies it when the changed-set lands.
        self.tree.set_show_ignored(self.show_ignored);
        self.tree.set_hide_hidden(self.hide_hidden);

        // A re-root happens mid-session, so input must never block (AC-17): compute the new root's
        // status + changed-set OFF the input thread and let `poll` apply the markers + changed-only
        // filter when they arrive (as content rendering does, AC-23). The structural re-root above
        // is synchronous, so the tree is immediately navigable. Then render the first frame.
        self.dispatch_status_refresh();
        self.dispatch_render();
    }

    /// Compute the new root's working-tree status + changed-set OFF the input thread (AC-17),
    /// to be applied by [`poll`]. A non-repo has no git state — apply the (empty) changed-only
    /// filter synchronously and clear any pending fetch. Unlike `refresh_git_state` (which runs
    /// synchronously on launch / editor-return / baseline-toggle / refresh / focus-gain), this is
    /// the re-root path, where the heavier status/changed-set work must not block input.
    fn dispatch_status_refresh(&mut self) {
        if !self.is_git_repo {
            self.tree.set_changed_only(self.changed_only, &self.changed); // self.changed is empty
            self.status_rx = None;
            return;
        }
        let git = Arc::clone(&self.git);
        let baseline = self.baseline;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // Contain a git-query panic so the thread can't abort the process — parity with the
            // render worker. On panic we simply don't send; `poll` already handles the resulting
            // `Disconnected`/empty channel (it drops the receiver), so the markers just don't fill
            // in for this switch rather than crashing.
            let computed = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let status = git.status();
                let changed = git.changed_set(baseline);
                (status, changed)
            }));
            if let Ok(result) = computed {
                let _ = tx.send(result); // receiver may be gone if re-rooted again — fine
            }
        });
        self.status_rx = Some(rx);
    }

    // ---- state accessors (used by the Presenter wiring and tests) ----------------------

    pub fn show_ignored(&self) -> bool {
        self.show_ignored
    }
    pub fn hide_hidden(&self) -> bool {
        self.hide_hidden
    }
    pub fn changed_only(&self) -> bool {
        self.changed_only
    }
    pub fn baseline(&self) -> Baseline {
        self.baseline
    }
    pub fn focus(&self) -> Focus {
        self.focus
    }
    pub fn zoomed(&self) -> bool {
        self.zoomed
    }
    /// The tree column's width as a percentage of the pane (carried across a re-root, AC-12).
    pub fn split_pct(&self) -> u16 {
        self.split_pct
    }
    /// Whether the `w` content-wrap override is on (carried across a re-root, AC-12).
    pub fn wrap_override(&self) -> bool {
        self.wrap_override
    }
    pub fn tree(&self) -> &TreeModel {
        &self.tree
    }
    /// The current tree root. Exposed so tests can assert re-root results (T-13).
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn content(&self) -> &Text<'static> {
        &self.content
    }

    /// The transient action notice from the last intent, if any. Exposed for tests that need
    /// to inspect it directly (e.g. the re-root failure guard, AC-16).
    pub fn action_notice(&self) -> Option<&str> {
        self.action_notice.as_deref()
    }

    /// The open worktree picker's state, or `None` when it is closed. Exposed so the Presenter
    /// (T-14) can draw it and tests can assert the rows / pre-selected cursor.
    pub fn picker(&self) -> Option<&PickerState> {
        self.picker.as_ref()
    }

    /// Whether a re-root's off-thread status/changed-set fetch is still pending (not yet applied
    /// by [`poll`]). Exposed so a test can assert that a synchronous refresh drops the pending
    /// async fetch, so a stale async result cannot later clobber the freshly-refreshed state
    /// (review-gate R1, G).
    pub fn status_refresh_pending(&self) -> bool {
        self.status_rx.is_some()
    }

    /// The session-level launch base-branch hint, carried across re-roots (review-gate R1, F).
    /// Exposed so a test can assert the hint survives a re_root.
    pub fn base_branch_hint(&self) -> Option<&str> {
        self.base_branch.as_deref()
    }

    /// All notices to surface: the transient action notice (if any) followed by the content
    /// pane's own notices.
    pub fn notices(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(n) = &self.action_notice {
            out.push(n.clone());
        }
        out.extend(self.content_notices.iter().cloned());
        out
    }

    /// The effective view mode for the selected file (override or policy default), or `None`
    /// when nothing / a directory is selected.
    pub fn selected_view_mode(&self) -> Option<ViewMode> {
        let node = self.tree.selected()?;
        if node.kind != NodeKind::File {
            return None;
        }
        Some(self.effective_mode(&node.path))
    }

    /// Record the pane width the run loop observed (session state, AC-21).
    pub fn set_width(&mut self, width: u16) {
        self.width = width;
    }

    /// Install the update-check result: the initial (cached) banner value plus the receiver the
    /// background probe will deliver a refreshed result on. Called once by the run loop after
    /// construction; absent ⇒ no banner (so existing call sites/tests are unaffected).
    pub fn set_update(&mut self, state: UpdateState) {
        self.update_available = state.initial;
        self.update_rx = state.rx;
    }

    /// Inject the host query channel + the viewer's own workspace id (mirrors [`set_update`]).
    /// Called by `app::run` after construction; tests that exercise the picker call it with a
    /// fake [`HerdrCli`]. Session-level — NOT reset on a re-root (the viewer's workspace doesn't
    /// change when the tree does). Absent ⇒ a git-only picker with no agent overlay (AC-15).
    ///
    /// [`set_update`]: Self::set_update
    pub fn set_host(&mut self, herdr: Box<dyn HerdrCli>, workspace_id: Option<String>) {
        self.herdr = Some(herdr);
        self.our_workspace_id = workspace_id;
    }

    /// Record the content viewport `(width, height)` the Presenter last drew into, so content
    /// scrolling can be clamped to it. Called by the run loop after each draw.
    pub fn set_content_viewport(&mut self, width: u16, height: u16) {
        if width == self.content_width && height == self.content_height {
            return; // unchanged — avoid recomputing the clamp on every (mostly idle) draw
        }
        self.content_width = width;
        self.content_height = height;
        // A smaller viewport shrinks the max offset, so an existing scroll could now point past
        // the end, leaving blank space; re-clamp both axes to the new geometry.
        self.content_scroll = self.content_scroll.min(self.max_content_scroll());
        self.content_hscroll = self.content_hscroll.min(self.max_content_hscroll());
    }

    /// Receive the hit-test geometry the Presenter drew this frame (fed back from the draw
    /// closure), so the next mouse event is mapped against the live layout.
    pub fn set_pane_geometry(&mut self, geom: PaneGeometry) {
        self.geom = geom;
    }

    /// The content scroll offset (lines). Exposed for the Presenter wiring and tests.
    pub fn content_scroll(&self) -> u16 {
        self.content_scroll
    }

    /// Assemble the [`ViewState`] the Presenter draws from: the visible tree rows + cursor,
    /// the current content and notices, focus, and the observed width (the narrow-split
    /// input, AC-21).
    pub fn view_state(&self) -> ViewState {
        // Build the visible node list once and read the selection from it, rather than calling
        // `tree.selected()` (which re-runs the gitignore-aware filesystem walk) a second time
        // for the wrap decision — `visible_nodes()` is the hot, per-frame path.
        let nodes = self.tree.visible_nodes();
        let selected = self.tree.cursor();
        let wrap = self.wrap_for(nodes.get(selected));
        // The wrapped-aware content row total, so the content vertical scrollbar sizes/positions
        // against the SAME extent the scroll clamp uses (raw `lines.len()` undercounts under wrap,
        // mis-sizing the thumb / hiding the bar). Computed with the wrap we already have — no extra
        // tree walk.
        let content_rows = self.rendered_line_count_for(wrap);
        ViewState {
            nodes,
            selected,
            content: self.content.clone(),
            notices: self.notices(),
            focus: self.focus,
            width: self.width,
            content_scroll: self.content_scroll,
            content_hscroll: self.content_hscroll,
            // Last frame's tree offset, so the Presenter scrolls minimally from it (#45): selecting
            // a row already in view — e.g. a mouse click — never jumps the viewport.
            tree_scroll: self.geom.tree_scroll,
            tree_hscroll: self.tree_hscroll,
            content_rows,
            wrap,
            split_pct: self.split_pct,
            zoomed: self.zoomed,
            update_banner: self.update_banner(),
            picker: self.picker_view(),
            finder: self.finder_view(),
        }
    }

    /// The owned picker draw model for the Presenter (AC-1, AC-5), or `None` when the picker is
    /// closed. Maps each worktree row to a [`PickerRowView`] (path + branch + detached + the
    /// current marker, AC-18, + the per-row agent status, AC-19) and carries the cursor; the path
    /// display string is the worktree's full path — informative for choosing among worktrees. The
    /// Presenter sanitizes the strings (AC-27) and renders the detached/current/agent markers.
    fn picker_view(&self) -> Option<PickerView> {
        let picker = self.picker.as_ref()?;
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

    /// The owned finder draw model for the Presenter (AC-1, AC-2, AC-5), or `None` when the finder
    /// is closed. Resolves the ranked match indices into owned root-relative path strings so the
    /// Presenter is borrow-free; carries the current query and cursor. The Presenter sanitizes the
    /// path strings (AC-27) and renders the query-input line + placeholder + match rows.
    fn finder_view(&self) -> Option<FinderView> {
        let f = self.finder.as_ref()?;
        Some(FinderView {
            query: f.query().to_string(),
            matches: f
                .matches()
                .iter()
                .map(|&i| f.candidates()[i].clone())
                .collect(),
            cursor: f.cursor(),
            hscroll: f.hscroll(),
        })
    }

    /// Whether the content pane wraps for `node`: forced on by the `w` override, else the
    /// per-mode default — prose (rendered markdown / plain text) wraps; diffs and code stay
    /// unwrapped so their columns align. Takes the node so the draw path needn't re-walk.
    fn wrap_for(&self, node: Option<&Node>) -> bool {
        if self.wrap_override {
            return true;
        }
        match node {
            Some(n) if n.kind == NodeKind::File => {
                // Only prose wraps; diffs (compact and full-context) and code keep their lines
                // so columns and the line-number gutter stay aligned.
                matches!(self.effective_mode(&n.path), ViewMode::RenderedMarkdown)
            }
            _ => false,
        }
    }

    // ---- intent handling ---------------------------------------------------------------

    /// Apply one intent, returning the effects the run loop should act on.
    pub fn handle(&mut self, intent: Intent) -> Effects {
        // The action notice is transient: clear it at the top of each intent so a stale
        // failure message does not linger past the next action.
        self.action_notice = None;
        // Modal: while the picker is open, Nav/Activate/Close drive the picker, not the tree;
        // every other intent is inert (a modal selection). (AC-5)
        if self.picker.is_some() {
            return self.handle_picker_intent(intent);
        }
        match intent {
            Intent::NavUp => self.navigate(-1),
            Intent::NavDown => self.navigate(1),
            Intent::Expand => self.expand(),
            Intent::Collapse => self.collapse(),
            Intent::Activate => self.activate(),
            Intent::ToggleIgnore => self.toggle_ignore(),
            Intent::ToggleHidden => self.toggle_hidden(),
            Intent::ToggleChangedOnly => self.toggle_changed_only(),
            Intent::ToggleBaseline => self.toggle_baseline(),
            Intent::CycleView => self.cycle_view(),
            Intent::OpenInEditor => self.open_in_editor(),
            Intent::CopyRepoPath => self.copy_path(PathKind::Repo),
            Intent::CopyAbsPath => self.copy_path(PathKind::Absolute),
            Intent::ToggleFocus => self.toggle_focus(),
            Intent::ShrinkTree => self.resize_split(-(SPLIT_STEP as i16)),
            Intent::GrowTree => self.resize_split(SPLIT_STEP as i16),
            Intent::ToggleWrap => self.toggle_wrap(),
            Intent::ToggleZoom => self.toggle_zoom(),
            Intent::Refresh => self.refresh(),
            Intent::DismissUpdate => self.dismiss_update(),
            Intent::SwitchWorktree => self.open_worktree_picker(),
            Intent::OpenFinder => self.open_finder(),
            Intent::Close => self.close_or_unzoom(),
        }
    }

    /// Route an intent while the worktree picker is open (modal). NavUp/NavDown move the
    /// highlight, Expand/Collapse (Right/Left) scroll the overlay rows horizontally so long
    /// worktree paths can be read sideways, Activate confirms (re-root to the selected worktree,
    /// AC-7; re-selecting the current worktree is a no-op via re_root, AC-11), Close cancels (no
    /// state change, AC-6). All other intents are inert.
    fn handle_picker_intent(&mut self, intent: Intent) -> Effects {
        match intent {
            Intent::NavUp => {
                if let Some(p) = self.picker.as_mut()
                    && p.cursor > 0
                {
                    p.cursor -= 1;
                    return Effects::redraw();
                }
                Effects::noop()
            }
            Intent::NavDown => {
                if let Some(p) = self.picker.as_mut()
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
                if let Some(p) = self.picker.as_mut() {
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
                if let Some(p) = self.picker.as_mut()
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
                    .picker
                    .as_ref()
                    .and_then(|p| p.rows.get(p.cursor))
                    .map(|w| w.path.clone());
                self.picker = None;
                if let Some(target) = target {
                    self.re_root(&target);
                }
                Effects::redraw()
            }
            Intent::Close => {
                // Cancel: close the picker; nothing else changes (AC-6).
                self.picker = None;
                Effects::redraw()
            }
            // Modal: any other intent is inert while picking.
            _ => Effects::noop(),
        }
    }

    /// Map a mouse event to a state change. Mouse is additive to the keyboard-first design
    /// (AC-18). A `Shift`+mouse event is left untouched so the terminal's own selection/copy
    /// still works (herdr reserves Shift+mouse for exactly that). Selection/activation happen
    /// on button *release*, so a divider drag is never mistaken for a click.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> Effects {
        // Modal: while the picker is open the mouse is fully inert — the picker is
        // keyboard-only. This mirrors the keyboard modal gate in `handle`. (review-gate R1, E)
        if self.picker.is_some() {
            return Effects::noop();
        }
        // The finder is also a modal overlay, but it IS mouse-interactive: wheel scrolls the
        // selection, click selects a result row, double-click confirms. Route to the finder's
        // own handler; it never leaks to the tree/content beneath.
        if self.finder.is_some() {
            return self.handle_finder_mouse(ev);
        }
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            return Effects::noop();
        }
        let (col, row) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::ScrollDown => self.scroll_at(col, row, WHEEL_STEP),
            MouseEventKind::ScrollUp => self.scroll_at(col, row, -WHEEL_STEP),
            MouseEventKind::ScrollRight => self.hscroll_at(col, row, HSCROLL_STEP as i32),
            MouseEventKind::ScrollLeft => self.hscroll_at(col, row, -(HSCROLL_STEP as i32)),
            MouseEventKind::Down(MouseButton::Left) => {
                // A press on the divider begins a resize drag; on a scrollbar it begins a scroll
                // drag AND jumps to the pressed position (click-to-scroll). Anything else waits for
                // the release (a click). Always (re)set `drag` from the press — so a stale drag from
                // a release we never saw (e.g. swallowed by a modal) can't keep acting on later moves.
                let region = self.hit_test(col, row);
                self.drag = match region {
                    MouseRegion::Divider => Some(Drag::Divider),
                    MouseRegion::ContentVBar => Some(Drag::ContentV),
                    MouseRegion::ContentHBar => Some(Drag::ContentH),
                    MouseRegion::TreeVBar => Some(Drag::TreeV),
                    MouseRegion::TreeHBar => Some(Drag::TreeH),
                    _ => None,
                };
                match region {
                    MouseRegion::ContentVBar => self.scroll_content_to_row(row),
                    MouseRegion::ContentHBar => self.scroll_content_h_to_col(col),
                    MouseRegion::TreeVBar => self.scroll_tree_to_row(row),
                    MouseRegion::TreeHBar => self.scroll_tree_h_to_col(col),
                    _ => Effects::noop(),
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                Some(Drag::Divider) => self.resize_split_to_col(col),
                Some(Drag::ContentV) => self.scroll_content_to_row(row),
                Some(Drag::ContentH) => self.scroll_content_h_to_col(col),
                Some(Drag::TreeV) => self.scroll_tree_to_row(row),
                Some(Drag::TreeH) => self.scroll_tree_h_to_col(col),
                None => Effects::noop(),
            },
            MouseEventKind::Up(MouseButton::Left) => {
                if self.drag.take().is_some() {
                    // End of a drag, not a click. Clear the pending-click so a tree-row click made
                    // before the drag can't pair with a later one as a double-click — the drag may
                    // have scrolled the viewport, so the same screen row now maps to a different node.
                    self.last_click = None;
                    return Effects::noop();
                }
                self.handle_click(col, row)
            }
            _ => Effects::noop(),
        }
    }

    /// Handle a mouse event while the go-to-file finder is open (the finder is mouse-interactive;
    /// it owns all mouse while open and never leaks events to the tree/content beneath).
    ///
    /// - `ScrollDown`/`ScrollUp` → move the finder selection by `WHEEL_STEP`, clamped.
    ///   Position-independent (the finder is the active modal).
    /// - `Up(Left)` → click on a result row (select; double-click confirms).
    /// - `Down`/`Drag`/other → inert no-op (no drag in the finder).
    /// - `Shift`+mouse → inert (terminal selection, same as the main gate).
    fn handle_finder_mouse(&mut self, ev: MouseEvent) -> Effects {
        // Shift+mouse: terminal selection — inert, same as the main mouse gate.
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            return Effects::noop();
        }
        match ev.kind {
            MouseEventKind::ScrollDown => self.finder_move_selection(WHEEL_STEP),
            MouseEventKind::ScrollUp => self.finder_move_selection(-WHEEL_STEP),
            // Horizontal wheel: scroll the result rows sideways, mirroring the vertical-wheel
            // handling above. Additive to the keyboard ←/→ scroll (AC-18 keyboard-first).
            MouseEventKind::ScrollRight => {
                if let Some(f) = self.finder.as_mut() {
                    f.scroll_right();
                    Effects::redraw()
                } else {
                    Effects::noop()
                }
            }
            MouseEventKind::ScrollLeft => {
                if let Some(f) = self.finder.as_mut() {
                    f.scroll_left();
                    Effects::redraw()
                } else {
                    Effects::noop()
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.handle_finder_click(ev.column, ev.row),
            // Down / Drag / other: inert (no drag in the finder).
            _ => Effects::noop(),
        }
    }

    /// Move the finder selection by `delta` rows (positive = down, negative = up), clamped. A
    /// no-op when the finder is closed or the match list is empty.
    fn finder_move_selection(&mut self, delta: isize) -> Effects {
        if let Some(f) = self.finder.as_mut() {
            f.move_selection(delta);
            Effects::redraw()
        } else {
            Effects::noop()
        }
    }

    /// Handle a left-button release while the finder is open. Maps the screen cell `(col, row)`
    /// to a result-row index via `self.geom.finder_rows` + `self.geom.finder_scroll`. A click
    /// inside the rows area selects that row (double-click confirms); a click anywhere else is a
    /// modal no-op (the finder stays open — Esc cancels, not an outside click).
    fn handle_finder_click(&mut self, col: u16, row: u16) -> Effects {
        use ratatui::layout::Position;
        let Some(rows_rect) = self.geom.finder_rows else {
            // No rows area (empty query or zero matches) — click is inert but modal.
            self.last_click = None;
            return Effects::noop();
        };
        if !rows_rect.contains(Position { x: col, y: row }) {
            // Click outside the rows area (on the border, query line, etc.) — inert, modal.
            self.last_click = None;
            return Effects::noop();
        }
        // Map screen row → absolute match-list index.
        let idx = self.geom.finder_scroll as usize + (row - rows_rect.y) as usize;
        let Some(finder) = self.finder.as_ref() else {
            return Effects::noop();
        };
        if idx >= finder.matches().len() {
            // Click landed in the empty area below the last result row — inert.
            self.last_click = None;
            return Effects::noop();
        }
        let now = Instant::now();
        let double = is_double_click(self.last_click, (col, row), now);
        self.last_click = Some((col, row, now));
        // Set the finder cursor to the clicked row.
        if let Some(f) = self.finder.as_mut() {
            f.set_cursor(idx);
        }
        if double {
            // Double-click: confirm (same as Enter — reveal + render + close).
            return self.confirm_finder();
        }
        Effects::redraw()
    }

    /// A completed left-click: select the tree row it landed on (or focus the content pane). A
    /// double-click [`activate`](Self::activate)s the row — a directory toggles expand/collapse,
    /// a file opens in zoom mode (the editor hand-off is the `e` key, not the mouse).
    fn handle_click(&mut self, col: u16, row: u16) -> Effects {
        let region = self.hit_test(col, row);
        let now = Instant::now();
        match region {
            MouseRegion::TreeRow(idx) => {
                if idx >= self.tree.visible_nodes().len() {
                    self.last_click = None; // empty area below the nodes — inert, and breaks any
                    return Effects::noop(); // pending double-click sequence
                }
                // A double-click is two clicks on the SAME tree row within the window. Because
                // every non-tree-row click clears `last_click` (below), AND the finder's
                // open/confirm/Esc paths also clear it, `last_click` only ever holds a prior
                // tree-row click — the column-agnostic same-row match in `is_double_click`
                // cannot be tripped by a click in a different context (another pane or the finder).
                let double = is_double_click(self.last_click, (col, row), now);
                self.last_click = Some((col, row, now));
                self.action_notice = None;
                self.focus = Focus::Tree;
                self.tree.set_cursor(idx);
                self.dispatch_render(); // selection changed → re-render the content pane
                if double {
                    return self.activate(); // folder → expand/collapse, file → zoom mode
                }
                Effects::redraw()
            }
            MouseRegion::Content => {
                self.last_click = None; // a non-tree click breaks any pending double-click
                self.focus = Focus::Content;
                Effects::redraw()
            }
            // Scrollbars are handled on press/drag (above), not as a click; reaching here is inert.
            MouseRegion::Divider
            | MouseRegion::ContentVBar
            | MouseRegion::ContentHBar
            | MouseRegion::TreeVBar
            | MouseRegion::TreeHBar
            | MouseRegion::Outside => {
                self.last_click = None;
                Effects::noop()
            }
        }
    }

    /// Scroll the pane under the cursor: the content pane scrolls vertically; over the tree the
    /// wheel moves the selection (the tree then scrolls to keep it in view, #45).
    fn scroll_at(&mut self, col: u16, row: u16, delta: isize) -> Effects {
        match self.hit_test(col, row) {
            MouseRegion::Content => {
                self.scroll_content(delta);
                Effects::redraw()
            }
            MouseRegion::TreeRow(_) => {
                self.focus = Focus::Tree;
                self.tree.move_cursor(delta.signum());
                self.dispatch_render();
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Horizontal wheel / trackpad swipe scrolls sideways: the content pane (like the `←`/`→`
    /// keys, for unwrapped long lines) or the tree (which has no h-scroll keys). Each clamps to
    /// `[0, widest − viewport]`, so it is inert when nothing overflows.
    fn hscroll_at(&mut self, col: u16, row: u16, delta: i32) -> Effects {
        match self.hit_test(col, row) {
            MouseRegion::Content => self.scroll_content_h(delta),
            MouseRegion::TreeRow(_) => self.scroll_tree_h(delta),
            _ => Effects::noop(),
        }
    }

    /// Scroll the tree horizontally by `delta` columns, clamped to `[0, widest − tree width]` from
    /// the last drawn frame, so a long / deeply-nested row can be read sideways without ever
    /// over-scrolling past the content.
    fn scroll_tree_h(&mut self, delta: i32) -> Effects {
        let max = self
            .geom
            .tree_inner
            .map_or(0, |t| self.geom.tree_content_width.saturating_sub(t.width));
        let next = (self.tree_hscroll as i32 + delta).clamp(0, max as i32);
        self.tree_hscroll = next as u16;
        Effects::redraw()
    }

    /// The fraction `[0,1]` of a press/drag along a scrollbar track of `len` cells starting at
    /// `start`, as a rounding numerator/denominator: returns `(rel, span)` so callers stay in
    /// integer math (`offset = round(rel/span * max)`). `span` is 0 for a degenerate 1-cell track.
    fn track_fraction(pos: u16, start: u16, len: u16) -> (u32, u32) {
        let rel = pos.saturating_sub(start).min(len.saturating_sub(1)) as u32;
        (rel, len.saturating_sub(1) as u32)
    }

    /// Map a vertical press/drag on the content scrollbar track to a content scroll offset. The
    /// track is the fed-back `content_vbar` rect; the fraction maps linearly onto
    /// `[0, max_content_scroll]`, rounded to the nearest line. No-op without overflow.
    fn scroll_content_to_row(&mut self, row: u16) -> Effects {
        let Some(track) = self.geom.content_vbar else {
            return Effects::noop();
        };
        let max = self.max_content_scroll();
        let (rel, span) = Self::track_fraction(row, track.y, track.height);
        if span == 0 || max == 0 {
            return Effects::noop();
        }
        self.content_scroll = ((rel * max as u32 + span / 2) / span) as u16;
        Effects::redraw()
    }

    /// Map a horizontal press/drag on the content horizontal scrollbar to a content h-scroll offset.
    fn scroll_content_h_to_col(&mut self, col: u16) -> Effects {
        let Some(track) = self.geom.content_hbar else {
            return Effects::noop();
        };
        let max = self.max_content_hscroll();
        let (rel, span) = Self::track_fraction(col, track.x, track.width);
        if span == 0 || max == 0 {
            return Effects::noop();
        }
        self.content_hscroll = ((rel * max as u32 + span / 2) / span) as u16;
        Effects::redraw()
    }

    /// Map a horizontal press/drag on the tree's horizontal scrollbar to a tree h-scroll offset.
    fn scroll_tree_h_to_col(&mut self, col: u16) -> Effects {
        let Some(track) = self.geom.tree_hbar else {
            return Effects::noop();
        };
        let max = self.geom.tree_content_width.saturating_sub(track.width);
        let (rel, span) = Self::track_fraction(col, track.x, track.width);
        if span == 0 || max == 0 {
            return Effects::noop();
        }
        self.tree_hscroll = ((rel * max as u32 + span / 2) / span) as u16;
        Effects::redraw()
    }

    /// Map a vertical press/drag on the tree's vertical scrollbar to a selection — scrubbing the
    /// cursor through the file list, which scrolls the tree to keep it in view (the tree has no
    /// independent vertical offset; its position follows the selection, #45).
    fn scroll_tree_to_row(&mut self, row: u16) -> Effects {
        let Some(track) = self.geom.tree_vbar else {
            return Effects::noop();
        };
        let len = self.tree.visible_nodes().len();
        let (rel, span) = Self::track_fraction(row, track.y, track.height);
        if span == 0 || len <= 1 {
            return Effects::noop();
        }
        let idx = ((rel * (len as u32 - 1) + span / 2) / span) as usize;
        self.focus = Focus::Tree;
        // A drag fires many events on the same row; only re-select (and re-render the content, an
        // expensive job) when the target actually changes, so a held scrub doesn't re-render the
        // same file every tick.
        if idx == self.tree.cursor() {
            return Effects::redraw();
        }
        self.tree.set_cursor(idx);
        self.dispatch_render();
        Effects::redraw()
    }

    /// During a divider drag, set the split so the divider tracks the cursor column — clamped
    /// like the keyboard resize so neither column can collapse.
    fn resize_split_to_col(&mut self, col: u16) -> Effects {
        if self.geom.area_width == 0 {
            return Effects::noop();
        }
        let tree_w = col.saturating_sub(self.geom.area_x) as i32;
        let pct =
            (tree_w * 100 / self.geom.area_width as i32).clamp(SPLIT_MIN as i32, SPLIT_MAX as i32);
        self.split_pct = pct as u16;
        Effects::redraw()
    }

    /// Which region of the last-drawn frame a cell falls in. The divider is checked first (it
    /// sits between the columns); a tree click maps to a visible node index by its row.
    fn hit_test(&self, col: u16, row: u16) -> MouseRegion {
        if let Some(dx) = self.geom.divider_x
            && (col == dx || col + 1 == dx)
        {
            return MouseRegion::Divider;
        }
        // Scrollbars live INSIDE the panes (a reserved gutter), fed back as 1-cell track rects that
        // are present only when that bar is drawn — so a hit on a `Some` track is a real bar. Check
        // them before the text rects. The tree's vertical bar no longer shares the divider column.
        let pos = Position { x: col, y: row };
        if self.geom.content_vbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::ContentVBar;
        }
        if self.geom.content_hbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::ContentHBar;
        }
        if self.geom.tree_vbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::TreeVBar;
        }
        if self.geom.tree_hbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::TreeHBar;
        }
        if let Some(t) = self.geom.tree_inner
            && t.contains(pos)
        {
            // Map the screen row to the node actually drawn there: the on-screen offset plus the
            // tree's scroll offset (#45), the same value `draw_tree` scrolled by. The row index may
            // still exceed the node count (the empty area below the last node): the click handler
            // treats that as inert, while the wheel still scrolls the column.
            return MouseRegion::TreeRow((row - t.y) as usize + self.geom.tree_scroll as usize);
        }
        if let Some(c) = self.geom.content_inner
            && c.contains(Position { x: col, y: row })
        {
            return MouseRegion::Content;
        }
        MouseRegion::Outside
    }

    /// Up/down navigation is focus-aware: it moves the tree cursor when the tree is focused
    /// (selecting a file, which re-renders the content), and scrolls the content pane when the
    /// content is focused (`Tab` switches focus). This reads each pane's natural keys without
    /// adding a separate scroll intent.
    fn navigate(&mut self, delta: isize) -> Effects {
        match self.focus {
            Focus::Content => {
                self.scroll_content(delta);
                Effects::redraw()
            }
            Focus::Tree => {
                self.tree.move_cursor(delta);
                self.dispatch_render(); // new selection → re-render (and reset the scroll)
                Effects::redraw()
            }
        }
    }

    /// Scroll the content pane by `delta` lines, clamped to `[0, max]` so it can never run
    /// above the first line or past the last screenful.
    fn scroll_content(&mut self, delta: isize) {
        let max = self.max_content_scroll() as isize;
        let next = (self.content_scroll as isize + delta).clamp(0, max);
        self.content_scroll = next as u16;
    }

    /// The largest valid scroll offset: total rendered lines minus the viewport height.
    fn max_content_scroll(&self) -> u16 {
        self.rendered_line_count()
            .saturating_sub(self.content_height)
    }

    /// How many rows the content occupies once laid out, so the vertical scroll clamps to the
    /// real last row. Without wrapping each source line is one (truncated) row. With wrapping a
    /// line spans multiple rows: ratatui's exact `line_count` is private, and an arithmetic
    /// `ceil`/`floor` undercounts word wrapping (words don't pack to the column), which would
    /// leave the bottom of wrapped prose unreachable — so [`wrapped_rows`] simulates the word
    /// packing, floored by the all-columns char-wrap count so leading/interior spaces can't
    /// make it undershoot. Off the per-frame path: only scroll / resize / wrap-toggle keypaths
    /// reach it (`set_content_viewport` early-returns on an unchanged size).
    fn rendered_line_count(&self) -> u16 {
        self.rendered_line_count_for(self.effective_wrap())
    }

    /// Like [`rendered_line_count`] but takes the wrap flag, so a caller that already knows it
    /// (e.g. `view_state`, which computes wrap from the visible nodes it just built) avoids the
    /// extra tree walk `effective_wrap` would do. This is the wrapped-aware row total the content
    /// vertical scrollbar must size/position against — raw `lines.len()` undercounts under wrap.
    fn rendered_line_count_for(&self, wrap: bool) -> u16 {
        let count = if wrap {
            let w = self.content_width.max(1) as usize;
            self.content
                .lines
                .iter()
                .map(|l| {
                    let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                    wrapped_rows(&text, w).max(l.width().max(1).div_ceil(w))
                })
                .sum::<usize>()
        } else {
            self.content.lines.len()
        };
        count.min(u16::MAX as usize) as u16
    }

    /// Scroll the content pane horizontally by `delta` columns, clamped to `[0, max]`.
    fn scroll_content_h(&mut self, delta: i32) -> Effects {
        let max = self.max_content_hscroll() as i32;
        let next = (self.content_hscroll as i32 + delta).clamp(0, max);
        self.content_hscroll = next as u16;
        Effects::redraw()
    }

    /// The largest valid horizontal offset: the widest content line minus the viewport width.
    /// Zero while wrapping (no line overflows the pane, so there is nothing to scroll past).
    fn max_content_hscroll(&self) -> u16 {
        if self.effective_wrap() {
            return 0;
        }
        let widest = self
            .content
            .lines
            .iter()
            .map(|l| l.width())
            .max()
            .unwrap_or(0);
        (widest.min(u16::MAX as usize) as u16).saturating_sub(self.content_width)
    }

    /// Right (→/l): expand the selected directory when the tree is focused, or scroll the
    /// content pane right when it is focused (so long unwrapped lines can be read).
    fn expand(&mut self) -> Effects {
        if self.focus == Focus::Content {
            return self.scroll_content_h(HSCROLL_STEP as i32);
        }
        if let Some(node) = self.tree.selected()
            && node.kind == NodeKind::Dir
        {
            self.tree.expand(&node.path);
            return Effects::redraw();
        }
        Effects::noop()
    }

    /// Left (←/h): collapse the selected directory when the tree is focused, or scroll the
    /// content pane left when it is focused.
    fn collapse(&mut self) -> Effects {
        if self.focus == Focus::Content {
            return self.scroll_content_h(-(HSCROLL_STEP as i32));
        }
        if let Some(node) = self.tree.selected()
            && node.kind == NodeKind::Dir
        {
            self.tree.collapse(&node.path);
            return Effects::redraw();
        }
        Effects::noop()
    }

    /// Activate the selected node (Enter / double-click): a directory toggles expand/collapse;
    /// a file opens in **zoom mode** — the content pane fills the frame (focused), so the file
    /// is read full-screen. Read-only: opening in an external editor stays on `e`
    /// ([`Intent::OpenInEditor`]). The content was already rendered when the file was selected,
    /// so this only flips the layout/focus — no re-render is dispatched.
    fn activate(&mut self) -> Effects {
        let Some(node) = self.tree.selected() else {
            return Effects::noop();
        };
        match node.kind {
            NodeKind::Dir => {
                if node.expanded {
                    self.tree.collapse(&node.path);
                } else {
                    self.tree.expand(&node.path);
                }
                Effects::redraw()
            }
            NodeKind::File => {
                self.zoomed = true;
                self.focus = Focus::Content;
                Effects::redraw()
            }
        }
    }

    fn toggle_ignore(&mut self) -> Effects {
        self.show_ignored = !self.show_ignored;
        self.tree.set_show_ignored(self.show_ignored);
        // Revealing/hiding ignored entries can shift which node the cursor lands on, so the
        // content pane must re-render for the (possibly) new selection — otherwise it shows a
        // file that is no longer highlighted.
        self.dispatch_render();
        Effects::redraw()
    }

    fn toggle_hidden(&mut self) -> Effects {
        self.hide_hidden = !self.hide_hidden;
        self.tree.set_hide_hidden(self.hide_hidden);
        // Hiding/revealing dotfiles can shift which node the cursor lands on, so re-render the
        // content pane for the (possibly) new selection — mirrors toggle_ignore.
        self.dispatch_render();
        Effects::redraw()
    }

    fn toggle_changed_only(&mut self) -> Effects {
        if !self.is_git_repo {
            return Effects::noop(); // inert without git (AC-26)
        }
        self.changed_only = !self.changed_only;
        self.tree.set_changed_only(self.changed_only, &self.changed);
        self.dispatch_render();
        Effects::redraw()
    }

    fn toggle_baseline(&mut self) -> Effects {
        if !self.is_git_repo {
            return Effects::noop(); // inert without git (AC-26)
        }
        self.baseline = match self.baseline {
            Baseline::Head => Baseline::Base,
            Baseline::Base => Baseline::Head,
        };
        // Recompute the changed-set against the new baseline (AC-16) and keep the changed-only
        // filter consistent with it.
        self.changed = self.git.changed_set(self.baseline);
        self.tree.set_changed_only(self.changed_only, &self.changed);
        // Drop any pending re-root async status fetch (review-gate R2): this synchronous
        // recompute is now authoritative, so a stale in-flight async result must not clobber
        // it in `poll`. Invariant: every synchronous git-state recompute invalidates a pending
        // re-root async fetch. Mirrors `refresh_git_state` → `drop_pending_status`.
        self.drop_pending_status();
        self.dispatch_render(); // a diff is relative to the baseline, so it must re-render
        Effects::redraw()
    }

    fn cycle_view(&mut self) -> Effects {
        let Some(node) = self.tree.selected() else {
            return Effects::noop();
        };
        if node.kind != NodeKind::File {
            return Effects::noop();
        }
        let modes = applicable_modes(&self.descriptor(&node.path));
        let current = self.effective_mode(&node.path);
        let idx = modes.iter().position(|m| *m == current).unwrap_or(0);
        let next = modes[(idx + 1) % modes.len()];
        self.overrides.insert(node.path.clone(), next);
        self.dispatch_render();
        Effects::redraw()
    }

    fn open_in_editor(&mut self) -> Effects {
        let Some(node) = self.tree.selected() else {
            return Effects::noop();
        };
        if node.kind != NodeKind::File {
            return Effects::noop();
        }
        match self.editor.open(&node.path) {
            Ok(true) => {
                // The editor took the terminal and may have changed the file: re-query git so
                // status markers and the changed-set reflect the edit, re-render the pane, and
                // force a full repaint (the external program drew over the screen).
                self.refresh_git_state();
                self.dispatch_render();
                Effects {
                    redraw: true,
                    clear: true,
                    ..Default::default()
                }
            }
            Ok(false) => Effects::redraw(), // hand-off without a terminal takeover
            Err(e) => {
                self.action_notice = Some(format!("Could not open editor: {e}"));
                // The hand-off may have suspended the terminal before failing, so force a full
                // repaint to recover from any partial screen state.
                Effects {
                    redraw: true,
                    clear: true,
                    ..Default::default()
                }
            }
        }
    }

    /// Copy the selected node's path to the clipboard (`y` repo-relative, `Y` absolute). Works
    /// for files and directories alike — copying a path mutates nothing (AC-N3). The outcome is
    /// surfaced as a transient notice ("Copied …" / a clipboard-failure message). Inert when
    /// nothing is selected. The repo-relative form falls back to the absolute path for a node
    /// outside the tree root (there is no relative form to give), which in practice cannot
    /// happen since every node is under the root.
    fn copy_path(&mut self, kind: PathKind) -> Effects {
        let Some(node) = self.tree.selected() else {
            return Effects::noop();
        };
        let raw = match kind {
            PathKind::Absolute => node.path.to_string_lossy().into_owned(),
            PathKind::Repo => self
                .rel(&node.path)
                .unwrap_or_else(|| node.path.clone())
                .to_string_lossy()
                .into_owned(),
        };
        // A filename is untrusted — an attacker can craft one in a browsed repo, and a path may
        // legally contain control bytes: ESC/BEL (a terminal escape, e.g. a forged OSC 52) or a
        // newline that would paste-inject into a shell. Strip them before the path reaches the
        // clipboard *or* the notice — the same defense `sanitize_label` applies to every other
        // filesystem-derived string we display. (The OSC 52 payload is base64-encoded only for
        // transport; the terminal decodes it back to this exact string onto the clipboard, so the
        // encoding alone does not make a control-bearing path safe to paste.)
        let text: String = raw.chars().filter(|c| !c.is_control()).collect();
        self.action_notice = Some(match self.clipboard.copy(&text) {
            Ok(()) => format!("Copied {text}"),
            Err(e) => format!("Could not copy path: {e}"),
        });
        Effects::redraw()
    }

    fn toggle_focus(&mut self) -> Effects {
        // While zoomed the tree is hidden, so there is nothing to switch focus to: keep focus
        // pinned to the content pane (entering zoom set it there). Without this guard, Tab would
        // move focus to the invisible tree and route j/k to its cursor — silently re-rendering a
        // different file behind the full-screen content (review-gate R1, 4-model finding).
        if self.zoomed {
            return Effects::noop();
        }
        self.focus = match self.focus {
            Focus::Tree => Focus::Content,
            Focus::Content => Focus::Tree,
        };
        Effects::redraw()
    }

    /// Toggle zoom: hide the tree so the content pane fills the frame, and back. Entering zoom
    /// moves focus to the content pane so up/down keys scroll the now-full-screen file; leaving
    /// it returns focus to the tree (back to picking files). Pure layout state — the selection
    /// and rendered content are unchanged, so no re-render is dispatched.
    fn toggle_zoom(&mut self) -> Effects {
        self.zoomed = !self.zoomed;
        self.focus = if self.zoomed {
            Focus::Content
        } else {
            Focus::Tree
        };
        Effects::redraw()
    }

    /// The close key (`q`/`Esc`): when zoomed, back out of zoom first (the instinctive "escape
    /// the full-screen view") and stay in the viewer; otherwise quit (AC-20). So from a zoomed
    /// file it takes two presses to leave — one to un-zoom, one to close.
    fn close_or_unzoom(&mut self) -> Effects {
        if self.zoomed {
            self.zoomed = false;
            self.focus = Focus::Tree;
            return Effects::redraw();
        }
        Effects {
            quit: true,
            ..Default::default()
        }
    }

    /// Move the tree/content divider by `delta` percentage points, clamped so neither column
    /// can collapse. Pure layout state — no re-render is needed (the content is unchanged).
    fn resize_split(&mut self, delta: i16) -> Effects {
        let next = (self.split_pct as i16 + delta).clamp(SPLIT_MIN as i16, SPLIT_MAX as i16);
        self.split_pct = next as u16;
        Effects::redraw()
    }

    /// Flip the content-wrap override. Pure layout state — the content text is unchanged, only
    /// how the Presenter lays it out; the scroll clamp recomputes from the new wrap.
    fn toggle_wrap(&mut self) -> Effects {
        self.wrap_override = !self.wrap_override;
        // Wrapping changes the content's layout, so re-clamp both offsets: vertical to the new
        // line count, and horizontal to zero while wrapped (no line overflows the pane).
        self.content_scroll = self.content_scroll.min(self.max_content_scroll());
        self.content_hscroll = self.content_hscroll.min(self.max_content_hscroll());
        Effects::redraw()
    }

    /// Whether the content pane wraps the current selection (the `w` override or the per-mode
    /// default). Off the per-frame path — the scroll-clamp helpers call it on a keypress —
    /// so resolving the selected node via `tree.selected()` here is fine.
    fn effective_wrap(&self) -> bool {
        self.wrap_for(self.tree.selected().as_ref())
    }

    /// `r` — explicitly re-read git state and re-render the current selection, so the viewer
    /// picks up changes made outside it (a merge, pull, or commit in another pane). A full
    /// refresh: it re-renders the content (resetting its scroll), since the user asked for it.
    fn refresh(&mut self) -> Effects {
        self.refresh_git_state();
        self.dispatch_render();
        Effects::redraw()
    }

    /// Hide the update banner for this session (the `u` key). Inert when no banner is showing,
    /// so the key does nothing (no wasted repaint) until an update is actually available.
    fn dismiss_update(&mut self) -> Effects {
        if self.update_available.is_some() && !self.update_dismissed {
            self.update_dismissed = true;
            return Effects::redraw();
        }
        Effects::noop()
    }

    /// The update-banner text to display, or `None` when up-to-date, dismissed, or unknown.
    fn update_banner(&self) -> Option<String> {
        if self.update_dismissed {
            return None;
        }
        self.update_available.as_ref().map(update::banner_text)
    }

    /// Open the worktree picker (AC-1). Gated to a git repo — outside one it is a no-op with a
    /// non-fatal notice and no picker (AC-14). Rows come from the read-only git worktree list; the
    /// pre-select is the agent-active worktree when herdr reports one (AC-3), else the current root
    /// (AC-4). A missing/failing herdr overlay degrades to the git-only list (AC-15).
    fn open_worktree_picker(&mut self) -> Effects {
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
        self.picker = Some(PickerState {
            rows,
            agent_statuses,
            cursor,
            hscroll: 0,
        });
        Effects::redraw()
    }

    /// Open the go-to-file finder (AC-1). Builds the file index for the current root, then
    /// installs a fresh `FinderState` with an empty query and the full candidate list.
    /// Returns [`Effects::redraw`] so the run loop paints the overlay on the next tick (T-8).
    ///
    /// Modal mutual-exclusion (finder inert while the picker is open) holds BY CONSTRUCTION:
    /// `handle()` routes to `handle_picker_intent()` while `self.picker.is_some()`, and its
    /// catch-all `_ => Effects::noop()` swallows `OpenFinder`. No extra guard is needed here.
    fn open_finder(&mut self) -> Effects {
        let candidates = crate::index::build(&self.root);
        self.finder = Some(FinderState::new(candidates));
        self.last_click = None; // opening the finder resets double-click state so a prior tree
        // click cannot pair with the first finder click as a double-click
        Effects::redraw()
    }

    /// Whether the go-to-file finder overlay is currently open.
    pub fn finder_open(&self) -> bool {
        self.finder.is_some()
    }

    /// The full candidate list loaded when the finder was opened, or an empty slice when
    /// the finder is closed. Exposed for tests (T-5); the Presenter/T-8 read via `finder()`.
    pub fn finder_candidates(&self) -> &[String] {
        self.finder.as_ref().map(|f| f.candidates()).unwrap_or(&[])
    }

    /// The current finder query string, or `""` when the finder is closed or the query is
    /// empty. Exposed for tests (T-5); the Presenter/T-8 reads via `finder()`.
    pub fn finder_query(&self) -> &str {
        self.finder.as_ref().map(|f| f.query()).unwrap_or("")
    }

    /// The current ranked match indices (into `finder_candidates()`), or `&[]` when the finder
    /// is closed or the query is empty. Exposed for tests (T-6) and the Presenter (T-8).
    pub fn finder_matches(&self) -> &[usize] {
        self.finder.as_ref().map(|f| f.matches()).unwrap_or(&[])
    }

    /// The cursor position within the match list, or `0` when the finder is closed or the
    /// list is empty. Exposed for tests (T-6) and confirm (T-7).
    pub fn finder_cursor(&self) -> usize {
        self.finder.as_ref().map(|f| f.cursor()).unwrap_or(0)
    }

    /// The horizontal scroll offset for the result rows, or `0` when the finder is closed.
    /// Exposed for tests that verify Left/Right keys and horizontal wheel move hscroll.
    pub fn finder_hscroll(&self) -> u16 {
        self.finder.as_ref().map(|f| f.hscroll()).unwrap_or(0)
    }

    /// Route a key event while the finder overlay is open.
    ///
    /// - A printable `Char(c)` with no modifier other than `SHIFT` pushes the character,
    ///   re-runs [`fuzzy::match_and_rank`] over the candidates, and resets the selection
    ///   to 0 (AC-7).
    /// - `Backspace` deletes the last character and re-matches (AC-7).
    /// - `Up`/`Down` move the selection within the current match list, clamped at both ends
    ///   (AC-8).
    /// - `Enter` confirms the selection — reveal + render, or a non-fatal notice on a vanished
    ///   target, or a no-op that keeps the finder open when there are no matches (AC-6, AC-10,
    ///   AC-11, AC-20). `Esc` discards the finder, leaving the prior state intact (AC-9).
    ///
    /// When the finder is not open, all keys are a no-op (defensive guard).
    pub fn handle_finder_key(&mut self, key: KeyEvent) -> Effects {
        let Some(finder) = self.finder.as_mut() else {
            return Effects::noop();
        };
        match key.code {
            KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                finder.push(c);
                Effects::redraw()
            }
            KeyCode::Backspace => {
                finder.backspace();
                Effects::redraw()
            }
            KeyCode::Up => {
                finder.move_selection(-1);
                Effects::redraw()
            }
            KeyCode::Down => {
                finder.move_selection(1);
                Effects::redraw()
            }
            // Left/Right: horizontal scroll of the result rows. The prompt is append-only so the
            // arrow keys are free — exactly as the picker uses ←/→ for hscroll. The Presenter
            // clamps to `max_row_width − inner_width` at draw, so over-scrolling is harmless here.
            KeyCode::Left => {
                finder.scroll_left();
                Effects::redraw()
            }
            KeyCode::Right => {
                finder.scroll_right();
                Effects::redraw()
            }
            KeyCode::Enter => self.confirm_finder(),
            KeyCode::Esc => {
                self.finder = None;
                self.last_click = None; // closing the finder resets double-click state so a
                // finder click cannot pair with the next tree click
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Confirm the current finder selection: take the selected candidate's root-relative path,
    /// join with the root, and call [`TreeModel::reveal`]. On success re-sync the controller's
    /// filter mirrors (reveal may have relaxed `changed_only`/`hide_hidden` in the tree),
    /// dispatch a render for the newly-selected file, close the finder, and return a redraw.
    ///
    /// - Zero matches (empty list) → no-op; finder stays open (AC-6).
    /// - Reveal returns `false` (target missing/removed since open) → close the finder, set a
    ///   non-fatal `action_notice`, leave the tree selection unchanged (AC-20).
    fn confirm_finder(&mut self) -> Effects {
        let Some(finder) = self.finder.as_ref() else {
            return Effects::noop();
        };
        let Some(cand_idx) = finder.selected_candidate_index() else {
            return Effects::noop(); // zero matches → no-op, finder stays open (AC-6)
        };
        let rel = finder.candidates()[cand_idx].clone();
        let abs = self.root.join(&rel);
        self.finder = None; // confirm dismisses the modal regardless of reveal outcome
        self.last_click = None; // closing the finder resets double-click state
        if self.tree.reveal(&abs) {
            // reveal() may have relaxed the tree's changed_only/hide_hidden fields — re-sync
            // the controller's mirror fields so a later `c`/`.` toggle stays consistent
            // (T-4 review note: the mirrors at controller.rs:166-168 drive those toggles).
            self.changed_only = self.tree.changed_only();
            self.hide_hidden = self.tree.hide_hidden();
            // If the content pane isn't currently visible — the narrow, tree-only layout where the
            // last frame drew no content column (`content_width == 0`) — open the jumped-to file in
            // zoom mode so the user actually SEES the file they jumped to, instead of landing on a
            // tree row with the file hidden off-screen. This mirrors the tree's Enter/activate on a
            // file (content full-screen). When the content is already visible (the wide two-column
            // layout, or already zoomed), the layout is left untouched and the file just renders.
            if self.content_width == 0 {
                self.zoomed = true;
                self.focus = Focus::Content;
            }
            self.dispatch_render();
            Effects::redraw()
        } else {
            // Target has disappeared since the finder was opened — non-fatal notice (AC-20).
            self.action_notice = Some(format!("Could not open {rel}"));
            Effects::redraw()
        }
    }

    /// Fetch the herdr agent overlay — the `worktree list` + `agent list` JSON — with exactly the
    /// two read-only queries (AC-20), or `None` when herdr is absent or either query fails (a
    /// git-only picker, AC-15). herdr's `worktree list` and `agent list` BOTH print JSON by
    /// default; `agent list` REJECTS a `--json` flag (verified live against herdr 0.7.x — it exits
    /// non-zero), so neither subcommand is passed the flag. (A prior `--json` on the agent query
    /// made this overlay silently fail → always fall back to the current root, AC-4/AC-15.)
    ///
    /// This is the single point both the per-row status badges and the agent-active pre-select
    /// derive from, so opening the picker issues exactly two herdr calls (T-10 spy test).
    fn herdr_overlay(&self) -> Option<(String, String)> {
        let herdr = self.herdr.as_ref()?;
        let wt_json = herdr.run_json(&["worktree", "list"]).ok()?;
        let ag_json = herdr.run_json(&["agent", "list"]).ok()?;
        Some((wt_json, ag_json))
    }

    /// The pane regained focus (the run loop forwards herdr's focus events): re-read git state
    /// so external changes show in the tree. No-op without a repo (AC-26) — so an external
    /// change to a non-git directory costs nothing. In **changed-only** mode the refresh
    /// re-filters the visible list, which can move the cursor to a different file; if the
    /// selection actually changed, re-render so the content pane matches the highlighted row —
    /// otherwise the content (and its scroll) is left untouched, the common case.
    pub fn handle_focus_gained(&mut self) -> Effects {
        if !self.is_git_repo {
            return Effects::noop();
        }
        let before = self.tree.selected().map(|n| n.path);
        self.refresh_git_state();
        if self.tree.selected().map(|n| n.path) != before {
            self.dispatch_render();
        }
        Effects::redraw()
    }

    /// Re-query git for the working-tree status (tree markers, AC-7) and the changed-set
    /// against the active baseline (AC-16), updating the tree caches. No-op without a repo
    /// (AC-26). Runs on the calling thread, but only on deliberate, infrequent actions —
    /// launch, editor return, baseline toggle, the `r` refresh key, and focus-gain — never the
    /// hot navigation path, where the diff is fetched off-thread (AC-23).
    fn refresh_git_state(&mut self) {
        if !self.is_git_repo {
            return;
        }
        let status = self.git.status();
        self.tree.set_status(&status);
        self.changed = self.git.changed_set(self.baseline);
        self.tree.set_changed_only(self.changed_only, &self.changed);
        // Drop any pending re-root async status fetch (review-gate R1, G + R2): this sync
        // refresh has just produced the authoritative status/changed-set, so an older in-flight
        // async result must not later clobber it in `poll`. Invariant: every synchronous
        // git-state recompute invalidates a pending re-root async fetch.
        self.drop_pending_status();
    }

    /// Drop any pending re-root async status/changed-set fetch so a stale in-flight result
    /// cannot later overwrite a freshly-recomputed synchronous git state in [`poll`]. Must be
    /// called after every synchronous git-state recompute (review-gate R1 G + R2).
    fn drop_pending_status(&mut self) {
        self.status_rx = None;
    }

    // ---- content coordination ----------------------------------------------------------

    /// Dispatch a render of the current selection to the worker thread (AC-23) — never
    /// blocking and doing **no git or rendering work on the input thread**: the worker reads
    /// the diff and delegates to the external renderer. A directory or empty selection clears
    /// the pane synchronously (no job). Every call bumps `latest_seq`, so any still-in-flight
    /// render for the previous selection is superseded and dropped by [`poll`].
    fn dispatch_render(&mut self) {
        self.latest_seq += 1;
        let seq = self.latest_seq;
        // A fresh render means new content — start it at the top-left, never inheriting the
        // previous file's scroll offsets.
        self.content_scroll = 0;
        self.content_hscroll = 0;

        let Some(node) = self.tree.selected() else {
            return self.clear_content();
        };
        if node.kind != NodeKind::File {
            return self.clear_content();
        }
        let mode = self.effective_mode(&node.path);
        let rel = self.rel(&node.path);
        // If the worker has gone (channel closed) the send simply fails; the pane keeps its
        // last content rather than panicking.
        let _ = self.job_tx.send(RenderJob {
            seq,
            path: node.path,
            rel,
            mode,
            baseline: self.baseline,
            is_git: self.is_git_repo,
        });
    }

    /// Clear the content pane (selection is a directory / nothing).
    fn clear_content(&mut self) {
        self.content = Text::raw("");
        self.content_notices.clear();
    }

    /// Drain finished renders from the worker, applying only the one matching the latest
    /// dispatched selection (stale results are discarded). Returns `Some` redraw effect when
    /// fresh content was applied, so the run loop repaints; `None` when nothing arrived.
    pub fn poll(&mut self) -> Option<Effects> {
        let mut applied = false;
        while let Ok((seq, result)) = self.result_rx.try_recv() {
            if seq == self.latest_seq {
                self.content = result.content;
                self.content_notices = result.notices;
                applied = true;
            }
            // else: a superseded selection's render — drop it.
        }
        // A re-root's off-thread status/changed-set (one-shot, AC-17): apply the new root's
        // markers and the carried changed-only filter against the freshly-arrived changed-set,
        // then drop the receiver. A *disconnected* channel means a second re-root superseded this
        // fetch (its `send` failed) — drop the receiver so we stop polling a dead channel.
        if let Some(rx) = &self.status_rx {
            match rx.try_recv() {
                Ok((status, changed)) => {
                    self.tree.set_status(&status);
                    self.changed = changed;
                    self.tree.set_changed_only(self.changed_only, &self.changed);
                    self.status_rx = None;
                    // The synchronous `re_root` dispatched the first render against the *empty*
                    // changed-set, so a changed file rendered in content/markdown mode, not Diff.
                    // Now that the real changed-set has landed, re-dispatch so the current
                    // selection re-renders in the correct view mode (changed → Diff, AC-9).
                    // (review-gate R1, B).
                    self.dispatch_render();
                    applied = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.status_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        // A finished background update check (one-shot): adopt its verdict and drop the receiver.
        // `Some(v)` shows/refreshes the banner; `None` (a successful check that found nothing
        // newer) clears a now-stale cached banner. A *disconnected* channel means the probe failed
        // and sent nothing — drop the receiver too, so we stop polling a dead channel every tick.
        if let Some(rx) = &self.update_rx {
            match rx.try_recv() {
                Ok(version) => {
                    self.update_available = version;
                    self.update_rx = None;
                    applied = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.update_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        applied.then(Effects::redraw)
    }

    /// The effective view mode for a file: the user's override, else the policy default.
    fn effective_mode(&self, path: &Path) -> ViewMode {
        self.overrides
            .get(path)
            .copied()
            .unwrap_or_else(|| default_mode(&self.descriptor(path)))
    }

    /// The View Policy facts about a file: markdown by extension, changed by the cached
    /// changed-set (so it tracks the active baseline).
    fn descriptor(&self, path: &Path) -> FileDescriptor {
        FileDescriptor {
            path: path.to_path_buf(),
            is_markdown: is_markdown(path),
            is_changed: self.is_changed(path),
        }
    }

    fn is_changed(&self, path: &Path) -> bool {
        self.rel(path)
            .map(|rel| self.changed.contains_key(&rel))
            .unwrap_or(false)
    }

    /// `path` made relative to the tree root (how git keys its maps); `None` if outside it.
    fn rel(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.root).ok().map(Path::to_path_buf)
    }
}

/// Which form of the selected node's path the copy keys put on the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    /// Relative to the tree/repo root (`y`), e.g. `src/app.rs`.
    Repo,
    /// The full absolute path (`Y`).
    Absolute,
}

/// Where a mouse cell falls in the drawn layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseRegion {
    TreeRow(usize),
    Content,
    Divider,
    /// The content pane's vertical scrollbar — drag up/down to scroll.
    ContentVBar,
    /// The content pane's horizontal scrollbar — drag left/right to scroll.
    ContentHBar,
    /// The tree's vertical scrollbar — drag up/down to scrub the selection through the list.
    TreeVBar,
    /// The tree's horizontal scrollbar — drag left/right to scroll the tree sideways.
    TreeHBar,
    Outside,
}

/// What a held left-button drag is currently manipulating. Set on press, cleared on release.
/// The scrollbars now live *inside* the panes (not on the borders), so all four are draggable —
/// the tree's vertical bar no longer collides with the divider.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Divider,
    ContentV,
    ContentH,
    TreeV,
    TreeH,
}

/// Two left-clicks on the same **row** within [`DOUBLE_CLICK`] are a double-click. The column
/// is ignored on purpose: a tree row is a single node end-to-end, so a click anywhere along it
/// targets that node, and a touchpad double-tap commonly lands a column or two apart between
/// taps — requiring the exact cell would silently drop those. (The column still matters for
/// *which* node a click selects; that is the caller's hit-test, not this timing rule.) Pure over
/// its timestamps so the timing rule is unit-testable without sleeping.
fn is_double_click(prev: Option<(u16, u16, Instant)>, pos: (u16, u16), now: Instant) -> bool {
    matches!(prev, Some((_px, py, t)) if py == pos.1 && now.saturating_duration_since(t) <= DOUBLE_CLICK)
}

/// Whether a path names a markdown file (by extension, case-insensitive).
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}

/// How many rows one rendered line occupies under ratatui's word wrapper (`Wrap{trim:false}`)
/// at `width` columns: greedy word packing — fill the row with space-separated words until the
/// next one doesn't fit, then wrap; a word wider than the row is broken across rows. A plain
/// `ceil(width/col)` undercounts this (words rarely pack flush to the column), which is what
/// would make the bottom of wrapped prose unreachable via the scroll clamp. Char counts stand
/// in for display width — close enough for the clamp, and the caller floors with the
/// all-columns char-wrap so it never undershoots.
fn wrapped_rows(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut rows = 1usize;
    let mut col = 0usize;
    for (i, word) in text.split(' ').enumerate() {
        let wl = word.chars().count();
        let sep = usize::from(i > 0);
        if col != 0 && col + sep + wl > width {
            rows += 1; // doesn't fit → start a new row
            col = 0;
        }
        if col == 0 {
            // word starts a fresh row; a word wider than the row breaks across full rows
            let extra = wl.saturating_sub(1) / width;
            rows += extra;
            col = wl - extra * width;
        } else {
            col += sep + wl;
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{DOUBLE_CLICK, is_double_click, wrapped_rows};
    use std::time::Instant;

    #[test]
    fn is_double_click_requires_the_same_row_within_the_window() {
        let t0 = Instant::now();
        let within = t0 + DOUBLE_CLICK / 2;
        let after = t0 + DOUBLE_CLICK * 2;
        // Same cell, inside the window → double-click.
        assert!(is_double_click(Some((5, 5, t0)), (5, 5), within));
        // Same ROW, different column, inside the window → still a double-click. A tree row is
        // one node end-to-end, and a touchpad double-tap often lands a column or two apart, so
        // requiring the exact cell would drop legitimate double-taps.
        assert!(is_double_click(Some((5, 5, t0)), (40, 5), within));
        // Too slow → not a double-click.
        assert!(!is_double_click(Some((5, 5, t0)), (5, 5), after));
        // A different ROW → not a double-click (it would target a different node).
        assert!(!is_double_click(Some((5, 5, t0)), (5, 6), within));
        // No previous click → never a double-click.
        assert!(!is_double_click(None, (5, 5), within));
    }

    #[test]
    fn wrapped_rows_counts_word_wrapping_not_just_char_wrapping() {
        // Four width-6 words in a 10-col pane pack one per row → 4 rows, even though the
        // 27-column line char-wraps to only 3. The scroll clamp must use the larger count.
        assert_eq!(wrapped_rows("aaaaaa aaaaaa aaaaaa aaaaaa", 10), 4);
        // A single over-long word is broken like char wrapping.
        assert_eq!(wrapped_rows(&"x".repeat(100), 25), 4);
        // Words that pack flush share rows.
        assert_eq!(wrapped_rows("ab cd ef", 8), 1); // "ab cd ef" = 8 cols, fits exactly
        // Short / empty / zero-width are one row.
        assert_eq!(wrapped_rows("hello", 80), 1);
        assert_eq!(wrapped_rows("", 80), 1);
        assert_eq!(wrapped_rows("anything", 0), 1);
    }
}
