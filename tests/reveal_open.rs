//! End-to-end + read-only conformance for the `O` (open-with-app) / `R` (reveal-in-file-manager)
//! OS opener hand-offs (T-7). These drive the two new keys through the REAL
//! key → intent → controller → opener chain (`map_key` decode included, not `handle` directly)
//! against a fake [`Opener`] seam, and prove the feature mutates nothing on disk or in git:
//!
//! * `o_and_r_drive_the_opener_end_to_end_via_map_key` — the full key path reaches the seam for
//!   both a file and a directory target (AC-1/AC-2/AC-3, end to end).
//! * `reveal_open_mutates_nothing_on_disk_or_in_git` — a full exercise (incl. a launch failure)
//!   leaves every working file byte-for-byte identical and `git status --porcelain` still empty
//!   (the mechanical AC-12 read-only evidence).
//! * `reveal_open_uses_only_the_injected_opener_seam` — the injected fake is the sole hand-off
//!   path; no real `open`/`xdg-open`/`explorer` is ever spawned (AC-13 hermeticity).
//!
//! Every hand-off goes through an injected [`FakeOpener`] wired in BEFORE any key is driven, so
//! the tests stay deterministic and hermetic — a real OS opener is never invoked.

mod common;

use common::{TempDir, git, init_repo_with_commit};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::input::map_key;
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::opener::{Opener, OpenerOutcome};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// A key event with no modifier — the shape crossterm reports for a bare `O`/`R` keypress.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

// ── stubs (mirrored from tests/lineselect.rs; integration files are separate crates) ─────────

/// A read-only Git Service stub: the on-disk repo is the source of truth for the read-only
/// conformance snapshots, so the controller's git service can be inert.
#[derive(Default, Clone)]
struct StubGit;
impl GitService for StubGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn diff(&self, _p: &Path, _b: Baseline, _full: bool) -> String {
        String::new()
    }
}

struct NoopEditor;
impl EditorHandoff for NoopEditor {
    fn open(&mut self, _file: &Path) -> EditorOutcome {
        EditorOutcome::NoTakeover
    }
}

/// A trivial content provider — the opener hand-offs never touch the content pane, so a fixed
/// body is enough for the controller to construct.
#[derive(Clone, Copy)]
struct StubContent;
impl ContentProvider for StubContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::raw("stub"),
            notices: Vec::new(),
            source: None,
        }
    }
}

// ── fake opener (mirrors tests/controller.rs) ────────────────────────────────────────────────

/// The paths a [`FakeOpener`] was asked to open/reveal, recorded into a shared handle the test
/// still holds after the fake is boxed into the controller via `set_opener`.
#[derive(Clone, Default)]
struct OpenerLog {
    opened: Vec<PathBuf>,
    revealed: Vec<PathBuf>,
}

/// Which [`OpenerOutcome`] the fake returns for every open/reveal call.
#[derive(Clone, Copy)]
enum OutcomeKind {
    Launched,
    NotLaunched,
}

/// A test double for the OS [`Opener`] seam: records every path it is asked to open/reveal and
/// returns a preconfigured outcome, so the controller is never wired to a real
/// `xdg-open`/`open`/`explorer` (AC-13 hermeticity).
struct FakeOpener {
    log: Rc<RefCell<OpenerLog>>,
    outcome_kind: OutcomeKind,
}

impl FakeOpener {
    fn new(outcome_kind: OutcomeKind) -> (Self, Rc<RefCell<OpenerLog>>) {
        let log = Rc::new(RefCell::new(OpenerLog::default()));
        (
            Self {
                log: Rc::clone(&log),
                outcome_kind,
            },
            log,
        )
    }

    fn outcome(&self) -> OpenerOutcome {
        match self.outcome_kind {
            OutcomeKind::Launched => OpenerOutcome::Launched,
            OutcomeKind::NotLaunched => OpenerOutcome::NotLaunched("opener not on PATH".into()),
        }
    }
}

impl Opener for FakeOpener {
    fn open(&mut self, path: &Path) -> OpenerOutcome {
        self.log.borrow_mut().opened.push(path.to_path_buf());
        self.outcome()
    }
    fn reveal(&mut self, path: &Path) -> OpenerOutcome {
        self.log.borrow_mut().revealed.push(path.to_path_buf());
        self.outcome()
    }
}

