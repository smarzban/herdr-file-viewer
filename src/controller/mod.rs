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

mod annotation;
mod finder;
mod git_apply;
mod help;
mod infile;
mod lineselect;
mod mouse;
mod picker;

use crate::annotation::AnnotationStore;
use crate::finder::FinderState;
use crate::git::{Baseline, Status};
use crate::help::{HelpSection, HelpSectionState, HelpState};
use crate::herdr::HerdrCli;
use crate::infile::{PromptMode, PromptState, SearchState};
use crate::intent::Intent;
use crate::picker::PickerState;
use crate::presenter::{
    AnnotationEditorKind, AnnotationEditorView, AnnotationIndicatorsView, AnnotationOverviewView,
    AnnotationRowView, AnnotationTargetView, CharSelView, ContentSearch, DiscardConfirmView,
    FinderView, Focus, HelpView, LineSelectView, PaneGeometry, PickerRowView, PickerView,
    ViewState,
};
use crate::render::{Prepared, Renderers};
use crate::root::Resolved;
use crate::tree::{Node, NodeKind, TreeModel};
use crate::update::{self, UpdateState, Version};
use crate::view_policy::{FileDescriptor, ViewMode, applicable_modes, default_mode};
use annotation::{AnnotationEditorState, AnnotationListState};
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
// The split range is owned by `crate::config` (the single source of truth), so the config-value
// clamp (`resolve`) and this live keyboard/drag-resize clamp can never drift. `SPLIT_DEFAULT` seeds
// the initial `split_pct` before any `apply_tree_width` from the effective config.
const SPLIT_DEFAULT: u16 = crate::config::DEFAULT_TREE_WIDTH;
const SPLIT_MIN: u16 = crate::config::MIN_TREE_WIDTH;
const SPLIT_MAX: u16 = crate::config::MAX_TREE_WIDTH;
/// How many percentage points one resize keypress moves the divider.
const SPLIT_STEP: u16 = 5;
// The floor for an INTERACTIVE resize (grow/shrink keys, divider drag), below the config/startup
// floor `SPLIT_MIN`. A hand resize may pull the tree narrower than the startup minimum — down to the
// same 10% the Presenter's `columns()` renders at — so on a wide pane the tree can be shrunk below a
// `tree_max_cols`-capped default (which can sit below `SPLIT_MIN`, leaving "shrink" nowhere to go
// otherwise). `SPLIT_MIN` still bounds the config-seeded startup value; this only widens the
// interactive range.
const SPLIT_DRAG_MIN: u16 = 10;
/// How many columns one horizontal-scroll keypress moves the content pane.
const HSCROLL_STEP: u16 = 8;
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
    /// Raw unified diff for every tracked change under a repo-root-relative directory against
    /// `baseline`. Empty `rel_dir` means the whole tree root. Used by git-status mode (`d`)
    /// when a directory is selected.
    fn diff_directory(&self, rel_dir: &Path, baseline: Baseline) -> String;
}

/// The rendered content pane for one file: ingested text plus any non-fatal notices
/// (truncation AC-13, renderer fallback AC-25).
pub struct RenderResult {
    pub content: Text<'static>,
    pub notices: Vec<String>,
    /// The raw SOURCE lines behind a source-mapped (`SyntaxContent`) render — the file text the
    /// renderer was fed, split into lines, 1:1 with `content.lines` — so the copy paths can
    /// produce byte-faithful text (real tabs, no renderer decoration) and anchor the gutter
    /// width exactly instead of heuristically. `None` for transformed views (markdown / diffs,
    /// where no per-display-line source exists) and for providers that don't supply it; the
    /// copy paths then fall back to display-text extraction.
    pub source: Option<Vec<String>>,
}

/// Produce the content-pane text for `(file, mode)`. `Send` so a later task can run it on a
/// worker thread (AC-23). Behind a trait so tests stub it instead of spawning glow/delta/bat.
pub trait ContentProvider: Send {
    fn render(&self, path: &Path, mode: ViewMode, raw_diff: Option<&str>) -> RenderResult;

    /// Render honoring the content pane's drawable text `width` (columns), so a width-sensitive
    /// delegate (glow, for markdown) can lay out and wrap tables to fit the pane rather than
    /// overflow it. `None` (or `0`) means "unknown / no bound" — behave exactly as [`render`].
    ///
    /// Defaulted to call [`render`], so a test double (which never spawns a real width-sensitive
    /// renderer) need not implement it; only the live [`LiveContent`] overrides it to thread the
    /// width into glow's `-w`.
    ///
    /// [`render`]: ContentProvider::render
    fn render_at_width(
        &self,
        path: &Path,
        mode: ViewMode,
        raw_diff: Option<&str>,
        width: Option<u16>,
    ) -> RenderResult {
        let _ = width;
        self.render(path, mode, raw_diff)
    }
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
            EmptyReason::Directory => "Directory: select a file to view",
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
    /// When true, the worker runs [`GitService::diff_directory`] on `rel` (directory-scoped
    /// working-tree / baseline diff) instead of a single-file [`GitService::diff`]. Used by
    /// git-status mode when a directory is selected.
    directory_diff: bool,
    /// The content pane's drawable text width (columns) at dispatch, or `None` when unknown
    /// (e.g. the very first render before the first draw measured the pane). Only the markdown
    /// delegate uses it: glow lays out and wraps tables to this width so they fit the pane
    /// instead of overflowing and being shattered by the Presenter's re-wrap.
    wrap_width: Option<u16>,
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
    Annotations(AnnotationListState),
    AnnotationEditor(AnnotationEditorState),
    /// The confirm raised when an action would discard unexported annotations. Carries what to do
    /// once the user decides; the store it guards is the controller's.
    DiscardConfirm(DiscardAction),
}

/// What a [`Modal::DiscardConfirm`] proceeds with once the user confirms: the two paths that
/// destroy session annotations. Both clear the store, so both are guarded identically.
#[derive(Debug, Clone)]
pub(crate) enum DiscardAction {
    /// Close the viewer.
    Quit,
    /// Re-root to an already-resolved worktree. Boxed: `Resolved` is much larger than the other
    /// variants, and this enum lives inside every `Modal`.
    SwitchRoot(Box<crate::root::Resolved>),
}

