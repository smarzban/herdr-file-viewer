//! App wiring — assemble the real components and run the terminal event loop (T-20).
//!
//! [`run`] is the binary's body: read the herdr launch context, resolve the root and
//! git-presence, build the live Git Service / Content Renderer / Editor Launcher behind the
//! controller's traits, then drive a draw → input → poll loop over a ratatui terminal until
//! the Close intent (AC-20). The terminal is restored on every exit path — including a panic,
//! via the hook `ratatui::try_init` installs.

use crate::controller::{
    Components, ContentProvider, Controller, EditorHandoff, GitService, RenderResult,
};
use crate::editor::{EditorLauncher, Spawner, Target};
use crate::git::{self, Baseline, Status};
use crate::presenter::{self, ViewState};
use crate::render::{self, Prepared, Renderers};
use crate::view_policy::ViewMode;
use crate::{host, input, root};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::DefaultTerminal;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// How long the input poll blocks each tick before draining finished off-thread renders, so
/// late content appears promptly without the loop busy-spinning.
const TICK: Duration = Duration::from_millis(50);

/// Per-render wall-clock budget for an external renderer before the plain-text fallback.
const RENDER_TIMEOUT: Duration = Duration::from_secs(5);

/// Wire the components and run the viewer until the user closes it.
pub fn run() -> io::Result<()> {
    let ctx = host::from_env();
    let resolved = root::resolve(&ctx);
    let baseline = git::default_baseline(&resolved);

    let git: Box<dyn GitService> = Box::new(LiveGit {
        // In a non-repo there is no repo_root; git is never queried then, but a path is still
        // required, so fall back to the tree root.
        repo_root: resolved.repo_root.clone().unwrap_or_else(|| resolved.root.clone()),
        base_hint: resolved.base_branch.clone(),
    });
    let content: Box<dyn ContentProvider> =
        Box::new(LiveContent { root: resolved.root.clone(), renderers: default_renderers() });
    let editor: Box<dyn EditorHandoff> =
        Box::new(LiveEditor { editor: std::env::var_os("EDITOR") });

    let mut controller = Controller::new(
        resolved.root.clone(),
        resolved.is_git_repo,
        baseline,
        Components { git, content, editor },
    );

    let mut terminal = ratatui::try_init()?;
    let outcome = event_loop(&mut terminal, &mut controller);
    ratatui::try_restore()?;
    outcome
}

/// Draw, read one input (or time out), drain renders; repeat until the Close intent.
fn event_loop(terminal: &mut DefaultTerminal, controller: &mut Controller) -> io::Result<()> {
    loop {
        terminal.draw(|frame| {
            controller.set_width(frame.area().width);
            let view: ViewState = controller.view_state();
            presenter::draw(frame, &view);
        })?;

        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && let Some(intent) = input::map_key(key)
        {
            let fx = controller.handle(intent);
            if fx.clear {
                terminal.clear()?; // an editor took the screen — force a full repaint
            }
            if fx.quit {
                return Ok(());
            }
        }
        // Surface any content finished by the render worker on the next draw (AC-23).
        controller.poll();
    }
}

/// The live Git Service: read-only queries against the resolved repository (AC-7/9/16).
struct LiveGit {
    repo_root: PathBuf,
    base_hint: Option<String>,
}

impl GitService for LiveGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        git::status(&self.repo_root)
    }
    fn changed_set(&self, baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        git::changed_set(&self.repo_root, baseline, self.base_hint.as_deref())
    }
    fn diff(&self, rel_path: &Path, baseline: Baseline) -> String {
        git::diff(&self.repo_root, rel_path, baseline, self.base_hint.as_deref())
    }
}

/// The live Content Renderer: classify + delegate to the external renderers, with guards.
struct LiveContent {
    root: PathBuf,
    renderers: Renderers,
}

impl ContentProvider for LiveContent {
    fn render(&self, path: &Path, mode: ViewMode, raw_diff: Option<&str>) -> RenderResult {
        // Diff mode renders from git's diff text, not the file bytes — so a deleted or binary
        // file still shows its diff (AC-9); other modes classify first (binary / size guards,
        // AC-12/13). `Prepared::Binary` is inert for the diff path inside `render`.
        let prepared = if mode == ViewMode::Diff {
            Prepared::Binary
        } else {
            render::classify(&self.root, path)
        };
        let name = path.file_name().and_then(OsStr::to_str);
        let (content, notice) = render::render(&self.renderers, &prepared, mode, raw_diff, name);
        RenderResult { content, notices: notice.into_iter().collect() }
    }
}

/// The live Editor Launcher: spawn `$EDITOR <file>` as a blocking hand-off, suspending and
/// restoring the TUI around it. A missing `$EDITOR` is a non-fatal error the controller
/// surfaces as a notice.
struct LiveEditor {
    editor: Option<OsString>,
}

impl EditorHandoff for LiveEditor {
    fn open(&mut self, file: &Path) -> io::Result<bool> {
        let Some(editor) = self.editor.clone() else {
            return Err(io::Error::other("no editor configured (set $EDITOR)"));
        };
        suspend_tui()?;
        let launched = EditorLauncher::new(editor).open(file, Target::Editor, &mut ProcessSpawner);
        let resumed = resume_tui();
        // Report the editor's own failure first, then any failure to restore the terminal.
        launched?;
        resumed?;
        Ok(true) // the editor drew over the screen → the run loop forces a full repaint
    }
}

/// Runs the editor command and waits for it (the hand-off is synchronous, as for a terminal
/// editor like vim). Only the local-spawn path is used by the keyboard intent; the new-pane
/// hand-off lives in the Host Adapter (T-17) and is not reached here.
struct ProcessSpawner;

impl Spawner for ProcessSpawner {
    fn spawn(&mut self, argv: &[OsString]) -> io::Result<()> {
        let (prog, args) =
            argv.split_first().ok_or_else(|| io::Error::other("empty editor command"))?;
        let status = Command::new(prog).args(args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("editor exited with {status}")))
        }
    }
    fn open_pane(&mut self, _editor: &OsStr, _file: &Path) -> io::Result<()> {
        // v1's keyboard OpenInEditor uses the configured-editor path (Target::Editor); the
        // new-pane sequence is implemented and tested in the Host Adapter (host.rs).
        Err(io::Error::other("new-pane hand-off is not on the v1 keyboard path"))
    }
}

/// Leave raw mode + the alternate screen so an external editor owns a clean terminal.
fn suspend_tui() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)
}

/// Re-enter raw mode + the alternate screen after the editor returns.
fn resume_tui() -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)
}

/// The default external renderers (the documented runtime deps). Each reads the untrusted
/// content on **stdin** (never as an argument); a missing one degrades to plain text +
/// notice (AC-24/25). `{name}` is substituted with the sanitized file name for language
/// detection.
fn default_renderers() -> Renderers {
    Renderers {
        markdown: vec!["glow".into(), "-".into()],
        diff: vec!["delta".into()],
        syntax: vec![
            "bat".into(),
            "--color=always".into(),
            "--style=plain".into(),
            "--paging=never".into(),
            "--file-name={name}".into(),
            "-".into(),
        ],
        timeout: RENDER_TIMEOUT,
    }
}