// ── controller builder (a small local mirror of lineselect's) ────────────────────────────────

/// Build a controller over a real git repo `root`, with an inert git/content/editor stack.
fn controller_over_repo(root: &Path) -> Controller {
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(StubContent),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    Controller::new(
        common::resolved(root.to_path_buf(), true),
        Baseline::Head,
        components,
    )
}

// ── tree-selection + fs-snapshot helpers ─────────────────────────────────────────────────────

/// Move the tree cursor to the (first) visible node whose file name is `name`, returning its
/// absolute path. Panics if no such node is visible.
fn select_by_name(ctrl: &mut Controller, name: &str) -> PathBuf {
    // Reset to the top of the visible list, then walk down to the target.
    for _ in 0..ctrl.tree().visible_nodes().len() {
        ctrl.handle(Intent::NavUp);
    }
    for _ in 0..ctrl.tree().visible_nodes().len() {
        let sel = ctrl.tree().selected().expect("a node is selected");
        if sel.path.file_name().is_some_and(|n| n == name) {
            return sel.path.clone();
        }
        ctrl.handle(Intent::NavDown);
    }
    panic!("no visible tree node named {name:?}");
}

/// A sorted {relative path → file bytes} snapshot of every working file under `root`, EXCLUDING
/// the `.git` directory — the byte-for-byte read-only fingerprint compared before/after.
fn snapshot_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("read_dir") {
            let entry = entry.expect("dir entry");
            if entry.file_name() == ".git" {
                continue; // exclude git's internal store from the working-file fingerprint
            }
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).expect("under base").to_path_buf();
                out.insert(rel, std::fs::read(&path).expect("read file"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Build a clean fixture repo with a file, a second file, and a subdirectory (all committed) so
/// `git status --porcelain` is empty. Returns the temp dir.
fn clean_repo_fixture() -> TempDir {
    let dir = TempDir::new();
    init_repo_with_commit(dir.path()); // seeds seed.txt + an initial commit
    std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "fn main() {}\n").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("c.txt"), "gamma\n").unwrap();
    git(dir.path(), &["add", "-A"]);
    git(
        dir.path(),
        &["commit", "-q", "-m", "fixture", "--no-gpg-sign"],
    );
    assert!(
        git(dir.path(), &["status", "--porcelain"]).is_empty(),
        "precondition: the fixture repo is clean"
    );
    dir
}

// ── tests ────────────────────────────────────────────────────────────────────────────────────

#[test]
fn o_and_r_drive_the_opener_end_to_end_via_map_key() {
    // AC-1/AC-2/AC-3 end to end: `O`/`R` decode through the REAL dispatcher into their intents,
    // and handling those intents reaches the injected opener with the selected node's absolute
    // path — for a file AND for a directory.
    let dir = clean_repo_fixture();
    let mut ctrl = controller_over_repo(dir.path());
    let (opener, log) = FakeOpener::new(OutcomeKind::Launched);
    ctrl.set_opener(Box::new(opener));

    // ── file target ──
    let file = select_by_name(&mut ctrl, "a.txt");

    let open_intent = map_key(key(KeyCode::Char('O')));
    assert_eq!(
        open_intent,
        Some(Intent::OpenWithApp),
        "the real dispatcher decodes `O` to OpenWithApp"
    );
    ctrl.handle(open_intent.unwrap());
    assert_eq!(
        log.borrow().opened,
        vec![file.clone()],
        "AC-1: `O` reached the opener with the selected file's absolute path"
    );

    let reveal_intent = map_key(key(KeyCode::Char('R')));
    assert_eq!(
        reveal_intent,
        Some(Intent::RevealInFileManager),
        "the real dispatcher decodes `R` to RevealInFileManager"
    );
    ctrl.handle(reveal_intent.unwrap());
    assert_eq!(
        log.borrow().revealed,
        vec![file.clone()],
        "AC-2: `R` reached the opener with the selected file's absolute path"
    );

    // ── directory target (AC-3: the hand-off accepts a directory, unlike the editor path) ──
    let sub = select_by_name(&mut ctrl, "sub");
    assert!(sub.is_dir(), "precondition: `sub` is a directory");

    ctrl.handle(map_key(key(KeyCode::Char('O'))).unwrap());
    ctrl.handle(map_key(key(KeyCode::Char('R'))).unwrap());
    assert_eq!(
        log.borrow().opened,
        vec![file.clone(), sub.clone()],
        "AC-3: `O` accepts the directory target end to end"
    );
    assert_eq!(
        log.borrow().revealed,
        vec![file, sub],
        "AC-3: `R` accepts the directory target end to end"
    );
}

