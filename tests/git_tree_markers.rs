//! Baseline-aware markers must populate the full tree before any filter key is pressed.

mod common;

use common::TempDir;
use herdr_file_viewer::controller::{Components, Controller, GitService, RootProviders};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Facts {
    status: BTreeMap<PathBuf, Status>,
    base: BTreeMap<PathBuf, Status>,
    head: BTreeMap<PathBuf, Status>,
}

struct MarkerGit(Arc<Mutex<Facts>>);

impl GitService for MarkerGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        self.0.lock().unwrap().status.clone()
    }

    fn changed_set(&self, baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        let facts = self.0.lock().unwrap();
        match baseline {
            Baseline::Base => facts.base.clone(),
            Baseline::Head => facts.head.clone(),
        }
    }

    fn diff(&self, _: &Path, _: Baseline, _: bool) -> String {
        String::new()
    }

    fn diff_directory(&self, _: &Path, _: Baseline) -> String {
        String::new()
    }
}

fn fixture() -> (TempDir, Arc<Mutex<Facts>>) {
    let root = TempDir::new();
    std::fs::create_dir(root.path().join("src")).unwrap();
    for name in ["src/nested.rs", "modified.rs", "added.rs", "clean.rs"] {
        std::fs::write(root.path().join(name), "fixture\n").unwrap();
    }
    let facts = Arc::new(Mutex::new(Facts {
        // A clean working tree still has committed changes relative to the branch baseline.
        base: BTreeMap::from([
            (PathBuf::from("modified.rs"), Status::Modified),
            (PathBuf::from("added.rs"), Status::Added),
            (PathBuf::from("src/nested.rs"), Status::Modified),
        ]),
        ..Facts::default()
    }));
    (root, facts)
}

fn controller(root: &Path, facts: Arc<Mutex<Facts>>) -> Controller {
    let git: Arc<dyn GitService> = Arc::new(MarkerGit(facts));
    Controller::new(
        common::resolved(root.to_path_buf(), true),
        Baseline::Base,
        Components {
            providers: Box::new(move |_| RootProviders {
                git: Arc::clone(&git),
                content: Box::new(common::NoopContent),
            }),
            editor: Box::new(common::NoopEditor),
            clipboard: Box::new(common::RecordingClipboard::default()),
            renderers: None,
        },
    )
}

fn markers(controller: &Controller) -> BTreeMap<PathBuf, (Option<Status>, bool)> {
    controller
        .view_state()
        .nodes
        .into_iter()
        .map(|node| {
            (
                node.path
                    .strip_prefix(controller.root())
                    .unwrap()
                    .to_path_buf(),
                (node.status, node.dir_dirty),
            )
        })
        .collect()
}

#[test]
fn full_tree_first_frame_shows_committed_file_and_directory_markers_without_c() {
    let (root, facts) = fixture();
    let mut controller = controller(root.path(), facts);
    assert!(!controller.changed_only());
    let initial = markers(&controller);
    assert_eq!(
        initial[Path::new("modified.rs")],
        (Some(Status::Modified), false)
    );
    assert_eq!(initial[Path::new("added.rs")], (Some(Status::Added), false));
    assert_eq!(initial[Path::new("src")], (None, true));
    assert_eq!(initial[Path::new("clean.rs")], (None, false));

    // Check the initial painted marker/color too, not just the controller's cache.
    let view = controller.view_state();
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal
        .draw(|frame| {
            presenter::draw(frame, &view);
        })
        .unwrap();
    for (symbol, color) in [
        ("M", Color::LightRed),
        ("A", Color::LightGreen),
        ("●", Color::LightRed),
    ] {
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == symbol && cell.fg == color),
            "initial frame must paint {symbol} with {color:?}"
        );
    }

    controller.handle(Intent::ToggleChangedOnly);
    controller.handle(Intent::ToggleChangedOnly);
    assert_eq!(
        markers(&controller),
        initial,
        "c filters rows, not whether changes have markers"
    );
}

#[test]
fn baseline_and_status_mode_changes_keep_full_tree_markers_current_without_c() {
    let (root, facts) = fixture();
    let mut controller = controller(root.path(), facts);
    assert_eq!(
        markers(&controller)[Path::new("modified.rs")].0,
        Some(Status::Modified)
    );

    controller.handle(Intent::ToggleStatusMode);
    assert!(
        controller.tree().visible_nodes().is_empty(),
        "d excludes committed-only changes"
    );
    controller.handle(Intent::ToggleBaseline);
    assert!(controller.status_mode());
    assert_eq!(controller.baseline(), Baseline::Head);
    assert!(
        controller.tree().visible_nodes().is_empty(),
        "b must not overwrite d's working-tree filter"
    );
    controller.handle(Intent::ToggleStatusMode);
    assert!(!controller.changed_only());
    for (_, (status, dirty)) in markers(&controller) {
        assert_eq!(
            status, None,
            "HEAD sees no changes in this clean working tree"
        );
        assert!(
            !dirty,
            "no stale branch-only directory dots after leaving d"
        );
    }

    controller.handle(Intent::ToggleBaseline);
    assert_eq!(controller.baseline(), Baseline::Base);
    assert_eq!(
        markers(&controller)[Path::new("modified.rs")].0,
        Some(Status::Modified)
    );
    assert!(markers(&controller)[Path::new("src")].1);
    controller.handle(Intent::ToggleBaseline);
    assert_eq!(markers(&controller)[Path::new("modified.rs")].0, None);
    assert!(!markers(&controller)[Path::new("src")].1);
}

