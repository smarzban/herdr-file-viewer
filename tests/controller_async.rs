//! Session Controller: off-thread rendering (AC-23). A select intent must dispatch
//! the (potentially slow) content render to a worker thread so `handle()` returns promptly
//! and never blocks input; the rendered content then arrives as a later effect, drained by
//! `poll()`. A deliberately slow renderer stub stands in for glow/delta/bat.

mod common;

use common::TempDir;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The loading placeholder shown in the content pane while an off-thread render is in flight
///. Spelled with the ellipsis here so a change to the placeholder string in
/// `dispatch_render` is caught by the tests that assert it appears.
const LOADING_PLACEHOLDER: &str = "Rendering\u{2026}";

/// A renderer that sleeps before producing output — the stand-in for a slow external CLI.
struct SlowContent {
    delay: Duration,
}
impl ContentProvider for SlowContent {
    fn render(&self, path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        std::thread::sleep(self.delay);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        RenderResult {
            content: Text::raw(format!("rendered:{name}")),
            notices: Vec::new(),
            source: None,
        }
    }
}

/// A renderer that panics only on `panic_file` and renders normally otherwise — so a test can
/// prove BOTH that a panic is contained (the panic file → placeholder) AND that the worker
/// survives it (a *different* file still renders real content afterwards, which can only arrive
/// if the worker thread lived through the panic).
struct PanicOnContent {
    panic_file: &'static str,
}
impl ContentProvider for PanicOnContent {
    fn render(&self, path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name == self.panic_file {
            panic!("renderer blew up on {name}");
        }
        RenderResult {
            content: Text::raw(format!("rendered:{name}")),
            notices: Vec::new(),
            source: None,
        }
    }
}

struct NoGit;
impl GitService for NoGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn changed_set(&self, _: Baseline) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn diff(&self, _: &Path, _: Baseline, _full: bool) -> String {
        String::new()
    }
}

struct NoEditor;
impl EditorHandoff for NoEditor {
    fn open(&mut self, _: &Path) -> EditorOutcome {
        EditorOutcome::NoTakeover
    }
}

/// A Git stub that records the `full_context` flag of every `diff()` call (made on the render
/// worker thread) and reports one changed file — so a test can prove the FullDiff view asks
/// git for whole-file context rather than the compact hunks-only diff.
struct RecordingGit {
    changed: BTreeMap<PathBuf, Status>,
    diff_full_calls: Arc<Mutex<Vec<bool>>>,
}
impl GitService for RecordingGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        self.changed.clone()
    }
    fn changed_set(&self, _: Baseline) -> BTreeMap<PathBuf, Status> {
        self.changed.clone()
    }
    fn diff(&self, _: &Path, _: Baseline, full_context: bool) -> String {
        self.diff_full_calls.lock().unwrap().push(full_context);
        if full_context {
            "FULL".into()
        } else {
            "COMPACT".into()
        }
    }
}