impl DiscardAction {
    /// The verb shown in the confirm's key hints (`copy & quit` / `copy & switch`).
    fn verb(&self) -> &'static str {
        match self {
            DiscardAction::Quit => "quit",
            DiscardAction::SwitchRoot(_) => "switch",
        }
    }

    /// The key that proceeds and discards: `q` mirrors the key that raised the quit confirm, and
    /// `Enter` mirrors the picker's own confirm key for a switch.
    fn proceed_key(&self) -> KeyCode {
        match self {
            DiscardAction::Quit => KeyCode::Char('q'),
            DiscardAction::SwitchRoot(_) => KeyCode::Enter,
        }
    }
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
    // Used by the line-select key handler (`handle_line_select_key`); the state exists here from
    // T-3 so the accessor pair mirrors picker/finder/prompt/help.
    fn line_select_mut(&mut self) -> Option<&mut LineSelectState> {
        match self {
            Modal::LineSelect(s) => Some(s),
            _ => None,
        }
    }
    fn annotations(&self) -> Option<&AnnotationListState> {
        match self {
            Modal::Annotations(s) => Some(s),
            _ => None,
        }
    }
    fn annotations_mut(&mut self) -> Option<&mut AnnotationListState> {
        match self {
            Modal::Annotations(s) => Some(s),
            _ => None,
        }
    }
    fn annotation_editor(&self) -> Option<&AnnotationEditorState> {
        match self {
            Modal::AnnotationEditor(s) => Some(s),
            _ => None,
        }
    }
    fn annotation_editor_mut(&mut self) -> Option<&mut AnnotationEditorState> {
        match self {
            Modal::AnnotationEditor(s) => Some(s),
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
    /// Whether quitting with annotations held raises the discard confirm (config
    /// `confirm_discard`, default `true`). When `false`, `q` quits and discards, which
    /// is the pre-confirm behavior.
    confirm_discard: bool,
    changed_only: bool,
    /// Sticky git-status mode (`d`): tree filtered to current working-tree status and content
    /// forced to working-tree diffs (file or directory-scoped). Mutually exclusive with
    /// [`Self::changed_only`] (baseline-aware `c`). Cleared only by a second `d` (or by
    /// entering `c`, which turns this off).
    status_mode: bool,
    /// Working-tree status (`git status`), cached separately from the baseline-dependent
    /// [`Self::changed`] set so status mode can filter independently of `b`.
    git_status: BTreeMap<PathBuf, Status>,
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
    /// How many lines (or finder list items, or help-overlay lines) one mouse-wheel event advances
    /// — the effective **scroll step** (config `scroll_lines`, else [`crate::config::DEFAULT_SCROLL_LINES`]).
    /// Set once at startup via [`apply_scroll_lines`](Self::apply_scroll_lines). Held as `isize`
    /// because the wheel handlers negate it for wheel-up. The directory tree ignores the magnitude
    /// (it advances one row per event via the delta's sign), so this only affects the content pane,
    /// the finder list, and the help overlay.
    wheel_step: isize,
    /// The tree column's share of the width, as a percentage (the rest is the content pane).
    /// Adjustable from the keyboard since the viewer owns both columns (ADR-0002). Seeded at
    /// startup from the effective config via [`apply_tree_width`](Self::apply_tree_width).
    split_pct: u16,
    /// Which side of the content pane the tree is drawn on. Seeded at startup from the effective
    /// config via [`apply_tree_position`](Self::apply_tree_position); a session preference carried
    /// across a re-root (like `split_pct`). The Presenter reads it through the view state.
    tree_position: crate::config::TreePosition,
    /// The maximum tree width in character columns (`tree_max_cols`): the tree is drawn at
    /// `min(split_pct% of the pane, tree_max_cols)` so it never over-allocates on a very wide pane.
    /// Seeded at startup via [`apply_tree_max_cols`](Self::apply_tree_max_cols); read by the
    /// Presenter through the view state. Carried across a re-root (like `split_pct`).
    tree_max_cols: u16,
    /// Whether the user has resized the split by hand this session (grow/shrink keys or a divider
    /// drag). `tree_max_cols` caps the tree only while this is `false`; the first manual resize seeds
    /// `split_pct` from the currently-displayed width and lifts the cap, so the resize is honoured
    /// exactly instead of looking frozen on a wide, capped pane. Carried across a re-root (like
    /// `split_pct`).
    split_manual: bool,
    /// User override of the per-mode wrap default (the `w` toggle). `None` ⇒ the per-mode default
    /// applies (prose wraps, code/diffs don't); `Some(true)` ⇒ force wrap on everywhere (read long
    /// code/diff lines); `Some(false)` ⇒ force wrap off everywhere — which, for rendered markdown,
    /// switches glow to natural-width layout so a wide table renders in full and the pane scrolls
    /// horizontally to reveal it (instead of the fit-to-pane view that ellipsizes over-long cells).
    wrap_override: Option<bool>,
    /// Hide the tree so the content pane fills the frame (the `z` zoom toggle). Pure layout
    /// state — the selection and rendered content are unchanged.
    zoomed: bool,
    /// Whether **this viewer** currently holds the pane in full-screen via `Z`
    /// ([`Intent::OpenFullscreen`]). Owned intent, not a herdr query: it drives the `Z` toggle and,
    /// crucially, lets every exit path (a second `Z`, `Esc`/`q`, `z`, a re-root, quit) release the
    /// host pane zoom. Only ever set through [`host_zoom`](Self::host_zoom), so it stays paired with
    /// the actual `pane zoom --on`/`--off` calls, and only ever in response to the user pressing
    /// `Z` — never on its own.
    ///
    /// One consequence of tracking intent rather than querying herdr: pressing `Z` while the pane
    /// is *already* full-screen (because the user zoomed it with herdr's own pane-zoom key) makes
    /// the viewer **adopt** that full-screen — the `--on` is a harmless no-op, but a later `Z` /
    /// `Esc` / `z` then issues `--off` and returns to the split. That is the deliberate trade-off
    /// for a toggle that works with no live herdr; the alternative (a `pane layout` query on every
    /// `Z`) is heavier and still can't tell "the user's zoom" from "ours". `Z` is only ever a no-op
    /// on the host when there is nothing to change.
    host_zoomed: bool,
    tree: TreeModel,
    /// Changed-set vs the active baseline, cached; recomputed on a baseline toggle (AC-16).
    changed: BTreeMap<PathBuf, Status>,
    /// Per-file view-mode override set by cycling (AC-11); absent ⇒ the policy default.
    overrides: HashMap<PathBuf, ViewMode>,
    /// The content pane's current text and its notices (truncation/fallback).
    content: Text<'static>,
    content_notices: Vec<String>,
    /// The raw source lines behind the displayed content when it is source-mapped
    /// (`SyntaxContent`), 1:1 with `content.lines` — see [`RenderResult::source`]. Applied and
    /// cleared in lockstep with `content` (`poll` / `clear_content`), so the copy paths can trust
    /// that when it is `Some`, index `n-1` IS displayed line `n`'s source.
    content_source: Option<Vec<String>>,
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
    /// Session-only annotations, bound to the current root and never persisted.
    annotations: AnnotationStore,
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
    /// The `seq` of an in-flight markdown re-render triggered by a content-pane *resize*
    /// ([`rerender_markdown_for_width`]), as opposed to a selection change. When [`poll`] applies a
    /// result whose seq matches, it preserves the current scroll and recomputes an active search
    /// (a resize must not jump to the top or drop the search), instead of the from-scratch reset a
    /// selection-change render gets. `None` when no resize re-render is pending.
    ///
    /// [`rerender_markdown_for_width`]: Controller::rerender_markdown_for_width
    reflow_seq: Option<u64>,
    /// Hit-test geometry from the last drawn frame (fed back by the Presenter), so a mouse
    /// event can be mapped to a tree row / the content pane / the divider.
    geom: PaneGeometry,
    /// The previous left-click `(col, row, time)`, for double-click detection.
    last_click: Option<(u16, u16, Instant)>,
    /// What the held left button is dragging (divider resize or a scrollbar), so the release is
    /// treated as the end of the drag, not a click. `None` ⇒ no drag in progress.
    drag: Option<Drag>,
    /// An ambient character selection dragged out in the content pane during normal navigation, held
    /// OUTSIDE [`Modal`] so `Modal::None` stays in force and every keyboard binding keeps its normal
    /// meaning — that is what makes it ambient, not a mode. Reuses the [`LineSelectState`] char
    /// primitives (always char-mode; `char_at_content_col` maps wrapped views too). Mutually
    /// exclusive with L line-select mode by construction: created only in `handle_column_mouse`,
    /// which runs only for `Modal::None`. Auto-copied on release.
    content_selection: Option<LineSelectState>,
    /// The newer version to advertise, if any (set from the cached value at startup and
    /// refreshed by the background check). `None` ⇒ up-to-date / unknown.
    update_available: Option<Version>,
    /// The pre-formatted Settings section body (AC-15, AC-18), or `None` before
    /// [`set_settings_display`](Self::set_settings_display) is called. Injected post-construction
    /// (mirrors [`set_update`](Self::set_update)) so the controller stays hermetic in tests — a
    /// test that never calls the setter gets the pre-T-9 two-section overlay unchanged.
    settings_display: Option<String>,
    /// The pre-formatted Keybindings section body (AC-16, AC-19, AC-20), or `None` before
    /// [`set_keybindings_display`](Self::set_keybindings_display) is called. Injected
    /// post-construction (mirrors [`settings_display`](Self::settings_display)) so the controller
    /// stays hermetic in tests — a test that never calls the setter keeps its overlay unchanged.
    keybindings_display: Option<String>,
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
    /// A queued line-select entry awaiting its source re-render: the render seq to wait for. Set when
    /// `L` enters line-select in a **transformed** view (RenderedMarkdown / Diff / FullDiff) or while a
    /// source render is still in flight — the file is switched to the source-mapped content view and the
    /// marker can't be placed until that render lands, so it is queued against the dispatched render's
    /// seq and applied by [`poll`] (AC-15). Unlike [`pending_goto`](Self::pending_goto) there is no
    /// user-specified target line: the marker always lands on the **top visible source line** of the
    /// freshly-switched view (line 1 after the render resets the scroll). `None` when no entry is
    /// pending; superseded (cleared) by any newer render dispatch — exactly like `pending_goto`.
    pending_line_select: Option<u64>,
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
    /// The OS opener seam used by the `O` / `R` hand-offs (open-with-default-app / reveal-in-file-
    /// manager). Injected post-construction via [`set_opener`](Self::set_opener) (like
    /// [`herdr`](Self::herdr)) so the controller stays hermetic in tests. `None` until then.
    opener: Option<Box<dyn crate::opener::Opener>>,
    /// The effective key -> intent bindings the run loop decodes against (Slice B, T-6): the
    /// keybinding registry resolved with the config's `[keys]` overrides (config > default).
    /// Initialized to [`default_bindings`](crate::input::default_bindings) so a controller always
    /// holds a valid map (including in tests that never wire config); `app::run` replaces it via
    /// [`set_keybindings`](Self::set_keybindings). Read-only input, never persisted (AC-23).
    bindings: crate::input::EffectiveBindings,
    /// The outcome of resolving the config's `[keys]` table — every rejected entry and why — kept so
    /// the T-7 Keybindings overlay can surface which bindings were ignored (AC-16). Empty until
    /// [`set_keybindings`](Self::set_keybindings) forwards the resolver's outcome.
    #[allow(dead_code)]
    // consumed by the T-7 Keybindings overlay; exercised by this module's tests.
    key_load_outcome: crate::input::KeyLoadOutcome,
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
            // Defaults ON, matching the resolver: a Controller built without config still guards.
            confirm_discard: true,
            tree_hscroll: 0,
            changed_only: false,
            status_mode: false,
            git_status: BTreeMap::new(),
            focus: Focus::Tree,
            width: 0,
            content_scroll: 0,
            content_hscroll: 0,
            content_width: 0,
            content_height: 0,
            wheel_step: crate::config::DEFAULT_SCROLL_LINES as isize,
            split_pct: SPLIT_DEFAULT,
            tree_position: crate::config::TreePosition::Left,
            tree_max_cols: crate::config::DEFAULT_TREE_MAX_COLS,
            split_manual: false,
            wrap_override: None,
            zoomed: false,
            host_zoomed: false,
            changed: BTreeMap::new(),
            overrides: HashMap::new(),
            content: Text::raw(""),
            content_notices: Vec::new(),
            content_source: None,
            content_path: None,
            content_rendering: false,
            action_notice: None,
            annotations: AnnotationStore::new(),
            git,
            editor,
            clipboard,
            providers,
            renderers,
            job_tx,
            result_rx,
            latest_seq: 0,
            reflow_seq: None,
            geom: PaneGeometry::default(),
            last_click: None,
            drag: None,
            content_selection: None,
            update_available: None,
            settings_display: None,
            keybindings_display: None,
            update_dismissed: false,
            update_rx: None,
            status_rx: None,
            modal: Modal::None,
            pending_goto: None,
            pending_line_select: None,
            applied_seq: 0,
            search: None,
            herdr: None,
            our_workspace_id: None,
            base_branch,
            current_branch,
            opener: None,
            // Valid default bindings so the run loop can decode before (and if) `app::run` wires the
            // config's `[keys]` overrides via `set_keybindings`; tests inherit these unchanged.
            bindings: crate::input::default_bindings(),
            key_load_outcome: crate::input::KeyLoadOutcome::default(),
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
                            if job.directory_diff {
                                // Status mode on a directory: pathspec-scoped working-tree/baseline
                                // diff (rel is empty for the tree root).
                                let rel = job.rel.as_deref().unwrap_or_else(|| Path::new(""));
                                Some(git.diff_directory(rel, job.baseline))
                            } else {
                                let full = job.mode == ViewMode::FullDiff;
                                job.rel
                                    .as_deref()
                                    .map(|rel| git.diff(rel, job.baseline, full))
                            }
                        } else {
                            None
                        };
                    content.render_at_width(
                        &job.path,
                        job.mode,
                        raw_diff.as_deref(),
                        job.wrap_width,
                    )
                }))
                .unwrap_or_else(|_| RenderResult {
                    content: Text::raw("[content unavailable: renderer error]"),
                    notices: vec!["the renderer failed unexpectedly; showing a placeholder".into()],
                    source: None,
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

        // Past both early-returns the switch is really going to happen, so this is the first point
        // where the annotations are genuinely at risk: a switch clears them exactly like a quit
        // does. Guarding earlier (at the picker) would confirm for a switch that would have
        // no-opped and lost nothing. `resolved` rides along so confirming costs no re-resolve.
        if self.confirm_discard && !self.annotations.is_empty() {
            self.modal = Modal::DiscardConfirm(DiscardAction::SwitchRoot(Box::new(resolved)));
            return;
        }
        self.apply_re_root(resolved);
    }

    /// Perform an already-resolved, already-confirmed re-root: rebuild the root-bound services and
    /// reset the per-root state (including clearing the annotations, whose targets belong to the old
    /// root). Split out of [`re_root`](Self::re_root) so the discard confirm can hold the resolved
    /// target and apply it later without re-resolving.
    fn apply_re_root(&mut self, resolved: crate::root::Resolved) {
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
        // Re-rooting returns to the two-column split, so release the viewer's own host pane zoom
        // too (no-op if `Z` was never used); otherwise the pane would stay full-screen while the
        // plugin resets to the split (`herdr` is session-level and survives the re-root).
        self.leave_host_zoom();
        self.content_scroll = 0;
        self.content_hscroll = 0;
        self.tree_hscroll = 0;
        self.overrides.clear();
        // The old root's rendered content is invalid under the new root — drop the displayed-file
        // path so the title falls back to a neutral label until the new selection's render lands
        //. `dispatch_render` below sets `content_rendering` and the loading placeholder.
        self.content_path = None;
        let cleared_annotations = self.annotations.clear();
        self.action_notice = (cleared_annotations > 0).then(|| {
            format!(
                "Cleared {cleared_annotations} annotation{} after switching root",
                if cleared_annotations == 1 { "" } else { "s" }
            )
        });
        self.changed = BTreeMap::new();
        self.git_status = BTreeMap::new();
        // Close whatever modal is open (one assignment, since `modal` is now a single value). A
        // re-root only fires via picker-confirm, so in practice it's the picker being torn down —
        // but a re-root also invalidates the finder's old-root candidate list and must not strand a
        // prompt/help over the freshly re-rooted tree, so closing the lot here stays correct for any
        // future re-root trigger.
        self.modal = Modal::None;
        self.last_click = None;

        // PREFERENCES ARE CARRIED (AC-12) — deliberately NOT reset: show_ignored, hide_hidden,
        // changed_only, status_mode, split_pct, tree_position, tree_max_cols, split_manual, wrap_override, baseline keep their current values. The fresh
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
            // Non-repo: both filters collapse to empty. Prefer the active mode's set.
            if self.status_mode {
                self.tree.set_changed_only(true, &self.git_status);
            } else {
                self.tree.set_changed_only(self.changed_only, &self.changed); // empty
            }
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
    /// Whether sticky git-status mode (`d`) is active.
    pub fn status_mode(&self) -> bool {
        self.status_mode
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
    /// Whether the `w` override is currently forcing wrap ON (carried across a re-root, AC-12).
    /// `Some(false)` (force wrap off / horizontal-scroll) and `None` (per-mode default) both read
    /// as `false` here — the getter reports the force-on state the tests assert after toggling.
    pub fn wrap_override(&self) -> bool {
        self.wrap_override == Some(true)
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
    /// when nothing is selected. Directories normally have no view mode, except in git-status
    /// mode (`d`) where they render a directory-scoped working-tree Diff.
    pub fn selected_view_mode(&self) -> Option<ViewMode> {
        let node = self.tree.selected()?;
        if node.kind != NodeKind::File {
            return if self.status_mode && self.is_git_repo {
                Some(ViewMode::Diff)
            } else {
                None
            };
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

    /// Install the formatted Settings section body (AC-15, AC-18), so [`open_help`](Self::open_help)
    /// appends a third "Settings" section to the `?` overlay. Called once by `app::run` after
    /// construction (mirrors [`set_update`](Self::set_update)); a test that never calls this keeps
    /// the pre-existing two-section overlay.
    pub fn set_settings_display(
        &mut self,
        eff: &crate::config::EffectiveSettings,
        outcome: &crate::config::LoadOutcome,
        config_path: &std::path::Path,
        wired: &crate::help::SettingsWired,
    ) {
        self.settings_display = Some(crate::help::settings_text(eff, outcome, config_path, wired));
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

    /// Inject the OS opener seam used by the `O` / `R` hand-offs (open-with-default-app /
    /// reveal-in-file-manager). Injected post-construction (like `set_host`) so the controller
    /// stays hermetic in tests — a test injects a fake; production injects the live opener (AC-13).
    pub fn set_opener(&mut self, opener: Box<dyn crate::opener::Opener>) {
        self.opener = Some(opener);
    }

    /// Install the effective key bindings resolved from the registry + the config's `[keys]` table,
    /// plus the resolver's [`KeyLoadOutcome`](crate::input::KeyLoadOutcome) (Slice B, T-6). Called
    /// once by `app::run` after construction (mirrors [`set_settings_display`](Self::set_settings_display));
    /// a test that never calls it keeps the [`default_bindings`](crate::input::default_bindings) set
    /// from `Controller::new`. Read-only wiring: it only stores in-memory state, never writes (AC-23).
    pub(crate) fn set_keybindings(
        &mut self,
        bindings: crate::input::EffectiveBindings,
        outcome: crate::input::KeyLoadOutcome,
    ) {
        self.bindings = bindings;
        self.key_load_outcome = outcome;
    }

    /// The effective key bindings the run loop decodes each key event against (Slice B, T-6).
    pub(crate) fn bindings(&self) -> &crate::input::EffectiveBindings {
        &self.bindings
    }

    /// The `[keys]` resolution outcome (rejected entries), for the Keybindings overlay to
    /// surface which bindings were ignored (AC-16).
    pub(crate) fn key_load_outcome(&self) -> &crate::input::KeyLoadOutcome {
        &self.key_load_outcome
    }

    /// Apply the startup hide-dotfiles default from config (AC-9). Called once by `app::run`
    /// right after construction, before the first draw. Mirrors `toggle_hidden`'s two-field
    /// update (the controller's own `hide_hidden` mirror plus the tree's filter) so the later
    /// interactive `.` toggle reads a value already in sync with what it's hiding, rather than
    /// re-applying (or silently undoing) the configured default on the very first press.
    pub fn apply_hide_dotfiles(&mut self, hide: bool) {
        if hide == self.hide_hidden {
            // No change from the current (startup) state — `Controller::new`'s initial render
            // already reflects it, so re-rendering would be redundant. This is the common
            // no-config case, where `hide` is the default `false`.
            return;
        }
        self.hide_hidden = hide;
        self.tree.set_hide_hidden(hide);
        // Hiding dotfiles can shift which node the cursor lands on (e.g. a leading dotfile
        // sorting first at cursor 0), so re-render the content pane for the (possibly) new
        // selection — mirrors toggle_hidden's own post-filter re-render, and supersedes
        // `Controller::new`'s single unfiltered render dispatched just before this runs.
        self.dispatch_render();
    }

    /// Apply the config-driven `confirm_discard` switch. Pure in-memory wiring.
    pub fn apply_confirm_discard(&mut self, confirm: bool) {
        self.confirm_discard = confirm;
    }

    /// Set the mouse-wheel **scroll step** from the effective config (`scroll_lines`). Called once
    /// at startup, mirroring [`apply_hide_dotfiles`](Self::apply_hide_dotfiles). The resolver has
    /// already clamped the value to ≥ 1, so a wheel event always advances at least one line/item;
    /// storing it as `isize` lets the wheel handlers negate it for wheel-up. Affects the content
    /// pane, the finder list, and the help overlay (not the tree, which is sign-only).
    pub fn apply_scroll_lines(&mut self, lines: u16) {
        // Defensive floor at the public boundary: the resolver already clamps to >= 1, but a `0`
        // reaching here (a future caller, a direct test) would freeze wheel scrolling — guard it
        // locally so the invariant holds regardless of how the value arrives.
        self.wheel_step = lines.max(1) as isize;
    }

    /// Seed the startup split ratio from the effective config (`tree_width`). Called once at
    /// startup, mirroring [`apply_hide_dotfiles`](Self::apply_hide_dotfiles). The live keyboard/drag
    /// resize adjusts `split_pct` afterward within the session; this only sets its initial value.
    pub fn apply_tree_width(&mut self, pct: u16) {
        // Defensive clamp at the public boundary: the resolver already clamps to
        // `SPLIT_MIN..=SPLIT_MAX`, but guard locally so a direct/out-of-range caller can never
        // collapse a column (matches the live-resize clamp).
        self.split_pct = pct.clamp(SPLIT_MIN, SPLIT_MAX);
    }

    /// Seed the startup tree side from the effective config (`tree_position`). Called once at
    /// startup, mirroring [`apply_hide_dotfiles`](Self::apply_hide_dotfiles). Pure layout state read
    /// by the Presenter through the view state — no re-render needed (the content is unchanged).
    pub fn apply_tree_position(&mut self, position: crate::config::TreePosition) {
        self.tree_position = position;
    }

    /// Seed the startup tree column cap from the effective config (`tree_max_cols`). Called once at
    /// startup, mirroring [`apply_hide_dotfiles`](Self::apply_hide_dotfiles). Pure layout state read
    /// by the Presenter through the view state — no re-render needed (the content is unchanged).
    pub fn apply_tree_max_cols(&mut self, cols: u16) {
        // Defensive floor at the public boundary: the resolver already clamps to >= MIN_TREE_MAX_COLS,
        // but guard locally so a direct/out-of-range caller can never cap the tree to nothing.
        self.tree_max_cols = cols.max(crate::config::MIN_TREE_MAX_COLS);
    }

    /// Record the content viewport `(width, height)` the Presenter last drew into, so content
    /// scrolling can be clamped to it. Called by the run loop after each draw.
    pub fn set_content_viewport(&mut self, width: u16, height: u16) {
        if width == self.content_width && height == self.content_height {
            return; // unchanged — avoid recomputing the clamp on every (mostly idle) draw
        }
        let width_changed = width != self.content_width;
        self.content_width = width;
        self.content_height = height;
        // A smaller viewport shrinks the max offset, so an existing scroll could now point past
        // the end, leaving blank space; re-clamp both axes to the new geometry.
        self.content_scroll = self.content_scroll.min(self.max_content_scroll());
        self.content_hscroll = self.content_hscroll.min(self.max_content_hscroll());
        // When markdown is fit-to-pane (wrapped), glow lays the table out to the pane width, so a
        // width change (terminal resize or split-bar drag) must reflow it, preserving scroll and
        // search (unlike a selection-change render). When unwrapped (the `w` horizontal-scroll view)
        // glow's natural-width output is width-independent — the pane just re-clamps h-scroll, no
        // re-render. A diff reflows on every width change unconditionally — delta's own `-w` tracks
        // the pane width regardless of the wrap toggle (see `diff_render_width`'s doc). A
        // height-only change never affects either delegate's layout.
        if width_changed {
            if self.effective_wrap() {
                self.rerender_markdown_reflow();
            }
            self.rerender_diff_reflow(); // a no-op unless the selection is a diff
        }
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
        // Inset the transformed views (rendered markdown / diff) one column from the left border so
        // their delegate output — which starts at column 0 — doesn't hug it; syntax/plain files
        // already get that gap from bat's line-number gutter, so they stay flush (no double gap).
        // Keyed off the DISPLAYED content's file (`content_path`, the title's source of truth), so
        // the gap switches in lockstep with the body — never off a still-loading selection.
        let content_pad_left = self.content_path.as_ref().is_some_and(|p| {
            matches!(
                self.effective_mode(p),
                ViewMode::RenderedMarkdown | ViewMode::Diff | ViewMode::FullDiff
            )
        });
        // Gutter width for a character selection's highlight (0 when not applicable); computed once
        // here so the line-select snapshot below stays a pure read.
        let sel_gutter = self.selection_gutter_len();
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
            content_pad_left,
            split_pct: self.split_pct,
            tree_position: self.tree_position,
            tree_max_cols: self.tree_max_cols,
            split_manual: self.split_manual,
            zoomed: self.zoomed,
            update_banner: self.update_banner(),
            picker: self.picker_view(),
            finder: self.finder_view(),
            annotation_count: self.annotations.len(),
            annotation_overview: self.annotation_overview_view(),
            annotation_editor: self.annotation_editor_view(),
            discard_confirm: self.discard_confirm_view(),
            annotation_indicators: self.annotation_indicators_view(),
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
            // Populate the line-select overlay from the active modal so the Presenter draws the
            // marker + selection highlight (AC-1, AC-7). `None` when the modal is closed → the
            // content path is byte-identical to the prior render (no other snapshot moves).
            line_select: self.modal.line_select().map(|s| {
                let (start, end) = s.selection();
                // A mouse drag carries character carets → the overlay highlights just those chars;
                // a keyboard selection has none → the whole-line highlight.
                let char_sel = if s.is_char_mode() {
                    let ((sl, sc), (el, ec)) = s.char_span();
                    Some(CharSelView {
                        start_line: sl,
                        start_col: sc,
                        end_line: el,
                        end_col: ec,
                        gutter: sel_gutter,
                    })
                } else {
                    None
                };
                LineSelectView {
                    marker: s.marker(),
                    start,
                    end,
                    char_sel,
                }
            }),
            // Snapshot the ambient selection only when non-collapsed, so a bare click never paints a
            // zero-width highlight (`draw_content` gives `line_select` precedence if both were set).
            content_selection: self
                .content_selection
                .as_ref()
                .filter(|s| {
                    let (a, b) = s.char_span();
                    a != b
                })
                .map(|s| {
                    let ((sl, sc), (el, ec)) = s.char_span();
                    CharSelView {
                        start_line: sl,
                        start_col: sc,
                        end_line: el,
                        end_col: ec,
                        gutter: self.content_gutter_len(sl),
                    }
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

    /// Whether the content pane wraps for `node`: the `w` override when set (`Some(true)` forces
    /// wrap on, `Some(false)` forces it off), else the per-mode default — prose (rendered markdown /
    /// plain text) wraps; diffs and code stay unwrapped so their columns align. Takes the node so
    /// the draw path needn't re-walk.
    fn wrap_for(&self, node: Option<&Node>) -> bool {
        let default = match node {
            Some(n) if n.kind == NodeKind::File => {
                // Only prose wraps by default; diffs (compact and full-context) and code keep their
                // lines so columns and the line-number gutter stay aligned.
                matches!(self.effective_mode(&n.path), ViewMode::RenderedMarkdown)
            }
            _ => false,
        };
        self.wrap_override.unwrap_or(default)
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
        // Raw-key modals own every key. Defensive guards prevent a direct/test caller from
        // leaking a globally-decoded intent to the tree or opening a second modal beneath one.
        if self.modal.line_select().is_some()
            || self.modal.annotations().is_some()
            || self.modal.annotation_editor().is_some()
        {
            return Effects::noop();
        }
        match intent {
            Intent::NavUp => self.navigate(-1),
            Intent::NavDown => self.navigate(1),
            Intent::Expand => self.expand(),
            Intent::Collapse => self.collapse(),
            Intent::Activate => self.activate(),
            Intent::OpenFullscreen => self.open_fullscreen(),
            Intent::ToggleIgnore => self.toggle_ignore(),
            Intent::ToggleHidden => self.toggle_hidden(),
            Intent::ToggleChangedOnly => self.toggle_changed_only(),
            Intent::ToggleStatusMode => self.toggle_status_mode(),
            Intent::ToggleBaseline => self.toggle_baseline(),
            Intent::CycleView => self.cycle_view(),
            Intent::OpenInEditor => self.open_in_editor(),
            Intent::OpenWithApp => self.open_with_app(),
            Intent::RevealInFileManager => self.reveal_in_file_manager(),
            Intent::CopyRepoPath => self.copy_path(PathKind::Repo),
            Intent::CopyAbsPath => self.copy_path(PathKind::Absolute),
            Intent::AddAnnotation => self.add_annotation(),
            Intent::ShowAnnotations => self.show_annotations(),
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
            // `L` is focus-gated (ADR-0010, copy-line-reference): on tree focus it is unchanged
            // (AC-2, still `scroll_tree_h_focus`); on content focus it instead enters line-select
            // at the top visible line (AC-1). The `is_empty()` inert branch below (AC-3) fires
            // only once a render has *completed* with a zero-line body — a render still in
            // flight shows the non-empty "Rendering…" placeholder, and no-file-selected/directory
            // states show non-empty guidance text (`clear_content`), so `L` enters line-select in
            // both of those. `TreeScrollLeft`/`H` is untouched — only `L` is overloaded. NOTE: the
            // `Intent::TreeScrollRight` doc comment in `src/intent.rs` still reads "Inert unless
            // the tree is focused" — that file is under a hard no-edit rule for this feature, so
            // this comment is the up-to-date behavior note instead.
            Intent::TreeScrollRight => match self.focus {
                Focus::Tree => self.scroll_tree_h_focus(HSCROLL_STEP as i32), // AC-2: unchanged
                Focus::Content => {
                    if self.content.lines.is_empty() {
                        Effects::noop() // AC-3: no rendered content → inert
                    } else {
                        self.enter_line_select_at_top(); // AC-1
                        Effects::redraw()
                    }
                }
            },
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
        let overlay = self.content_overlay_glyph_cols();
        self.content
            .lines
            .iter()
            .take(n)
            .map(|l| crate::text_layout::line_wrapped_rows_prefixed(l, w, overlay))
            .sum::<usize>()
    }

    /// The 0-based display-row offset at which 1-based source `line` begins. Without wrap a source
    /// line maps 1:1 to a row (`line - 1`); with the `w` override wrapping every mode, earlier long
    /// lines occupy several rows, so the offset is the cumulative wrapped-row count of the lines
    /// BEFORE it — the same mapping [`scroll_to_line`](Self::scroll_to_line) uses, so line-select's
    /// keep-marker-visible math agrees with the actual layout under wrap (the copy-line-reference
    /// wrap fix). Shared by the line-select handlers in the `lineselect`/`mouse` submodules.
    fn content_row_of_line(&self, line: usize) -> usize {
        if self.effective_wrap() {
            self.wrapped_rows_before(line.saturating_sub(1))
        } else {
            line.saturating_sub(1)
        }
    }

    /// The inverse of [`content_row_of_line`]: the 1-based source line displayed at 0-based
    /// display-row offset `row`. Without wrap that is simply `row + 1`; with wrap on, a source line
    /// spans multiple rows, so walk the cumulative wrapped-row counts until `row` falls inside a
    /// line. Used to place the line-select marker at the top visible source line on entry and to map
    /// a mouse click's screen row back to a source line, so both are correct under the `w` wrap
    /// override (the copy-line-reference wrap fix). Returns at least 1; the last source line for a
    /// `row` past the end. `content.lines` empty ⇒ 1 (callers guard the empty case separately).
    fn line_at_content_row(&self, row: usize) -> usize {
        if !self.effective_wrap() {
            return row + 1;
        }
        let w = self.content_width as usize;
        let overlay = self.content_overlay_glyph_cols();
        let mut acc = 0usize;
        for (i, line) in self.content.lines.iter().enumerate() {
            acc += crate::text_layout::line_wrapped_rows_prefixed(line, w, overlay);
            if row < acc {
                return i + 1;
            }
        }
        self.content.lines.len().max(1)
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

    /// `Z` (Shift+`z`): a full-screen **toggle** for reading the selected file. When the pane is
    /// not already full-screen, open the selection the way [`activate`](Self::activate) does and —
    /// for a **file** — additionally zoom this pane in herdr so the file takes over the whole
    /// terminal, not just the plugin's split. When the pane IS full-screen, reverse it: un-zoom the
    /// pane and restore the two-column split, back to browsing.
    ///
    /// The toggle keys off [`host_zoomed`](Self::host_zoomed) — the viewer's own record of whether
    /// it opened the host zoom — so it works with **or without** a live herdr (with herdr absent the
    /// flag still flips and the in-plugin zoom toggles). Because it is owned state, every other exit
    /// path releases it too: `Esc`/`q` ([`close_or_unzoom`](Self::close_or_unzoom)), `z`
    /// ([`toggle_zoom`](Self::toggle_zoom)), and a re-root ([`re_root`](Self::re_root)) all call
    /// [`leave_host_zoom`](Self::leave_host_zoom), so the host pane never lingers zoomed after the
    /// viewer returns to the split. Read-only w.r.t. files/git (herdr layout only). A directory
    /// (only reachable when not full-screen) just expands/collapses. The file kind is read — and the
    /// tree borrow dropped — *before* `activate` so the borrow checker is satisfied.
    fn open_fullscreen(&mut self) -> Effects {
        if self.host_zoomed {
            // Full-screen is on (the viewer opened it) → a second `Z` returns to the split.
            self.leave_host_zoom();
            self.zoomed = false;
            self.focus = Focus::Tree;
            return Effects::redraw();
        }
        // Not full-screen → open the selection; a file additionally goes full-screen.
        let is_file = matches!(self.tree.selected().map(|n| n.kind), Some(NodeKind::File));
        let effects = self.activate();
        if is_file {
            self.host_zoom(true);
        }
        effects
    }

    /// Zoom this pane to full-screen (`on`) or restore it (`!on`) via
    /// `herdr pane zoom --current --on|--off`, and record the viewer's intent in
    /// [`host_zoomed`](Self::host_zoomed). `--current` resolves to the focused pane, always the
    /// viewer while it is processing a keystroke. The argv is entirely static — no pane id is
    /// interpolated — so there is no option-injection surface. Best-effort and read-only w.r.t.
    /// files/git (a herdr layout op): a missing or failing herdr is swallowed, and the flag still
    /// tracks intent so `Z` stays a toggle (and teardown still fires) even with no herdr present.
    fn host_zoom(&mut self, on: bool) {
        self.host_zoomed = on;
        if let Some(herdr) = self.herdr.as_ref() {
            let flag = if on { "--on" } else { "--off" };
            let _ = herdr.run(&["pane", "zoom", "--current", flag]);
        }
    }

    /// Release the viewer's own host pane zoom if it holds one — the single teardown hook called
    /// from every path that leaves full-screen (a second `Z`, `Esc`/`q`, `z`, a re-root, quit), so
    /// the host pane never stays zoomed after the viewer has returned to (or left) the split. A
    /// no-op when the viewer did not zoom the pane, so an ordinary `Esc`/`z`/`W` never spawns a
    /// stray `pane zoom` call.
    fn leave_host_zoom(&mut self) {
        if self.host_zoomed {
            self.host_zoom(false);
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
        // Mutually exclusive with status mode (`d`): entering baseline-aware `c` leaves `d`.
        if !self.changed_only && self.status_mode {
            self.status_mode = false;
        }
        self.changed_only = !self.changed_only;
        self.tree.set_changed_only(self.changed_only, &self.changed);
        self.dispatch_render();
        Effects::redraw()
    }

    /// Toggle sticky git-status mode (`d`): filter the tree to current working-tree status
    /// and force working-tree diffs. Mutually exclusive with baseline-aware `c`.
    fn toggle_status_mode(&mut self) -> Effects {
        // Entering status mode needs a repo (AC-26). Leaving it must stay possible even after a
        // re-root into a non-git directory, otherwise a carried-on `status_mode` can leave the
        // user stuck on an empty filtered tree with no way to turn `d` off.
        if !self.is_git_repo && !self.status_mode {
            return Effects::noop();
        }
        if self.status_mode {
            self.status_mode = false;
            // Leaving status mode: if `c` is not on, restore the full tree.
            if !self.changed_only {
                self.tree.set_changed_only(false, &self.changed);
            }
        } else {
            // Entering status mode turns off baseline-aware changed-only.
            self.changed_only = false;
            self.status_mode = true;
            self.tree.set_changed_only(true, &self.git_status);
        }
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

    /// Open the selected entry with the OS default app (`O`).
    fn open_with_app(&mut self) -> Effects {
        self.hand_off_to_opener(false)
    }

    /// Reveal the selected entry in the OS file manager (`R`).
    fn reveal_in_file_manager(&mut self) -> Effects {
        self.hand_off_to_opener(true)
    }

    /// Hand the selected entry off to the OS opener (open-with-default-app or reveal-in-file-
    /// manager). Read-only: resolves the tree selection (file OR directory, focus-independent) and
    /// launches an external process via the injected opener — never reads or writes the file
    /// (AC-1/2/3/4/12). Non-blocking: a successful hand-off does NOT take over the terminal (no
    /// `clear`), unlike the editor path (AC-6). A missing selection or absent opener is an inert
    /// no-op (AC-5).
    fn hand_off_to_opener(&mut self, reveal: bool) -> Effects {
        // Resolve the selection FIRST and clone its path, so the immutable tree borrow is released
        // before we take a mutable borrow of self.opener (borrow-checker).
        let Some(path) = self.tree.selected().map(|n| n.path.clone()) else {
            return Effects::noop(); // AC-5: nothing selected
        };
        let Some(opener) = self.opener.as_mut() else {
            return Effects::noop(); // no opener injected (defensive; production always injects one)
        };
        let outcome = if reveal {
            opener.reveal(&path)
        } else {
            opener.open(&path)
        };
        match outcome {
            crate::opener::OpenerOutcome::Launched => Effects::redraw(), // AC-6: no `clear`
            crate::opener::OpenerOutcome::NotLaunched(reason) => {
                self.action_notice = Some(if reveal {
                    format!("Could not reveal in file manager: {reason}")
                } else {
                    format!("Could not open with default app: {reason}")
                });
                Effects::redraw()
            }
            crate::opener::OpenerOutcome::NonZeroExit(detail) => {
                self.action_notice = Some(if reveal {
                    format!("File manager exited with {detail}")
                } else {
                    format!("Opener exited with {detail}")
                });
                Effects::redraw()
            }
        }
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
        if self.zoomed {
            self.focus = Focus::Content;
        } else {
            self.focus = Focus::Tree;
            // `z` fully exits full-screen: if the viewer had host-zoomed the pane (via `Z`), release
            // it too, so the two-column split never reappears inside a still-full-screen host pane.
            self.leave_host_zoom();
        }
        Effects::redraw()
    }

    /// The close key (`q`/`Esc`): layered dismissal in order — clear a committed search first,
    /// then un-zoom if zoomed, then quit. So from a committed search the sequence is:
    /// Esc → clears the search; Esc again → un-zooms (if zoomed) or quits. (AC-20, owner UX.)
    fn close_or_unzoom(&mut self) -> Effects {
        // Esc drops an ambient selection first — the outermost layer of the Esc stack, ahead of
        // search / unzoom / quit. (A collapsed selection held mid-press is swallowed too; harmless.)
        if self.content_selection.take().is_some() {
            return Effects::redraw();
        }
        // A committed search (prompt closed, highlights persisting) is dismissed first — Esc/q
        // "come out of the search" before they unzoom or close (layered like unzoom). (owner UX)
        if self.search.is_some() && !self.prompt_open() {
            self.search = None;
            return Effects::redraw();
        }
        if self.zoomed {
            self.zoomed = false;
            self.focus = Focus::Tree;
            // Returning to the split also releases the viewer's own host pane zoom (no-op if `Z`
            // was never used), so `Esc`/`q` never leaves the host pane full-screen behind a split.
            self.leave_host_zoom();
            return Effects::redraw();
        }
        // Annotations are session-only, so quitting destroys them. Confirm first rather than lose
        // work to a stray `q`. The outermost layer, after search/unzoom have had their turn.
        // Opt out with `confirm_discard = false`.
        if self.confirm_discard && !self.annotations.is_empty() {
            self.modal = Modal::DiscardConfirm(DiscardAction::Quit);
            return Effects::redraw();
        }
        // Quitting: release any host pane zoom the viewer opened so it does not outlive the viewer.
        self.leave_host_zoom();
        Effects {
            quit: true,
            ..Default::default()
        }
    }

    /// Quit, releasing any host pane zoom the viewer opened so it does not outlive the viewer.
    fn quit_now(&mut self) -> Effects {
        self.modal = Modal::None;
        self.leave_host_zoom();
        Effects {
            quit: true,
            ..Default::default()
        }
    }

    /// Whether the discard confirm is the open modal. Drives the Presenter and the app's key
    /// routing.
    pub fn discard_confirm_open(&self) -> bool {
        matches!(self.modal, Modal::DiscardConfirm(_))
    }

    /// Carry out a confirmed [`DiscardAction`], discarding the annotations with it.
    fn proceed_with(&mut self, action: DiscardAction) -> Effects {
        match action {
            DiscardAction::Quit => self.quit_now(),
            DiscardAction::SwitchRoot(resolved) => {
                // The confirm held this target across arbitrary user think-time, and `apply_re_root`
                // validates nothing, so re-check what `re_root` checked before committing. Without
                // this, a worktree removed while the dialog was open would clear the annotations AND
                // re-root the viewer to a dead path: AC-16 says a failed switch leaves every piece of
                // state intact, and losing the notes to a switch that itself failed is precisely the
                // loss this confirm exists to prevent.
                self.modal = Modal::None;
                if !resolved.root.is_dir() {
                    self.action_notice = Some(format!(
                        "cannot switch worktree: {} is not an accessible directory",
                        resolved.root.display()
                    ));
                    return Effects::redraw();
                }
                // The current root can also have MOVED under us, making this a no-op switch (AC-11).
                let target_canon = resolved
                    .root
                    .canonicalize()
                    .unwrap_or_else(|_| resolved.root.clone());
                let current_canon = self
                    .root
                    .canonicalize()
                    .unwrap_or_else(|_| self.root.clone());
                if target_canon == current_canon {
                    return Effects::redraw();
                }
                // `apply_re_root` resets the modal and clears the store itself.
                self.apply_re_root(*resolved);
                Effects::redraw()
            }
        }
    }

    /// Route the discard confirm's fixed keys: `y` copies the annotations then proceeds, the
    /// action's own proceed key (`q` to quit, `Enter` to switch) discards them and proceeds, and
    /// `Esc` cancels back to the viewer. Every other key is an inert no-op the modal still owns, so
    /// nothing leaks to a global action.
    ///
    /// `y` only proceeds when the copy actually succeeded: proceeding on a failed clipboard write
    /// would destroy the annotations at the exact moment the viewer promised to save them, so a
    /// failure holds the dialog open with the error showing.
    pub fn handle_discard_confirm_key(&mut self, key: KeyEvent) -> Effects {
        if key.modifiers.difference(KeyModifiers::SHIFT) != KeyModifiers::NONE {
            return Effects::noop();
        }
        let Modal::DiscardConfirm(action) = &self.modal else {
            return Effects::noop();
        };
        let action = action.clone();
        if key.code == action.proceed_key() {
            return self.proceed_with(action);
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if self.copy_annotations_to_clipboard() {
                    self.proceed_with(action)
                } else {
                    Effects::redraw()
                }
            }
            KeyCode::Esc => {
                self.modal = Modal::None;
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Move the tree/content divider by `delta` percentage points, clamped so neither column
    /// can collapse. Pure layout state — no re-render is needed (the content is unchanged).
    fn resize_split(&mut self, delta: i16) -> Effects {
        self.engage_manual_split();
        let next =
            (self.split_pct as i16 + delta).clamp(self.split_floor_pct() as i16, SPLIT_MAX as i16);
        self.split_pct = next as u16;
        Effects::redraw()
    }

    /// First-manual-resize seam: mark the split user-controlled (which lifts the `tree_max_cols`
    /// cap) and, so the tree doesn't jump on that first keypress/drag, seed `split_pct` from the
    /// width the tree is *currently displayed* at (which may be capped below `split_pct`%). A no-op
    /// after the first call, and when no frame has been drawn yet (no geometry to read).
    fn engage_manual_split(&mut self) {
        if self.split_manual {
            return;
        }
        // Convert the displayed tree column to a percentage of the drawn body width, so the resize
        // continues from what's on screen rather than snapping to the uncapped `split_pct`%.
        // `tree_inner` is the tree's TEXT rect: reconstruct the outer column as interior + its two
        // block borders + the 2-cell scrollbar gutter the Presenter reserves when a tree vbar is drawn.
        if let Some(tree) = self.geom.tree_inner
            && self.geom.area_width > 0
        {
            let gutter = if self.geom.tree_vbar.is_some() { 2 } else { 0 };
            let tree_outer = tree.width.saturating_add(2).saturating_add(gutter);
            let pct = (tree_outer as u32 * 100 / self.geom.area_width as u32) as u16;
            self.split_pct = pct.clamp(self.split_floor_pct(), SPLIT_MAX);
        }
        self.split_manual = true;
    }

    /// The lowest split percentage an interactive resize may reach on the current frame: the
    /// pane-aware "≥ [`MIN_TREE_MAX_COLS`](crate::config::MIN_TREE_MAX_COLS) columns" floor, so a hand
    /// resize can pull the tree as narrow as the cap allows on any pane width (a fixed percentage
    /// floor would be far more than the cap's minimum on a very wide pane). Falls back to
    /// [`SPLIT_DRAG_MIN`] before the first frame is drawn (no geometry width yet).
    fn split_floor_pct(&self) -> u16 {
        if self.geom.area_width > 0 {
            crate::presenter::min_tree_split_pct(self.geom.area_width)
        } else {
            SPLIT_DRAG_MIN
        }
    }

    /// Flip the content-wrap state (the `w` key): force it to the opposite of what the current
    /// selection shows now, so a single press always visibly toggles. The override is a session
    /// preference applied uniformly — `Some(true)` wraps everywhere, `Some(false)` unwraps
    /// everywhere — so switching files doesn't spring a surprise wrap on the next one.
    ///
    /// For code AND diffs this is pure layout (delta/bat's output is unchanged — a diff's `-w`
    /// tracks the pane width unconditionally, never this wrap toggle, see
    /// [`diff_render_width`](Self::diff_render_width) — only how the Presenter lays the existing
    /// output out changes). For rendered markdown the wrap state changes glow's `-w` (fit-to-pane
    /// vs. natural width for horizontal scroll), so the content itself is re-rendered — preserving
    /// scroll and search, like a resize reflow. The scroll clamp recomputes from the new layout.
    fn toggle_wrap(&mut self) -> Effects {
        self.wrap_override = Some(!self.effective_wrap());
        self.content_scroll = self.content_scroll.min(self.max_content_scroll());
        self.content_hscroll = self.content_hscroll.min(self.max_content_hscroll());
        // Markdown must re-render at the new wrap width (fit vs. natural); a no-op for other modes,
        // which only need the re-clamp above.
        self.rerender_markdown_reflow();
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

    /// Whether a deferred line-select entry is queued against an in-flight source render (AC-15):
    /// set when `L` enters on a transformed / still-rendering view (the marker waits for the
    /// source-mapped re-render), cleared by `poll` once that render lands and the marker is placed,
    /// or superseded by any newer render dispatch. Exposed for tests.
    pub fn line_select_pending(&self) -> bool {
        self.pending_line_select.is_some()
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

    /// The wrap width to hand a markdown render job: the content pane's drawable text width when
    /// markdown is effectively **wrapped** (the fit-to-pane default) so glow lays the table out to
    /// fit; `None` when unwrapped (the `w` horizontal-scroll view) so glow keeps its base `-w 0`
    /// natural-width layout and the pane scrolls to reveal the whole table, and `None` before the
    /// first draw has measured the pane (`content_width == 0`).
    fn md_wrap_width(&self) -> Option<u16> {
        (self.content_width > 0 && self.effective_wrap()).then_some(self.content_width)
    }

    /// The width to hand a diff/full-context-diff render job: the measured pane width,
    /// unconditionally (`None` only before the first draw has measured one) — unlike
    /// [`md_wrap_width`](Self::md_wrap_width), NOT gated on [`effective_wrap`](Self::effective_wrap).
    /// delta sizes its own layout from the *terminal* width it detects via a tty ioctl on its own
    /// stdout — but here stdout is a plain OS pipe (`Stdio::piped()` in
    /// `render::renderer_command`), never a tty, so that detection always fails and delta falls back
    /// to a fixed, tty-less width instead (observed as 80 columns for bare delta). This chiefly
    /// matters when delta runs `side-by-side` (its own `~/.gitconfig` `[delta]` setting, read
    /// directly since this app never passes `--no-gitconfig`): there, `-w` genuinely governs each
    /// column's wrap width — verified against delta 0.19.2 — so without this it stays stuck at
    /// whatever delta falls back to, not the pane, even as the pane is resized. In delta's default
    /// (non-side-by-side) mode `-w` only changes a decorative rule's width; content isn't
    /// wrapped by it either way (see `render_at_width`'s doc in `app.rs` for the full detail). The
    /// wrap toggle plays no part here: it only ever governs how the *Presenter* lays
    /// out a diff's already-rendered lines (reflow vs. h-scroll), never how wide delta itself
    /// believes the pane to be.
    fn diff_render_width(&self) -> Option<u16> {
        (self.content_width > 0).then_some(self.content_width)
    }

    /// The width to hand `mode`'s render job for the delegate's own layout, at initial dispatch
    /// ([`dispatch_render`](Self::dispatch_render)) where — unlike the two incremental reflow paths
    /// below — there's no "did this actually change" question to optimize, so the two width sources
    /// are simply selected by mode: [`md_wrap_width`](Self::md_wrap_width) for markdown,
    /// [`diff_render_width`](Self::diff_render_width) for a diff, and `None` for syntax content
    /// (bat has no `-w` flag and never wraps or pads, so it is genuinely width-independent).
    fn render_wrap_width(&self, mode: ViewMode) -> Option<u16> {
        match mode {
            ViewMode::RenderedMarkdown => self.md_wrap_width(),
            ViewMode::Diff | ViewMode::FullDiff => self.diff_render_width(),
            ViewMode::SyntaxContent => None,
        }
    }

    /// Re-render the current markdown selection **without** the view-state reset
    /// [`dispatch_render`](Self::dispatch_render) performs — the triggers are a content-pane resize
    /// (while wrapped) and the `w` wrap toggle, neither of which is a selection change, so scroll
    /// position and any active search must survive. Only rendered markdown is re-rendered here:
    /// glow's layout is tied to the wrap width we pass it (fit-to-pane vs. natural width), so *any*
    /// wrap-toggle flip must re-run glow — including wrapped → unwrapped, which still needs a fresh
    /// render to drop back to natural width, so this must NOT skip just because the resulting width
    /// is `None`. Every other mode is a no-op here: diffs get their own reflow path
    /// ([`rerender_diff_reflow`](Self::rerender_diff_reflow), since delta's width is independent of
    /// this wrap toggle entirely) and syntax content never needs one (bat is width-independent). The
    /// current content stays on screen until the new render lands (no `Rendering…` placeholder), so
    /// a live split-drag or `w` press doesn't flash; the worker collapses the backlog so only the
    /// final state renders. [`poll`] applies the result by `seq` and, seeing it flagged in
    /// `reflow_seq`, keeps the scroll and recomputes an active search.
    fn rerender_markdown_reflow(&mut self) {
        let Some(node) = self.tree.selected() else {
            return;
        };
        if node.kind != NodeKind::File
            || self.effective_mode(&node.path) != ViewMode::RenderedMarkdown
        {
            return;
        }
        self.latest_seq += 1;
        let seq = self.latest_seq;
        self.reflow_seq = Some(seq);
        let rel = self.rel(&node.path);
        // Ignore a send error: if the worker is gone the current content simply stays; `poll` will
        // never receive a result for this seq, which is fine (nothing was cleared).
        let _ = self.job_tx.send(RenderJob {
            seq,
            path: node.path,
            rel,
            mode: ViewMode::RenderedMarkdown,
            baseline: self.baseline,
            is_git: self.is_git_repo,
            directory_diff: false,
            wrap_width: self.md_wrap_width(),
        });
    }

    /// Re-render the current diff/full-context-diff selection on a content-pane resize — the same
    /// shape as [`rerender_markdown_reflow`](Self::rerender_markdown_reflow), but a diff always
    /// re-renders on a width change (delta's width is unconditional on the wrap toggle — see
    /// [`diff_render_width`](Self::diff_render_width)'s doc), so unlike markdown there's no wrap-
    /// toggle path calling this: `toggle_wrap` never needs it, since a diff's delta invocation
    /// doesn't depend on the wrap axis at all. A no-op for every other mode.
    fn rerender_diff_reflow(&mut self) {
        let Some(node) = self.tree.selected() else {
            return;
        };
        if node.kind != NodeKind::File {
            return;
        }
        let mode = self.effective_mode(&node.path);
        if !matches!(mode, ViewMode::Diff | ViewMode::FullDiff) {
            return;
        }
        self.latest_seq += 1;
        let seq = self.latest_seq;
        self.reflow_seq = Some(seq);
        let rel = self.rel(&node.path);
        let _ = self.job_tx.send(RenderJob {
            seq,
            path: node.path,
            rel,
            mode,
            baseline: self.baseline,
            is_git: self.is_git_repo,
            directory_diff: false,
            wrap_width: self.diff_render_width(),
        });
    }

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
        // Same for a queued line-select entry (AC-15): a newer render supersedes an OLDER auto-switch
        // entry. The auto-switch path re-sets `pending_line_select` AFTER calling this (capturing the
        // fresh `latest_seq`), so its own entry survives; a re-root clears it too, since `re_root` ends
        // in `dispatch_render` — the same mechanism that clears `pending_goto`.
        self.pending_line_select = None;
        // AC-20: any displayed-content change (file-select, view-cycle, baseline-toggle, refresh,
        // go-to-line auto-switch, re-root, etc.) clears a committed search and its highlighting.
        // `refresh_search` (the incremental-typing path) only calls `scroll_to_line` and sets
        // `self.search` directly — it does NOT call `dispatch_render` — so live typing is NOT
        // wiped by this clear.
        self.search = None;
        // Drop an ambient selection on any content change: its line/char coordinates only mean
        // something against the body it was dragged over, so a stale highlight (and copy) must not
        // carry onto new content. Scrolling keeps it — it doesn't dispatch, and the coords stay valid.
        self.content_selection = None;

        let Some(node) = self.tree.selected() else {
            // No visible node: an empty tree or a filter (changed-only, gitignore, etc.)
            // that matched nothing. Show guidance instead of a blank pane.
            return self.clear_content(EmptyReason::NoFiles);
        };
        // Git-status mode (`d`): directories render a pathspec-scoped working-tree diff instead of
        // empty-state guidance. Files still go through the normal job path with forced Diff.
        let (mode, directory_diff, baseline) = if self.status_mode && self.is_git_repo {
            match node.kind {
                NodeKind::Dir => (ViewMode::Diff, true, Baseline::Head),
                NodeKind::File => (
                    ViewMode::Diff,
                    false,
                    Baseline::Head, // status mode always diffs the working tree, not merge-base
                ),
            }
        } else if node.kind != NodeKind::File {
            // A directory is selected outside status mode — no content; show guidance.
            return self.clear_content(EmptyReason::Directory);
        } else {
            (self.effective_mode(&node.path), false, self.baseline)
        };
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
                baseline,
                is_git: self.is_git_repo,
                directory_diff,
                wrap_width: self.render_wrap_width(mode),
            })
            .is_ok()
        {
            self.content = Text::raw("Rendering\u{2026}");
            self.content_notices.clear();
            self.content_source = None; // the placeholder has no source; the landing render brings its own
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
        self.content_source = None; // guidance text has no source behind it
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
                // A width-reflow re-render (a resize, not a selection change): its content replaces
                // the current body, but scroll and search must survive — the user did not navigate.
                // Cleared unconditionally so a later selection-change render is never mistaken for a
                // reflow (its seq won't match a stale value anyway, but keep the flag tight).
                let is_reflow = self.reflow_seq == Some(seq);
                self.reflow_seq = None;
                self.content = result.content;
                // Covers the placeholder→land window: a selection dragged over "Rendering…" after
                // dispatch_render's clear must not carry its stale coordinates onto the new body.
                // (Also dropped on a reflow: an ambient selection's line/col coords are against the
                // pre-reflow rendered lines, so they no longer point at the same text.)
                self.content_selection = None;
                self.content_notices = result.notices;
                self.content_source = result.source; // in lockstep with `content` (copy fidelity)
                self.applied_seq = seq; // the displayed content is now this render (go-to-line guard)
                // A reflow keeps the user's scroll position, but the reflowed body may have a
                // different rendered-row count (a table re-lays-out at the new width), so re-clamp
                // the offset to the new content. A selection-change render already reset scroll to 0
                // in dispatch_render, so this is gated to the reflow path.
                if is_reflow {
                    self.content_scroll = self.content_scroll.min(self.max_content_scroll());
                }
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
                // A queued line-select entry (auto-switch from a transformed view, AC-15) opens once
                // ITS render lands — the same seq-guard `pending_goto` uses. The source-mapped content
                // is now applied and the scroll was reset to the top by `dispatch_render`, so the marker
                // lands on the top visible source line, mapped through `line_at_content_row` so it is
                // correct even if the `w` wrap override is on. A render that landed EMPTY (a zero-line
                // body) opens nothing: the marker would otherwise fabricate a `path:1` reference to a
                // line that does not exist — the same guard the synchronous `L` entry has (its
                // `content.lines.is_empty()` inert branch). The pending entry is cleared either way.
                if let Some(pseq) = self.pending_line_select
                    && pseq == seq
                {
                    if !self.content.lines.is_empty() {
                        let last = self.content.lines.len();
                        let top = self
                            .line_at_content_row(self.content_scroll as usize)
                            .clamp(1, last);
                        self.modal = Modal::LineSelect(LineSelectState::new(top));
                    }
                    self.pending_line_select = None;
                }
                // A render that was in flight when a search was opened/committed lands here and
                // swaps self.content; matches computed against the OLD content are now stale.
                // Mirror dispatch_render's AC-20 clear: recompute an open Search prompt against
                // the new content, else drop a committed search — UNLESS this was a width reflow,
                // where a committed search must be recomputed (not dropped), so a resize does not
                // silently clear the user's active highlighting.
                if self.modal.prompt().map(|p| p.mode) == Some(crate::infile::PromptMode::Search) {
                    self.refresh_search();
                } else if is_reflow {
                    self.recompute_committed_search();
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
    /// Git-status mode (`d`) forces Diff regardless of override — the mode is the product.
    fn effective_mode(&self, path: &Path) -> ViewMode {
        if self.status_mode && self.is_git_repo {
            return ViewMode::Diff;
        }
        self.overrides
            .get(path)
            .copied()
            .unwrap_or_else(|| default_mode(&self.descriptor(path)))
    }

    /// The View Policy facts about a file: markdown by extension, changed by the cached
    /// changed-set (so it tracks the active baseline). In status mode, "changed" means present
    /// in the working-tree status set so Diff is always the default for filtered files.
    fn descriptor(&self, path: &Path) -> FileDescriptor {
        FileDescriptor {
            path: path.to_path_buf(),
            is_markdown: is_markdown(path),
            is_changed: self.is_changed(path),
        }
    }

    fn is_changed(&self, path: &Path) -> bool {
        self.rel(path)
            .map(|rel| {
                if self.status_mode {
                    self.git_status.contains_key(&rel)
                } else {
                    self.changed.contains_key(&rel)
                }
            })
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
    /// Dragging out a character-granular text selection in the content pane — in L mode (handled
    /// in `handle_line_select_mouse`, on the modal's state) or ambient (handled in
    /// `handle_column_mouse`, on `content_selection`; the release auto-copies).
    ContentSelect,
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

    // ---- T-6 Bindings Wiring (AC-16, AC-23) --------------------------------------------
    //
    // These exercise the wiring end-to-end: a `[keys]` remap resolved via `input::resolve_bindings`
    // and stored through `set_keybindings` must reach the run loop's decode source
    // (`controller.bindings()`), and the resolver's `KeyLoadOutcome` must be forwarded/stored so the
    // T-7 Keybindings overlay can surface rejected entries. AC-23 (read-only) is reviewer-checked:
    // the whole binding path here only *reads* the already-loaded config and builds in-memory state
    // (`resolve_bindings` is pure; `set_keybindings` just stores) — no filesystem or git write is
    // reached, so there is nothing for a test to assert beyond that (the empanel gate confirms it).

    // `super::*` re-exports everything mod.rs has in scope, incl. the injected-component traits, the
    // `Controller` internals, `Resolved`/`Baseline`/`Status`, and the crossterm key types + `Intent`
    // + `BTreeMap`/`Arc` its own `use`s pulled in — so only the names mod.rs does NOT already import
    // are added below (`KeySpec` and the `input` module path).
    use super::*;
    use crate::config::KeySpec;
    use crate::input;

    /// A no-op Git Service stub (`is_git_repo = false` below means it is never actually queried).
    struct StubGit;
    impl GitService for StubGit {
        fn status(&self) -> BTreeMap<PathBuf, Status> {
            BTreeMap::new()
        }
        fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
            BTreeMap::new()
        }
        fn diff(&self, _rel: &Path, _baseline: Baseline, _full: bool) -> String {
            String::new()
        }
        fn diff_directory(&self, _rel_dir: &Path, _baseline: Baseline) -> String {
            String::new()
        }
    }

    /// A Content Renderer stub returning empty text — the wiring tests never inspect the pane.
    struct StubContent;
    impl ContentProvider for StubContent {
        fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
            RenderResult {
                content: Text::raw(""),
                notices: Vec::new(),
                source: None,
            }
        }
    }

    struct StubEditor;
    impl EditorHandoff for StubEditor {
        fn open(&mut self, _file: &Path) -> EditorOutcome {
            EditorOutcome::NoTakeover
        }
    }

    struct StubClipboard;
    impl Clipboard for StubClipboard {
        fn copy(&mut self, _text: &str) -> io::Result<()> {
            Ok(())
        }
    }

    /// Build a minimal controller over an empty (non-repo) temp dir with fully stubbed components,
    /// so the wiring tests can call `set_keybindings` / `bindings()` without a real git/renderer.
    fn wiring_controller() -> Controller {
        let root = std::env::temp_dir().join(format!(
            "hfv-kbwire-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&root);
        let resolved = Resolved {
            repo_root: None,
            root,
            is_git_repo: false,
            is_worktree: false,
            base_branch: None,
        };
        let git: Arc<dyn GitService> = Arc::new(StubGit);
        let components = Components {
            providers: Box::new(move |_r: &Resolved| RootProviders {
                git: Arc::clone(&git),
                content: Box::new(StubContent),
            }),
            editor: Box::new(StubEditor),
            clipboard: Box::new(StubClipboard),
            renderers: None,
        };
        Controller::new(resolved, Baseline::Head, components)
    }

    #[test]
    fn remap_takes_effect_through_the_controller_bindings() {
        // AC-16 end-to-end: a `[keys]` remap of `refresh` to `g`, resolved and stored on the
        // controller, makes `g` decode to Refresh through `controller.bindings()` (the run loop's
        // decode source) while the displaced default `r` no longer decodes — replace-semantics
        // reached the run loop, so the run loop now decodes against config-derived bindings.
        let mut ctrl = wiring_controller();
        let mut keys: BTreeMap<String, KeySpec> = BTreeMap::new();
        keys.insert("refresh".into(), KeySpec::One("g".into()));

        let (bindings, outcome) = input::resolve_bindings(input::registry(), Some(&keys));
        ctrl.set_keybindings(bindings, outcome);

        let g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        let r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(
            input::decode(g, ctrl.bindings()),
            Some(Intent::Refresh),
            "'g' decodes to Refresh through the controller's config-derived bindings"
        );
        assert_eq!(
            input::decode(r, ctrl.bindings()),
            None,
            "the displaced default 'r' no longer decodes to Refresh"
        );
    }

    #[test]
    fn rejected_entry_outcome_is_stored_on_the_controller() {
        // AC-16 surfacing: a rejected `[keys]` entry (an unknown intent name) is forwarded through
        // `set_keybindings` and stored, so the outcome the T-7 overlay reads is non-empty.
        let mut ctrl = wiring_controller();
        let mut keys: BTreeMap<String, KeySpec> = BTreeMap::new();
        keys.insert("bogus_intent".into(), KeySpec::One("g".into()));

        let (bindings, outcome) = input::resolve_bindings(input::registry(), Some(&keys));
        assert!(!outcome.is_empty(), "an unknown intent name is rejected");
        ctrl.set_keybindings(bindings, outcome);

        assert!(
            !ctrl.key_load_outcome().is_empty(),
            "the rejected-entry outcome is stored on the controller (AC-16 surfacing path)"
        );
    }

    // ---- T-7 Keybindings View-Model (AC-19) --------------------------------------------

    #[test]
    fn open_help_appends_keybindings_section_only_after_set_keybindings_display() {
        // T-7/AC-19: with the Keybindings display injected, the `?` overlay gains a "Keybindings"
        // section (appended LAST). Without it, the overlay has no such section — so existing
        // count/label-based overlay tests stay green for controllers that never wire it.
        let mut ctrl = wiring_controller();

        // Before injection: no Keybindings section.
        ctrl.open_help();
        assert!(
            !ctrl
                .help_state()
                .expect("help open")
                .section_labels()
                .contains(&"Keybindings"),
            "without set_keybindings_display the overlay must have no Keybindings section"
        );
        ctrl.close_help();

        // After injection: a "Keybindings" section is present.
        ctrl.set_keybindings_display();
        ctrl.open_help();
        let help = ctrl.help_state().expect("help open");
        assert!(
            help.section_labels().contains(&"Keybindings"),
            "set_keybindings_display must make open_help append a Keybindings section"
        );

        // ...and its BODY carries the real registry content, not just the label: assert the wiring
        // (set_keybindings_display -> stored field -> open_help -> section body) preserved a known
        // action description, so a swapped-argument or wrong-text regression in the glue is caught
        // here, not only in help.rs's isolated `keybindings_text` tests.
        let kb = help
            .sections
            .iter()
            .find(|s| s.label == "Keybindings")
            .expect("Keybindings section present");
        let body: String = kb
            .body
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|sp| sp.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("Re-read git state"),
            "the appended Keybindings section body must carry the registry descriptions, got: {body}"
        );
    }
}
