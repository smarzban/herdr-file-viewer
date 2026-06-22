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

use crate::git::{Baseline, Status};
use crate::intent::Intent;
use crate::presenter::{Focus, PaneGeometry, ViewState};
use crate::tree::{Node, NodeKind, TreeModel};
use crate::view_policy::{FileDescriptor, ViewMode, applicable_modes, default_mode};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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
/// expand/collapse; a file hands off to the editor).
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
    /// Raw unified diff text for one repo-root-relative path against `baseline` (AC-9).
    fn diff(&self, rel_path: &Path, baseline: Baseline) -> String;
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
/// `Ok(false)` for an off-screen hand-off (e.g. a new herdr pane). Behind a trait so the
/// controller never edits or even spawns directly — and tests launch nothing.
pub trait EditorHandoff {
    fn open(&mut self, file: &Path) -> io::Result<bool>;
}

/// The injected components the controller orchestrates.
pub struct Components {
    /// Shared (`Arc`) because both the controller (status / changed-set) and the render
    /// worker (diff, off the input thread) query git.
    pub git: Arc<dyn GitService>,
    pub content: Box<dyn ContentProvider>,
    pub editor: Box<dyn EditorHandoff>,
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
        Effects { redraw: true, ..Default::default() }
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

/// The interaction orchestrator and the ephemeral session state.
pub struct Controller {
    root: PathBuf,
    is_git_repo: bool,
    baseline: Baseline,
    show_ignored: bool,
    changed_only: bool,
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
    /// True while the left button is held after pressing on the divider (a resize drag), so the
    /// release is treated as the end of the drag, not a click.
    dragging_divider: bool,
}

impl Controller {
    /// Build the controller rooted at `root`. When `is_git_repo`, the initial working-tree
    /// status (tree markers, AC-7) and the changed-set against `baseline` are loaded from
    /// git; otherwise the viewer is a plain browser (AC-26). The initial selection's content
    /// is rendered so the first frame is populated.
    pub fn new(root: PathBuf, is_git_repo: bool, baseline: Baseline, components: Components) -> Self {
        let Components { git, content, editor } = components;
        // The Content Renderer (and the diff query it needs) live on a worker thread; the
        // controller talks to it over a job channel and reads finished renders off a result
        // channel (AC-23). The worker exits when the job sender (held by the controller) is
        // dropped.
        let (job_tx, job_rx) = mpsc::channel::<RenderJob>();
        let (result_tx, result_rx) = mpsc::channel::<(u64, RenderResult)>();
        let worker_git = Arc::clone(&git);
        std::thread::spawn(move || {
            while let Ok(mut job) = job_rx.recv() {
                // Collapse any backlog: under rapid navigation only the most recent selection
                // matters, so skip superseded jobs rather than render each in turn.
                while let Ok(newer) = job_rx.try_recv() {
                    job = newer;
                }
                // The diff is read here, off the input thread, so a large/slow diff never
                // blocks input (AC-23). Other modes don't need git.
                let raw_diff = if job.mode == ViewMode::Diff && job.is_git {
                    job.rel.as_deref().map(|rel| worker_git.diff(rel, job.baseline))
                } else {
                    None
                };
                // A renderer panic must not kill the worker (rendering would stop forever) nor
                // fire the global panic hook from this thread (it would reset the main thread's
                // terminal mid-session). Contain it and surface a placeholder instead.
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

        let mut ctrl = Controller {
            tree: TreeModel::new(root.clone()),
            root,
            is_git_repo,
            baseline,
            show_ignored: false,
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
            job_tx,
            result_rx,
            latest_seq: 0,
            geom: PaneGeometry::default(),
            last_click: None,
            dragging_divider: false,
        };
        ctrl.refresh_git_state();
        ctrl.dispatch_render();
        ctrl
    }

    // ---- state accessors (used by the Presenter wiring and tests) ----------------------

    pub fn show_ignored(&self) -> bool {
        self.show_ignored
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
    pub fn tree(&self) -> &TreeModel {
        &self.tree
    }
    pub fn content(&self) -> &Text<'static> {
        &self.content
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
        ViewState {
            nodes,
            selected,
            content: self.content.clone(),
            notices: self.notices(),
            focus: self.focus,
            width: self.width,
            content_scroll: self.content_scroll,
            content_hscroll: self.content_hscroll,
            wrap,
            split_pct: self.split_pct,
            zoomed: self.zoomed,
        }
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
                matches!(self.effective_mode(&n.path), ViewMode::RenderedMarkdown | ViewMode::RawContent)
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
        match intent {
            Intent::NavUp => self.navigate(-1),
            Intent::NavDown => self.navigate(1),
            Intent::Expand => self.expand(),
            Intent::Collapse => self.collapse(),
            Intent::ToggleIgnore => self.toggle_ignore(),
            Intent::ToggleChangedOnly => self.toggle_changed_only(),
            Intent::ToggleBaseline => self.toggle_baseline(),
            Intent::CycleView => self.cycle_view(),
            Intent::OpenInEditor => self.open_in_editor(),
            Intent::ToggleFocus => self.toggle_focus(),
            Intent::ShrinkTree => self.resize_split(-(SPLIT_STEP as i16)),
            Intent::GrowTree => self.resize_split(SPLIT_STEP as i16),
            Intent::ToggleWrap => self.toggle_wrap(),
            Intent::ToggleZoom => self.toggle_zoom(),
            Intent::Refresh => self.refresh(),
            Intent::Close => Effects { quit: true, ..Default::default() },
        }
    }

    /// Map a mouse event to a state change. Mouse is additive to the keyboard-first design
    /// (AC-18). A `Shift`+mouse event is left untouched so the terminal's own selection/copy
    /// still works (herdr reserves Shift+mouse for exactly that). Selection/activation happen
    /// on button *release*, so a divider drag is never mistaken for a click.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> Effects {
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
                // A press on the divider begins a resize drag; otherwise wait for the release.
                self.dragging_divider = matches!(self.hit_test(col, row), MouseRegion::Divider);
                Effects::noop()
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_divider => {
                self.resize_split_to_col(col)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if std::mem::take(&mut self.dragging_divider) {
                    return Effects::noop(); // end of a resize drag, not a click
                }
                self.handle_click(col, row)
            }
            _ => Effects::noop(),
        }
    }

    /// A completed left-click: select the tree row it landed on (or focus the content pane). A
    /// double-click activates — a directory toggles expand/collapse, a file hands off to the
    /// editor (mirrors the `e` key).
    fn handle_click(&mut self, col: u16, row: u16) -> Effects {
        let region = self.hit_test(col, row);
        let now = Instant::now();
        let double = is_double_click(self.last_click, (col, row), now);
        self.last_click = Some((col, row, now));
        match region {
            MouseRegion::TreeRow(idx) => {
                if idx >= self.tree.visible_nodes().len() {
                    return Effects::noop(); // the empty area below the last node — inert
                }
                self.action_notice = None;
                self.focus = Focus::Tree;
                self.tree.set_cursor(idx);
                self.dispatch_render(); // selection changed → re-render the content pane
                if double
                    && let Some(node) = self.tree.selected()
                {
                    return match node.kind {
                        NodeKind::Dir => {
                            if node.expanded {
                                self.tree.collapse(&node.path);
                            } else {
                                self.tree.expand(&node.path);
                            }
                            Effects::redraw()
                        }
                        NodeKind::File => self.open_in_editor(),
                    };
                }
                Effects::redraw()
            }
            MouseRegion::Content => {
                self.focus = Focus::Content;
                Effects::redraw()
            }
            MouseRegion::Divider | MouseRegion::Outside => Effects::noop(),
        }
    }

    /// Scroll the pane under the cursor: the content pane scrolls vertically; over the tree
    /// (which does not scroll) the wheel moves the selection — the closest equivalent.
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

    /// Horizontal wheel / trackpad swipe over the content pane scrolls it sideways, like the
    /// `←`/`→` keys (for unwrapped long lines). Over the tree it does nothing — the tree has no
    /// horizontal scroll. `scroll_content_h` clamps to `[0, widest − viewport]` (zero while
    /// wrapping), so it is inert on wrapped prose, matching the keys.
    fn hscroll_at(&mut self, col: u16, row: u16, delta: i32) -> Effects {
        if matches!(self.hit_test(col, row), MouseRegion::Content) {
            self.scroll_content_h(delta)
        } else {
            Effects::noop()
        }
    }

    /// During a divider drag, set the split so the divider tracks the cursor column — clamped
    /// like the keyboard resize so neither column can collapse.
    fn resize_split_to_col(&mut self, col: u16) -> Effects {
        if self.geom.area_width == 0 {
            return Effects::noop();
        }
        let tree_w = col.saturating_sub(self.geom.area_x) as i32;
        let pct = (tree_w * 100 / self.geom.area_width as i32)
            .clamp(SPLIT_MIN as i32, SPLIT_MAX as i32);
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
        if let Some(t) = self.geom.tree_inner
            && t.contains(Position { x: col, y: row })
        {
            // The row index may exceed the node count (the empty area below the last node): the
            // click handler treats that as inert, while the wheel still scrolls the column.
            return MouseRegion::TreeRow((row - t.y) as usize);
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
        self.rendered_line_count().saturating_sub(self.content_height)
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
        let count = if self.effective_wrap() {
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
        let widest = self.content.lines.iter().map(|l| l.width()).max().unwrap_or(0);
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

    fn toggle_ignore(&mut self) -> Effects {
        self.show_ignored = !self.show_ignored;
        self.tree.set_show_ignored(self.show_ignored);
        // Revealing/hiding ignored entries can shift which node the cursor lands on, so the
        // content pane must re-render for the (possibly) new selection — otherwise it shows a
        // file that is no longer highlighted.
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
        self.dispatch_render(); // a diff is relative to the baseline, so it must re-render
        Effects::redraw()
    }

    fn cycle_view(&mut self) -> Effects {
        let Some(node) = self.tree.selected() else { return Effects::noop() };
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
        let Some(node) = self.tree.selected() else { return Effects::noop() };
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
                Effects { redraw: true, clear: true, ..Default::default() }
            }
            Ok(false) => Effects::redraw(), // off-screen hand-off (new pane) — no takeover
            Err(e) => {
                self.action_notice = Some(format!("Could not open editor: {e}"));
                // The hand-off may have suspended the terminal before failing, so force a full
                // repaint to recover from any partial screen state.
                Effects { redraw: true, clear: true, ..Default::default() }
            }
        }
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
        self.focus = if self.zoomed { Focus::Content } else { Focus::Tree };
        Effects::redraw()
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

        let Some(node) = self.tree.selected() else { return self.clear_content() };
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
        self.rel(path).map(|rel| self.changed.contains_key(&rel)).unwrap_or(false)
    }

    /// `path` made relative to the tree root (how git keys its maps); `None` if outside it.
    fn rel(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.root).ok().map(Path::to_path_buf)
    }
}

/// Where a mouse cell falls in the drawn layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseRegion {
    TreeRow(usize),
    Content,
    Divider,
    Outside,
}

/// Two left-clicks at the same cell within [`DOUBLE_CLICK`] are a double-click. Pure over its
/// timestamps so the timing rule is unit-testable without sleeping.
fn is_double_click(prev: Option<(u16, u16, Instant)>, pos: (u16, u16), now: Instant) -> bool {
    matches!(prev, Some((px, py, t)) if (px, py) == pos && now.saturating_duration_since(t) <= DOUBLE_CLICK)
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
    fn is_double_click_requires_same_cell_within_the_window() {
        let t0 = Instant::now();
        let within = t0 + DOUBLE_CLICK / 2;
        let after = t0 + DOUBLE_CLICK * 2;
        // Same cell, inside the window → double-click.
        assert!(is_double_click(Some((5, 5, t0)), (5, 5), within));
        // Too slow → not a double-click.
        assert!(!is_double_click(Some((5, 5, t0)), (5, 5), after));
        // A different cell → not a double-click (even if fast).
        assert!(!is_double_click(Some((5, 5, t0)), (6, 5), within));
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
