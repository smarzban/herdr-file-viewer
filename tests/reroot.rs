//! T-6 — Provider Factory (ADR-0004). The controller is built from a *factory* closure that
//! yields the root-bound providers (Git Service + Content Renderer) for a given [`Resolved`],
//! rather than from fixed instances. This is the construction shape a later re-root (T-7/T-8)
//! re-invokes to rebuild those providers against a new root. Here we prove the seam in
//! isolation: a fake factory returns fake providers, `Controller::new` builds, and the first
//! frame renders the fake content — touching no real git, renderer, or editor.

mod common;

use common::TempDir;
use herdr_file_viewer::controller::{
    Clipboard, Components, ContentProvider, Controller, EditorHandoff, GitService, RenderResult,
    RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::root::Resolved;
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A fake Git Service that knows nothing — the reroot seam needs no real git.
struct FakeGit;
impl GitService for FakeGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn diff(&self, _rel: &Path, _baseline: Baseline, _full: bool) -> String {
        String::new()
    }
}

/// A fake Content Renderer that emits a known marker, so we can see the first frame populate.
struct FakeContent;
impl ContentProvider for FakeContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::raw("fake-rendered-content"),
            notices: Vec::new(),
        }
    }
}

struct FakeEditor;
impl EditorHandoff for FakeEditor {
    fn open(&mut self, _file: &Path) -> io::Result<bool> {
        Ok(false)
    }
}

struct FakeClipboard;
impl Clipboard for FakeClipboard {
    fn copy(&mut self, _text: &str) -> io::Result<()> {
        Ok(())
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
fn controller_builds_from_a_provider_factory_and_renders_the_first_frame() {
    // ADR-0004: `Controller::new` takes a `Resolved` plus a factory closure that builds the
    // root-bound providers for it. The factory is invoked once at launch; the first selection's
    // content is rendered through the factory's content provider.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();

    let providers: Box<dyn Fn(&Resolved) -> RootProviders> =
        Box::new(|_resolved: &Resolved| RootProviders {
            git: Arc::new(FakeGit),
            content: Box::new(FakeContent),
        });
    let components = Components {
        providers,
        editor: Box::new(FakeEditor),
        clipboard: Box::new(FakeClipboard),
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

    // The first frame is producible (the two-column view state assembles).
    let _ = ctrl.view_state();

    // The fake factory's content renders for the initial selection — drained off the worker.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()).contains("fake-rendered-content") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the factory's content never rendered the first frame"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
