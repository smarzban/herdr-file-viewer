//! App wiring — assemble the real components and run the terminal event loop.
//!
//! [`run`] is the binary's body: read the herdr launch context, resolve the root and
//! git-presence, build the live Git Service / Content Renderer / Editor Launcher behind the
//! controller's traits, then drive a draw → input → poll loop over a ratatui terminal until
//! the Close intent (AC-20). The terminal is restored on every exit path — including a panic,
//! via the hook `ratatui::try_init` installs.

use crate::controller::{
    Clipboard, Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, Effects,
    GitService, RenderResult, RootProviders,
};
use crate::editor::{EditorLauncher, SpawnError, Spawner};
use crate::git::{self, Baseline, Status};
use crate::presenter::{self, ViewState};
use crate::render::{self, Caps, Prepared, Renderers};
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
use std::process::{Command, Stdio};
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

    // Load + resolve the plugin's optional TOML config once, up front (AC-3..AC-5, AC-14, AC-16,
    // AC-17): `eff` is the fully-resolved config > env > default settings the rest of `run` wires
    // in below. Kept alive (borrowed, never moved wholesale) so the Settings section (T-9) below
    // can read `eff`/`load_outcome` for the in-app Settings display. With no config file present,
    // `load_config_from_env` returns `Config::default()`, so default behavior is unchanged.
    let (cfg, load_outcome) = crate::config::load_config_from_env();
    let eff = crate::config::resolve(&cfg, |k| std::env::var(k).ok());

    // The effective renderers (config overrides layered onto the built-in defaults, AC-7) — built
    // once and reused for both renderer sites below (the root-bound factory's `LiveContent` and
    // `Components`), so a config override is honored identically wherever the renderers are used.
    let renderers = crate::config::effective_renderers(&eff, &default_renderers());

    // The Content Renderer size caps (config `preview_max_lines` / `preview_max_kib`, already clamped
    // and byte-converted). `Copy`, so the factory closure below captures it by value.
    let caps = eff.preview_caps();

    // The root-bound providers are built by a factory so a later re-root rebuilds them against
    // the new root (ADR-0004). Non-capturing — it reads the passed `Resolved`, so re-root gets
    // the new root's git/renderer rather than closing over the launch root.
    let factory_renderers = renderers.clone();
    let providers: Box<dyn Fn(&root::Resolved) -> RootProviders> =
        Box::new(move |resolved: &root::Resolved| {
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
                renderers: factory_renderers.clone(),
                caps,
            });
            RootProviders { git, content }
        });
    // The effective editor (AC-6): config > `$EDITOR` (already encoded in `eff.editor`) >
    // platform default (`resolve_editor(None)` — e.g. Notepad on Windows).
    let platform_editor = resolve_editor(None);
    let editor: Box<dyn EditorHandoff> = Box::new(LiveEditor {
        editor: crate::config::effective_editor(&eff, platform_editor.clone()),
    });
    let clipboard: Box<dyn Clipboard> = Box::new(Osc52Clipboard);

    // Wired values for the Settings display (AC-1..AC-4): built from the same startup resolution
    // as the live components below so the overlay shows what's actually in effect.
    let settings_wired = settings_wired(&eff, current_os_kind(), platform_editor);

    // `Controller::new` now consumes `resolved` by value; `baseline` was already built from it
    // above (`git::default_baseline(&resolved)`), so moving it here is the last use.
    let mut controller = Controller::new(
        resolved,
        baseline,
        Components {
            providers,
            editor,
            clipboard,
            renderers: Some(renderers),
        },
    );
    // Apply the config-driven startup hide-dotfiles default (AC-9). The interactive `.` toggle
    // still flips it later.
    controller.apply_hide_dotfiles(eff.hide_dotfiles);
    // Apply the config-driven quit guard (`confirm_discard`): whether quitting with
    // session annotations held confirms first or discards them immediately.
    controller.apply_confirm_discard(eff.confirm_discard);
    // Apply the config-driven mouse-wheel scroll step (`scroll_lines`); already clamped to >= 1 by
    // the resolver, so the wheel always advances at least one line/item.
    controller.apply_scroll_lines(eff.scroll_lines);
    // Apply the config-driven startup layout: the tree/content split ratio (`tree_width`, already
    // clamped to the split range) and the tree side (`tree_position`). The live grow/shrink keys and
    // divider drag still adjust the split within the session.
    controller.apply_tree_width(eff.tree_width);
    controller.apply_tree_position(eff.tree_position);
    controller.apply_tree_max_cols(eff.tree_max_cols);
    // Format the Settings section body for the `?` overlay (AC-15, AC-18): reflects the load
    // outcome plus every effective setting, so a user can see what's actually in effect, and the
    // resolved config-file location so they know what to fix or create.
    controller.set_settings_display(
        &eff,
        &load_outcome,
        &crate::config::config_path_from_env(),
        &settings_wired,
    );
    // Resolve the effective key bindings from the registry + the config's `[keys]` table (Slice B,
    // T-6): `config > default`, defensively (a rejected entry reverts to its default key set). This
    // is read-only wiring (AC-23) — it only reads the already-loaded `cfg` and builds in-memory
    // state, never touching the filesystem or git. The run loop's key arm below decodes against
    // these instead of the hardwired default map; the `KeyLoadOutcome` is stored for the T-7
    // Keybindings overlay to surface any ignored entries (AC-16). Consuming both here avoids an
    // unused-variable warning.
    let (bindings, key_outcome) =
        crate::input::resolve_bindings(crate::input::registry(), cfg.keys.as_ref());
    controller.set_keybindings(bindings, key_outcome);
    // Format the Keybindings section body for the `?` overlay (AC-16, AC-19, AC-20): reads the
    // effective bindings + load outcome just stored above, so the live overlay ships a "Keybindings"
    // tab listing every action's effective key(s), marking custom bindings, and surfacing any
    // ignored `[keys]` entries. Pure/read-only (AC-23).
    controller.set_keybindings_display();
    // Kick off the once-a-day update check (off the UI thread; disabled by
    // HERDR_FILE_VIEWER_NO_UPDATE_CHECK, or by config `update_check = false`, AC-10). The banner,
    // if any, appears on a later draw.
    if crate::config::should_start_update_check(&eff) {
        // Gate on the RESOLVED `update_check` (config > env > default) and pass that decision
        // straight through (disabled = false here, since the gate already applied precedence).
        // `start_default()` would re-read HERDR_FILE_VIEWER_NO_UPDATE_CHECK and let the env
        // silently override a config `update_check = true` (AC-3/AC-10).
        controller.set_update(crate::update::start_default_with(false));
    }
    // Inject the herdr query channel + the viewer's own workspace id for the worktree picker's
    // agent-active overlay (AC-3) — the first real use of the host seam. `ctx` is still in
    // scope (only borrowed by `root::resolve`). A missing/failing herdr degrades to a git-only
    // picker (AC-15).
    controller.set_host(
        Box::new(crate::herdr::LiveHerdr::from_env()),
        ctx.workspace_id.clone(),
    );
    // Inject the live OS opener for the `O` / `R` hand-offs (AC-13). Non-blocking: unlike the
    // editor hand-off it does NOT suspend the TUI; the opener runs with stdio redirected to null
    // so it cannot draw onto our screen. `with_overrides` layers the config's `open`/`reveal`
    // command overrides on top of the per-OS defaults (AC-8).
    let to_argv =
        |v: Option<Vec<String>>| v.map(|xs| xs.into_iter().map(OsString::from).collect::<Vec<_>>());
    controller.set_opener(Box::new(
        crate::opener::CommandOpener::new(current_os_kind(), Box::new(OpenerSpawner))
            .with_overrides(to_argv(eff.open.clone()), to_argv(eff.reveal.clone())),
    ));

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

