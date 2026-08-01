//! Typed remote-notice facts reach Help only from the coordinator snapshot.

mod common;

use common::TempDir;
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::update::gateway::Gateway;
use herdr_file_viewer::update::{
    DiscoveryRunner, ObjectId, ReleaseState, ReleaseTag, RemoteRef, Source, StartDeps, UpdateState,
    Version, start_with,
};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TAGGED_DETAILS: &str = "## [9.2.0]\n- TAGGED-DETAILS-ONLY\n";
const DEFAULT_DETAILS: &str = "## [9.2.0]\n- DEFAULT-DETAILS-MUST-NOT-DISPLAY\n";
const HEAD_SPOTLIGHT: &str = "# Head spotlight\nHEAD-SPOTLIGHT-BODY-ONLY\n";
const OTHER_REF_SPOTLIGHT: &str = "# Other spotlight\nOTHER-REF-SPOTLIGHT-MUST-NOT-DISPLAY\n";
const TAG_OBJECT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_OBJECT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Git;

impl GitService for Git {
    fn status(&self) -> BTreeMap<std::path::PathBuf, Status> {
        BTreeMap::new()
    }

    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<std::path::PathBuf, Status> {
        BTreeMap::new()
    }

    fn diff(&self, _path: &Path, _baseline: Baseline, _full_context: bool) -> String {
        String::new()
    }

    fn diff_directory(&self, _path: &Path, _baseline: Baseline) -> String {
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

struct Editor;

impl EditorHandoff for Editor {
    fn open(&mut self, _file: &Path) -> EditorOutcome {
        EditorOutcome::NoTakeover
    }
}

#[derive(Clone)]
struct GatewayStub {
    calls: Arc<Mutex<Vec<String>>>,
    changelog: Source<Option<Vec<u8>>>,
    spotlight: Source<Option<Vec<u8>>>,
}

impl Gateway for GatewayStub {
    fn changelog(
        &self,
        release: &ReleaseTag,
        _deadline: std::time::Instant,
    ) -> Source<Option<Vec<u8>>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("changelog:{}", release.object_id.as_str()));
        self.changelog.clone()
    }

    fn spotlight(
        &self,
        state: &ReleaseState,
        _deadline: std::time::Instant,
    ) -> Source<Option<Vec<u8>>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("spotlight:{}", state.head_object_id.as_str()));
        self.spotlight.clone()
    }
}

/// A gateway whose alternate source bytes deliberately differ. The coordinator must request only
/// the detected tag and discovered HEAD, or Help would receive the DEFAULT/other-ref sentinels.
struct PinnedGateway {
    calls: Arc<Mutex<Vec<String>>>,
}

impl Gateway for PinnedGateway {
    fn changelog(
        &self,
        release: &ReleaseTag,
        _deadline: std::time::Instant,
    ) -> Source<Option<Vec<u8>>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("changelog:{}", release.object_id.as_str()));
        let details = if release.object_id.as_str() == TAG_OBJECT {
            TAGGED_DETAILS
        } else {
            DEFAULT_DETAILS
        };
        Source::Available(Some(details.as_bytes().to_vec()))
    }

    fn spotlight(
        &self,
        state: &ReleaseState,
        _deadline: std::time::Instant,
    ) -> Source<Option<Vec<u8>>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("spotlight:{}", state.head_object_id.as_str()));
        let spotlight = if state.head_object_id.as_str() == HEAD_OBJECT {
            HEAD_SPOTLIGHT
        } else {
            OTHER_REF_SPOTLIGHT
        };
        Source::Available(Some(spotlight.as_bytes().to_vec()))
    }
}

fn controller(root: &Path) -> Controller {
    Controller::new(
        common::resolved(root.to_path_buf(), false),
        Baseline::Head,
        Components {
            providers: Box::new(|_| RootProviders {
                git: Arc::new(Git),
                content: Box::new(Content),
            }),
            editor: Box::new(Editor),
            clipboard: Box::new(common::RecordingClipboard::default()),
            renderers: None,
        },
    )
}