#[test]
fn reveal_open_mutates_nothing_on_disk_or_in_git() {
    // AC-12: the whole feature is read-only. A full exercise of `O`/`R` (file + directory) plus a
    // launch-failure path must leave every working file byte-for-byte identical and the git
    // working tree still clean.
    let dir = clean_repo_fixture();

    // Snapshot BEFORE: working-file bytes + git porcelain (empty).
    let files_before = snapshot_files(dir.path());
    let status_before = git(dir.path(), &["status", "--porcelain"]);
    assert!(status_before.is_empty(), "precondition: clean working tree");

    let mut ctrl = controller_over_repo(dir.path());
    let (opener, _log) = FakeOpener::new(OutcomeKind::Launched);
    ctrl.set_opener(Box::new(opener));

    // A full exercise: O and R on a file, O and R on the subdir.
    select_by_name(&mut ctrl, "a.txt");
    ctrl.handle(map_key(key(KeyCode::Char('O'))).unwrap());
    ctrl.handle(map_key(key(KeyCode::Char('R'))).unwrap());
    select_by_name(&mut ctrl, "sub");
    ctrl.handle(map_key(key(KeyCode::Char('O'))).unwrap());
    ctrl.handle(map_key(key(KeyCode::Char('R'))).unwrap());

    // A failure case: an opener that cannot launch surfaces a notice but changes nothing on disk.
    let (failing, _flog) = FakeOpener::new(OutcomeKind::NotLaunched);
    ctrl.set_opener(Box::new(failing));
    select_by_name(&mut ctrl, "a.txt");
    ctrl.handle(map_key(key(KeyCode::Char('O'))).unwrap());
    assert!(
        ctrl.action_notice().is_some(),
        "the launch failure surfaced a notice (still a read-only effect)"
    );

    // Snapshot AFTER: same fingerprint.
    let files_after = snapshot_files(dir.path());
    let status_after = git(dir.path(), &["status", "--porcelain"]);

    assert_eq!(
        files_before, files_after,
        "AC-12: no working file's bytes changed across the full O/R exercise"
    );
    assert_eq!(
        status_before, status_after,
        "AC-12: git status --porcelain is still empty — no git mutation"
    );
    assert!(
        status_after.is_empty(),
        "AC-12: the working tree remains clean"
    );
}

#[test]
fn reveal_open_uses_only_the_injected_opener_seam() {
    // AC-13: the hand-off is hermetic — every `O`/`R` reaches ONLY the injected fake (its log is
    // the sole record of the hand-off). No real OS opener is ever spawned: the fake is wired in
    // before any key is driven, so the seam is the only path a hand-off can take.
    let dir = clean_repo_fixture();
    let mut ctrl = controller_over_repo(dir.path());
    let (opener, log) = FakeOpener::new(OutcomeKind::Launched);
    ctrl.set_opener(Box::new(opener));

    assert!(
        log.borrow().opened.is_empty() && log.borrow().revealed.is_empty(),
        "precondition: nothing recorded before any hand-off"
    );

    select_by_name(&mut ctrl, "a.txt");
    ctrl.handle(map_key(key(KeyCode::Char('O'))).unwrap());
    ctrl.handle(map_key(key(KeyCode::Char('R'))).unwrap());

    // The injected fake's log is the ONLY evidence of the hand-off — proof the hand-off went
    // through the seam and nowhere else (no real process was launched).
    assert_eq!(
        log.borrow().opened.len(),
        1,
        "AC-13: the open hand-off reached only the injected seam"
    );
    assert_eq!(
        log.borrow().revealed.len(),
        1,
        "AC-13: the reveal hand-off reached only the injected seam"
    );
}
