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
fn re_root_to_missing_path_keeps_root_and_sets_notice() {
    // AC-16: if the target path does not exist (or is not a directory), re_root must leave all
    // state intact and set a non-fatal action notice — no partial re-root, no panic.
    let a = TempDir::new();
    common::init_repo_with_commit(a.path());
    std::fs::write(a.path().join("a.txt"), "a\n").unwrap();

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

    // Move the cursor and set a preference so we can confirm nothing changed.
    ctrl.handle(Intent::GrowTree);
    let split_before = ctrl.split_pct();

    // The target path does not exist.
    let missing = a.path().join("does-not-exist");
    assert!(
        !missing.exists(),
        "precondition: missing path must not exist"
    );

    ctrl.re_root(&missing);

    // Root is unchanged — the tree is still rooted at A.
    let nodes = ctrl.tree().visible_nodes();
    let first = nodes.first().expect("A should still have nodes");
    assert!(
        first.path.starts_with(common::canon(a.path())),
        "tree root must still be A after a failed re_root; got {}",
        first.path.display()
    );

    // A non-fatal notice is set.
    assert!(
        ctrl.action_notice().is_some(),
        "a non-fatal notice must be set when re_root targets a missing path"
    );
    let notice = ctrl.action_notice().unwrap();
    assert!(
        notice.contains("cannot switch worktree"),
        "notice should mention 'cannot switch worktree'; got: {notice}"
    );

    // Preferences are undisturbed.
    assert_eq!(
        ctrl.split_pct(),
        split_before,
        "split_pct must be unchanged"
    );
}

#[test]
fn re_root_to_current_root_is_a_noop() {
    // AC-11: re-selecting the current root is a clean no-op — no rebuild, no notice set.
    let a = TempDir::new();
    common::init_repo_with_commit(a.path());
    std::fs::write(a.path().join("a.txt"), "a\n").unwrap();

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

    // Move the cursor and set a preference to confirm nothing is disturbed.
    ctrl.handle(Intent::GrowTree);
    let split_before = ctrl.split_pct();
    ctrl.handle(Intent::NavDown);
    let cursor_before = ctrl.tree().cursor();

    // Re-root to A — the same root we're already at.
    ctrl.re_root(a.path());

    // No notice — a clean no-op emits nothing.
    assert!(
        ctrl.action_notice().is_none(),
        "re_root to the current root must not set a notice; got: {:?}",
        ctrl.action_notice()
    );

    // The tree is still at A (cursor unchanged — nav state was not reset).
    assert_eq!(
        ctrl.tree().cursor(),
        cursor_before,
        "cursor must be undisturbed by a no-op re_root"
    );
    assert_eq!(
        ctrl.split_pct(),
        split_before,
        "split_pct must be unchanged by a no-op re_root"
    );
}