fn state() -> ReleaseState {
    ReleaseState::new(
        RemoteRef::parse("refs/heads/main").unwrap(),
        ObjectId::parse(HEAD_OBJECT).unwrap(),
        vec![ReleaseTag::new(
            Version::parse("9.2.0").unwrap(),
            ObjectId::parse(TAG_OBJECT).unwrap(),
        )],
    )
    .unwrap()
}

fn completed(state: UpdateState) -> UpdateState {
    let UpdateState { initial, rx } = state;
    let initial = match rx
        .expect("eligible refresh starts")
        .recv_timeout(Duration::from_secs(1))
    {
        Ok(snapshot) => snapshot,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => initial,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("refresh did not settle"),
    };
    UpdateState { initial, rx: None }
}

fn refresh(
    discovery: Source<ReleaseState>,
    changelog: Source<Option<Vec<u8>>>,
    spotlight: Source<Option<Vec<u8>>>,
    calls: Arc<Mutex<Vec<String>>>,
) -> UpdateState {
    completed(start_with(StartDeps {
        disabled: false,
        now_unix: 1_000_000,
        cache: Some(Default::default()),
        cache_dir: None,
        run: Box::new(move |_| discovery.clone()) as DiscoveryRunner,
        gateway: Box::new(GatewayStub {
            calls,
            changelog,
            spotlight,
        }),
    }))
}

fn flatten(text: &Text<'_>) -> String {
    text.lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn help_composes_exact_tagged_and_head_documents_from_the_completed_snapshot() {
    let dir = TempDir::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let update = completed(start_with(StartDeps {
        disabled: false,
        now_unix: 1_000_000,
        cache: Some(Default::default()),
        cache_dir: None,
        run: Box::new(|_| Source::Available(state())) as DiscoveryRunner,
        gateway: Box::new(PinnedGateway {
            calls: Arc::clone(&calls),
        }),
    }));
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            format!("changelog:{TAG_OBJECT}"),
            format!("spotlight:{HEAD_OBJECT}"),
        ],
        "the coordinator fetches details at the detected tag and spotlight at discovered HEAD"
    );

    let mut controller = controller(dir.path());
    controller.set_update(update);
    controller.handle(Intent::ShowHelp);

    assert!(
        controller
            .view_state()
            .remote_notice_status
            .is_some_and(|status| status.contains("Head spotlight")),
        "the accepted title reaches the one status row"
    );
    let body = flatten(controller.help_state().unwrap().active_body());
    for expected in [
        "HEAD-SPOTLIGHT-BODY-ONLY",
        "TAGGED-DETAILS-ONLY",
        "herdr plugin install smarzban/herdr-file-viewer",
    ] {
        assert!(body.contains(expected), "missing {expected:?}:\n{body}");
    }
    for forbidden in [
        DEFAULT_DETAILS,
        OTHER_REF_SPOTLIGHT,
        "DEFAULT-DETAILS-MUST-NOT-DISPLAY",
        "OTHER-REF-SPOTLIGHT-MUST-NOT-DISPLAY",
    ] {
        assert!(
            !body.contains(forbidden),
            "Help must only display the exact typed source: {forbidden:?}\n{body}"
        );
    }
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            format!("changelog:{TAG_OBJECT}"),
            format!("spotlight:{HEAD_OBJECT}"),
        ],
        "Help consumes the in-memory snapshot and never calls the source gateway"
    );
}

