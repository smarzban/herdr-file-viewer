//! Media view clear/set discipline (the plan's task 4): the controller must place the image on
//! screen exactly when and where it should be, and clear it when it should not — proven with a
//! `GraphicsSink` recorder, never a real socket.
//!
//! The discipline is one comparison, not N call sites: after every draw the controller computes
//! the desired media state and issues `clear()` + `set()` on a difference (`clear()` alone when
//! nothing should be shown). A `GraphicsHost` recorder is the automated oracle here — ratatui's
//! `TestBackend` renders a text grid, so no snapshot can prove an image appeared.

mod common;

use common::TempDir;
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::graphics::{CellMetrics, GraphicsCommand, GraphicsSink};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter::PaneGeometry;
use herdr_file_viewer::render::{MediaPayload, Renderers};
use ratatui::layout::Rect;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A Content Renderer stub that answers a media payload for `*.png` / `*.mp4` and plain text
/// otherwise — standing in for the real `render_media` delegation without an external converter.
struct MediaContent;

impl ContentProvider for MediaContent {
    fn render(&self, path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        match path.extension().and_then(|e| e.to_str()) {
            Some("png") => RenderResult {
                content: Text::raw("[image: 4×3 PNG]"),
                notices: Vec::new(),
                source: None,
                media: Some(MediaPayload {
                    kind: herdr_file_viewer::media::MediaKind::Png,
                    png: png_bytes(4, 3),
                    natural: (4, 3),
                    duration_s: None,
                }),
            },
            Some("mp4") => RenderResult {
                content: Text::raw("[video: 4×3 — p to play]"),
                notices: Vec::new(),
                source: None,
                media: Some(MediaPayload {
                    kind: herdr_file_viewer::media::MediaKind::Video,
                    png: png_bytes(4, 3), // the still preview (frame 0)
                    natural: (4, 3),
                    duration_s: Some(12.0),
                }),
            },
            _ => RenderResult {
                content: Text::raw("plain"),
                notices: Vec::new(),
                source: None,
                media: None,
            },
        }
    }
}

/// A minimal PNG header (the 24-byte IHDR prefix) — enough for `png_dimensions` / the fast path.
fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    b.extend_from_slice(&13u32.to_be_bytes());
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b
}

/// A [`GitService`] stub that reports nothing changed — media tests don't care about git.
struct StubGit;

impl GitService for StubGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn diff(&self, _rel_path: &Path, _baseline: Baseline, _full_context: bool) -> String {
        String::new()
    }
    fn diff_directory(&self, _rel_dir: &Path, _baseline: Baseline) -> String {
        String::new()
    }
}

/// An editor stub that never launches anything.
struct StubEditor;
impl EditorHandoff for StubEditor {
    fn open(&mut self, _file: &Path) -> EditorOutcome {
        EditorOutcome::NotLaunched("no editor".into())
    }
}

/// A [`GraphicsSink`] recorder: captures every command synchronously instead of touching a socket.
/// Shared (`Arc<Mutex<_>>`) so the test keeps a handle to read back after handing it over.
#[derive(Default, Clone)]
struct RecordingSink {
    commands: Arc<Mutex<Vec<String>>>,
}

impl GraphicsSink for RecordingSink {
    fn send(&self, command: GraphicsCommand) {
        let line = match &command {
            GraphicsCommand::Hide => "hide".to_string(),
            GraphicsCommand::Show(frame) => format!(
                "show {}x{} png@{}x{} ",
                frame.width, frame.height, frame.placement.grid_cols, frame.placement.grid_rows
            ),
        };
        self.commands.lock().unwrap().push(line);
    }
}

fn build(root: &Path) -> (Controller, RecordingSink) {
    let git: Arc<dyn GitService> = Arc::new(StubGit);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::clone(&git),
            content: Box::new(MediaContent),
        }),
        editor: Box::new(StubEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(root.to_path_buf(), false),
        Baseline::Head,
        components,
    );
    let sink = RecordingSink::default();
    ctrl.set_graphics(
        Box::new(sink.clone()),
        Some(CellMetrics {
            cell_width_px: 20,
            cell_height_px: 41,
        }),
    );
    (ctrl, sink)
}