/// Route annotation-modal raw keys before configurable global decoding. Returning `Some` means
/// the modal consumed ownership even when the particular key is an inert no-op, so no printable or
/// fixed modal key can leak to a global quit/editor/copy action.
fn route_annotation_key(
    controller: &mut Controller,
    key: crossterm::event::KeyEvent,
) -> Option<Effects> {
    if controller.annotation_list().is_some() {
        Some(controller.handle_annotations_key(key))
    } else if controller.annotation_editor().is_some() {
        Some(controller.handle_annotation_editor_key(key))
    } else if controller.discard_confirm_open() {
        Some(controller.handle_discard_confirm_key(key))
    } else {
        None
    }
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
                // While a bottom prompt (go-to-line) is open, route every key press to handle_prompt_key so
                // digits/printables edit the prompt instead of firing viewer intents (AC-21). Mutually exclusive
                // with the finder arm above — only one modal is ever open.
                Event::Key(key) if key.kind == KeyEventKind::Press && controller.prompt_open() => {
                    let fx = controller.handle_prompt_key(key);
                    if fx.clear {
                        let _ = terminal.clear();
                        dirty = true;
                    }
                    if fx.quit {
                        return Ok(());
                    }
                    dirty |= fx.redraw;
                }
                // While the help overlay is open, every key press is routed directly to
                // `handle_help_key` so printable keys (including `j`, `q`, …) navigate the
                // overlay instead of firing viewer intents (AC-20). Mutually exclusive with the
                // finder/prompt arms above — only one modal is ever open.
                Event::Key(key) if key.kind == KeyEventKind::Press && controller.help_open() => {
                    let fx = controller.handle_help_key(key);
                    if fx.clear {
                        let _ = terminal.clear();
                        dirty = true;
                    }
                    if fx.quit {
                        return Ok(());
                    }
                    dirty |= fx.redraw;
                }
                // While line-select mode is active, route every key press to
                // `handle_line_select_key` so `j`/`k`/arrows (and their Shift-extend forms) move
                // the marker instead of firing viewer intents. Mutually exclusive with the
                // finder/prompt/help arms above — only one modal is ever open.
                Event::Key(key)
                    if key.kind == KeyEventKind::Press && controller.line_select_active() =>
                {
                    let fx = controller.handle_line_select_key(key);
                    if fx.clear {
                        let _ = terminal.clear();
                        dirty = true;
                    }
                    if fx.quit {
                        return Ok(());
                    }
                    dirty |= fx.redraw;
                }
                // Annotation overview/editor keys are fixed modal controls/raw text. Route them
                // before global decoding so `q`, `e`, `d`, `D`, `y`, and remapped printables cannot
                // trigger viewer actions beneath the modal.
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && controller.annotation_raw_keys_owned() =>
                {
                    let fx = route_annotation_key(controller, key)
                        .expect("an open annotation modal has a raw-key route");
                    if fx.clear {
                        let _ = terminal.clear();
                        dirty = true;
                    }
                    if fx.quit {
                        return Ok(());
                    }
                    dirty |= fx.redraw;
                }
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && let Some(intent) = input::decode(key, controller.bindings()) =>
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
    /// The size caps (line + byte) for classifying/previewing content, resolved from config
    /// (`preview_max_lines` / `preview_max_kib`) at startup. `Copy`.
    caps: Caps,
}