/// Flatten a content `Text` to a plain string for assertions.
fn flatten(text: &Text) -> String {
    text.lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_select_intent_does_not_block_on_a_slow_render_and_content_arrives_later() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "1\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "2\n").unwrap();

    let delay = Duration::from_millis(150);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(SlowContent { delay }), // `delay` is Copy → fresh each call
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

    // A select intent must return far faster than the render takes — it only dispatches.
    let start = Instant::now();
    let fx = ctrl.handle(Intent::NavDown);
    let handle_took = start.elapsed();
    assert!(
        fx.redraw,
        "the select still asks for a redraw (stale content shown meanwhile)"
    );
    // Non-blocking proof: had handle() waited for the render it would take at least `delay`
    // (the worker's sleep). The dispatch is an in-process channel send (sub-millisecond), so
    // a comfortable margin below `delay` is a robust, non-flaky bound.
    assert!(
        handle_took < delay,
        "handle() must not block on the slow render (took {handle_took:?}, render is {delay:?})"
    );
    // The fresh content has not arrived yet — proof the render is off-thread (AC-23).
    assert!(
        !flatten(ctrl.content()).contains("b.rs"),
        "selected file's content must not be ready synchronously"
    );

    // Drain results until the latest selection's content arrives as a later effect.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut redrew = false;
    loop {
        if let Some(p) = ctrl.poll() {
            redrew |= p.redraw;
        }
        if flatten(ctrl.content()).contains("b.rs") {
            break;
        }
        assert!(Instant::now() < deadline, "rendered content never arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(redrew, "the arriving content signalled a redraw via poll()");
    assert_eq!(
        flatten(ctrl.content()),
        "rendered:b.rs",
        "the selected file rendered"
    );
}

#[test]
fn full_diff_mode_asks_git_for_whole_file_context() {
    // PR2 (AC-23 path): cycling a changed file to FullDiff dispatches a render whose worker
    // reads the diff with full_context=true — so the whole file (not just hunks) is diffed.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("c.rs"), "fn main() {}\n").unwrap();
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("c.rs"), Status::Modified);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let git: Arc<dyn GitService> = Arc::new(RecordingGit {
        changed,
        diff_full_calls: calls.clone(),
    });
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::clone(&git),
            content: Box::new(SlowContent {
                delay: Duration::from_millis(0),
            }),
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );

    // The changed file defaults to the compact Diff; one cycle advances to FullDiff, which
    // dispatches a render whose worker requests a full-context diff.
    ctrl.handle(Intent::CycleView);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if calls.lock().unwrap().iter().any(|&full| full) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the worker never requested a full-context diff"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        calls.lock().unwrap().contains(&true),
        "FullDiff mode must ask git for whole-file context (full_context=true)"
    );
}

#[test]
fn a_superseded_render_does_not_overwrite_a_newer_selection() {
    // Rapid navigation: an earlier file's slow render must not clobber the content of the
    // file the user has since moved to (stale results are dropped by sequence).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "1\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "2\n").unwrap();
    std::fs::write(dir.path().join("c.rs"), "3\n").unwrap();

    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(SlowContent {
                delay: Duration::from_millis(80),
            }),
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

    // Fire several selections back-to-back; only the last (c.rs) should win.
    ctrl.handle(Intent::NavDown); // b.rs
    ctrl.handle(Intent::NavDown); // c.rs

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()) == "rendered:c.rs" {
            break;
        }
        assert!(Instant::now() < deadline, "final selection never rendered");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Give any stale (a.rs/b.rs) results a chance to wrongly land, then re-check.
    std::thread::sleep(Duration::from_millis(50));
    ctrl.poll();
    assert_eq!(
        flatten(ctrl.content()),
        "rendered:c.rs",
        "a superseded render must not overwrite the newer selection"
    );
}

