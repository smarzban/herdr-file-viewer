//! App wiring — assemble the real components and run the terminal event loop (T-20).
//!
//! [`run`] is the binary's body: read the herdr launch context, resolve the root and
//! git-presence, build the live Git Service / Content Renderer / Editor Launcher behind the
//! controller's traits, then drive a draw → input → poll loop over a ratatui terminal until
//! the Close intent (AC-20). The terminal is restored on every exit path — including a panic,
//! via the hook `ratatui::try_init` installs.

use crate::controller::{
    Clipboard, Components, ContentProvider, Controller, EditorHandoff, GitService, RenderResult,
    RootProviders,
};
use crate::editor::{EditorLauncher, Spawner};
use crate::git::{self, Baseline, Status};
use crate::presenter::{self, ViewState};
use crate::render::{self, Prepared, Renderers};
use crate::view_policy::ViewMode;
use crate::{host, input, root};
use crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event,
    KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::DefaultTerminal;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::io::Write;
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

    // The root-bound providers are built by a factory so a later re-root rebuilds them against
    // the new root (ADR-0004). Non-capturing — it reads the passed `Resolved`, so re-root gets
    // the new root's git/renderer rather than closing over the launch root.
    let providers: Box<dyn Fn(&root::Resolved) -> RootProviders> =
        Box::new(|resolved: &root::Resolved| {
            let git: Arc<dyn GitService> = Arc::new(LiveGit {
                // In a non-repo there is no repo_root; git is never queried then, but a path is
                // still required, so fall back to the tree root.
                repo_root: resolved
                    .repo_root
                    .clone()
                    .unwrap_or_else(|| resolved.root.clone()),
                base_hint: resolved.base_branch.clone(),
            });
            let content: Box<dyn ContentProvider> = Box::new(LiveContent {
                root: resolved.root.clone(),
                renderers: default_renderers(),
            });
            RootProviders { git, content }
        });
    let editor: Box<dyn EditorHandoff> = Box::new(LiveEditor {
        editor: std::env::var_os("EDITOR"),
    });
    let clipboard: Box<dyn Clipboard> = Box::new(Osc52Clipboard);

    // `Controller::new` now consumes `resolved` by value; `baseline` was already built from it
    // above (`git::default_baseline(&resolved)`), so moving it here is the last use.
    let mut controller = Controller::new(
        resolved,
        baseline,
        Components {
            providers,
            editor,
            clipboard,
        },
    );
    // Kick off the once-a-day update check (off the UI thread; disabled by
    // HERDR_FILE_VIEWER_NO_UPDATE_CHECK). The banner, if any, appears on a later draw.
    controller.set_update(crate::update::start_default());
    // Inject the herdr query channel + the viewer's own workspace id for the worktree picker's
    // agent-active overlay (AC-3) — the first real use of the T-4 host seam. `ctx` is still in
    // scope (only borrowed by `root::resolve`). A missing/failing herdr degrades to a git-only
    // picker (AC-15).
    controller.set_host(
        Box::new(crate::herdr::LiveHerdr::from_env()),
        ctx.workspace_id.clone(),
    );

    let mut terminal = ratatui::try_init()?;
    // Mouse is additive to the keyboard-first design (AC-18): herdr forwards mouse events to a
    // pane that requests capture, while reserving Shift+mouse for the terminal's own
    // selection/copy. Best-effort so a terminal without mouse support still runs.
    let _ = execute!(io::stdout(), EnableMouseCapture);
    // Request focus-change reporting so the viewer can refresh git state when it regains focus
    // (herdr forwards FocusGained/FocusLost to a pane that opts in). Best-effort.
    let _ = execute!(io::stdout(), EnableFocusChange);
    // ratatui's panic hook restores the terminal but doesn't know we enabled mouse capture, so
    // chain a disable in front of it — otherwise a panic would leave the host terminal stuck in
    // mouse-reporting mode.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        let _ = execute!(io::stdout(), DisableFocusChange);
        prev_hook(info);
    }));
    let outcome = event_loop(&mut terminal, &mut controller);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    let _ = execute!(io::stdout(), DisableFocusChange);
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
                // While the finder overlay is open, every key press is routed directly to
                // `handle_finder_key` so printable keys (including `j`, `w`, `q`, …) edit the
                // query instead of firing viewer intents (AC-7). The arm is gated on
                // `finder_open()` so it is entirely skipped when the finder is closed, leaving
                // the existing `map_key` arm below to run unchanged.
                Event::Key(key) if key.kind == KeyEventKind::Press && controller.finder_open() => {
                    let fx = controller.handle_finder_key(key);
                    if fx.clear {
                        let _ = terminal.clear();
                        dirty = true;
                    }
                    if fx.quit {
                        return Ok(()); // finder never quits; harmless for symmetry
                    }
                    dirty |= fx.redraw;
                }
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
                // The pane regained focus (herdr forwards focus events to a pane that opts in):
                // re-read git state so external changes — a merge, pull, or commit in another
                // pane — show in the tree without a relaunch. FocusLost needs no action.
                Event::FocusGained => dirty |= controller.handle_focus_gained().redraw,
                Event::FocusLost => {}
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
    fn diff(&self, rel_path: &Path, baseline: Baseline, full_context: bool) -> String {
        git::diff(
            &self.repo_root,
            rel_path,
            baseline,
            self.base_hint.as_deref(),
            full_context,
        )
    }
}