#[test]
fn re_root_clears_an_open_picker() {
    // AC-13: a re-root resets navigation/view state — including closing any open worktree
    // picker. Open the picker in A (a real git repo so `worktree::list` enumerates rows), then
    // re-root to B and confirm the picker is gone.
    let a = TempDir::new();
    common::init_repo_with_commit(a.path());
    std::fs::write(a.path().join("a.txt"), "a\n").unwrap();

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

    ctrl.handle(Intent::SwitchWorktree);
    assert!(
        ctrl.picker().is_some(),
        "the picker opens inside a git repo before the re-root"
    );

    let b = TempDir::new();
    common::init_repo_with_commit(b.path());
    std::fs::write(b.path().join("b.txt"), "b\n").unwrap();
    ctrl.re_root(b.path());

    assert!(
        ctrl.picker().is_none(),
        "re_root clears the open picker (AC-13)"
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

// ---------------------------------------------------------------------------
// T-16 — Read-only / ephemeral / repo-only invariants (AC-N1, AC-N2, AC-N3, AC-N4)
// ---------------------------------------------------------------------------

/// Recursively collect all entries under `root` into a sorted Vec of (relative_path, contents).
/// Files carry their byte contents; directories carry an empty `Vec`. The result is sorted by
/// path so before/after comparisons are order-stable. Used to prove no file was created,
/// modified, or deleted by a switch operation (AC-N1, AC-N2, AC-N3).
///
/// Implemented over `std` only — zero new dependencies (constitution.md).
fn snapshot_dir(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn collect(dir: &Path, root: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut children: Vec<_> = std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        children.sort_by_key(|e| e.file_name());
        for entry in children {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                // Treat symlinks as files whose content is their target string.
                let target = std::fs::read_link(&path)
                    .map(|t| t.to_string_lossy().into_owned().into_bytes())
                    .unwrap_or_default();
                out.push((rel, target));
            } else if ft.is_file() {
                let contents = std::fs::read(&path).unwrap_or_default();
                out.push((rel, contents));
            } else if ft.is_dir() {
                out.push((rel, Vec::new()));
                collect(&path, root, out);
            }
        }
    }
    let mut entries = Vec::new();
    collect(root, root, &mut entries);
    entries
}

/// Drive a full switch via the public picker API: SwitchWorktree → NavDown → Activate.
/// Returns the path that was switched to (the linked worktree).
fn drive_full_switch(ctrl: &mut Controller) -> PathBuf {
    // Open the picker — the current root is pre-selected (cursor = 0 for the main worktree).
    ctrl.handle(Intent::SwitchWorktree);
    assert!(
        ctrl.picker().is_some(),
        "picker must open inside a git repo"
    );
    // Move the cursor to the linked worktree (index 1).
    ctrl.handle(Intent::NavDown);
    let target_path = ctrl
        .picker()
        .expect("picker still open after NavDown")
        .rows
        .get(ctrl.picker().unwrap().cursor)
        .expect("cursor row exists")
        .path
        .clone();
    // Confirm → triggers re_root to the linked worktree, closes the picker.
    ctrl.handle(Intent::Activate);
    assert!(
        ctrl.picker().is_none(),
        "picker must be closed after Activate"
    );
    target_path
}

/// AC-N1, AC-N2: a full switch (open picker → NavDown → Activate) performs only read-only
/// queries. The filesystem of both worktrees and the `git worktree list --porcelain` output
/// and HEAD of the repo are byte-for-byte unchanged before and after the switch.
#[test]
fn switch_mutates_no_file_and_no_worktree() {
    let repo = TempDir::new();
    common::init_repo_with_commit(repo.path());
    std::fs::write(repo.path().join("main.txt"), "main content\n").unwrap();

    // Create a linked worktree alongside the main one.
    let linked = TempDir::new();
    common::git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "switch-test-branch",
        ],
    );
    std::fs::write(linked.path().join("linked.txt"), "linked content\n").unwrap();

    // --- BEFORE snapshots ---
    let fs_main_before = snapshot_dir(repo.path());
    let fs_linked_before = snapshot_dir(linked.path());
    let wt_list_before = common::git(repo.path(), &["worktree", "list", "--porcelain"]);
    let head_main_before = common::git(repo.path(), &["rev-parse", "HEAD"]);
    let head_linked_before = common::git(linked.path(), &["rev-parse", "HEAD"]);

    // Build controller rooted at the main worktree.
    let components = Components {
        providers: fake_factory(),
        editor: Box::new(FakeEditor),
        clipboard: Box::new(FakeClipboard),
    };
    let mut ctrl = Controller::new(
        common::resolved(repo.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );

    // Drive the full switch flow.
    let switched_to = drive_full_switch(&mut ctrl);
    assert_eq!(
        common::canon(&switched_to),
        common::canon(linked.path()),
        "should have switched to the linked worktree"
    );

    // Drain poll() so the async status fill completes before re-snapshotting.
    poll_until(&mut ctrl, Duration::from_secs(5), |c| {
        c.tree()
            .visible_nodes()
            .first()
            .is_some_and(|n| n.path.starts_with(common::canon(linked.path())))
    });

    // --- AFTER snapshots ---
    let fs_main_after = snapshot_dir(repo.path());
    let fs_linked_after = snapshot_dir(linked.path());
    let wt_list_after = common::git(repo.path(), &["worktree", "list", "--porcelain"]);
    let head_main_after = common::git(repo.path(), &["rev-parse", "HEAD"]);
    let head_linked_after = common::git(linked.path(), &["rev-parse", "HEAD"]);

    // Filesystem of both worktrees: byte-for-byte unchanged (AC-N1).
    assert_eq!(
        fs_main_before, fs_main_after,
        "AC-N1: main worktree filesystem must be unchanged after the switch"
    );
    assert_eq!(
        fs_linked_before, fs_linked_after,
        "AC-N1: linked worktree filesystem must be unchanged after the switch"
    );

    // git worktree set: unchanged (AC-N2).
    assert_eq!(
        wt_list_before, wt_list_after,
        "AC-N2: `git worktree list` output must be unchanged after the switch"
    );

    // HEADs: unchanged (AC-N2).
    assert_eq!(
        head_main_before, head_main_after,
        "AC-N2: main HEAD must be unchanged after the switch"
    );
    assert_eq!(
        head_linked_before, head_linked_after,
        "AC-N2: linked HEAD must be unchanged after the switch"
    );
}