#[test]
fn a_panicking_renderer_is_contained_and_the_worker_survives() {
    // AC-23 resilience: a renderer panic must not kill the worker (rendering would stop
    // forever) nor crash the app. (The deliberate panic prints to stderr; that is expected.)
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "1\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "2\n").unwrap();
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(PanicOnContent { panic_file: "b.rs" }), // `&'static str` is Copy
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

    // Select b.rs → its render() panics; the worker must catch it and surface a placeholder.
    ctrl.handle(Intent::NavDown); // b.rs
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()).contains("[content unavailable: renderer error]") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the contained-panic placeholder never arrived (the worker likely died)"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // Now select a.rs → a NORMAL render. Its DISTINCT content can only arrive if the worker
    // survived the earlier panic — a dead worker would leave the placeholder showing forever.
    // This (not the placeholder, which was already on screen) is what proves survival.
    ctrl.handle(Intent::NavUp); // a.rs renders normally
    let deadline2 = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()) == "rendered:a.rs" {
            break;
        }
        assert!(
            Instant::now() < deadline2,
            "the worker did not survive the panic (the post-panic render never arrived)"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// while an off-thread render for a newly-selected file is in flight, the content pane
/// must show a loading placeholder (NOT the previous file's body), and the content title must NOT
/// jump to the new file before its body arrives — title and body switch together when the render
/// result lands. A superseded render result (user moved on) must not overwrite the pane.
#[test]
fn a_slow_render_shows_a_loading_placeholder_and_switches_title_with_body() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "1\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "2\n").unwrap();
    std::fs::write(dir.path().join("c.rs"), "3\n").unwrap();

    let delay = Duration::from_millis(120);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(SlowContent { delay }),
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

    // Land the initial render for a.rs so a real (non-placeholder) title + body are on screen,
    // giving the loading-state assertion below a meaningful "previous file" to compare against.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()) == "rendered:a.rs" {
            break;
        }
        assert!(Instant::now() < deadline, "initial render never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Precondition: a.rs is the displayed file — its name is the content title.
    assert_eq!(
        ctrl.view_state().content_title.as_deref(),
        Some("a.rs"),
        "precondition: a.rs content landed, title is a.rs"
    );

    // Select b.rs — dispatch_render fires. While the render is in flight:
    //   - the body must be the loading placeholder (NOT a.rs's "rendered:a.rs"), and
    //   - the title must still be a.rs (NOT b.rs) — title + body switch together on landing.
    let start = Instant::now();
    let fx = ctrl.handle(Intent::NavDown);
    let handle_took = start.elapsed();
    assert!(
        fx.redraw,
        "the select asks for a redraw (loading state needs a repaint)"
    );
    assert!(
        handle_took < delay,
        "handle() must not block on the slow render (took {handle_took:?}, render is {delay:?})"
    );
    // (a) The body is the loading placeholder — the previous file's content is gone.
    assert_eq!(
        flatten(ctrl.content()),
        LOADING_PLACEHOLDER,
        "while a render is in flight the pane shows the loading placeholder, not the previous \
         file's body"
    );
    // (b) The title has NOT jumped to b.rs ahead of its body — it still names the displayed
    //     content's file (a.rs).
    assert_eq!(
        ctrl.view_state().content_title.as_deref(),
        Some("a.rs"),
        "the content title does not update ahead of the body — it stays on the displayed file \
         (a.rs) until b.rs's render lands"
    );

    // Drain poll until b.rs's render lands. The body and the title switch together.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(p) = ctrl.poll() {
            assert!(p.redraw, "the landing render signals a redraw");
        }
        if flatten(ctrl.content()) == "rendered:b.rs" {
            break;
        }
        assert!(Instant::now() < deadline, "b.rs render never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        flatten(ctrl.content()),
        "rendered:b.rs",
        "the selected file's rendered content arrived"
    );
    assert_eq!(
        ctrl.view_state().content_title.as_deref(),
        Some("b.rs"),
        "the title switched to b.rs together with its body"
    );
}

/// the content-pane left gap (`content_pad_left`) keys off the DISPLAYED content, not the tree
/// cursor — the same lockstep the title obeys. Selecting a markdown file while a code file is shown
/// must NOT flip the gap on before the markdown body lands, or the gap would jump ahead of the body
/// during an async render (the exact bug the `content_path`-keyed design avoids).
#[test]
fn the_left_gap_follows_the_displayed_file_not_the_selection_during_a_slow_render() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap(); // code → no gap
    std::fs::write(dir.path().join("b.md"), "# hi\n").unwrap(); // markdown → gap

    let delay = Duration::from_millis(120);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(SlowContent { delay }),
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

    // Land a.rs (code): no gap.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()) == "rendered:a.rs" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "initial a.rs render never landed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !ctrl.view_state().content_pad_left,
        "precondition: the displayed code file has no gap"
    );

    // Select b.md (markdown). Its render is in flight: the gap must STAY OFF (it follows the still-
    // displayed a.rs), exactly as the title stays on a.rs — never flipping to b.md's mode early.
    ctrl.handle(Intent::NavDown);
    assert_eq!(
        flatten(ctrl.content()),
        LOADING_PLACEHOLDER,
        "precondition: b.md's render is in flight"
    );
    assert!(
        !ctrl.view_state().content_pad_left,
        "the gap does not flip on ahead of the body — it tracks the displayed a.rs, not selected b.md"
    );

    // b.md's body lands: now the gap turns on, in lockstep with the body/title.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()) == "rendered:b.md" {
            break;
        }
        assert!(Instant::now() < deadline, "b.md render never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        ctrl.view_state().content_pad_left,
        "once the markdown body lands, the gap is on"
    );
}

