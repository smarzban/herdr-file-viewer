//! Opening and dismissing remote notice details are display-only operations.

mod common;

use common::{TempDir, git, init_repo_with_commit};
use herdr_file_viewer::controller::{
    Clipboard, Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::opener::{Opener, OpenerOutcome};
use herdr_file_viewer::update::spotlight_policy::{
    SpotlightCache, SpotlightInput, cache_delta, project,
};
use herdr_file_viewer::update::{NoticeSnapshot, UpdateState, Version};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct SpyGit {
    calls: Arc<Mutex<Vec<&'static str>>>,
}

impl GitService for SpyGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        self.calls.lock().unwrap().push("status");
        BTreeMap::new()
    }

    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        self.calls.lock().unwrap().push("changed_set");
        BTreeMap::new()
    }

    fn diff(&self, _path: &Path, _baseline: Baseline, _full_context: bool) -> String {
        self.calls.lock().unwrap().push("diff");
        String::new()
    }

    fn diff_directory(&self, _path: &Path, _baseline: Baseline) -> String {
        self.calls.lock().unwrap().push("diff_directory");
        String::new()
    }
}

struct Content;

impl ContentProvider for Content {
    fn render(&self, _path: &Path, _mode: ViewMode, _diff: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::raw("content"),
            notices: Vec::new(),
            source: None,
        }
    }
}

struct SpyEditor {
    calls: Arc<Mutex<Vec<PathBuf>>>,
}

impl EditorHandoff for SpyEditor {
    fn open(&mut self, path: &Path) -> EditorOutcome {
        self.calls.lock().unwrap().push(path.to_path_buf());
        EditorOutcome::NoTakeover
    }
}

struct SpyClipboard {
    calls: Arc<Mutex<Vec<String>>>,
}

impl Clipboard for SpyClipboard {
    fn copy(&mut self, text: &str) -> std::io::Result<()> {
        self.calls.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

struct SpyOpener {
    calls: Arc<Mutex<Vec<(&'static str, PathBuf)>>>,
}

impl Opener for SpyOpener {
    fn open(&mut self, path: &Path) -> OpenerOutcome {
        self.calls
            .lock()
            .unwrap()
            .push(("open", path.to_path_buf()));
        OpenerOutcome::Launched
    }

    fn reveal(&mut self, path: &Path) -> OpenerOutcome {
        self.calls
            .lock()
            .unwrap()
            .push(("reveal", path.to_path_buf()));
        OpenerOutcome::Launched
    }
}

#[derive(Debug, PartialEq, Eq)]
struct WorkspaceFingerprint {
    bytes: BTreeMap<PathBuf, Vec<u8>>,
    head: String,
    porcelain: String,
    worktrees: String,
}

fn workspace_fingerprint(root: &Path) -> WorkspaceFingerprint {
    fn collect(root: &Path, dir: &Path, bytes: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("read workspace").flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            let metadata = entry.metadata().expect("workspace metadata");
            if metadata.is_dir() {
                collect(root, &path, bytes);
            } else if metadata.is_file() {
                bytes.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).expect("read workspace file"),
                );
            }
        }
    }

    let mut bytes = BTreeMap::new();
    collect(root, root, &mut bytes);
    WorkspaceFingerprint {
        bytes,
        head: git(root, &["rev-parse", "HEAD"]),
        porcelain: git(root, &["status", "--porcelain=v1"]),
        worktrees: git(root, &["worktree", "list", "--porcelain"]),
    }
}

fn flatten(text: &Text<'_>) -> String {
    text.lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect()
}

fn snapshot() -> NoticeSnapshot {
    let source = concat!(
        "# Safety spotlight\n",
        "remote command: rm -rf /\n",
        "https://example.invalid/download?run=1\n",
        "\x1b]52;c;not-a-clipboard\x07cursor:\x1b[2Jstill display text\n",
    );
    let mut spotlight = SpotlightCache::default();
    spotlight.apply(cache_delta(
        project(SpotlightInput::Available(source.as_bytes().to_vec())),
        1,
    ));
    NoticeSnapshot {
        detected_release: Some(Version::parse("9.2.0").unwrap()),
        spotlight,
        ..NoticeSnapshot::default()
    }
}

#[test]
fn displaying_and_dismissing_remote_details_never_calls_external_ports_or_mutates_the_workspace() {
    let dir = TempDir::new();
    init_repo_with_commit(dir.path());
    std::fs::write(dir.path().join("note.md"), "workspace bytes\n").unwrap();
    git(dir.path(), &["add", "note.md"]);
    git(dir.path(), &["commit", "-qm", "add note"]);

    let git_calls = Arc::new(Mutex::new(Vec::new()));
    let editor_calls = Arc::new(Mutex::new(Vec::new()));
    let clipboard_calls = Arc::new(Mutex::new(Vec::new()));
    let opener_calls = Arc::new(Mutex::new(Vec::new()));
    let git_service: Arc<dyn GitService> = Arc::new(SpyGit {
        calls: Arc::clone(&git_calls),
    });
    let mut controller = Controller::new(
        common::resolved(dir.path().to_path_buf(), true),
        Baseline::Head,
        Components {
            providers: Box::new(move |_| RootProviders {
                git: Arc::clone(&git_service),
                content: Box::new(Content),
            }),
            editor: Box::new(SpyEditor {
                calls: Arc::clone(&editor_calls),
            }),
            clipboard: Box::new(SpyClipboard {
                calls: Arc::clone(&clipboard_calls),
            }),
            renderers: None,
        },
    );
    controller.set_opener(Box::new(SpyOpener {
        calls: Arc::clone(&opener_calls),
    }));
    controller.set_update(UpdateState {
        initial: snapshot(),
        rx: None,
    });

    let workspace_before = workspace_fingerprint(dir.path());
    let git_before = git_calls.lock().unwrap().clone();
    controller.handle(Intent::ShowHelp);
    let body = flatten(controller.help_state().unwrap().active_body());
    assert!(body.contains("remote command: rm -rf /"), "{body}");
    assert!(
        body.contains("https://example.invalid/download?run=1"),
        "{body}"
    );
    assert!(body.contains("still display text"), "{body}");
    assert!(
        !body.contains('\x1b'),
        "remote control-shaped bytes are neutralized before display: {body:?}"
    );
    assert_eq!(
        workspace_fingerprint(dir.path()),
        workspace_before,
        "opening Help is display-only for workspace bytes, HEAD, porcelain, and worktree state"
    );
    assert_eq!(git_calls.lock().unwrap().as_slice(), git_before.as_slice());
    assert!(editor_calls.lock().unwrap().is_empty());
    assert!(clipboard_calls.lock().unwrap().is_empty());
    assert!(opener_calls.lock().unwrap().is_empty());

    controller.close_help();
    assert!(controller.handle(Intent::DismissUpdate).redraw);
    assert!(controller.view_state().remote_notice_status.is_none());
    assert_eq!(
        workspace_fingerprint(dir.path()),
        workspace_before,
        "dismissal is display-state only and cannot mutate the workspace"
    );
    assert_eq!(git_calls.lock().unwrap().as_slice(), git_before.as_slice());
    assert!(editor_calls.lock().unwrap().is_empty());
    assert!(clipboard_calls.lock().unwrap().is_empty());
    assert!(opener_calls.lock().unwrap().is_empty());

    controller.handle(Intent::ShowHelp);
    let dismissed_body = flatten(controller.help_state().unwrap().active_body());
    assert!(
        dismissed_body.contains("remote command: rm -rf /"),
        "a status-line dismissal never discards the accepted spotlight body"
    );
}
