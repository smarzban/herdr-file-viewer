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
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter::Focus;
use herdr_file_viewer::root::Resolved;
use herdr_file_viewer::tree::NodeKind;
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

/// A factory that hands the same kind of fakes to every (re-)root — so re-rooting rebuilds
/// providers without ever touching real git/renderers. Used to prove `re_root` rebuilds the
/// root-bound state, carries the prefs, and resets navigation (AC-7/8/12/13).
fn fake_factory() -> Box<dyn Fn(&Resolved) -> RootProviders> {
    Box::new(|_resolved: &Resolved| RootProviders {
        git: Arc::new(FakeGit),
        content: Box::new(FakeContent),
    })
}

#[test]
fn re_root_rebuilds_at_the_new_root_carrying_prefs_and_resetting_nav() {
    // Root A is a real git repo (so the git-only toggles — changed-only, baseline — actually
    // take effect and can be observed to carry). It has a subdirectory with a file so a dir can
    // be expanded and the cursor moved, making the nav-reset observable.
    let a = TempDir::new();
    common::init_repo_with_commit(a.path());
    std::fs::create_dir_all(a.path().join("sub")).unwrap();
    std::fs::write(a.path().join("sub/child.txt"), "child\n").unwrap();
    std::fs::write(a.path().join("top.txt"), "top\n").unwrap();

    let components = Components {
        providers: fake_factory(),
        editor: Box::new(FakeEditor),
        clipboard: Box::new(FakeClipboard),
    };
    let mut ctrl = Controller::new(
        common::resolved(a.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );

    // --- Mutate prefs away from their defaults via the public intent surface. ---
    let split_default = ctrl.split_pct();
    ctrl.handle(Intent::GrowTree); // tree-resize → split_pct changes off its default
    ctrl.handle(Intent::ToggleWrap); // `w` → wrap_override true
    ctrl.handle(Intent::ToggleChangedOnly); // changed-only on (git repo → takes effect)
    ctrl.handle(Intent::ToggleIgnore); // show-ignored on
    ctrl.handle(Intent::ToggleBaseline); // baseline Head → Base

    let split_pref = ctrl.split_pct();
    assert_ne!(split_pref, split_default, "GrowTree should move the split");
    assert!(ctrl.wrap_override(), "wrap toggled on");
    assert!(ctrl.changed_only(), "changed-only toggled on");
    assert!(ctrl.show_ignored(), "show-ignored toggled on");
    assert_eq!(ctrl.baseline(), Baseline::Base, "baseline toggled to Base");

    // --- Set navigation/view state that re_root must reset. ---
    // Drive a real directory expansion + cursor move + zoom, so the nav reset is observable.
    // Changed-only mode is built from the (empty fake) changed-set, so flip it off to see the
    // real filesystem tree; assert the FS-driven nav state here, then turn changed-only back on
    // so it is the value carried across the re-root.
    ctrl.handle(Intent::ToggleChangedOnly); // off → full filesystem tree, dirs collapsed
    assert!(
        ctrl.tree()
            .visible_nodes()
            .first()
            .is_some_and(|n| n.kind == NodeKind::Dir),
        "row 0 is the `sub` directory to expand"
    );
    ctrl.handle(Intent::Expand); // expand the selected dir (cursor at row 0 = `sub`)
    assert!(
        ctrl.tree().visible_nodes().iter().any(|n| n.expanded),
        "the dir should be expanded before re_root"
    );
    ctrl.handle(Intent::NavDown); // move the cursor off the root row
    assert!(
        ctrl.tree().cursor() > 0,
        "cursor moved off the root row before re_root"
    );
    ctrl.handle(Intent::ToggleChangedOnly); // changed-only back on (the carried pref state)
    ctrl.handle(Intent::ToggleZoom); // zoom on, focus → content
    assert!(ctrl.zoomed());
    assert_eq!(ctrl.focus(), Focus::Content);

    // --- Re-root to a fresh directory B (its own git repo, so the carried changed-only flag can
    // be flipped off below to inspect the real filesystem tree). ---
    let b = TempDir::new();
    common::init_repo_with_commit(b.path());
    std::fs::create_dir_all(b.path().join("bsub")).unwrap();
    std::fs::write(b.path().join("bsub/inner.txt"), "inner\n").unwrap();
    std::fs::write(b.path().join("b.txt"), "b\n").unwrap();
    ctrl.re_root(b.path());

    // Preferences are CARRIED (AC-12) — none reset. (Accessor-based, mode-independent.)
    assert_eq!(ctrl.split_pct(), split_pref, "split_pct carried");
    assert!(ctrl.wrap_override(), "wrap_override carried");
    assert!(ctrl.changed_only(), "changed_only carried");
    assert!(ctrl.show_ignored(), "show_ignored carried");
    assert_eq!(ctrl.baseline(), Baseline::Base, "baseline carried");

    // Navigation/view state is RESET (AC-13).
    assert_eq!(ctrl.tree().cursor(), 0, "cursor back at the root row");
    assert!(!ctrl.zoomed(), "unzoomed after re_root");
    assert_eq!(ctrl.focus(), Focus::Tree, "focus back on the tree");

    // The tree is rooted at B with a fresh (collapsed) expansion set. The fake changed-set is
    // empty, so the carried `changed_only` pref currently shows an empty tree; flip it off (B is
    // a git repo, so the toggle takes effect) to observe the real filesystem tree.
    ctrl.handle(Intent::ToggleChangedOnly);
    let nodes = ctrl.tree().visible_nodes();
    let root_child = nodes.first().expect("B has at least one node");
    assert!(
        root_child.path.starts_with(common::canon(b.path())),
        "tree should be rooted under B after re_root; got {}",
        root_child.path.display()
    );
    assert!(
        !nodes.iter().any(|n| n.expanded),
        "no expansions carried into the new tree"
    );
}