/// a superseded render result (the user navigated on before it landed) must not
/// overwrite the loading placeholder nor the current pane — it's dropped by the seq guard in
/// `poll`. Two back-to-back selects leave only the LATEST file's render eligible to land.
#[test]
fn a_superseded_render_does_not_overwrite_the_loading_placeholder_nor_the_pane() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "1\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "2\n").unwrap();
    std::fs::write(dir.path().join("c.rs"), "3\n").unwrap();

    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(SlowContent {
                delay: Duration::from_millis(80),
            }),
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

    // Land the initial render for a.rs first (real content on screen).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()) == "rendered:a.rs" {
            break;
        }
        assert!(Instant::now() < deadline, "initial render never landed");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Rapid back-to-back navigation: a.rs → b.rs → c.rs. Each dispatch bumps `latest_seq`, so
    // b.rs's render is superseded the moment c.rs is selected — its result must be dropped by
    // `poll` (never applied), leaving only c.rs eligible to land.
    ctrl.handle(Intent::NavDown); // b.rs (loading placeholder showing; b.rs render in flight)
    assert_eq!(
        flatten(ctrl.content()),
        LOADING_PLACEHOLDER,
        "after selecting b.rs the pane shows the loading placeholder"
    );
    ctrl.handle(Intent::NavDown); // c.rs (supersedes b.rs; loading placeholder still showing)
    assert_eq!(
        flatten(ctrl.content()),
        LOADING_PLACEHOLDER,
        "after selecting c.rs the pane still shows the loading placeholder (b.rs's render was \
         superseded, not applied)"
    );

    // Only c.rs's render may land. Give any stale (b.rs) result a chance to wrongly land, then
    // re-check that c.rs is the displayed content.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()) == "rendered:c.rs" {
            break;
        }
        assert!(Instant::now() < deadline, "c.rs render never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    ctrl.poll();
    assert_eq!(
        flatten(ctrl.content()),
        "rendered:c.rs",
        "a superseded render (b.rs) must not overwrite the newer selection (c.rs)"
    );
}

// ---- content-pane resize → markdown reflow (the table fix) --------------------------------

/// A content provider that records the wrap `width` handed to every `render_at_width` call and
/// returns `lines` short lines, the first of which encodes that width (`w=<width>:<name>`). It lets
/// a test observe (a) that a content-pane resize threads the new pane width into the render, and
/// (b) that a reflow preserves view state — the body has enough lines to scroll and carries a
/// stable `lineN` token to search for.
struct WidthProbe {
    widths: Arc<Mutex<Vec<Option<u16>>>>,
    lines: usize,
}
impl WidthProbe {
    fn body(&self, width: Option<u16>, name: &str) -> RenderResult {
        let mut s = format!("w={width:?}:{name}");
        for i in 1..self.lines {
            s.push_str(&format!("\nline{i}"));
        }
        RenderResult {
            content: Text::raw(s),
            notices: Vec::new(),
            source: None,
        }
    }
}
impl ContentProvider for WidthProbe {
    fn render(&self, path: &Path, mode: ViewMode, raw_diff: Option<&str>) -> RenderResult {
        self.render_at_width(path, mode, raw_diff, None)
    }
    fn render_at_width(
        &self,
        path: &Path,
        _mode: ViewMode,
        _raw_diff: Option<&str>,
        width: Option<u16>,
    ) -> RenderResult {
        self.widths.lock().unwrap().push(width);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        self.body(width, &name)
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Spin `poll()` until the content pane contains `marker` (or the deadline trips).
fn await_contains(ctrl: &mut Controller, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()).contains(marker) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "content never contained {marker:?}; was {:?}",
            flatten(ctrl.content())
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn width_probe_components(widths: Arc<Mutex<Vec<Option<u16>>>>, lines: usize) -> Components {
    Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(WidthProbe {
                widths: Arc::clone(&widths),
                lines,
            }),
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    }
}

/// The core of the table fix: a content-pane *width* change re-renders rendered markdown at the new
/// pane width (so glow lays out tables to fit), while a *height*-only change does not (glow's layout
/// does not depend on height — re-rendering would be wasted work and would flash the pane).
#[test]
fn a_width_change_reflows_markdown_at_the_new_width_but_a_height_change_does_not() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("doc.md"), "# hi\n").unwrap();
    let widths = Arc::new(Mutex::new(Vec::new()));
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        width_probe_components(Arc::clone(&widths), 5),
    );

    // The initial render happens before any draw measured the pane → width unknown (None).
    await_contains(&mut ctrl, "w=None:doc.md");

    // A width change reflows markdown at the new pane width.
    ctrl.set_content_viewport(50, 10);
    await_contains(&mut ctrl, "w=Some(50):doc.md");
    assert!(
        widths.lock().unwrap().contains(&Some(50)),
        "a resize must thread the new pane width into the markdown render: {:?}",
        widths.lock().unwrap()
    );

    // A height-only change (same width) must NOT reflow — no further render is dispatched.
    let before = widths.lock().unwrap().len();
    ctrl.set_content_viewport(50, 14);
    std::thread::sleep(Duration::from_millis(50)); // give any (wrong) reflow time to land
    ctrl.poll();
    assert_eq!(
        widths.lock().unwrap().len(),
        before,
        "a height-only change must not re-render markdown"
    );
}

