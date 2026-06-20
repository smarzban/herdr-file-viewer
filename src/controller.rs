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

use crate::git::{Baseline, Status};
use crate::intent::Intent;
use crate::presenter::Focus;
use crate::tree::{NodeKind, TreeModel};
use crate::view_policy::{FileDescriptor, ViewMode, applicable_modes, default_mode};
use ratatui::text::Text;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};

/// Read-only git queries the controller coordinates. Behind a trait so tests stub it and
/// the run loop injects an implementation bound to the real repository.
pub trait GitService {
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
    pub git: Box<dyn GitService>,
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
    git: Box<dyn GitService>,
    provider: Box<dyn ContentProvider>,
    editor: Box<dyn EditorHandoff>,
}

impl Controller {
    /// Build the controller rooted at `root`. When `is_git_repo`, the initial working-tree
    /// status (tree markers, AC-7) and the changed-set against `baseline` are loaded from
    /// git; otherwise the viewer is a plain browser (AC-26). The initial selection's content
    /// is rendered so the first frame is populated.
    pub fn new(root: PathBuf, is_git_repo: bool, baseline: Baseline, components: Components) -> Self {
        let Components { git, content, editor } = components;
        let mut ctrl = Controller {
            tree: TreeModel::new(root.clone()),
            root,
            is_git_repo,
            baseline,
            show_ignored: false,
            changed_only: false,
            focus: Focus::Tree,
            width: 0,
            changed: BTreeMap::new(),
            overrides: HashMap::new(),
            content: Text::raw(""),
            content_notices: Vec::new(),
            action_notice: None,
            git,
            provider: content,
            editor,
        };
        if is_git_repo {
            let status = ctrl.git.status();
            ctrl.tree.set_status(&status);
            ctrl.changed = ctrl.git.changed_set(baseline);
        }
        ctrl.refresh_content();
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
            Intent::Close => Effects { quit: true, ..Default::default() },
        }
    }

    fn navigate(&mut self, delta: isize) -> Effects {
        self.tree.move_cursor(delta);
        self.refresh_content();
        Effects::redraw()
    }

    fn expand(&mut self) -> Effects {
        if let Some(node) = self.tree.selected()
            && node.kind == NodeKind::Dir
        {
            self.tree.expand(&node.path);
            return Effects::redraw();
        }
        Effects::noop()
    }

    fn collapse(&mut self) -> Effects {
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
        Effects::redraw()
    }

    fn toggle_changed_only(&mut self) -> Effects {
        if !self.is_git_repo {
            return Effects::noop(); // inert without git (AC-26)
        }
        self.changed_only = !self.changed_only;
        self.tree.set_changed_only(self.changed_only, &self.changed);
        self.refresh_content();
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
        self.refresh_content(); // a diff is relative to the baseline, so it must re-render
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
        self.refresh_content();
        Effects::redraw()
    }

    fn open_in_editor(&mut self) -> Effects {
        let Some(node) = self.tree.selected() else { return Effects::noop() };
        if node.kind != NodeKind::File {
            return Effects::noop();
        }
        match self.editor.open(&node.path) {
            // The hand-off took the terminal: force a full repaint on return.
            Ok(true) => Effects { redraw: true, clear: true, ..Default::default() },
            Ok(false) => Effects::redraw(),
            Err(e) => {
                self.action_notice = Some(format!("Could not open editor: {e}"));
                Effects::redraw()
            }
        }
    }

    fn toggle_focus(&mut self) -> Effects {
        self.focus = match self.focus {
            Focus::Tree => Focus::Content,
            Focus::Content => Focus::Tree,
        };
        Effects::redraw()
    }

    // ---- content coordination ----------------------------------------------------------

    /// Re-render the content pane for the current selection. A directory or empty selection
    /// shows nothing; a file is rendered in its effective mode, pulling the raw diff from git
    /// for diff mode.
    fn refresh_content(&mut self) {
        let Some(node) = self.tree.selected() else {
            self.content = Text::raw("");
            self.content_notices.clear();
            return;
        };
        if node.kind != NodeKind::File {
            self.content = Text::raw("");
            self.content_notices.clear();
            return;
        }
        let mode = self.effective_mode(&node.path);
        let raw_diff = if mode == ViewMode::Diff && self.is_git_repo {
            self.rel(&node.path).map(|rel| self.git.diff(&rel, self.baseline))
        } else {
            None
        };
        let result = self.provider.render(&node.path, mode, raw_diff.as_deref());
        self.content = result.content;
        self.content_notices = result.notices;
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

/// Whether a path names a markdown file (by extension, case-insensitive).
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}