/// The live Content Renderer: classify + delegate to the external renderers, with guards.
struct LiveContent {
    root: PathBuf,
    renderers: Renderers,
}

impl ContentProvider for LiveContent {
    fn render(&self, path: &Path, mode: ViewMode, raw_diff: Option<&str>) -> RenderResult {
        // Both diff modes render from git's diff text, not the file bytes — so a deleted or
        // binary file still shows its diff (AC-9), and there is no point classifying (a wasted
        // bounded file read). Other modes classify first (binary / size guards, AC-12/13).
        // `Prepared::Binary` is inert for the diff path inside `render`.
        let prepared = if matches!(mode, ViewMode::Diff | ViewMode::FullDiff) {
            Prepared::Binary
        } else {
            render::classify(&self.root, path)
        };
        let name = path.file_name().and_then(OsStr::to_str);
        let (content, notice) = render::render(&self.renderers, &prepared, mode, raw_diff, name);
        RenderResult {
            content,
            notices: notice.into_iter().collect(),
        }
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
            EditorLauncher::new(editor).open(file, &mut ProcessSpawner)
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

/// The live Clipboard: copy via the OSC 52 terminal escape sequence. This is the portable,
/// dependency-free way for a TUI in a pane to set the *host* terminal's clipboard — it travels
/// through terminal multiplexers (herdr, tmux with passthrough) and SSH, unlike a native
/// clipboard API bound to the local display. The sequence produces no visible output, so
/// writing it mid-loop never disturbs the ratatui screen.
struct Osc52Clipboard;

impl Clipboard for Osc52Clipboard {
    fn copy(&mut self, text: &str) -> io::Result<()> {
        // OSC 52: ESC ] 52 ; c ; <base64 payload> BEL — `c` selects the clipboard. The payload is
        // a path, which can include an untrusted, attacker-chosen file name (trust boundary #1).
        // base64-encoding it confines the bytes to `[A-Za-z0-9+/=]`, so a name containing ESC/BEL
        // or another OSC sequence cannot break out of this one and drive the terminal — the same
        // defense-in-depth the renderer path applies to file content.
        let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
        let mut out = io::stdout();
        out.write_all(seq.as_bytes())?;
        out.flush()
    }
}

/// Standard base64 (RFC 4648) — a few lines so OSC 52 needs no extra dependency, matching the
/// project's minimal-deps style. OSC 52 payloads are base64; the terminal decodes them.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        // Pad the final partial group with '=' (one byte → "xx==", two bytes → "xxx=").
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Runs the editor command and waits for it (the hand-off is synchronous, as for a terminal
/// editor like vim).
struct ProcessSpawner;

impl Spawner for ProcessSpawner {
    fn spawn(&mut self, argv: &[OsString]) -> io::Result<()> {
        let (prog, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::other("empty editor command"))?;
        let status = Command::new(prog).args(args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("editor exited with {status}")))
        }
    }
}

/// Leave raw mode + the alternate screen so an external editor owns a clean terminal. Mouse
/// capture is dropped too: otherwise our capture mode leaks into the editor, which would see
/// raw mouse escape sequences instead of normal input.
fn suspend_tui() -> io::Result<()> {
    let _ = execute!(io::stdout(), DisableMouseCapture);
    let _ = execute!(io::stdout(), DisableFocusChange);
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)
}

