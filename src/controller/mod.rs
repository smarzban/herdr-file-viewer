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
//!
//! The `impl Controller` surface is split across feature submodules (each `use super::*` and adds
//! its own `pub(super)` methods to the same `Controller`): `help`, `finder`, `picker`, `infile`
//! (the bottom prompt), `mouse` (column/tree pointer handling), and `git_apply`. This module keeps
//! the type definitions, construction, the intent/poll/render core, and tree-navigation intents.

mod finder;
mod git_apply;
mod help;
mod infile;
mod lineselect;
mod mouse;
mod picker;

use crate::finder::FinderState;
use crate::git::{Baseline, Status};
use crate::help::{HelpSection, HelpSectionState, HelpState};
use crate::herdr::HerdrCli;
use crate::infile::{PromptMode, PromptState, SearchState};
use crate::intent::Intent;
use crate::picker::PickerState;
use crate::presenter::{
    ContentSearch, FinderView, Focus, HelpView, PaneGeometry, PickerRowView, PickerView, ViewState,
};
use crate::render::{Prepared, Renderers};
use crate::root::Resolved;
use crate::tree::{Node, NodeKind, TreeModel};
use crate::update::{self, UpdateState, Version};
use crate::view_policy::{FileDescriptor, ViewMode, applicable_modes, default_mode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lineselect::LineSelectState;
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
/// Wall-clock bound for the synchronous What's New markdown render in `open_help`. The render runs
/// on the input thread (the design settled on prerender-at-open), so it must be bounded well within
/// the AC-22 responsiveness budget — far tighter than the shared 5s content `RENDER_TIMEOUT`, which
/// would let a wedged `glow` freeze input for up to 5s. On timeout the existing render path falls
/// back to plain text + a notice (AC-15). This reconciles prerender-at-open with AC-22.
const HELP_RENDER_TIMEOUT: Duration = Duration::from_millis(250);
/// The help overlay's self-operating key-hints footer (AC-11) — at minimum how to switch sections
/// and how to close. Carried in `HelpView` so the Presenter stays mode-agnostic; matches the keys
/// `handle_help_key` actually handles (Tab/←→ switch · digits/1-9 also; Esc/q/`?` close).
const HELP_FOOTER_HINT: &str = "Tab/←→ switch · 1-9 jump · j/k scroll · Esc/q/? close";

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

/// The outcome of an editor hand-off (AC-19). Distinguishing the failure modes lets the
/// controller word its user-facing notice correctly:
/// - [`EditorOutcome::TookOver`] — the editor ran and drew over the terminal; the run loop
///   must force a full repaint afterwards.
/// - [`EditorOutcome::NoTakeover`] — the hand-off returned without a terminal takeover (no
///   repaint/refresh needed); used by stubs and any future no-op path.
/// - [`EditorOutcome::NotLaunched`] — the editor process could not be started (e.g. missing
///   binary, no `$EDITOR` configured). The terminal was not handed over.
/// - [`EditorOutcome::NonZeroExit`] — the editor launched and ran, then exited with a
///   non-zero status. The hand-off took place; only the exit code signals a problem.
///
/// Behind the trait so the controller never edits or even spawns directly — and tests
/// launch nothing.
pub enum EditorOutcome {
    /// The editor took the terminal (it ran, with any exit status). The run loop forces a
    /// full repaint to recover from the screen the editor drew over.
    TookOver,
    /// The hand-off returned without a terminal takeover — no repaint or git refresh is
    /// needed. (Used by test stubs that don't really launch an editor, and any future no-op
    /// hand-off path.)
    NoTakeover,
    /// The editor process could not be started — nothing ran. `reason` is a short,
    /// user-facing message (e.g. "no editor configured (set $EDITOR)").
    NotLaunched(String),
    /// The editor launched and ran, then exited with a non-zero status. `detail` is a
    /// short, user-facing description of the status (e.g. "exit status: 1").
    NonZeroExit(String),
}

/// Why the content pane is empty — selects the empty-state copy shown instead of a blank
/// pane. A directory has nothing to render; an empty/zero-match tree (no files, or
/// a filter — changed-only / gitignore / hidden — that matched nothing) leaves no selection.
/// The label is a short, first-party, control-byte-free string rendered through the normal
/// content path (no AC-27 sanitization needed for static first-party text).
enum EmptyReason {
    /// A directory is selected — it has no file content to render.
    Directory,
    /// The tree is empty or a filter matched no files (no selection at all).
    NoFiles,
}

impl EmptyReason {
    /// The empty-state guidance copy for this case.
    fn label(self) -> &'static str {
        match self {
            EmptyReason::Directory => "Directory — select a file to view",
            EmptyReason::NoFiles => "No files",
        }
    }
}