impl ContentProvider for LiveContent {
    fn render(&self, path: &Path, mode: ViewMode, raw_diff: Option<&str>) -> RenderResult {
        // The width-less entry point: no pane width known, so glow keeps its `-w 0` (no wrap).
        self.render_at_width(path, mode, raw_diff, None)
    }

    fn render_at_width(
        &self,
        path: &Path,
        mode: ViewMode,
        raw_diff: Option<&str>,
        width: Option<u16>,
    ) -> RenderResult {
        // A document (docx/pptx/xlsx/pdf/odt) is binary, so `classify` would reject it as
        // `Binary` — instead it renders via a path-based converter (→ markdown/text → glow),
        // dispatched here before the classify path. Width is threaded to glow exactly as for
        // rendered markdown so the converted content wraps to the pane.
        if mode == ViewMode::RenderedDocument
            && let Some(kind) = crate::document::DocKind::from_path(path)
        {
            let renderers = match width.filter(|w| *w > 0) {
                Some(w) => Renderers {
                    markdown: render::with_wrap_width(&self.renderers.markdown, w),
                    ..self.renderers.clone()
                },
                None => self.renderers.clone(),
            };
            let (content, notice) = render::render_document(&renderers, path, kind, self.caps);
            return RenderResult {
                content,
                notices: notice.into_iter().collect(),
                source: None,
            };
        }
        // Both diff modes render from git's diff text, not the file bytes — so a deleted or
        // binary file still shows its diff (AC-9), and there is no point classifying (a wasted
        // bounded file read). Other modes classify first (binary / size guards, AC-12/13).
        // `Prepared::Binary` is inert for the diff path inside `render`.
        let prepared = if matches!(mode, ViewMode::Diff | ViewMode::FullDiff) {
            Prepared::Binary
        } else {
            render::classify(&self.root, path, self.caps)
        };
        let name = path.file_name().and_then(OsStr::to_str);
        // Retain the raw source lines behind a source-mapped render: `SyntaxContent` displays one
        // rendered line per source line, so the copy paths can hand back the file's own text
        // (byte-faithful — real tabs, no `bat` gutter/tab-expansion) and anchor the gutter width
        // exactly. Transformed views (markdown / diffs) have no per-display-line source → `None`.
        let source = match (&mode, &prepared) {
            (ViewMode::SyntaxContent, Prepared::Full { text })
            | (ViewMode::SyntaxContent, Prepared::Truncated { text, .. }) => {
                Some(text.lines().map(str::to_owned).collect())
            }
            _ => None,
        };
        // For rendered markdown at a known pane width, point glow's `-w` at that width so it lays
        // out and wraps tables to fit the pane (columns sized, cells ellipsized, borders intact),
        // and pads every line to exactly that width — the Presenter's re-wrap is then a no-op
        // rather than shattering a natural-width `-w 0` table across the border rows. The bundled
        // markdown style has `margin: 0`, so glow's output lines are exactly `width` wide. Every
        // other mode is width-independent here (delta/bat manage their own width; they h-scroll,
        // never re-wrap), so they use the base renderers unchanged.
        let (content, notice) = match (mode, width.filter(|w| *w > 0)) {
            (ViewMode::RenderedMarkdown, Some(w)) => {
                let wrapped = Renderers {
                    markdown: render::with_wrap_width(&self.renderers.markdown, w),
                    ..self.renderers.clone()
                };
                render::render(&wrapped, &prepared, mode, raw_diff, name, self.caps)
            }
            _ => render::render(&self.renderers, &prepared, mode, raw_diff, name, self.caps),
        };
        RenderResult {
            content,
            notices: notice.into_iter().collect(),
            source,
        }
    }
}