/// Re-enter raw mode + the alternate screen after the editor returns, and re-arm mouse capture
/// for the viewer (best-effort, matching `run`'s setup).
fn resume_tui() -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let _ = execute!(io::stdout(), EnableMouseCapture);
    let _ = execute!(io::stdout(), EnableFocusChange);
    Ok(())
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
        // The full-file diff view adds delta's line-number gutter, so the whole file is shown
        // with its line numbers and the diff inline (the compact `diff` omits the gutter).
        full_diff: vec!["delta".into(), "--line-numbers".into()],
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
        assert!(
            s.ends_with("markdown-style.json"),
            "points at the bundled style: {s}"
        );
        assert!(
            Path::new(&s).is_file(),
            "the referenced style file exists: {s}"
        );
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
        assert_eq!(
            bundled_style_path(Some(&exe)),
            None,
            "no asset in the install tree → None"
        );
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
        assert!(
            r.syntax.iter().any(|a| a == "--color=always"),
            "bat color forced: {:?}",
            r.syntax
        );
        assert!(
            r.syntax.iter().any(|a| a == "--style=numbers"),
            "bat line numbers: {:?}",
            r.syntax
        );
    }

    #[test]
    fn base64_encode_matches_rfc4648_including_padding() {
        // Padding is what OSC 52 consumers expect; the partial-group cases are the easy ones to
        // get wrong. Vectors from RFC 4648 §10.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // A path with a slash exercises the '+/' tail of the alphabet via high bytes.
        assert_eq!(base64_encode(b"src/app.rs"), "c3JjL2FwcC5ycw==");
    }

    #[test]
    fn base64_confines_control_bytes_to_the_safe_alphabet() {
        // Security: the OSC 52 payload may carry an attacker-chosen file name (trust boundary
        // #1). Encoding must leave no ESC/BEL or other raw control byte that could break out of
        // the escape sequence — the output is strictly `[A-Za-z0-9+/=]`.
        let hostile = b"\x1b]52;c;evil\x07\x1b\\name\nwith\x07bel";
        let encoded = base64_encode(hostile);
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')),
            "encoded output stays within the base64 alphabet: {encoded}"
        );
    }

    #[test]
    fn full_diff_renderer_adds_delta_line_numbers() {
        // The full-file diff view (AC-11) is delta WITH a line-number gutter — that gutter is
        // what makes it "the whole file with line numbers". The compact diff omits it.
        let r = default_renderers();
        assert_eq!(
            r.full_diff.first().map(String::as_str),
            Some("delta"),
            "full_diff uses delta: {:?}",
            r.full_diff
        );
        assert!(
            r.full_diff.iter().any(|a| a == "--line-numbers"),
            "full_diff shows line numbers: {:?}",
            r.full_diff
        );
        assert!(
            !r.diff.iter().any(|a| a == "--line-numbers"),
            "the compact diff does NOT add line numbers: {:?}",
            r.diff
        );
    }
}