/// AC-N3: the switch is ephemeral — a fresh controller from the original launch context
/// re-resolves the original root, not the switched-to one. Also asserts that no persistent
/// state file was created under the repo or the XDG cache dir.
#[test]
fn switch_does_not_persist_root() {
    let repo = TempDir::new();
    common::init_repo_with_commit(repo.path());
    std::fs::write(repo.path().join("main.txt"), "main content\n").unwrap();

    let linked = TempDir::new();
    common::git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "persist-test-branch",
        ],
    );

    // Snapshot repo state BEFORE the switch.
    let repo_files_before = snapshot_dir(repo.path());

    // Controller A: rooted at the main repo.
    // HOME-override is not feasible from within this test (the controller reads env::var at
    // runtime and there is no injection point here); instead we snapshot the real XDG cache dir
    // before/after as the persistence guard — that is the actual path the app could write to.
    let original_resolved = common::resolved(repo.path().to_path_buf(), true);
    let components_a = Components {
        providers: fake_factory(),
        editor: Box::new(FakeEditor),
        clipboard: Box::new(FakeClipboard),
    };
    let mut ctrl_a = Controller::new(original_resolved.clone(), Baseline::Head, components_a);

    // Snapshot the real XDG/home cache dir before the switch.
    let cache_dir_path = herdr_file_viewer::update::cache::cache_dir();
    let cache_before = cache_dir_path
        .as_deref()
        .map(snapshot_dir)
        .unwrap_or_default();

    // Drive the full switch.
    let switched_to = drive_full_switch(&mut ctrl_a);
    assert_eq!(
        common::canon(&switched_to),
        common::canon(linked.path()),
        "ctrl_a should have switched to the linked worktree"
    );

    // Drain poll() so any async I/O completes.
    poll_until(&mut ctrl_a, Duration::from_secs(5), |c| {
        c.tree()
            .visible_nodes()
            .first()
            .is_some_and(|n| n.path.starts_with(common::canon(linked.path())))
    });

    // --- Snapshot AFTER: no new files under the repo or the cache dir. ---
    let repo_files_after = snapshot_dir(repo.path());
    let cache_after = cache_dir_path
        .as_deref()
        .map(snapshot_dir)
        .unwrap_or_default();

    // No new files under the repo (the controller writes nothing to the working tree).
    assert_eq!(
        repo_files_before, repo_files_after,
        "AC-N3: no persistent state file must appear under the repo after a switch"
    );
    // The update cache is not written by the controller (only by app.rs's `set_update` path).
    assert_eq!(
        cache_before, cache_after,
        "AC-N3: the update cache must not be written by the controller during a switch"
    );

    // Controller B: fresh controller from the ORIGINAL launch context. Must root at the
    // original worktree (repo.path()), not at `linked` — state is in-memory only (AC-N3).
    let components_b = Components {
        providers: fake_factory(),
        editor: Box::new(FakeEditor),
        clipboard: Box::new(FakeClipboard),
    };
    let mut ctrl_b = Controller::new(original_resolved, Baseline::Head, components_b);

    // Drain so the tree populates.
    poll_until(&mut ctrl_b, Duration::from_secs(5), |c| {
        !c.tree().visible_nodes().is_empty()
    });

    // Fresh controller is rooted at the original path (AC-N3).
    assert_eq!(
        common::canon(ctrl_b.root()),
        common::canon(repo.path()),
        "AC-N3: fresh controller must root at the original path, not the switched-to one"
    );

    // No nodes from the linked worktree leak into the fresh controller's tree.
    let nodes = ctrl_b.tree().visible_nodes();
    assert!(
        !nodes
            .iter()
            .any(|n| n.path.starts_with(common::canon(linked.path()))),
        "AC-N3: fresh controller must not show nodes from the switched-to worktree"
    );
}