/// Hand the selected file to an external editor (AC-19). Behind a trait so tests launch
/// nothing; see [`EditorOutcome`] for the distinguished failure modes.
pub trait EditorHandoff {
    fn open(&mut self, file: &Path) -> EditorOutcome;
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
    /// again per re-root. `Fn` (not `FnOnce`) because a re-root re-invokes it.
    /// ADR-0004.
    pub providers: Box<dyn Fn(&Resolved) -> RootProviders>,
    pub editor: Box<dyn EditorHandoff>,
    pub clipboard: Box<dyn Clipboard>,
    /// The external renderer commands used for the in-app help overlay's What's New section
    /// (render CHANGELOG_MD as markdown via the same renderer the content pane uses).
    /// `None` ⇒ the markdown renderer is absent; `render::render` falls back to plain text
    /// and a notice (AC-15) — the same fallback it applies for any missing renderer.
    pub renderers: Option<Renderers>,
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

/// The single open modal overlay, or [`Modal::None`] when the columns have focus. Collapses what
/// were four parallel `Option<…State>` fields (picker / finder / prompt / help) into one value, so
/// "at most one modal is open at a time" is enforced by the type rather than by hand: opening any
/// modal (`self.modal = Modal::Picker(…)`) implicitly closes whatever else was open, and a single
/// `Modal::None` closes the lot (the old per-field teardown in [`re_root`](Controller::re_root)).
/// The variants:
/// - `Picker` — the worktree picker (AC-1); a re-root closes it (its candidate list is old-root).
/// - `Finder` — the go-to-file finder (AC-1), opened by `f`; closed by confirm/cancel/re-root.
/// - `Prompt` — the in-file-nav bottom prompt (go-to-line / search). While open the run loop routes
///   raw keys to `handle_prompt_key` and the mouse is inert, so the selection can't change beneath it.
/// - `Help` — the help overlay (AC-1, AC-6), opened by `?`; dismissed by Esc/`q`. While open,
///   `handle()`/`handle_mouse()` return early (AC-N4).
/// - `LineSelect` — the copy-line-reference selection (a content-pane marker, not a popup). While
///   active `handle()` returns early — the run loop routes keys to its own handler (like the
///   prompt/finder) — and a re-root / exit resets it to `Modal::None` (no clipboard touch here).
enum Modal {
    None,
    Picker(PickerState),
    Finder(FinderState),
    Prompt(PromptState),
    Help(HelpState),
    LineSelect(LineSelectState),
}

impl Modal {
    /// The picker state when the picker is the open modal, else `None` — the enum's `Option<&_>`
    /// view, so call sites read like the old `self.modal.picker()`.
    fn picker(&self) -> Option<&PickerState> {
        match self {
            Modal::Picker(s) => Some(s),
            _ => None,
        }
    }
    fn picker_mut(&mut self) -> Option<&mut PickerState> {
        match self {
            Modal::Picker(s) => Some(s),
            _ => None,
        }
    }
    fn finder(&self) -> Option<&FinderState> {
        match self {
            Modal::Finder(s) => Some(s),
            _ => None,
        }
    }
    fn finder_mut(&mut self) -> Option<&mut FinderState> {
        match self {
            Modal::Finder(s) => Some(s),
            _ => None,
        }
    }
    fn prompt(&self) -> Option<&PromptState> {
        match self {
            Modal::Prompt(s) => Some(s),
            _ => None,
        }
    }
    fn prompt_mut(&mut self) -> Option<&mut PromptState> {
        match self {
            Modal::Prompt(s) => Some(s),
            _ => None,
        }
    }
    fn help(&self) -> Option<&HelpState> {
        match self {
            Modal::Help(s) => Some(s),
            _ => None,
        }
    }
    fn help_mut(&mut self) -> Option<&mut HelpState> {
        match self {
            Modal::Help(s) => Some(s),
            _ => None,
        }
    }
    fn line_select(&self) -> Option<&LineSelectState> {
        match self {
            Modal::LineSelect(s) => Some(s),
            _ => None,
        }
    }
    // Used by the line-select key handler in T-5; the state exists here from T-3 so the accessor
    // pair mirrors picker/finder/prompt/help.
    #[allow(dead_code)]
    fn line_select_mut(&mut self) -> Option<&mut LineSelectState> {
        match self {
            Modal::LineSelect(s) => Some(s),
            _ => None,
        }
    }
}

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
    /// The path of the file whose content is currently displayed in the pane — the title's
    /// source of truth, so the border label switches in lockstep with the body. `None`
    /// while no file's content has landed yet (launch, a re-root, or a directory/empty tree
    /// selection — the title then falls back to the selected node's name or "Content"). Updated
    /// only by [`poll`](Self::poll) when a render result is applied, and cleared by
    /// [`clear_content`](Self::clear_content); a render in flight does NOT update it ahead of the
    /// body, so the pane never shows a new file's title over the old file's body.
    content_path: Option<PathBuf>,
    /// True iff an off-thread render for a file is in flight — set when [`dispatch_render`]
    /// sends a `RenderJob`, cleared when [`poll`](Self::poll) applies the matching result (and
    /// by [`clear_content`](Self::clear_content), which sends no job). The Presenter uses this
    /// to pick a neutral title while the body shows the loading placeholder, so the title never
    /// jumps to a freshly-selected file before its content arrives.
    content_rendering: bool,
    /// A transient notice from the last action (e.g. an editor-launch failure); shown until
    /// the next intent is handled.
    action_notice: Option<String>,
    git: Arc<dyn GitService>,
    editor: Box<dyn EditorHandoff>,
    clipboard: Box<dyn Clipboard>,
    /// The provider factory (ADR-0004), kept so a re-root can rebuild the root-bound providers
    /// (Git Service + Content Renderer) against the new root.
    providers: Box<dyn Fn(&Resolved) -> RootProviders>,
    /// The external renderer commands for the help overlay's What's New section.
    /// Built from `Components::renderers` at construction; `None` ⇒ fallback.
    renderers: Renderers,
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
    /// The single open modal overlay (picker / finder / prompt / help), or [`Modal::None`] when the
    /// columns have focus. One field instead of four parallel `Option`s, so mutual exclusion is
    /// type-enforced — see [`Modal`]. A re-root resets it to `Modal::None` (the old symmetric
    /// per-modal teardown), since a re-root invalidates the picker/finder old-root candidate lists
    /// and must not strand a prompt/help over the freshly re-rooted tree.
    modal: Modal,
    /// The herdr query channel for the agent-active overlay (AC-3), injected post-construction
    /// via [`set_host`](Self::set_host). `None` until then ⇒ a git-only picker (AC-15).
    /// Session-level — survives a re-root unchanged.
    herdr: Option<Box<dyn HerdrCli>>,
    /// The viewer's own herdr workspace id (the agent-overlay's Tier-1 hint). Session-level —
    /// survives a re-root unchanged.
    our_workspace_id: Option<String>,
    /// The launch base-branch hint (the branch a worktree forked from), carried into a re-root's
    /// re-resolution so the post-switch Base-mode baseline can recover the common shared-base case.
    /// Session-level — survives a re-root unchanged: the herdr per-worktree
    /// hint isn't available cross-worktree, so the launch hint is the best shared-base recovery.
    base_branch: Option<String>,
    /// The current git branch (e.g. `"main"`, `"feat/x"`), shown on the tree's bottom border.
    /// `None` outside a repo or on a detached HEAD. Computed ONCE from the freshly-resolved root
    /// at construction and on each re-root and cached here — never queried per-frame, since the
    /// branch can only change by a re-root, not by navigation.
    current_branch: Option<String>,
    /// A queued go-to-line jump awaiting its re-render: `(render seq, 1-based source line)`. Set when
    /// `:` confirms in a **transformed** view (RenderedMarkdown / Diff / FullDiff) — the view is
    /// switched to the source-mapped content view and the jump can't run until that render lands, so
    /// it is queued against the dispatched render's seq and applied by [`poll`] (AC-7). `None` when no
    /// jump is pending; superseded (cleared) by any newer render dispatch.
    pending_goto: Option<(u64, usize)>,
    /// The seq of the render result currently held in [`content`](Self::content), bumped by [`poll`]
    /// each time it applies a result. Equal to `latest_seq` exactly when the latest dispatched render
    /// has landed; lagging while one is in flight. Lets a synchronous go-to-line jump tell "content is
    /// current" from "a render is still coming" — so `:N` only jumps in-place when the source-mapped
    /// content is actually applied, and otherwise queues against the in-flight render (AC-3/AC-7).
    applied_seq: u64,
    /// Live incremental-search state: the most-recently-typed query, the matches it produced,
    /// and which match is current. `None` until the first keystroke in a Search prompt; `Some`
    /// (even with empty matches) once typing begins. Enter-commit retains it so `n`/`N` can
    /// navigate the committed matches (AC-14). Cleared to `None` on Esc-cancel (AC-17), on
    /// opening a new search (AC-20), and at the top of `dispatch_render` so any displayed-content
    /// change (file-select, view-cycle, baseline-toggle, refresh, re-root, etc.) wipes a committed
    /// search + its highlighting (AC-20). The incremental-typing path (`refresh_search`) does NOT
    /// call `dispatch_render`, so live typing is never wiped by that clear.
    search: Option<SearchState>,
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
            renderers,
        } = components;
        // Materialise the renderers: `None` ⇒ a Renderers with an absent markdown command so
        // `render::render` falls back to plain text + a notice (AC-15) without extra branching.
        let renderers = renderers.unwrap_or_else(|| Renderers {
            markdown: vec!["herdr-no-such-markdown-renderer".into()],
            diff: vec!["herdr-no-such-diff-renderer".into()],
            full_diff: vec!["herdr-no-such-full-diff-renderer".into()],
            syntax: vec!["herdr-no-such-syntax-renderer".into()],
            timeout: std::time::Duration::from_millis(100),
        });
        let RootProviders { git, content } = providers(&resolved);
        let root = resolved.root.clone();
        let is_git_repo = resolved.is_git_repo;
        // The launch base-branch hint is session-level — recorded once here and carried across
        // re-roots (F). It is `None` outside a repo / when herdr gave no hint.
        let base_branch = resolved.base_branch.clone();
        // The current branch for the tree's bottom-border title: queried once here from
        // the resolved repo root (never per-frame), `None` outside a repo / on detached HEAD.
        let current_branch = resolved
            .repo_root
            .as_deref()
            .and_then(crate::git::current_branch);
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
            content_path: None,
            content_rendering: false,
            action_notice: None,
            git,
            editor,
            clipboard,
            providers,
            renderers,
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
            modal: Modal::None,
            pending_goto: None,
            applied_seq: 0,
            search: None,
            herdr: None,
            our_workspace_id: None,
            base_branch,
            current_branch,
        };
        ctrl.refresh_git_state();
        ctrl.dispatch_render();
        ctrl
    }

    /// Spawn the off-thread render worker that owns `git` (for the diff query) and `content`
    /// (the Content Renderer), returning the job sender and result receiver the controller keeps
    /// (AC-23). The worker runs until the job sender is dropped — so `new` spawns it once, and a
    /// re-root spawns a fresh one and drops the old sender to retire the old worker. The loop
    /// body is the same one `new` used inline before it was extracted; behavior is unchanged.
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
                // Contain a panic anywhere in the per-job work — the diff read AND the render —
                // so the worker thread always survives to send a result. The diff is read here,
                // off the input thread, so a large/slow diff never blocks input (AC-23); other
                // modes don't need git, the full-file diff view asks git for whole-file context,
                // the compact diff uses git's default. The diff read MUST sit inside this guard:
                // if `git.diff` panicked the worker would die without sending, no result would
                // ever reach `poll`, and `content_rendering` would never clear — stranding the
                // pane on the `Rendering…` placeholder for the rest of the session.
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let raw_diff =
                        if matches!(job.mode, ViewMode::Diff | ViewMode::FullDiff) && job.is_git {
                            let full = job.mode == ViewMode::FullDiff;
                            job.rel
                                .as_deref()
                                .map(|rel| git.diff(rel, job.baseline, full))
                        } else {
                            None
                        };
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
        // Carry the launch base-branch hint into the re-resolution: herdr's
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
        // Recompute the cached branch for the new root's bottom-border title. Cheap and
        // synchronous: a single `git rev-parse` against the already-resolved repo root, done once
        // per re-root (not per-frame). `None` when the new root is outside a repo / detached.
        self.current_branch = resolved
            .repo_root
            .as_deref()
            .and_then(crate::git::current_branch);

        // Reset navigation/view state (AC-13). The picker is closed on a switch (AC-13 "picker
        // is closed"); `herdr`/`our_workspace_id` are session-level and deliberately left intact.
        self.focus = Focus::Tree;
        self.zoomed = false;
        self.content_scroll = 0;
        self.content_hscroll = 0;
        self.tree_hscroll = 0;
        self.overrides.clear();
        // The old root's rendered content is invalid under the new root — drop the displayed-file
        // path so the title falls back to a neutral label until the new selection's render lands
        //. `dispatch_render` below sets `content_rendering` and the loading placeholder.
        self.content_path = None;
        self.action_notice = None;
        self.changed = BTreeMap::new();
        // Close whatever modal is open (one assignment, since `modal` is now a single value). A
        // re-root only fires via picker-confirm, so in practice it's the picker being torn down —
        // but a re-root also invalidates the finder's old-root candidate list and must not strand a
        // prompt/help over the freshly re-rooted tree, so closing the lot here stays correct for any
        // future re-root trigger.
        self.modal = Modal::None;
        self.last_click = None;

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
    /// The current tree root. Exposed so tests can assert re-root results.
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
    /// can draw it and tests can assert the rows / pre-selected cursor.
    pub fn picker(&self) -> Option<&PickerState> {
        self.modal.picker()
    }

    /// Whether a re-root's off-thread status/changed-set fetch is still pending (not yet applied
    /// by [`poll`]). Exposed so a test can assert that a synchronous refresh drops the pending
    /// async fetch, so a stale async result cannot later clobber the freshly-refreshed state.
    pub fn status_refresh_pending(&self) -> bool {
        self.status_rx.is_some()
    }

    /// The session-level launch base-branch hint, carried across re-roots.
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
    ///
    /// Also re-clamps the finder's stored horizontal scroll to the maximum the Presenter just
    /// measured (`finder_max_hscroll`) — mirroring how [`set_content_viewport`](Self::set_content_viewport)
    /// re-clamps `content_hscroll`. `scroll_right` is monotonic, so over-scrolling right would
    /// otherwise leave the offset parked past the widest row, making the first few left presses
    /// appear to do nothing until the overshoot burned down.
    pub fn set_pane_geometry(&mut self, geom: PaneGeometry) {
        // Read the measured maxima before `geom` is moved into `self.geom`, then clamp each modal's
        // stored hscroll to it — both Expand (finder/picker scroll_right) are monotonic, so without
        // this an over-scroll right parks the offset past the widest row.
        let finder_max_hscroll = geom.finder_max_hscroll;
        let picker_max_hscroll = geom.picker_max_hscroll;
        // The help body's measured viewport height and its WRAPPED row total — used to enforce the
        // scroll bottom-bound that was deferred (AC-9): `scroll_by` only saturates at 0, so the lower
        // clamp is applied here against the live geometry, exactly as the finder/picker re-clamp their
        // hscroll. The body is drawn with `Paragraph::wrap`, so its offset is in wrapped rows — clamp
        // against the wrapped total the Presenter measured (`help_body_rows`), NOT raw `lines.len()`,
        // or a long changelog's last entries stay unreachable (mirrors the content pane's clamp).
        let help_body_height = geom.help_body_height;
        let help_body_rows = geom.help_body_rows;
        self.geom = geom;
        if let Some(finder) = self.modal.finder_mut() {
            finder.clamp_hscroll(finder_max_hscroll);
        }
        if let Some(picker) = self.modal.picker_mut() {
            picker.clamp_hscroll(picker_max_hscroll);
        }
        if let Some(help) = self.modal.help_mut() {
            help.clamp_scroll(help_body_rows, help_body_height);
        }
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
            // The tree's top-border title is the root directory basename; the bottom is the cached
            // current branch. The basename is empty only for a filesystem-root `/`, where
            // the Presenter falls back to "Files".
            root_name: self
                .root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            branch: self.current_branch.clone(),
            prompt: self.bottom_line(),
            // the content pane's border title. `content_path` is the displayed content's
            // file (set by `poll` when a render lands, cleared by `clear_content`/re-root), so the
            // title switches in lockstep with the body — it never jumps to a freshly-selected file
            // before that file's content arrives. `None` while no file's content has landed (launch,
            // re-root, or a directory/empty selection); the Presenter then falls back to the selected
            // node's name (a directory) or "Content". `content_rendering` tells the Presenter a
            // render is in flight so the `None` fallback doesn't pick up the new (still-loading)
            // selection's name and re-introduce the title-ahead-of-body bug.
            content_title: self
                .content_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned()),
            content_rendering: self.content_rendering,
            // Populate the highlight overlay from the committed/live search state so the Presenter
            // overlays matches via highlight::apply. `None` when no search is active → draw_content
            // falls through to `state.content.clone()`, byte-identical to the prior path.
            search: self.search.as_ref().map(|s| ContentSearch {
                matches: s.matches.clone(),
                current: s.current,
            }),
            help: self.help_view(),
        }
    }

    /// Build the bottom-line string shown in `ViewState.prompt`. Single source of truth for all
    /// three cases: an open prompt (go-to-line or search while typing), a committed search (prompt
    /// closed but `self.search` is Some), and nothing active (returns `None`).
    ///
    /// Priority:
    ///   1. Open prompt → label + query + (for Search) live match count.
    ///   2. Committed search (no open prompt) → query + count + n/N/Esc hint.
    ///   3. Neither → `None` (Presenter draws nothing on the bottom row).
    fn bottom_line(&self) -> Option<String> {
        if let Some(p) = self.modal.prompt() {
            match p.mode {
                crate::infile::PromptMode::GoToLine => {
                    Some(format!("Go to line: {}", p.input.query()))
                }
                crate::infile::PromptMode::Search => {
                    let q = p.input.query();
                    let count = self.search_count_fragment(q);
                    Some(format!("Search: {q}{count}"))
                }
            }
        } else {
            self.search_status_line()
        }
    }

    /// The count/hint fragment appended after the query while a search is active.
    ///
    /// - Empty query → empty string (nothing appended; label reads `Search: `).
    /// - Non-empty, 0 matches → ` (no matches)`.
    /// - Non-empty, ≥1 match → ` ({current+1}/{total})`.
    fn search_count_fragment(&self, query: &str) -> String {
        if query.is_empty() {
            return String::new();
        }
        match &self.search {
            None => String::new(),
            Some(s) if s.matches.is_empty() => " (no matches)".to_owned(),
            Some(s) => format!(" ({}/{})", s.current + 1, s.matches.len()),
        }
    }

    /// Build the committed-search status + hint bar shown while a search is committed (prompt
    /// closed, `self.search` is `Some`). Returns `None` when no committed search is active.
    ///
    /// Format:
    /// - ≥1 match: `Search: {query} ({current+1}/{total}) · n next · N prev · Esc clear`
    /// - 0 matches: `Search: {query} (no matches) · Esc clear`
    fn search_status_line(&self) -> Option<String> {
        let s = self.search.as_ref()?;
        let q = &s.query;
        let line = if s.matches.is_empty() {
            format!("Search: {q} (no matches) · Esc clear")
        } else {
            format!(
                "Search: {q} ({}/{}) · n next · N prev · Esc clear",
                s.current + 1,
                s.matches.len()
            )
        };
        Some(line)
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
        if self.modal.picker().is_some() {
            return self.handle_picker_intent(intent);
        }
        // The finder is modal too: while it is open the run loop (app.rs) routes raw keys to
        // `handle_finder_key`, so `handle` should not be reached. Guard structurally anyway —
        // symmetric with the picker guard above — so a future or test caller can't leak an intent
        // to the tree or open a second modal beneath the finder overlay.
        if self.modal.finder().is_some() {
            return Effects::noop();
        }
        // A prompt is modal too: the run loop routes raw keys to handle_prompt_key while it is open, so
        // handle() should not be reached. Guard structurally — symmetric with the finder guard.
        if self.modal.prompt().is_some() {
            return Effects::noop();
        }
        // The help overlay is modal: while it is open, all other intents are inert. The run loop
        // routes keys to handle_help_key instead; this guard mirrors finder/prompt.
        if self.modal.help().is_some() {
            return Effects::noop();
        }
        // Line-select is modal too: while it is active the run loop (T-5) routes raw keys to a
        // dedicated handler, so any non-routed intent reaching `handle` is inert — mirrors the
        // finder/prompt/help guards above.
        if self.modal.line_select().is_some() {
            return Effects::noop();
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
            Intent::OpenGoToLine => self.open_go_to_line(),
            Intent::OpenSearch => self.open_search(),
            Intent::NextMatch => self.next_match(),
            Intent::PrevMatch => self.prev_match(),
            Intent::TreeScrollLeft => self.scroll_tree_h_focus(-(HSCROLL_STEP as i32)),
            Intent::TreeScrollRight => self.scroll_tree_h_focus(HSCROLL_STEP as i32),
            Intent::ShowHelp => self.open_help(),
            Intent::Close => self.close_or_unzoom(),
        }
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
    /// leave the bottom of wrapped prose unreachable — so [`crate::text_layout::wrapped_rows`]
    /// simulates the word packing, floored by the all-columns char-wrap count so leading/interior
    /// spaces can't
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
            self.wrapped_rows_before(self.content.lines.len())
        } else {
            self.content.lines.len()
        };
        count.min(u16::MAX as usize) as u16
    }

    /// Cumulative display rows the first `n` content (source) lines occupy at the current content
    /// width when wrapping is on (each line is ≥ 1 row). Shared by [`rendered_line_count_for`] (with
    /// `n` = the whole line count → the wrapped-row total) and [`scroll_to_line`] (with `n` = line-1 →
    /// the display-row offset of a source line), so the scroll clamp and the go-to-line target are
    /// computed by the SAME wrapping logic and therefore always agree (AC-3/AC-4).
    fn wrapped_rows_before(&self, n: usize) -> usize {
        let w = self.content_width as usize;
        self.content
            .lines
            .iter()
            .take(n)
            .map(|l| crate::text_layout::line_wrapped_rows(l, w))
            .sum::<usize>()
    }

    /// Extract the plain-text content of every displayed line (ANSI spans joined, no styling).
    /// Used by the incremental search to feed `search::find_matches`; search always
    /// operates on the DISPLAYED content, not the source file, so it works identically across
    /// every view mode (SyntaxContent, Diff, RenderedMarkdown — AC-13).
    fn content_plain_lines(&self) -> Vec<String> {
        self.content
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
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
        self.set_changed(self.git.changed_set(self.baseline));
        // Drop any pending re-root async status fetch: this synchronous
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
            EditorOutcome::TookOver => {
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
            EditorOutcome::NoTakeover => Effects::redraw(), // hand-off without a terminal takeover
            EditorOutcome::NotLaunched(reason) => {
                // The editor never ran: report the launch failure. The hand-off may
                // have suspended the terminal before failing, so force a full repaint to
                // recover from any partial screen state.
                self.action_notice = Some(format!("Could not open editor: {reason}"));
                Effects {
                    redraw: true,
                    clear: true,
                    ..Default::default()
                }
            }
            EditorOutcome::NonZeroExit(detail) => {
                // The editor DID run and exited non-zero: the terminal was handed
                // over, so the file may have changed and a full repaint is still needed — but
                // this is not a launch failure, so the notice says so (and stays silent-free
                // for callers that treat a non-zero exit as benign by returning TookOver).
                self.action_notice = Some(format!("Editor exited with {detail}"));
                self.refresh_git_state();
                self.dispatch_render();
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
        // clipboard *or* the notice — the same AC-27 neutralizer the Presenter applies to every
        // filesystem-derived string it displays. (The OSC 52 payload is base64-encoded only for
        // transport; the terminal decodes it back to this exact string onto the clipboard, so the
        // encoding alone does not make a control-bearing path safe to paste.)
        let text = crate::text_layout::sanitize_control(&raw);
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
        // different file behind the full-screen content.
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

    /// The close key (`q`/`Esc`): layered dismissal in order — clear a committed search first,
    /// then un-zoom if zoomed, then quit. So from a committed search the sequence is:
    /// Esc → clears the search; Esc again → un-zooms (if zoomed) or quits. (AC-20, owner UX.)
    fn close_or_unzoom(&mut self) -> Effects {
        // A committed search (prompt closed, highlights persisting) is dismissed first — Esc/q
        // "come out of the search" before they unzoom or close (layered like unzoom). (owner UX)
        if self.search.is_some() && !self.prompt_open() {
            self.search = None;
            return Effects::redraw();
        }
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

    /// Whether the go-to-file finder overlay is currently open.
    pub fn finder_open(&self) -> bool {
        self.modal.finder().is_some()
    }

    /// Whether the help overlay is currently open.
    pub fn help_open(&self) -> bool {
        self.modal.help().is_some()
    }

    /// The mode of the currently-open bottom-prompt, or `None` when no prompt is open.
    /// Exposed for tests that need to assert which prompt variant was opened.
    pub fn prompt_mode(&self) -> Option<PromptMode> {
        self.modal.prompt().map(|p| p.mode)
    }

    /// The pending auto-switch go-to-line target (1-based source line), or `None`. Set when `:`
    /// confirms in a transformed view (the jump waits for the source-mapped re-render); cleared by
    /// `poll` once that render lands and the jump applies (AC-7). Exposed for tests.
    pub fn pending_goto_line(&self) -> Option<usize> {
        self.pending_goto.map(|(_, line)| line)
    }

    /// The current go-to-line prompt buffer, or `""` when no prompt is open. Exposed for tests
    /// (AC-2) and the Presenter's bottom prompt line. Mirrors `finder_query()`.
    pub fn prompt_query(&self) -> &str {
        self.modal.prompt().map(|p| p.input.query()).unwrap_or("")
    }

    /// Route a key event while a bottom-prompt modal is open. The run loop calls this
    /// instead of the normal key→intent map while `prompt_open()`. Dispatches by the prompt's
    /// mode. (AC-2…AC-6)
    pub fn handle_prompt_key(&mut self, key: KeyEvent) -> Effects {
        // `PromptMode` is `Copy`; read it and drop the borrow before the per-mode handler runs.
        let Some(mode) = self.modal.prompt().map(|p| p.mode) else {
            return Effects::noop();
        };
        match mode {
            PromptMode::GoToLine => self.go_to_line_key(key),
            // Search key handling: incremental — every printable char or Backspace re-runs the
            // match query and refreshes the highlight overlay (AC-14); Enter commits, Esc cancels.
            PromptMode::Search => self.search_prompt_key(key),
        }
    }

    /// The live incremental-search state, or `None` when no search is active (no Search prompt
    /// has been typed into yet, or the prompt was closed and state cleared). Exposed for tests;
    /// the Presenter reads it for the highlight overlay.
    pub fn search(&self) -> Option<&SearchState> {
        self.search.as_ref()
    }

    /// The full candidate list loaded when the finder was opened, or an empty slice when
    /// the finder is closed. Exposed for tests; the Presenter reads via `finder()`.
    pub fn finder_candidates(&self) -> &[String] {
        self.modal.finder().map(|f| f.candidates()).unwrap_or(&[])
    }

    /// The current finder query string, or `""` when the finder is closed or the query is
    /// empty. Exposed for tests; the Presenter reads via `finder()`.
    pub fn finder_query(&self) -> &str {
        self.modal.finder().map(|f| f.query()).unwrap_or("")
    }

    /// The current ranked match indices (into `finder_candidates()`), or `&[]` when the finder
    /// is closed or the query is empty. Exposed for tests and the Presenter.
    pub fn finder_matches(&self) -> &[usize] {
        self.modal.finder().map(|f| f.matches()).unwrap_or(&[])
    }

    /// The cursor position within the match list, or `0` when the finder is closed or the
    /// list is empty. Exposed for tests and the confirm path.
    pub fn finder_cursor(&self) -> usize {
        self.modal.finder().map(|f| f.cursor()).unwrap_or(0)
    }

    /// The horizontal scroll offset for the result rows, or `0` when the finder is closed.
    /// Exposed for tests that verify Left/Right keys and horizontal wheel move hscroll.
    pub fn finder_hscroll(&self) -> u16 {
        self.modal.finder().map(|f| f.hscroll()).unwrap_or(0)
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
        // A new render supersedes any queued go-to-line jump from an OLDER render (e.g. the user
        // navigated away before an auto-switch render landed). The auto-switch path sets its own
        // `pending_goto` AFTER calling this, so its jump survives; only stale ones are cleared.
        self.pending_goto = None;
        // AC-20: any displayed-content change (file-select, view-cycle, baseline-toggle, refresh,
        // go-to-line auto-switch, re-root, etc.) clears a committed search and its highlighting.
        // `refresh_search` (the incremental-typing path) only calls `scroll_to_line` and sets
        // `self.search` directly — it does NOT call `dispatch_render` — so live typing is NOT
        // wiped by this clear.
        self.search = None;

        let Some(node) = self.tree.selected() else {
            // No visible node: an empty tree or a filter (changed-only, gitignore, etc.)
            // that matched nothing. Show guidance instead of a blank pane.
            return self.clear_content(EmptyReason::NoFiles);
        };
        if node.kind != NodeKind::File {
            // A directory is selected — it has no content to render; show guidance so
            // the pane is not a blank void.
            return self.clear_content(EmptyReason::Directory);
        }
        let mode = self.effective_mode(&node.path);
        let rel = self.rel(&node.path);
        // a slow render used to leave the PREVIOUS file's body visible under the NEW
        // selection's title (the title is derived from the tree cursor, which moves immediately,
        // while the body arrives off-thread). Show a loading placeholder for the body now and
        // mark a render in flight; `content_path` (the title's source of truth) is NOT touched
        // here — it updates only when the matching result lands in `poll`, so the title and body
        // switch to the new file together. The `latest_seq`/`applied_seq` gap already keys the
        // supersession, so a stale result for a superseded selection is dropped by `poll`.
        // Dispatch first, and only show the loading placeholder if the job was actually
        // queued. If the worker has gone (channel closed) the send fails — keep the last
        // rendered content instead of stranding the pane on a `Rendering…` placeholder that
        // no result will ever arrive to clear (`poll` only clears `content_rendering` when a
        // matching result lands). The send never panics, so the viewer stays alive either way.
        if self
            .job_tx
            .send(RenderJob {
                seq,
                path: node.path,
                rel,
                mode,
                baseline: self.baseline,
                is_git: self.is_git_repo,
            })
            .is_ok()
        {
            self.content = Text::raw("Rendering\u{2026}");
            self.content_notices.clear();
            self.content_rendering = true;
        }
    }

    /// Clear the content pane, showing empty-state guidance instead of a blank pane
    ///. The reason selects the copy: a directory selection vs. an empty/zero-match
    /// tree. The strings are static and first-party, so they need no AC-27 sanitization (they
    /// carry no control bytes); they flow through the same content path the renderer uses.
    fn clear_content(&mut self, reason: EmptyReason) {
        self.content = Text::raw(reason.label());
        self.content_notices.clear();
        // No file content is displayed for a directory/empty tree, and no render is in flight
        // (this path sends no `RenderJob`), so the title falls back to the selected node's name
        //.
        self.content_path = None;
        self.content_rendering = false;
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
                self.applied_seq = seq; // the displayed content is now this render (go-to-line guard)
                // the body has landed — now switch the title to match it. The latest
                // dispatched render always corresponds to the current tree selection (every
                // selection change calls `dispatch_render`), so the applied result's file is the
                // selected node. A stale result for a superseded selection was dropped above by
                // the `seq == latest_seq` guard, so this never points `content_path` at a file
                // the user has already moved past. The render is no longer in flight.
                self.content_path = self.tree.selected().map(|n| n.path.clone());
                self.content_rendering = false;
                applied = true;
                // A queued go-to-line jump (auto-switch from a transformed view, AC-7) applies once
                // ITS render lands: now that the source-mapped content is in, scroll to the line.
                if let Some((pseq, line)) = self.pending_goto
                    && pseq == seq
                {
                    self.scroll_to_line(line);
                    self.pending_goto = None;
                }
                // A render that was in flight when a search was opened/committed lands here and
                // swaps self.content; matches computed against the OLD content are now stale.
                // Mirror dispatch_render's AC-20 clear: recompute an open Search prompt against
                // the new content, else drop a committed search.
                if self.modal.prompt().map(|p| p.mode) == Some(crate::infile::PromptMode::Search) {
                    self.refresh_search();
                } else {
                    self.search = None;
                }
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
                    self.apply_git_state(&status, changed);
                    self.status_rx = None;
                    // The synchronous `re_root` dispatched the first render against the *empty*
                    // changed-set, so a changed file rendered in content/markdown mode, not Diff.
                    // Now that the real changed-set has landed, re-dispatch so the current
                    // selection re-renders in the correct view mode (changed → Diff, AC-9).
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
    /// Dragging the finder overlay's vertical scrollbar (handled in `handle_finder_mouse`).
    FinderV,
}

/// Whether a path names a markdown file (by extension, case-insensitive).
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::HELP_RENDER_TIMEOUT;

    #[test]
    fn help_render_timeout_within_ac22_budget() {
        // FIX-B / AC-22: open_help renders What's New synchronously on the input thread, so the
        // worst-case input-thread block is HELP_RENDER_TIMEOUT. Since R3 item 1, `run_renderer`
        // enforces a SINGLE combined wall-clock deadline (the stdout-wait and the exit-wait share
        // one `timeout`, not two), so the real worst-case is now exactly `HELP_RENDER_TIMEOUT`, not
        // ~2× it — making this `≤ 300ms` assertion a TRUE single wall-clock bound rather than a
        // best-case one. A slow/wedged renderer is killed at it and the plain-text fallback applies.
        // This pins that bound deterministically within the 300 ms responsiveness budget: bumping
        // the timeout past it fails HERE, covering the slow real-renderer path that a wall-clock
        // timing assertion could only check flakily.
        assert!(
            HELP_RENDER_TIMEOUT <= std::time::Duration::from_millis(300),
            "HELP_RENDER_TIMEOUT ({HELP_RENDER_TIMEOUT:?}) must stay within the 300ms AC-22 budget"
        );
    }
}
