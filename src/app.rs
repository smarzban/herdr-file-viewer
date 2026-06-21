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
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
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
use std::sync::Arc;
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

    let git: Arc<dyn GitService> = Arc::new(LiveGit {
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
    // Mouse is additive to the keyboard-first design (AC-18): herdr forwards mouse events to a
    // pane that requests capture, while reserving Shift+mouse for the terminal's own
    // selection/copy. Best-effort so a terminal without mouse support still runs.
    let _ = execute!(io::stdout(), EnableMouseCapture);
    let outcome = event_loop(&mut terminal, &mut controller);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::try_restore()?;
    outcome
}

/// Draw (only when something changed), read one input (or time out), drain renders; repeat
/// until the Close intent. Drawing only when `dirty` avoids re-walking the filesystem (the
/// tree enumeration in `view_state`) on every idle tick.
fn event_loop(terminal: &mut DefaultTerminal, controller: &mut Controller) -> io::Result<()> {
    let mut dirty = true; // paint the first frame
    loop {
        if dirty {
            terminal.draw(|frame| {
                controller.set_width(frame.area().width);
                let view: ViewState = controller.view_state();
                let (cw, ch) = presenter::draw(frame, &view);
                // Feed the drawn content viewport back so content scrolling can be clamped to
                // it on the next intent, and the hit-test geometry so a mouse event maps to the
                // live layout.
                controller.set_content_viewport(cw, ch);
                controller.set_pane_geometry(presenter::geometry(frame.area(), &view));
            })?;
            dirty = false;
        }

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && let Some(intent) = input::map_key(key) =>
                {
                    let fx = controller.handle(intent);
                    if fx.clear {
                        // An external program (an editor) drew over the screen, so force a
                        // full repaint: `terminal.clear()` resets ratatui's back buffer so the
                        // next draw rewrites every cell (a plain redraw would only diff against
                        // the stale buffer and skip cells). `clear()` first issues a cursor-
                        // position (DSR) query to preserve the cursor; a real interactive
                        // terminal answers it, so this succeeds and the repaint is full. We
                        // make it best-effort because a terminal that never answers (e.g. a
                        // headless/test pty) must not crash the viewer — there the repaint is
                        // skipped and the pane may stay stale until the next change, which is
                        // strictly better than aborting (constitution: the loop never crashes).
                        // The e2e editor test exercises exactly this failure path.
                        let _ = terminal.clear();
                        dirty = true;
                    }
                    if fx.quit {
                        return Ok(());
                    }
                    dirty |= fx.redraw;
                }
                // Mouse input (capture is enabled in `run`): clicks select / activate, the
                // wheel scrolls, dragging the divider resizes. It never quits the viewer.
                Event::Mouse(me) => {
                    let fx = controller.handle_mouse(me);
                    if fx.clear {
                        let _ = terminal.clear();
                        dirty = true;
                    }
                    dirty |= fx.redraw;
                }
                // The pane was resized: redraw so the two-column layout and content reflow to
                // the new geometry. `terminal.draw` autoresizes its buffers before drawing, so
                // marking the frame dirty is enough.
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
        // A render finished by the worker becomes visible on the next draw (AC-23).
        if let Some(fx) = controller.poll() {
            dirty |= fx.redraw;
        }
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
            // No terminal change yet, so nothing to restore.
            return Err(io::Error::other("no editor configured (set $EDITOR)"));
        };
        // Once we touch the terminal we must always try to restore it, even if suspending or
        // the editor itself fails — otherwise the viewer would keep running with raw mode off
        // or outside the alternate screen.
        let suspended = suspend_tui();
        let launched = if suspended.is_ok() {
            EditorLauncher::new(editor).open(file, Target::Editor, &mut ProcessSpawner)
        } else {
            // Don't run the editor over a half-suspended terminal.
            Ok(())
        };
        let resumed = resume_tui();
        // Report the first failure in order: suspend, then the editor, then restore.
        suspended?;
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

/// Resolve glow's `-s` style argument: the bundled palette style if it ships in the
/// executable's own install tree (prose in the host ANSI palette, stripped `##` markers,
/// syntax-highlighted code blocks), else glow's built-in `dark` so markdown still renders
/// rather than failing on a missing `-s` file.
///
/// The style is a glow *argument* — trusted, operator-configured — so it is located ONLY
/// relative to the executable, **never the cwd**: the viewed repo is untrusted and could
/// otherwise plant an `assets/markdown-style.json` for the cwd fallback to pick up, turning
/// repo content into the (trusted) renderer's config.
fn markdown_style() -> String {
    bundled_style_path(std::env::current_exe().ok().as_deref())
        .unwrap_or_else(|| "dark".to_string())
}

/// Find `assets/markdown-style.json` among the ancestors of the executable — the trusted
/// install dir (the binary runs from `<root>/target/<profile>/herdr-file-viewer`, with the
/// asset at `<root>/assets/…`, under both `herdr plugin link` and `plugin install`). Pure
/// over its input, so it is unit-testable and provably never consults the cwd. `None` (→
/// caller uses `dark`) when `exe` is absent or no ancestor bundles the asset.
fn bundled_style_path(exe: Option<&Path>) -> Option<String> {
    exe?.ancestors()
        .map(|anc| anc.join("assets/markdown-style.json"))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.to_string_lossy().into_owned())
}

/// The default external renderers (the documented runtime deps). Each reads the untrusted
/// content on **stdin** (never as an argument); a missing one degrades to plain text +
/// notice (AC-24/25). `{name}` is substituted with the sanitized file name for language
/// detection.
fn default_renderers() -> Renderers {
    Renderers {
        // glow's default `auto` style downgrades to the plain "notty" renderer when stdout is a
        // pipe (the viewer always captures it), leaving `#`, `**`, etc. literal — so a concrete
        // style is forced (the bundled palette style, else `dark`) and color is forced on the
        // subprocess (see `render::renderer_command`). `-w 0` disables glow's own wrapping/
        // line-padding: otherwise it pads every line to 80 cols, and in a narrower pane that
        // trailing padding wraps to a blank row after each line ("gaps"); the Presenter's own
        // wrap reflows to the actual pane width instead.
        markdown: vec![
            "glow".into(),
            "-s".into(),
            markdown_style(),
            "-w".into(),
            "0".into(),
            "-".into(),
        ],
        // delta already colorizes piped output (its default), and has no `--color=always` flag.
        diff: vec!["delta".into()],
        syntax: vec![
            "bat".into(),
            "--color=always".into(),
            // `numbers` shows a line-number gutter (still shown when piped, given --color).
            "--style=numbers".into(),
            "--paging=never".into(),
            "--file-name={name}".into(),
            "-".into(),
        ],
        timeout: RENDER_TIMEOUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh empty temp dir for a test (no tempfile dep — matches the project's hermetic
    /// test style). Distinct per `tag` so parallel unit tests don't collide.
    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hfv-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn bundled_style_is_found_in_the_executables_install_tree() {
        // A binary at <root>/target/release/<bin> resolves <root>/assets/markdown-style.json,
        // so glow is pointed at the bundled palette style (not the built-in `dark`).
        let root = tmp("bundled-present");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/markdown-style.json"), "{}").unwrap();
        let exe = root.join("target/release/herdr-file-viewer");
        let s = bundled_style_path(Some(&exe)).expect("style found in the install tree");
        assert!(s.ends_with("markdown-style.json"), "points at the bundled style: {s}");
        assert!(Path::new(&s).is_file(), "the referenced style file exists: {s}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_bundled_style_yields_none_and_never_consults_the_cwd() {
        // Security regression: the style is a trusted glow argument, located ONLY via the
        // executable's tree — never the cwd, which may be an untrusted viewed repo. Here the
        // exe's tree ships no asset, so we get None EVEN THOUGH the real cwd (this repo) ships
        // one. `markdown_style()` then falls back to glow's built-in `dark`.
        let root = tmp("bundled-absent"); // deliberately no assets/ created
        let exe = root.join("target/release/herdr-file-viewer");
        assert_eq!(bundled_style_path(Some(&exe)), None, "no asset in the install tree → None");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_executable_path_yields_none() {
        assert_eq!(bundled_style_path(None), None);
    }

    #[test]
    fn markdown_renderer_forces_a_concrete_glow_style() {
        // Regression: glow's default `auto` style degrades to the plain "notty" renderer when
        // stdout is a pipe (as the viewer always captures it), leaving literal `#`/`**`. A
        // concrete `-s` style (the bundled file or `dark`) must be passed, and `-w 0` must
        // follow `-w` to disable glow's line-padding (else blank-row gaps in a narrow pane).
        let r = default_renderers();
        let s = r.markdown.iter().position(|a| a == "-s");
        assert!(
            s.is_some_and(|i| r.markdown.get(i + 1).is_some_and(|v| !v.is_empty())),
            "a concrete -s style is passed: {:?}",
            r.markdown
        );
        let w = r.markdown.iter().position(|a| a == "-w");
        assert!(
            w.is_some_and(|i| r.markdown.get(i + 1).is_some_and(|v| v == "0")),
            "glow width disabled with `-w 0`: {:?}",
            r.markdown
        );
    }

    #[test]
    fn syntax_renderer_forces_color_and_line_numbers() {
        // bat, like glow, must be told to colorize since its output is piped, not a TTY; and
        // `--style=numbers` shows the line-number gutter the viewer wants for code.
        let r = default_renderers();
        assert!(r.syntax.iter().any(|a| a == "--color=always"), "bat color forced: {:?}", r.syntax);
        assert!(r.syntax.iter().any(|a| a == "--style=numbers"), "bat line numbers: {:?}", r.syntax);
    }
}