/// AC-N4: the picker lists only the current repository's worktrees — no paths belonging to a
/// second, unrelated git repo and no arbitrary directories outside the current repo's worktree
/// set.
#[test]
fn picker_lists_only_this_repos_worktrees() {
    // Repo R with a linked worktree.
    let repo_r = TempDir::new();
    common::init_repo_with_commit(repo_r.path());
    std::fs::write(repo_r.path().join("r.txt"), "r\n").unwrap();

    let linked_r = TempDir::new();
    common::git(
        repo_r.path(),
        &[
            "worktree",
            "add",
            linked_r.path().to_str().unwrap(),
            "-b",
            "repo-r-branch",
        ],
    );

    // Unrelated repo Q (completely independent — separate git dir, no shared history).
    let repo_q = TempDir::new();
    common::init_repo_with_commit(repo_q.path());
    std::fs::write(repo_q.path().join("q.txt"), "q\n").unwrap();
    let linked_q = TempDir::new();
    common::git(
        repo_q.path(),
        &[
            "worktree",
            "add",
            linked_q.path().to_str().unwrap(),
            "-b",
            "repo-q-branch",
        ],
    );

    // Collect R's own worktree paths from git (canonical).
    let r_wt_raw = common::git(repo_r.path(), &["worktree", "list", "--porcelain"]);
    let r_wt_paths: std::collections::HashSet<PathBuf> = r_wt_raw
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .map(|l| PathBuf::from(l.trim_start_matches("worktree ")))
        .map(|p| common::canon(&p))
        .collect();

    // Collect Q's worktree paths (canonical) — these must NOT appear in R's picker.
    let q_wt_raw = common::git(repo_q.path(), &["worktree", "list", "--porcelain"]);
    let q_wt_paths: std::collections::HashSet<PathBuf> = q_wt_raw
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .map(|l| PathBuf::from(l.trim_start_matches("worktree ")))
        .map(|p| common::canon(&p))
        .collect();

    // Precondition: R's and Q's worktree sets are disjoint.
    for qp in &q_wt_paths {
        assert!(
            !r_wt_paths.contains(qp),
            "precondition: repo Q path {qp:?} must not be in repo R's worktree set"
        );
    }

    // Build controller rooted at repo R.
    let components = Components {
        providers: fake_factory(),
        editor: Box::new(FakeEditor),
        clipboard: Box::new(FakeClipboard),
    };
    let mut ctrl = Controller::new(
        common::resolved(repo_r.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );

    // Open the picker in repo R.
    ctrl.handle(Intent::SwitchWorktree);
    let picker = ctrl
        .picker()
        .expect("picker must open inside repo R (a git repo)");

    // Every row's path must be a member of R's own worktree set (AC-N4).
    for row in &picker.rows {
        let canon_row = common::canon(&row.path);
        assert!(
            r_wt_paths.contains(&canon_row),
            "AC-N4: picker row {canon_row:?} is not in repo R's worktree set"
        );
    }

    // No row must belong to repo Q.
    for row in &picker.rows {
        let canon_row = common::canon(&row.path);
        assert!(
            !q_wt_paths.contains(&canon_row),
            "AC-N4: picker row {canon_row:?} belongs to unrelated repo Q — must be excluded"
        );
    }
}