/// Resolve the editor command to use, given `$EDITOR`'s raw value (AC-8 part).
///
/// unix: unchanged (AC-3) — a set `$EDITOR` is used as-is; unset stays `None` (the editor
/// hand-off then reports "no editor configured", today's behaviour). Windows: a set `$EDITOR`
/// is likewise used as-is; unset falls back to a known default (`notepad.exe`, always present
/// on Windows) instead of leaving the hand-off unconfigured.
#[cfg(not(windows))]
fn resolve_editor(editor: Option<OsString>) -> Option<OsString> {
    editor
}

#[cfg(windows)]
fn resolve_editor(editor: Option<OsString>) -> Option<OsString> {
    editor.or_else(|| {
        // Default to Notepad, resolved to its ABSOLUTE `System32` path. A bare `notepad.exe`
        // would be subject to Windows' executable search order — which can include the process
        // working directory, here an *untrusted* browsed repo — so a `notepad.exe` planted in the
        // repo could be spawned instead of the system editor, breaking the read-only/no-repo-code
        // boundary (constitution #1). Resolving via `%SystemRoot%` closes that hole. If
        // `SystemRoot` is somehow unset, fall back to no editor (a "no editor configured" notice)
        // rather than a hijackable bare name.
        let root = std::env::var_os("SystemRoot").filter(|r| !r.is_empty())?;
        let mut p = std::path::PathBuf::from(root);
        p.push("System32");
        p.push("notepad.exe");
        Some(p.into_os_string())
    })
}

/// The live Editor Launcher: spawn `$EDITOR <file>` as a blocking hand-off, suspending and
/// restoring the TUI around it. A missing `$EDITOR` is a non-fatal error the controller
/// surfaces as a notice.
struct LiveEditor {
    editor: Option<OsString>,
}