/// Wait for the worker's render of the current selection to land (`poll` applies it), i.e. the
/// displayed content stops being a `Rendering…` placeholder.
fn await_content(ctrl: &mut Controller, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !flatten(ctrl.content()).contains(marker) {
        assert!(
            Instant::now() < deadline,
            "content '{marker}' never rendered"
        );
        ctrl.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn flatten(t: &Text) -> String {
    t.lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect()
}

use herdr_file_viewer::view_policy::ViewMode;

#[test]
fn media_selecting_png_clears_then_sets_and_never_repeats() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.png"), png_bytes(4, 3)).unwrap();
    let (mut ctrl, sink) = build(dir.path());
    ctrl.set_pane_geometry(geom_with_inner(Rect::new(0, 0, 40, 24)));
    await_content(&mut ctrl, "4×3");

    ctrl.sync_media();
    let calls = sink.commands.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        2,
        "clear-then-set on first placement: {calls:?}"
    );
    assert_eq!(calls[0], "hide");
    assert!(
        calls[1].starts_with("show 4x3 png@"),
        "the image is placed at a fitted rect: {calls:?}"
    );

    // An unchanged placement must NOT issue a redundant set on every idle draw.
    ctrl.sync_media();
    assert_eq!(
        sink.commands.lock().unwrap().len(),
        2,
        "no redundant set when nothing changed"
    );
}

#[test]
fn media_replacing_media_clears_then_sets_the_new_image() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("b.png"), png_bytes(4, 3)).unwrap();
    std::fs::write(dir.path().join("c.png"), png_bytes(4, 3)).unwrap();
    let (mut ctrl, sink) = build(dir.path());
    ctrl.set_pane_geometry(geom_with_inner(Rect::new(0, 0, 40, 24)));
    await_content(&mut ctrl, "4×3");
    ctrl.sync_media();
    let before = sink.commands.lock().unwrap().len();
    assert_eq!(before, 2);

    // Select the second image → the worker renders it → the discipline clears-then-sets.
    ctrl.handle(Intent::NavDown);
    ctrl.poll();
    let deadline = Instant::now() + Duration::from_secs(5);
    while ctrl.tree().cursor() != 1 {
        assert!(Instant::now() < deadline, "cursor never advanced");
        ctrl.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    await_content(&mut ctrl, "4×3");
    ctrl.sync_media();
    let calls = sink.commands.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        before + 2,
        "a different image clears-then-sets again: {calls:?}"
    );
    assert_eq!(calls[before], "hide", "old image cleared");
    assert_eq!(&calls[before + 1][..13], "show 4x3 png@", "new image shown");
}

#[test]
fn leaving_media_clears_the_image_and_keeping_it_alone_does_nothing() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.png"), png_bytes(4, 3)).unwrap();
    std::fs::write(dir.path().join("note.txt"), "plain").unwrap();
    let (mut ctrl, sink) = build(dir.path());
    ctrl.set_pane_geometry(geom_with_inner(Rect::new(0, 0, 40, 24)));
    await_content(&mut ctrl, "4×3");
    ctrl.sync_media();
    let before = sink.commands.lock().unwrap().len();

    // Select the text file → its render has no media → the discipline clears alone.
    ctrl.handle(Intent::NavDown);
    await_content(&mut ctrl, "plain");
    ctrl.sync_media();
    let calls = sink.commands.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        before + 1,
        "leaving media issues a clear alone, no phantom set: {calls:?}"
    );
    assert_eq!(calls[before], "hide");
    assert!(calls[before + 1..].is_empty());

    // Still on the text file → desired is still None → nothing more.
    ctrl.sync_media();
    assert_eq!(sink.commands.lock().unwrap().len(), before + 1);
}