#[test]
fn help_keeps_valid_neighbors_when_each_remote_document_fails_independently() {
    let cases = [
        (
            "changelog unavailable",
            Source::Unavailable,
            Source::Available(Some(HEAD_SPOTLIGHT.as_bytes().to_vec())),
            true,
            false,
        ),
        (
            "changelog absent",
            Source::Available(None),
            Source::Available(Some(HEAD_SPOTLIGHT.as_bytes().to_vec())),
            true,
            false,
        ),
        (
            "spotlight unavailable",
            Source::Available(Some(TAGGED_DETAILS.as_bytes().to_vec())),
            Source::Unavailable,
            false,
            true,
        ),
        (
            "both unavailable",
            Source::Unavailable,
            Source::Unavailable,
            false,
            false,
        ),
    ];

    for (name, changelog, spotlight, has_spotlight, has_details) in cases {
        let dir = TempDir::new();
        let mut controller = controller(dir.path());
        controller.set_update(refresh(
            Source::Available(state()),
            changelog,
            spotlight,
            Arc::new(Mutex::new(Vec::new())),
        ));
        controller.handle(Intent::ShowHelp);
        let body = flatten(controller.help_state().unwrap().active_body());

        assert!(
            body.contains("herdr plugin install smarzban/herdr-file-viewer"),
            "{name}: a detected release retains fixed local install copy:\n{body}"
        );
        assert!(
            body.contains("## [1.14.0]"),
            "{name}: embedded history remains"
        );
        assert_eq!(
            body.contains("HEAD-SPOTLIGHT-BODY-ONLY"),
            has_spotlight,
            "{name}: a failed neighbor cannot change spotlight visibility"
        );
        assert_eq!(
            body.contains("TAGGED-DETAILS-ONLY"),
            has_details,
            "{name}: a failed neighbor cannot change exact release details"
        );
    }
}

#[test]
fn invalid_spotlight_inputs_are_silent_and_never_replace_embedded_history() {
    let invalid = [
        ("missing", Source::Available(None)),
        ("empty", Source::Available(Some(Vec::new()))),
        ("blank", Source::Available(Some(b" \r\n\t".to_vec()))),
        ("non-utf8", Source::Available(Some(vec![b'#', b' ', 0xff]))),
        (
            "no heading",
            Source::Available(Some(b"INVALID-SPOTLIGHT-no-heading\n".to_vec())),
        ),
        (
            "blank heading",
            Source::Available(Some(b"# \r\nINVALID-SPOTLIGHT-blank-heading\n".to_vec())),
        ),
        ("unavailable", Source::Unavailable),
    ];

    for (name, spotlight) in invalid {
        let dir = TempDir::new();
        let mut controller = controller(dir.path());
        controller.set_update(refresh(
            Source::Available(
                ReleaseState::new(
                    RemoteRef::parse("refs/heads/main").unwrap(),
                    ObjectId::parse(HEAD_OBJECT).unwrap(),
                    Vec::new(),
                )
                .unwrap(),
            ),
            Source::Unavailable,
            spotlight,
            Arc::new(Mutex::new(Vec::new())),
        ));
        assert!(
            controller
                .notice_snapshot()
                .spotlight
                .status_title()
                .is_none(),
            "{name}: invalid source has no status title"
        );
        assert!(
            controller
                .notice_snapshot()
                .spotlight
                .whats_new_body()
                .is_none(),
            "{name}: invalid source has no What's New body"
        );

        controller.handle(Intent::ShowHelp);
        let body = flatten(controller.help_state().unwrap().active_body());
        assert!(
            body.contains("## [1.14.0]"),
            "{name}: embedded history remains"
        );
        assert!(
            !body.contains("INVALID-SPOTLIGHT"),
            "{name}: rejected source never reaches Help: {body}"
        );
    }
}

#[test]
fn discovery_failure_never_calls_documents_or_adds_remote_help_content() {
    let dir = TempDir::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut controller = controller(dir.path());
    controller.set_update(refresh(
        Source::Unavailable,
        Source::Available(Some(TAGGED_DETAILS.as_bytes().to_vec())),
        Source::Available(Some(HEAD_SPOTLIGHT.as_bytes().to_vec())),
        Arc::clone(&calls),
    ));

    assert!(
        calls.lock().unwrap().is_empty(),
        "discovery failure stops both document ports"
    );
    controller.handle(Intent::ShowHelp);
    let body = flatten(controller.help_state().unwrap().active_body());
    assert!(body.contains("## [1.14.0]"));
    assert!(!body.contains("TAGGED-DETAILS-ONLY"));
    assert!(!body.contains("HEAD-SPOTLIGHT-BODY-ONLY"));
    assert!(!body.contains("herdr plugin install smarzban/herdr-file-viewer"));
}