impl EditorHandoff for LiveEditor {
    fn open(&mut self, file: &Path) -> EditorOutcome {
        let Some(editor) = self.editor.clone() else {
            // No terminal change yet, so nothing to restore.
            return EditorOutcome::NotLaunched("no editor configured (set $EDITOR)".into());
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
        let suspend_err = suspended.err();
        let launched_err = launched.err();
        let resume_err = resumed.err();
        if let Some(e) = suspend_err {
            return EditorOutcome::NotLaunched(e.to_string());
        }
        if let Some(e) = launched_err {
            // Distinguish a launch failure from a successful launch that exited non-zero
            //: only NotLaunched is reported as "could not open editor".
            return match e {
                SpawnError::NotLaunched(e) => EditorOutcome::NotLaunched(e.to_string()),
                SpawnError::NonZeroExit(detail) => EditorOutcome::NonZeroExit(detail),
            };
        }
        if let Some(_e) = resume_err {
            // The editor launched and ran (we are past the launch-error check), then drawing
            // it over the screen left the terminal in editor mode and the restore failed. The
            // editor DID take over and may have changed the file, so this is a takeover, not a
            // launch failure: returning `TookOver` makes the controller refresh git state +
            // re-render (the file may differ) and forces the run loop's full repaint, which is
            // also the recovery for the failed restore. Reporting it as `NotLaunched` would both
            // skip that refresh (stale markers/content after a real edit) and show a misleading
            // "Could not open editor" notice for an editor that did open.
            return EditorOutcome::TookOver;
        }
        // The editor drew over the screen → the run loop forces a full repaint.
        EditorOutcome::TookOver
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
    fn spawn(&mut self, argv: &[OsString]) -> Result<(), SpawnError> {
        let (prog, args) = argv
            .split_first()
            .ok_or_else(|| SpawnError::NotLaunched(io::Error::other("empty editor command")))?;
        // A failed `Command::status` (e.g. the binary is not on PATH) is a launch failure —
        // the editor never ran. Map it through `NotLaunched` so the controller words the
        // notice as "could not open editor" rather than as an editor exit.
        let status = Command::new(prog)
            .args(args)
            .status()
            .map_err(SpawnError::NotLaunched)?;
        if status.success() {
            Ok(())
        } else {
            // The editor launched and ran; only its exit code is non-zero. Reported separately
            // so the controller says "editor exited with …", not "could not open editor".
            Err(SpawnError::NonZeroExit(format!("{status}")))
        }
    }
}

/// Spawns the OS opener **fire-and-forget**: the child is launched with stdio redirected to null
/// (so a chatty opener like `xdg-open` printing a warning cannot draw onto the live TUI) and the
/// **event loop is never blocked** — pressing `O`/`R` returns immediately and cannot freeze the
/// viewer even if an opener is slow or hangs (AC-6, non-blocking). `open`/`xdg-open`/`explorer`
/// are short-lived launchers that dispatch to the GUI app and exit on their own. A launch failure
/// (e.g. the opener binary is missing) is reported as `NotLaunched` (AC-7); a post-launch exit code
/// is intentionally NOT observed (that is what fire-and-forget means), unlike the blocking editor
/// hand-off's `ProcessSpawner`, which waits and can surface a `NonZeroExit`.
///
/// The launched child is **reaped on a throwaway thread** (`std::thread` — the same off-thread
/// idiom the renderer uses) rather than by dropping its handle: on unix, dropping a `Child`
/// without waiting leaves the exited launcher as a **zombie** until the viewer itself exits, so a
/// long session with many `O`/`R` presses would accumulate defunct processes (constitution §5,
/// good plugin citizen). `wait()` runs off the input thread, so this stays non-blocking.
struct OpenerSpawner;
impl Spawner for OpenerSpawner {
    fn spawn(&mut self, argv: &[OsString]) -> Result<(), SpawnError> {
        let (prog, args) = argv
            .split_first()
            .ok_or_else(|| SpawnError::NotLaunched(io::Error::other("empty opener command")))?;
        // `spawn` (not `status`): launch and return immediately, never blocking the event loop.
        let mut child = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(SpawnError::NotLaunched)?;
        // Reap the short-lived launcher off the input thread so it doesn't linger as a zombie.
        // The thread ends as soon as the child exits (~immediately); its status is discarded
        // (fire-and-forget). A failed reaper thread-spawn is non-fatal — worst case is the same
        // zombie we accept today, never a blocked or crashed viewer.
        let _ = std::thread::Builder::new()
            .name("opener-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
        Ok(())
    }
}

/// Map the compile-time build target to the [`OsKind`](crate::opener::OsKind) whose opener
/// convention to build for. `xdg-open` is the freedesktop default on non-mac/non-windows unixes.
fn current_os_kind() -> crate::opener::OsKind {
    if cfg!(target_os = "macos") {
        crate::opener::OsKind::Mac
    } else if cfg!(target_os = "windows") {
        crate::opener::OsKind::Windows
    } else {
        crate::opener::OsKind::Linux
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

/// Build the [`crate::help::SettingsWired`] the Settings display reads, from the same startup
/// resolution the live components use. Extracted from `run` (which needs a terminal) so the wiring
/// itself is testable: the `open`/`reveal` labels must not be crossed, and the editor must follow
/// config > `$EDITOR` > platform default.
fn settings_wired(
    eff: &crate::config::EffectiveSettings,
    os: crate::opener::OsKind,
    platform_editor: Option<std::ffi::OsString>,
) -> crate::help::SettingsWired {
    crate::help::SettingsWired {
        editor: crate::config::effective_editor(eff, platform_editor),
        open: crate::opener::default_opener_display(os, crate::opener::OpenAction::Open),
        reveal: crate::opener::default_opener_display(os, crate::opener::OpenAction::Reveal),
    }
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
        documents: crate::document::DocConverters::defaults(),
        timeout: RENDER_TIMEOUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- resolve_editor: default-editor platform seam (AC-8, T-5) --------------

    /// A set `$EDITOR` is always used as-is, on every platform.
    #[test]
    fn resolve_editor_passes_through_a_set_value() {
        assert_eq!(
            resolve_editor(Some(OsString::from("vim"))),
            Some(OsString::from("vim"))
        );
    }

    /// unix: an unset `$EDITOR` stays `None` (today's behaviour, unchanged — AC-3).
    #[cfg(not(windows))]
    #[test]
    fn resolve_editor_unix_unset_stays_none() {
        assert_eq!(resolve_editor(None), None);
    }

    /// Windows: an unset `$EDITOR` falls back to an ABSOLUTE `System32\notepad.exe` (never a
    /// bare, search-path-hijackable `notepad.exe` — see the seam's security note). `windows-latest`
    /// always has `%SystemRoot%` set, so the default resolves to a real absolute path.
    #[cfg(windows)]
    #[test]
    fn resolve_editor_windows_unset_falls_back_to_absolute_notepad() {
        let got = resolve_editor(None).expect("a default editor on Windows");
        let p = std::path::Path::new(&got);
        assert!(
            p.is_absolute(),
            "the default editor path is absolute: {got:?}"
        );
        assert!(
            got.to_string_lossy()
                .to_lowercase()
                .ends_with("system32\\notepad.exe"),
            "defaults to System32\\notepad.exe: {got:?}"
        );
    }

    /// A fresh empty temp dir for a test (no tempfile dep — matches the project's hermetic
    /// test style). Distinct per `tag` so parallel unit tests don't collide.
    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hfv-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[derive(Default)]
    struct RouteGit;

    impl GitService for RouteGit {
        fn status(&self) -> BTreeMap<PathBuf, Status> {
            BTreeMap::new()
        }

        fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
            BTreeMap::new()
        }

        fn diff(&self, _path: &Path, _baseline: Baseline, _full_context: bool) -> String {
            String::new()
        }
    }

    struct RouteContent;

    impl ContentProvider for RouteContent {
        fn render(&self, _path: &Path, _mode: ViewMode, _diff: Option<&str>) -> RenderResult {
            RenderResult {
                content: ratatui::text::Text::raw("body"),
                notices: Vec::new(),
                source: None,
            }
        }
    }

    struct RouteEditor;

    impl EditorHandoff for RouteEditor {
        fn open(&mut self, _file: &Path) -> EditorOutcome {
            EditorOutcome::NoTakeover
        }
    }

    struct RouteClipboard;

    impl Clipboard for RouteClipboard {
        fn copy(&mut self, _text: &str) -> io::Result<()> {
            Ok(())
        }
    }

    fn route_controller(tag: &str) -> (Controller, PathBuf) {
        let root = tmp(tag);
        std::fs::write(root.join("note.rs"), "fn main() {}\n").unwrap();
        let resolved = crate::root::Resolved {
            root: root.clone(),
            is_git_repo: false,
            repo_root: None,
            is_worktree: false,
            base_branch: None,
        };
        let controller = Controller::new(
            resolved,
            Baseline::Head,
            Components {
                providers: Box::new(|_| RootProviders {
                    git: Arc::new(RouteGit),
                    content: Box::new(RouteContent),
                }),
                editor: Box::new(RouteEditor),
                clipboard: Box::new(RouteClipboard),
                renderers: None,
            },
        );
        (controller, root)
    }

    fn route_key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    #[test]
    fn annotation_editor_raw_printables_route_before_global_decoding() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (mut controller, root) = route_controller("app-route-editor");
        controller.handle(crate::intent::Intent::AddAnnotation);
        assert!(controller.annotation_editor().is_some());
        route_annotation_key(&mut controller, route_key(KeyCode::Enter)).unwrap();
        assert_eq!(
            controller
                .view_state()
                .annotation_editor
                .expect("validation stays in the projected editor")
                .error
                .as_deref(),
            Some("Annotation text cannot be empty")
        );

        for key in [
            route_key(KeyCode::Char('q')),
            route_key(KeyCode::Char('e')),
            route_key(KeyCode::Char('d')),
            KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT),
            route_key(KeyCode::Char('y')),
        ] {
            let effects = route_annotation_key(&mut controller, key).expect("editor owns raw key");
            assert!(
                !effects.quit,
                "a printable modal key cannot leak to global quit"
            );
        }
        assert_eq!(controller.annotation_editor().unwrap().text(), "qedDy");
        let view = controller.view_state();
        let editor = view.annotation_editor.expect("typed editor projection");
        assert_eq!(editor.text, "qedDy");
        assert_eq!(editor.target.path, PathBuf::from("note.rs"));
        assert_eq!(editor.kind, crate::presenter::AnnotationEditorKind::Add);
        assert!(view.annotation_overview.is_none());
        drop(controller);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn annotation_overview_fixed_keys_route_before_globals_and_project_owned_rows() {
        use crossterm::event::KeyCode;

        let (mut controller, root) = route_controller("app-route-overview");
        controller.handle(crate::intent::Intent::AddAnnotation);
        for c in "note".chars() {
            route_annotation_key(&mut controller, route_key(KeyCode::Char(c))).unwrap();
        }
        route_annotation_key(&mut controller, route_key(KeyCode::Enter)).unwrap();
        controller.handle(crate::intent::Intent::ShowAnnotations);

        let view = controller.view_state();
        assert_eq!(view.annotation_count, 1);
        let overview = view.annotation_overview.expect("owned overview projection");
        assert_eq!(overview.cursor, 0);
        assert_eq!(overview.rows.len(), 1);
        assert_eq!(overview.rows[0].target.path, PathBuf::from("note.rs"));
        assert_eq!(overview.rows[0].note, "note");

        route_annotation_key(&mut controller, route_key(KeyCode::Char('e')))
            .expect("fixed edit key is routed to the overview");
        let editor = controller
            .view_state()
            .annotation_editor
            .expect("edit projection");
        assert_eq!(editor.kind, crate::presenter::AnnotationEditorKind::Edit);
        assert_eq!(editor.text, "note");
        route_annotation_key(&mut controller, route_key(KeyCode::Esc))
            .expect("editor cancel returns to overview");

        let effects = route_annotation_key(&mut controller, route_key(KeyCode::Char('d')))
            .expect("overview owns fixed delete");
        assert!(effects.redraw && !effects.quit);
        assert!(controller.annotations().is_empty());
        assert!(controller.annotation_list().is_some());

        let effects = route_annotation_key(&mut controller, route_key(KeyCode::Char('q')))
            .expect("overview owns fixed close");
        assert!(effects.redraw && !effects.quit, "q closes only the modal");
        assert!(!controller.annotation_modal_open());
        assert!(
            route_annotation_key(&mut controller, route_key(KeyCode::Char('q'))).is_none(),
            "without an annotation modal the key proceeds to normal global decoding"
        );
        drop(controller);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn quit_confirm_keys_route_before_globals() {
        use crossterm::event::KeyCode;

        // The gate the event loop matches on MUST cover the quit confirm: when it only covered the
        // overview/editor, `route_annotation_key` was never reached and `y`/`q`/`Esc` fell through
        // to global decoding, so the dialog was drawn but inert. Controller-level tests that call
        // `handle_discard_confirm_key` directly cannot see that hole; this drives the real gate.
        let (mut controller, root) = route_controller("app-route-quit-confirm");
        controller.handle(crate::intent::Intent::AddAnnotation);
        for c in "note".chars() {
            route_annotation_key(&mut controller, route_key(KeyCode::Char(c))).unwrap();
        }
        route_annotation_key(&mut controller, route_key(KeyCode::Enter)).unwrap();

        controller.handle(crate::intent::Intent::Close);
        assert!(
            controller.discard_confirm_open(),
            "the close raised the confirm"
        );
        assert!(
            controller.annotation_raw_keys_owned(),
            "the event loop's gate must own the confirm's keys"
        );

        let effects = route_annotation_key(&mut controller, route_key(KeyCode::Esc))
            .expect("the confirm owns esc");
        assert!(!effects.quit, "esc cancels");
        assert!(!controller.discard_confirm_open());

        controller.handle(crate::intent::Intent::Close);
        let effects = route_annotation_key(&mut controller, route_key(KeyCode::Char('y')))
            .expect("the confirm owns y");
        assert!(effects.quit, "y copies and quits through the real route");

        drop(controller);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn switch_confirm_enter_routes_before_globals() {
        use crossterm::event::KeyCode;

        // The SwitchRoot variant's proceed key is `Enter`, not `q`. Every switch_confirm_* test
        // calls `handle_discard_confirm_key` directly, which cannot see whether the event loop's
        // gate routes to it: an `Enter` falling through to global decoding would pass them all.
        // That is exactly how the quit confirm shipped inert. (empanel round 1, lens-tests.)
        let (mut controller, root) = route_controller("app-route-switch-confirm");
        let second = tmp("app-route-switch-target");
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join("other.rs"), "fn other() {}\n").unwrap();

        controller.handle(crate::intent::Intent::AddAnnotation);
        for c in "note".chars() {
            route_annotation_key(&mut controller, route_key(KeyCode::Char(c))).unwrap();
        }
        route_annotation_key(&mut controller, route_key(KeyCode::Enter)).unwrap();

        controller.re_root(&second);
        assert!(controller.discard_confirm_open(), "the switch confirmed");
        assert!(
            controller.annotation_raw_keys_owned(),
            "the event loop's gate must own the switch confirm's keys too"
        );

        let effects = route_annotation_key(&mut controller, route_key(KeyCode::Enter))
            .expect("the switch confirm owns Enter");
        assert!(!effects.quit, "proceeding with a switch is not a quit");
        assert!(!controller.discard_confirm_open());
        assert!(controller.annotations().is_empty(), "the switch proceeded");

        drop(controller);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(second);
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
    fn settings_wired_maps_each_opener_to_its_own_label_and_resolves_the_editor() {
        use crate::opener::OsKind;
        // The wiring `run` does, minus the terminal: Open and Reveal must not be crossed, and the
        // editor must follow config > `$EDITOR` > platform default.
        let eff = crate::config::EffectiveSettings {
            editor: Some(std::ffi::OsString::from("nvim")),
            ..crate::config::resolve(&crate::config::Config::default(), |_| None)
        };
        let w = settings_wired(&eff, OsKind::Mac, Some(std::ffi::OsString::from("vi")));
        assert_eq!(
            w.editor,
            Some(std::ffi::OsString::from("nvim")),
            "config wins"
        );
        assert_eq!(
            w.open,
            crate::opener::default_opener_display(OsKind::Mac, crate::opener::OpenAction::Open)
        );
        assert_eq!(
            w.reveal,
            crate::opener::default_opener_display(OsKind::Mac, crate::opener::OpenAction::Reveal)
        );
        assert_ne!(
            w.open, w.reveal,
            "Open and Reveal labels must not be crossed"
        );

        // No config editor: the platform default is what the row must report.
        let bare = crate::config::resolve(&crate::config::Config::default(), |_| None);
        let w = settings_wired(&bare, OsKind::Linux, Some(std::ffi::OsString::from("vi")));
        assert_eq!(w.editor, Some(std::ffi::OsString::from("vi")));
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

    /// A `LiveContent` whose markdown delegate echoes back the `-w` value it was actually invoked
    /// with (as `W=<n>`), so a test can prove the pane width was threaded into glow's `-w`. The
    /// base `-w` is `0` (matching the real default), so an unknown/zero width reads back `W=0`.
    #[cfg(unix)]
    fn echoing_md_content(root: &Path) -> LiveContent {
        LiveContent {
            root: root.to_path_buf(),
            renderers: Renderers {
                // `sh -c SCRIPT sh -w 0 -`: inside SCRIPT, `$2` is the token after `-w`, which
                // `render_at_width` rewrites (via `with_wrap_width`) to the requested width.
                // `cat >/dev/null` drains the piped (untrusted) stdin so the writer never hits a
                // broken pipe.
                markdown: vec![
                    "sh".into(),
                    "-c".into(),
                    "printf 'W=%s' \"$2\"; cat >/dev/null".into(),
                    "sh".into(),
                    "-w".into(),
                    "0".into(),
                    "-".into(),
                ],
                diff: vec!["cat".into()],
                full_diff: vec!["cat".into()],
                syntax: vec!["cat".into()],
                documents: crate::document::DocConverters::defaults(),
                timeout: Duration::from_secs(5),
            },
            caps: Caps::default(),
        }
    }

    /// End-to-end wiring: a NON-default cap on `LiveContent` must reach `classify` and truncate,
    /// guarding the `eff.preview_caps()` → `LiveContent.caps` → `render::classify(.., self.caps)`
    /// thread that the config/render unit tests each cover only on their own side.
    #[cfg(unix)]
    #[test]
    fn livecontent_threads_a_configured_cap_into_classify() {
        let root = tmp("cfg-cap-wiring");
        let file = root.join("many.txt");
        std::fs::write(&file, "line\n".repeat(200)).unwrap(); // 200 lines, well under the default cap
        let content = LiveContent {
            root: root.clone(),
            renderers: Renderers {
                markdown: vec!["cat".into()],
                diff: vec!["cat".into()],
                full_diff: vec!["cat".into()],
                syntax: vec!["cat".into()],
                documents: crate::document::DocConverters::defaults(),
                timeout: Duration::from_secs(5),
            },
            // A 50-line cap the default would never apply — proves the injected cap is what bites.
            caps: Caps {
                max_lines: 50,
                max_bytes: 1024 * 1024,
            },
        };
        let out = content.render_at_width(&file, ViewMode::SyntaxContent, None, None);
        assert!(
            out.notices.iter().any(|n| n.contains("50-line")),
            "the configured 50-line cap must reach classify through LiveContent: {:?}",
            out.notices
        );
    }

    #[cfg(unix)]
    fn flatten_content(r: &RenderResult) -> String {
        r.content
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// The table fix: rendered markdown at a known pane width hands glow `-w <width>` so it lays
    /// out (and wraps) tables to fit the pane, instead of overflowing at natural `-w 0` width and
    /// being shattered by the Presenter's re-wrap.
    #[cfg(unix)]
    #[test]
    fn render_at_width_points_markdown_wrap_at_the_pane_width() {
        let root = tmp("md-wrap-width");
        let md = root.join("doc.md");
        std::fs::write(&md, "| a | b |\n|---|---|\n| 1 | 2 |\n").unwrap();
        let content = echoing_md_content(&root);
        let out = content.render_at_width(&md, ViewMode::RenderedMarkdown, None, Some(80));
        assert!(
            flatten_content(&out).contains("W=80"),
            "the pane width must reach glow's -w: {:?}",
            flatten_content(&out)
        );
    }

    /// No width known yet (the first render, before the first draw has measured the pane) — glow
    /// keeps its `-w 0` (no wrap). The width-less `render` entry point delegates to the same path
    /// with `None`, so it too leaves `-w 0`.
    #[cfg(unix)]
    #[test]
    fn render_at_width_leaves_wrap_unbounded_without_a_pane_width() {
        let root = tmp("md-wrap-none");
        let md = root.join("doc.md");
        std::fs::write(&md, "# Title\n").unwrap();
        let content = echoing_md_content(&root);
        // Explicit None, an inert zero width, and the width-less `render` all keep the base `-w 0`.
        for w in [None, Some(0)] {
            let out = content.render_at_width(&md, ViewMode::RenderedMarkdown, None, w);
            assert!(
                flatten_content(&out).contains("W=0"),
                "width {w:?} must leave -w at 0: {:?}",
                flatten_content(&out)
            );
        }
        let via_render = content.render(&md, ViewMode::RenderedMarkdown, None);
        assert!(
            flatten_content(&via_render).contains("W=0"),
            "the width-less render() must keep -w 0: {:?}",
            flatten_content(&via_render)
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