/// A width reflow is not a selection change: it must preserve the user's scroll position (a
/// split-bar drag must not yank the pane to the top) and recompute — rather than drop — a committed
/// search (a resize must not silently clear the active highlighting).
#[test]
fn a_width_reflow_preserves_scroll_and_recomputes_a_committed_search() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("doc.md"), "# hi\n").unwrap();
    let widths = Arc::new(Mutex::new(Vec::new()));
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        width_probe_components(Arc::clone(&widths), 50), // 50 lines → scrollable
    );
    await_contains(&mut ctrl, "w=None:doc.md");

    // Measure the pane (width 40, height 10) and land that reflow.
    ctrl.set_content_viewport(40, 10);
    await_contains(&mut ctrl, "w=Some(40):doc.md");

    // Commit a search for a token present in every render, then scroll away from the top.
    ctrl.handle(Intent::OpenSearch);
    for c in "line5".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(
        ctrl.search()
            .map(|s| !s.matches.is_empty())
            .unwrap_or(false),
        "precondition: a committed search with matches"
    );
    ctrl.scroll_to_line(20);
    let scrolled = ctrl.content_scroll();
    assert!(scrolled > 0, "precondition: scrolled away from the top");

    // Resize narrower (a width change) → reflow. Scroll and the committed search must survive.
    ctrl.set_content_viewport(30, 10);
    await_contains(&mut ctrl, "w=Some(30):doc.md");
    assert_eq!(
        ctrl.content_scroll(),
        scrolled,
        "a width reflow must preserve the scroll position, not reset to the top"
    );
    let search = ctrl
        .search()
        .expect("a committed search must survive a width reflow (recomputed, not dropped)");
    assert_eq!(search.query, "line5", "the committed query is unchanged");
    assert!(
        !search.matches.is_empty(),
        "the search is recomputed against the reflowed content"
    );
}

/// A resize only reflows rendered markdown — a non-markdown view (code / plain text) is
/// width-independent here (it h-scrolls, and its delegate manages its own width), so a width change
/// must not re-render it.
#[test]
fn a_width_change_does_not_reflow_non_markdown() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    let widths = Arc::new(Mutex::new(Vec::new()));
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        width_probe_components(Arc::clone(&widths), 5),
    );
    await_contains(&mut ctrl, "w=None:a.rs");

    let before = widths.lock().unwrap().len();
    ctrl.set_content_viewport(50, 10); // width change, but the selection is code, not markdown
    std::thread::sleep(Duration::from_millis(50));
    ctrl.poll();
    assert_eq!(
        widths.lock().unwrap().len(),
        before,
        "a resize must not re-render a non-markdown view"
    );
}
