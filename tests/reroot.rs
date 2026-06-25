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

/// A fake Git Service whose status / changed-set are canned per construction, so a re-root's
/// factory can hand back a git that reports a *distinct* status for the new root — letting a
/// test prove the new root's markers fill in (and only via `poll`, asynchronously). The
/// `diff` carries a canned string so a post-switch render through the respawned worker is
/// observable too.
struct CannedGit {
    status: BTreeMap<PathBuf, Status>,
    changed: BTreeMap<PathBuf, Status>,
    diff: String,
}
impl GitService for CannedGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        self.status.clone()
    }
    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        self.changed.clone()
    }
    fn diff(&self, _rel: &Path, _baseline: Baseline, _full: bool) -> String {
        self.diff.clone()
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

    // T-8: the git-derived state (status markers + the changed-only filter built from the
    // changed-set) now fills in ASYNCHRONOUSLY, applied by `poll` rather than synchronously in
    // `re_root`. Wait for the carried `changed_only` filter to actually be applied against B's
    // (empty) changed-set — observable as the filtered tree becoming empty — before inspecting
    // the visible tree below. Waiting on this exact condition (not on the first `poll` to apply
    // *anything*, which the render worker can satisfy first) makes the drain race-free. The
    // pref-value and nav-reset assertions above are mode-independent and stay synchronous.
    poll_until(&mut ctrl, Duration::from_secs(5), |c| {
        c.tree().visible_nodes().is_empty()
    });

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

/// A factory that, for the root whose path ends in `b_dir_name`, hands back a [`CannedGit`]
/// reporting `b_status` as its working-tree status (and changed-set) plus `b_diff` as its
/// diff; every other root gets the empty [`FakeGit`]. This lets the tests build at A (no git
/// markers) then re-root to B and observe B's distinct fake git fill in. The content provider
/// is the usual [`FakeContent`] marker.
fn factory_varying_by_root(
    b_root: PathBuf,
    b_status: BTreeMap<PathBuf, Status>,
    b_diff: String,
) -> Box<dyn Fn(&Resolved) -> RootProviders> {
    let b_canon = common::canon(&b_root);
    Box::new(move |resolved: &Resolved| {
        let git: Arc<dyn GitService> = if common::canon(&resolved.root) == b_canon {
            Arc::new(CannedGit {
                status: b_status.clone(),
                changed: b_status.clone(),
                diff: b_diff.clone(),
            })
        } else {
            Arc::new(FakeGit)
        };
        RootProviders {
            git,
            content: Box::new(FakeContent),
        }
    })
}

/// Drain `poll` until `cond(ctrl)` actually holds, or panic when `deadline` elapses. `re_root`
/// dispatches TWO async producers (the render worker and the status thread); `poll` returns
/// `Some` when it applies EITHER, so stopping on the first `Some` can return before the state a
/// test asserts on has landed. Waiting on the asserted condition itself (not merely on "something
/// was applied") makes the wait race-free regardless of which producer lands first.
fn poll_until(ctrl: &mut Controller, deadline: Duration, cond: impl Fn(&Controller) -> bool) {
    let limit = Instant::now() + deadline;
    loop {
        ctrl.poll();
        if cond(ctrl) {
            return;
        }
        assert!(
            Instant::now() < limit,
            "condition never held within {deadline:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn re_root_status_fills_in_asynchronously() {
    // A is a real git repo with no extra markers (empty fake git). B is a real git repo whose
    // factory-supplied fake git reports `b.txt` as Modified — a marker that exists ONLY for B.
    let a = TempDir::new();
    common::init_repo_with_commit(a.path());
    std::fs::write(a.path().join("a.txt"), "a\n").unwrap();

    let b = TempDir::new();
    common::init_repo_with_commit(b.path());
    std::fs::write(b.path().join("b.txt"), "b\n").unwrap();
    let mut b_status = BTreeMap::new();
    b_status.insert(PathBuf::from("b.txt"), Status::Modified);

    let components = Components {
        providers: factory_varying_by_root(b.path().to_path_buf(), b_status, String::new()),
        editor: Box::new(FakeEditor),
        clipboard: Box::new(FakeClipboard),
    };
    let mut ctrl = Controller::new(
        common::resolved(a.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );

    ctrl.re_root(b.path());

    // STRUCTURAL state is immediate: the tree is rooted at B and navigable right away, with no
    // `poll` yet. (The synchronous re-root resolved the new root and built a fresh tree.)
    let nodes = ctrl.tree().visible_nodes();
    let root_child = nodes.first().expect("B has at least one node");
    assert!(
        root_child.path.starts_with(common::canon(b.path())),
        "tree rooted under B immediately (structural re-root is synchronous)"
    );

    // But B's git STATUS has NOT been applied yet — the off-thread computation has not been
    // drained by `poll`. So no node carries B's Modified marker synchronously.
    assert!(
        ctrl.tree()
            .visible_nodes()
            .iter()
            .all(|n| n.status.is_none()),
        "status must NOT be applied synchronously in re_root — it fills in via poll (AC-17)"
    );

    // Now drain `poll` until B's Modified marker on b.txt actually lands — waiting on the
    // asserted condition, not on the first `poll` to apply *something* (the render worker may
    // land before the status thread). The post-poll assertion then cannot flake.
    poll_until(&mut ctrl, Duration::from_secs(5), |c| {
        c.tree()
            .visible_nodes()
            .iter()
            .any(|n| n.path.ends_with("b.txt") && n.status == Some(Status::Modified))
    });
    assert!(
        ctrl.tree()
            .visible_nodes()
            .iter()
            .any(|n| n.path.ends_with("b.txt") && n.status == Some(Status::Modified)),
        "after poll, B's fake-git Modified marker on b.txt is applied (async fill)"
    );
}

#[test]
fn re_root_markers_reflect_new_root_git() {
    // A has its own (empty) fake git; B's factory git reports `b.txt` as Added. After a re-root
    // to B and draining poll, the tree markers reflect B's git, not A's.
    let a = TempDir::new();
    common::init_repo_with_commit(a.path());
    std::fs::write(a.path().join("a.txt"), "a\n").unwrap();

    let b = TempDir::new();
    common::init_repo_with_commit(b.path());
    std::fs::write(b.path().join("b.txt"), "b\n").unwrap();
    let mut b_status = BTreeMap::new();
    b_status.insert(PathBuf::from("b.txt"), Status::Added);

    let components = Components {
        providers: factory_varying_by_root(b.path().to_path_buf(), b_status, String::new()),
        editor: Box::new(FakeEditor),
        clipboard: Box::new(FakeClipboard),
    };
    let mut ctrl = Controller::new(
        common::resolved(a.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );

    ctrl.re_root(b.path());
    // Wait until B's `b.txt` node actually carries the expected Added marker — not until the
    // first `poll` applies something (the render worker may land before the status thread).
    poll_until(&mut ctrl, Duration::from_secs(5), |c| {
        c.tree()
            .visible_nodes()
            .iter()
            .any(|n| n.path.ends_with("b.txt") && n.status == Some(Status::Added))
    });

    let b_marked = ctrl
        .tree()
        .visible_nodes()
        .into_iter()
        .find(|n| n.path.ends_with("b.txt"))
        .expect("b.txt is visible under B");
    assert_eq!(
        b_marked.status,
        Some(Status::Added),
        "markers reflect B's fake git after re_root + poll"
    );
    // A's file is not even in B's tree — the marker set is B's alone.
    assert!(
        !ctrl
            .tree()
            .visible_nodes()
            .iter()
            .any(|n| n.path.ends_with("a.txt")),
        "A's nodes do not leak into B's tree"
    );
}

#[test]
fn re_root_render_resolves_through_the_respawned_worker() {
    // After a re-root, selecting a file dispatches a render that must resolve through the
    // worker respawned for B — drained by `poll`, the content shows the (B) factory's marker.
    let a = TempDir::new();
    common::init_repo_with_commit(a.path());
    std::fs::write(a.path().join("a.txt"), "a\n").unwrap();

    let b = TempDir::new();
    common::init_repo_with_commit(b.path());
    std::fs::write(b.path().join("b.txt"), "b\n").unwrap();

    let components = Components {
        providers: factory_varying_by_root(b.path().to_path_buf(), BTreeMap::new(), String::new()),
        editor: Box::new(FakeEditor),
        clipboard: Box::new(FakeClipboard),
    };
    let mut ctrl = Controller::new(
        common::resolved(a.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );

    ctrl.re_root(b.path());
    // re_root dispatches a render for B's initial selection; the respawned worker (FakeContent)
    // resolves it. Drain until the content reflects the fake render.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()).contains("fake-rendered-content") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the respawned worker never rendered after re_root"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