#[test]
fn leaving_status_mode_by_reveal_restores_baseline_markers() {
    let (root, facts) = fixture();
    let mut controller = controller(root.path(), facts);
    controller.handle(Intent::ToggleStatusMode);
    assert!(controller.status_mode());

    // `clean.rs` is not in working-tree status, so revealing it relaxes `d` without the key.
    let target = herdr_file_viewer::open_target::parse_open_target("clean.rs").unwrap();
    controller.apply_open_target(&target);
    assert!(!controller.status_mode());
    assert!(!controller.changed_only());

    let markers = markers(&controller);
    assert_eq!(markers[Path::new("modified.rs")].0, Some(Status::Modified));
    assert_eq!(markers[Path::new("added.rs")].0, Some(Status::Added));
    assert!(
        markers[Path::new("src")].1,
        "baseline dirty-directory dot is restored"
    );
}

#[test]
fn refresh_removes_stale_baseline_markers_from_an_unfiltered_tree() {
    let (root, facts) = fixture();
    let mut controller = controller(root.path(), Arc::clone(&facts));
    // Seed via c as well: the old code otherwise never populated the fallback map at startup.
    controller.handle(Intent::ToggleChangedOnly);
    controller.handle(Intent::ToggleChangedOnly);
    assert!(markers(&controller)[Path::new("src")].1);
    facts.lock().unwrap().base.clear();
    assert!(controller.handle(Intent::Refresh).redraw);
    assert!(!controller.changed_only());
    assert_eq!(markers(&controller)[Path::new("modified.rs")].0, None);
    assert!(!markers(&controller)[Path::new("src")].1);
}

#[test]
fn working_tree_status_keeps_precedence_over_baseline_fallback() {
    let (root, facts) = fixture();
    {
        let mut facts = facts.lock().unwrap();
        facts
            .status
            .insert(PathBuf::from("added.rs"), Status::Modified);
        facts
            .status
            .insert(PathBuf::from("clean.rs"), Status::Untracked);
    }
    let controller = controller(root.path(), facts);
    let markers = markers(&controller);
    assert_eq!(
        markers[Path::new("added.rs")].0,
        Some(Status::Modified),
        "a branch-added file subsequently edited is working-tree modified"
    );
    assert_eq!(markers[Path::new("clean.rs")].0, Some(Status::Untracked));
    assert_eq!(
        markers[Path::new("modified.rs")].0,
        Some(Status::Modified),
        "committed-only fallback is also present"
    );
}

#[cfg(unix)]
#[test]
fn real_viewer_paints_clean_feature_branch_markers_before_any_filter_key() {
    use expectrl::process::unix::WaitStatus;
    use expectrl::{Eof, Expect, Regex, Session};
    use std::time::Duration;

    let root = TempDir::new();
    let support = TempDir::new();
    common::init_repo_with_commit(root.path());
    // Pin the base branch: `git init` inherits the host's `init.defaultBranch`, and base
    // resolution only recognises main/master, so a `trunk` default would hide every marker.
    common::git(root.path(), &["checkout", "-qB", "main"]);
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("modified.rs"), "before\n").unwrap();
    std::fs::write(root.path().join("src/nested.rs"), "before\n").unwrap();
    common::git(root.path(), &["add", "."]);
    common::git(root.path(), &["commit", "-qm", "baseline"]);
    common::git(root.path(), &["checkout", "-qb", "feature/markers"]);
    std::fs::write(root.path().join("modified.rs"), "after\n").unwrap();
    std::fs::write(root.path().join("src/nested.rs"), "after\n").unwrap();
    std::fs::write(root.path().join("added.rs"), "new\n").unwrap();
    common::git(root.path(), &["add", "."]);
    common::git(root.path(), &["commit", "-qm", "committed branch changes"]);
    assert!(
        common::git(root.path(), &["status", "--porcelain"])
            .trim()
            .is_empty()
    );
    let before = common::workspace_fingerprint(root.path());

    std::fs::write(support.path().join("config.toml"), "baseline = \"base\"\n").unwrap();
    let mut command = common::viewer_command(root.path());
    command
        .env("HERDR_PLUGIN_CONFIG_DIR", support.path())
        .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
        .env_remove("HERDR_FILE_VIEWER_OPEN");
    let mut session = Session::spawn(command).unwrap();
    session.set_expect_timeout(Some(Duration::from_secs(15)));
    // These glyphs must arrive before ANY key, with a clean working tree; waiting for a generic
    // filename would not distinguish a correct initial colored tree from the broken one.
    session
        .expect(Regex("● +▸ src"))
        .expect("committed nested change marks its directory");
    session
        .expect(Regex("A +added\\.rs"))
        .expect("branch-added file is marked at startup");
    session
        .expect(Regex("M +modified\\.rs"))
        .expect("branch-modified file is marked at startup");
    // Only the startup tree was observed. No modal, selection, search, or zoom was entered.
    session.send("q").unwrap();
    session.expect(Eof).unwrap();
    assert!(matches!(
        session.get_process().wait().unwrap(),
        WaitStatus::Exited(_, 0)
    ));
    assert_eq!(
        common::workspace_fingerprint(root.path()),
        before,
        "displaying branch markers stays read-only"
    );
}
