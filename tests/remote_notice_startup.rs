//! Startup refresh remains advisory: injected remote failures never block or draw diagnostics.

mod common;

use common::{NoopContent, NoopEditor, NoopGit, TempDir};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_file_viewer::controller::{Components, Controller, RootProviders};
use herdr_file_viewer::git::Baseline;
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter;
use herdr_file_viewer::update::cache::Cache;
use herdr_file_viewer::update::gateway::Gateway;
use herdr_file_viewer::update::{
    DiscoveryRunner, NoticeSnapshot, ObjectId, ReleaseState, ReleaseTag, Source, StartDeps,
    UpdateState, Version, start_with,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Clone)]
struct Documents {
    changelog: Source<Option<Vec<u8>>>,
    spotlight: Source<Option<Vec<u8>>>,
}

impl Gateway for Documents {
    fn changelog(
        &self,
        _release: &ReleaseTag,
        _deadline: std::time::Instant,
    ) -> Source<Option<Vec<u8>>> {
        self.changelog.clone()
    }

    fn spotlight(
        &self,
        _state: &ReleaseState,
        _deadline: std::time::Instant,
    ) -> Source<Option<Vec<u8>>> {
        self.spotlight.clone()
    }
}

fn controller(dir: &Path) -> Controller {
    Controller::new(
        common::resolved(dir.to_path_buf(), false),
        Baseline::Head,
        Components {
            providers: Box::new(|_| RootProviders {
                git: Arc::new(NoopGit),
                content: Box::new(NoopContent),
            }),
            editor: Box::new(NoopEditor),
            clipboard: Box::new(common::RecordingClipboard::default()),
            renderers: None,
        },
    )
}

fn release_state(release: Option<Version>) -> ReleaseState {
    let releases = release
        .into_iter()
        .map(|version| {
            ReleaseTag::new(
                version,
                ObjectId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            )
        })
        .collect();
    ReleaseState::new(
        ObjectId::parse("0123456789012345678901234567890123456789").unwrap(),
        releases,
    )
    .unwrap()
}

fn start(run: DiscoveryRunner, documents: Documents) -> UpdateState {
    start_with(StartDeps {
        disabled: false,
        now_unix: 1_000_000,
        cache: Some(Cache::default()),
        cache_dir: None,
        run,
        gateway: Box::new(documents),
    })
}

fn completed(state: UpdateState) -> UpdateState {
    let UpdateState { initial, rx } = state;
    match rx {
        Some(rx) => match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(snapshot) => UpdateState {
                initial: snapshot,
                rx: None,
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => UpdateState { initial, rx: None },
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("refresh worker did not settle"),
        },
        None => UpdateState { initial, rx: None },
    }
}

fn draw(controller: &mut Controller) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            controller.set_width(frame.area().width);
            presenter::draw(frame, &controller.view_state());
        })
        .unwrap();
    format!("{}", terminal.backend())
}

#[test]
fn blocked_discovery_leaves_the_initial_snapshot_drawable_and_navigation_live() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "b\n").unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let state = start(
        Box::new(move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Source::Unavailable
        }),
        Documents {
            changelog: Source::Unavailable,
            spotlight: Source::Unavailable,
        },
    );

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("the background source started after startup returned");
    assert!(state.initial.detected_release.is_none());
    assert!(state.rx.is_some());

    let mut ctrl = controller(dir.path());
    ctrl.set_update(state);
    let before = ctrl.view_state().selected;
    let frame = draw(&mut ctrl);
    let mut baseline = controller(dir.path());
    assert_eq!(frame, draw(&mut baseline));
    assert!(ctrl.handle(Intent::NavDown).redraw);
    assert_ne!(ctrl.view_state().selected, before);

    release_tx.send(()).unwrap();
}

#[test]
fn disabled_startup_is_absent_from_status_and_help_without_touching_the_source() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("note.rs"), "fn main() {}\n").unwrap();
    let state = start_with(StartDeps {
        disabled: true,
        now_unix: 1_000_000,
        cache: Some(Cache {
            latest_seen: Some("99.0.0".into()),
            ..Cache::default()
        }),
        cache_dir: None,
        run: Box::new(|_| panic!("disabled startup must not discover")),
        gateway: Box::new(Documents {
            changelog: Source::Unavailable,
            spotlight: Source::Unavailable,
        }),
    });
    assert!(state.rx.is_none());

    let mut controller = controller(dir.path());
    controller.set_update(state);
    let status = draw(&mut controller);
    assert!(!status.contains("99.0.0"), "{status}");
    assert!(!status.contains("available ·"), "{status}");

    controller.handle(Intent::ShowHelp);
    controller.handle_help_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let help = draw(&mut controller);
    assert!(help.contains("Up to date"), "{help}");
    assert!(!help.contains("Update available"), "{help}");
    assert!(!help.contains("remote failure"), "{help}");
}

#[test]
fn every_remote_failure_is_silent_in_the_controller_presenter_projection() {
    let release = Version::parse("99.0.0").unwrap();
    let cases = [
        (
            "discovery",
            Box::new(|_| Source::Unavailable) as DiscoveryRunner,
            Documents {
                changelog: Source::Unavailable,
                spotlight: Source::Unavailable,
            },
            NoticeSnapshot::default(),
        ),
        (
            "changelog",
            Box::new(move |_| Source::Available(release_state(Some(release)))) as DiscoveryRunner,
            Documents {
                changelog: Source::Unavailable,
                spotlight: Source::Unavailable,
            },
            NoticeSnapshot {
                detected_release: Some(release),
                ..NoticeSnapshot::default()
            },
        ),
        (
            "spotlight",
            Box::new(|_| Source::Available(release_state(None))) as DiscoveryRunner,
            Documents {
                changelog: Source::Unavailable,
                spotlight: Source::Unavailable,
            },
            NoticeSnapshot::default(),
        ),
    ];

    for (name, run, documents, expected) in cases {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("note.rs"), "fn main() {}\n").unwrap();
        let mut ctrl = controller(dir.path());
        ctrl.set_update(completed(start(run, documents)));
        let frame = draw(&mut ctrl);

        let mut baseline = controller(dir.path());
        baseline.set_update(UpdateState {
            initial: expected,
            rx: None,
        });
        assert_eq!(
            frame,
            draw(&mut baseline),
            "{name}: no remote diagnostic reaches the frame"
        );
        assert!(
            ctrl.view_state().active.notices.is_empty(),
            "{name}: no notice strip"
        );
        assert!(ctrl.view_state().prompt.is_none(), "{name}: no prompt");
        assert_eq!(ctrl.flash_text(), None, "{name}: no status flash");
        assert_eq!(ctrl.action_notice(), None, "{name}: no action notice");
    }
}