/// A geometry whose content column is drawn, so `content_inner` is measurable.
fn geom_with_inner(inner: Rect) -> PaneGeometry {
    PaneGeometry {
        content_inner: Some(inner),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Video playback intents (the decoder thread is driven through the injected `video` renderer,
// which here is THIS test binary re-executing as a fixture that emits PNG frames).
// ---------------------------------------------------------------------------

const VIDEO_FIXTURE_ARG: &str = "--hfv-video-fixture=";

/// A minimal PNG (signature + header + IEND trailer) — enough for the splitter and the host-side
/// `png_dimensions`; the exact contents don't matter to the controller under test.
fn video_frame(tag: u8) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    b.extend_from_slice(&13u32.to_be_bytes());
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&4u32.to_be_bytes());
    b.extend_from_slice(&3u32.to_be_bytes());
    b.push(tag);
    b.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D']);
    b.extend_from_slice(&[0, 0, 0, 0]);
    b
}

/// Run as the video fixture: write `n` PNG frames to stdout (self-exec), then exit. When run by
/// the test harness normally (no fixture arg), it is a no-op so the suite itself also passes.
#[test]
fn video_fixture() {
    let Some(n) = std::env::args()
        .find_map(|a| a.strip_prefix(VIDEO_FIXTURE_ARG).map(str::to_owned))
        .and_then(|s| s.parse::<usize>().ok())
    else {
        return; // not an execution of the fixture — a no-op in a normal suite run
    };
    use std::io::Write;
    for i in 0..n {
        let _ = std::io::stdout().write_all(&video_frame(i as u8));
    }
    let _ = std::io::stdout().flush();
}

/// The `video` renderer: this test binary re-invoked as the fixture, emitting 3 frames then EOF.
fn video_renderer() -> Renderers {
    Renderers {
        markdown: vec!["cat".into()],
        diff: vec!["cat".into()],
        full_diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
        image: vec!["cat".into()],
        probe: Vec::new(),
        video: vec![
            std::env::current_exe()
                .expect("test binary path")
                .display()
                .to_string(),
            "--exact".into(),
            "video_fixture".into(),
            "--".into(),
            format!("{VIDEO_FIXTURE_ARG}3"),
        ],
        timeout: Duration::from_secs(5),
    }
}

/// Build a controller over `root` whose `video` renderer is the fixture (hermetic — no ffmpeg).
fn build_video(root: &Path) -> (Controller, RecordingSink) {
    let git: Arc<dyn GitService> = Arc::new(StubGit);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::clone(&git),
            content: Box::new(MediaContent),
        }),
        editor: Box::new(StubEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: Some(video_renderer()),
    };
    let mut ctrl = Controller::new(
        common::resolved(root.to_path_buf(), false),
        Baseline::Head,
        components,
    );
    let sink = RecordingSink::default();
    ctrl.set_graphics(
        Box::new(sink.clone()),
        Some(CellMetrics {
            cell_width_px: 20,
            cell_height_px: 41,
        }),
    );
    (ctrl, sink)
}

#[test]
fn video_play_pause_seek_restart_drive_the_decoder() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("clip.mp4"), b"not a real mp4").unwrap();
    let (mut ctrl, sink) = build_video(dir.path());
    ctrl.set_pane_geometry(geom_with_inner(Rect::new(0, 0, 40, 24)));
    await_content(&mut ctrl, "p to play");
    ctrl.sync_media();
    let still_count = sink.commands.lock().unwrap().len();
    assert_eq!(
        still_count, 2,
        "the still preview is placed (clear-then-set) on selection"
    );

    // `p` starts playback: the decoder streams frames; tick_media paces one per tick.
    ctrl.handle(Intent::MediaPlayPause);
    let mut frames = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while frames < 2 {
        assert!(Instant::now() < deadline, "playback never produced frames");
        if ctrl.tick_media(Instant::now()) {
            frames += 1;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let calls = sink.commands.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .filter(|c| c.starts_with("show 4x3 png@"))
            .count()
            >= frames,
        "playback frames are shown: {calls:?}"
    );

    // `p` again pauses: no new frames are pushed to the graphics sink. Synchronous tell, no sleep:
    // a paused (or just-finished) player's tick must send nothing (per AGENTS.md a "nothing happened"
    // sleep proves nothing).
    ctrl.handle(Intent::MediaPlayPause);
    let before = sink.commands.lock().unwrap().len();
    ctrl.tick_media(Instant::now());
    assert_eq!(
        sink.commands.lock().unwrap().len(),
        before,
        "paused playback sends nothing"
    );

    // `0` restarts: a fresh decoder is spawned and a frame reaches the sink — either via the
    // paused single-frame preview (`media_start` pulls one immediately) or via a tick when the
    // restart resumes playing. `media_start` first sends the Hide, so wait for the SHOW that must
    // follow it rather than any command growth.
    ctrl.handle(Intent::MediaRestart);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let calls = sink.commands.lock().unwrap();
        let shown_since_restart = calls.iter().skip(before).any(|c| c.starts_with("show "));
        assert!(
            Instant::now() < deadline,
            "restart never re-decoded a frame: {calls:?}"
        );
        if shown_since_restart {
            break;
        }
        drop(calls);
        ctrl.tick_media(Instant::now());
        std::thread::sleep(Duration::from_millis(1));
    }
    // `media_start` clears first (Hide) then shows the restarted segment's first frame, so a
    // stale frame of the previous segment can never survive the restart.
    let calls = sink.commands.lock().unwrap().clone();
    let restart_calls: Vec<&String> = calls.iter().skip(before).collect();
    assert_eq!(
        restart_calls[0].as_str(),
        "hide",
        "restart clears the old frame first"
    );
    assert!(
        restart_calls.iter().any(|c| c.starts_with("show 4x3 png@")),
        "restart re-decodes and shows"
    );
}
