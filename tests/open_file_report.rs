//! Integration: the **open-file report** — which file the controller shows (`displayed_origin`),
//! and how [`OpenFileReporter`] turns changes of it into herdr `pane report-metadata` calls.
//!
//! Every herdr call goes to an injected recording [`HerdrCli`]; nothing reaches a real herdr.

mod common;

use common::{NoopContent, NoopEditor, NoopGit, TempDir};
use herdr_file_viewer::controller::{Components, Controller, RootProviders};
use herdr_file_viewer::git::Baseline;
use herdr_file_viewer::herdr::HerdrCli;
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::open_report::{OpenFileReporter, Report, report_argv, token_value};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Generous for a loaded runner; only bounds a wait for something that does happen.
const WAIT: Duration = Duration::from_secs(10);

fn controller(root: &Path) -> Controller {
    Controller::new(
        common::resolved(root.to_path_buf(), false),
        Baseline::Head,
        Components {
            providers: Box::new(|_resolved| RootProviders {
                git: Arc::new(NoopGit),
                content: Box::new(NoopContent),
            }),
            editor: Box::new(NoopEditor),
            clipboard: Box::new(common::RecordingClipboard::default()),
            renderers: None,
        },
    )
}

/// Walk the cursor to the visible row named `name`; returns its absolute path.
fn select(ctrl: &mut Controller, name: &str) -> PathBuf {
    let rows = ctrl.tree().visible_nodes().len();
    for _ in 0..rows {
        ctrl.handle(Intent::NavUp);
    }
    for _ in 0..rows {
        let node = ctrl.tree().selected().expect("a row is selected");
        if node.path.file_name().is_some_and(|n| n == name) {
            return node.path;
        }
        ctrl.handle(Intent::NavDown);
    }
    panic!("no visible row named {name}");
}

/// Poll renders until the controller shows `file`.
fn settle_on(ctrl: &mut Controller, file: &Path) {
    let deadline = Instant::now() + WAIT;
    while ctrl.displayed_origin().map(|o| o.absolute_path()) != Some(file) {
        ctrl.poll();
        assert!(Instant::now() < deadline, "{file:?} never settled");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn shown(ctrl: &Controller) -> Option<(&Path, &Path)> {
    ctrl.displayed_origin()
        .map(|o| (o.root(), o.absolute_path()))
}

/// Holds one herdr call: signals `entered` when it starts, then waits for `release`.
type Gate = Arc<Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>>;

/// Records every argv; optionally holds the first call until released.
#[derive(Clone, Default)]
struct RecordingHerdr {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    gate: Option<Gate>,
}

impl HerdrCli for RecordingHerdr {
    fn run_json(&self, args: &[&str]) -> io::Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(args.iter().map(|a| a.to_string()).collect());
        if let Some(gate) = &self.gate
            && let Some((entered, release)) = gate.lock().unwrap().take()
        {
            entered.send(()).unwrap();
            release.recv().unwrap();
        }
        Ok(String::new())
    }
}

impl RecordingHerdr {
    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }

    fn wait_for_calls(&self, n: usize) {
        let deadline = Instant::now() + WAIT;
        while self.calls.lock().unwrap().len() < n {
            assert!(Instant::now() < deadline, "herdr call {n} never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn set(value: &str) -> Vec<String> {
    report_argv("w1:p2", &Report::Set(value.into()))
}

fn clear() -> Vec<String> {
    report_argv("w1:p2", &Report::Clear)
}

#[test]
fn displayed_origin_follows_the_settled_file_and_is_none_on_a_directory() {
    let dir = TempDir::new();
    let root = dir.path();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/inner.txt"), "inner\n").unwrap();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    let mut ctrl = controller(root);

    let a = select(&mut ctrl, "a.txt");
    settle_on(&mut ctrl, &a);
    let (shown_root, shown_file) = shown(&ctrl).expect("a file is shown");
    assert_eq!(
        token_value(shown_root, shown_file).as_deref(),
        Some("a.txt"),
        "the token value is the tree-root-relative path"
    );

    select(&mut ctrl, "sub");
    assert!(
        ctrl.displayed_origin().is_none(),
        "a directory shows no file, so there is nothing to report"
    );
}

#[test]
fn reporter_sets_the_shown_file_then_clears_it_on_finish() {
    let dir = TempDir::new();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    let mut ctrl = controller(root);
    let herdr = RecordingHerdr::default();
    let mut reporter =
        OpenFileReporter::start(Box::new(herdr.clone()), "w1:p2".into(), root.to_path_buf());

    let a = select(&mut ctrl, "a.txt");
    settle_on(&mut ctrl, &a);
    reporter.observe(shown(&ctrl));
    herdr.wait_for_calls(1);
    // Every later tick with the same file is a no-op.
    for _ in 0..3 {
        reporter.observe(shown(&ctrl));
    }
    reporter.finish(WAIT);

    assert_eq!(herdr.calls(), vec![set("a.txt"), clear()]);
}

#[test]
fn reporter_sends_nothing_when_no_file_was_ever_shown() {
    let herdr = RecordingHerdr::default();
    let mut reporter = OpenFileReporter::start(
        Box::new(herdr.clone()),
        "w1:p2".into(),
        PathBuf::from("/repo"),
    );
    reporter.observe(None);
    reporter.finish(WAIT);
    assert!(herdr.calls().is_empty());
}

#[test]
fn reporter_skips_values_that_queue_behind_a_slow_herdr_call() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let herdr = RecordingHerdr {
        gate: Some(Arc::new(Mutex::new(Some((entered_tx, release_rx))))),
        ..RecordingHerdr::default()
    };
    let mut reporter = OpenFileReporter::start(
        Box::new(herdr.clone()),
        "w1:p2".into(),
        PathBuf::from("/repo"),
    );
    let root = PathBuf::from("/repo");

    reporter.observe(Some((&root, &root.join("a.rs"))));
    entered_rx
        .recv_timeout(WAIT)
        .expect("the first call started");
    // Both queue behind the held call; only the newer may ever be sent.
    reporter.observe(Some((&root, &root.join("b.rs"))));
    reporter.observe(Some((&root, &root.join("c.rs"))));
    release_tx.send(()).unwrap();
    reporter.finish(WAIT);

    let calls = herdr.calls();
    assert_eq!(calls.first(), Some(&set("a.rs")));
    assert_eq!(calls.last(), Some(&clear()), "quitting clears the token");
    assert!(
        !calls.contains(&set("b.rs")),
        "a superseded value is never sent: {calls:?}"
    );
    assert!(calls.len() <= 3, "at most a, c, clear: {calls:?}");
}
