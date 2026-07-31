//! Cross-process cache writer regressions. Each child is this integration-test executable, selected
//! by environment, so the barriers exercise real process-local cache snapshots and file locks.

mod common;

use common::TempDir;
use herdr_file_viewer::update::cache::{
    Cache, CacheDelta, PersistedReleaseDetails, load, store, store_delta,
};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const FIXTURE_ENV: &str = "HERDR_FILE_VIEWER_CACHE_FIXTURE";
const FIXTURE_DIR_ENV: &str = "HERDR_FILE_VIEWER_CACHE_FIXTURE_DIR";
const FIXTURE_ID_ENV: &str = "HERDR_FILE_VIEWER_CACHE_FIXTURE_ID";
const FIXTURE_ACTION_ENV: &str = "HERDR_FILE_VIEWER_CACHE_FIXTURE_ACTION";
const WAIT: Duration = Duration::from_secs(5);
const OLD: &[u8] = b"# Project\nold body\n";
const NEW: &[u8] = b"# Project\nnew body\n";

#[test]
fn cache_writer_fixture() {
    let Ok(kind) = std::env::var(FIXTURE_ENV) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(FIXTURE_DIR_ENV).expect("fixture cache directory"));
    let id = std::env::var(FIXTURE_ID_ENV).expect("fixture id");

    match kind.as_str() {
        "writer" => {
            let before = load(&dir).expect("fixture starts with a complete cache revision");
            std::fs::write(
                marker(&dir, "ready", &id),
                serde_json::to_vec(&before).unwrap(),
            )
            .expect("write ready barrier");
            wait_for(&marker(&dir, "release", &id));
            std::fs::write(marker(&dir, "attempt", &id), []).expect("write attempt barrier");
            store_delta(
                &dir,
                fixture_delta(&std::env::var(FIXTURE_ACTION_ENV).expect("fixture action")),
            );
            std::fs::write(marker(&dir, "done", &id), []).expect("write done barrier");
        }
        "lock-holder" => {
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.join("update-check.lock"))
                .expect("open cache writer lock");
            lock.lock().expect("acquire cache writer lock");
            std::fs::write(marker(&dir, "ready", &id), []).expect("write ready barrier");
            wait_for(&marker(&dir, "release", &id));
            drop(lock);
            std::fs::write(marker(&dir, "done", &id), []).expect("write done barrier");
        }
        "stage" => {
            let staged = Cache {
                last_check_unix: 99,
                latest_seen: Some("2.0.0".into()),
                spotlight: Some(NEW.to_vec()),
                spotlight_retrieved_at_unix: Some(70),
                ..Cache::default()
            };
            let staged_path = dir.join("update-check.json.fixture-stage");
            std::fs::write(&staged_path, serde_json::to_vec(&staged).unwrap())
                .expect("write complete staging revision");
            std::fs::write(marker(&dir, "ready", &id), []).expect("write ready barrier");
            wait_for(&marker(&dir, "release", &id));
            std::fs::rename(staged_path, dir.join("update-check.json"))
                .expect("publish complete revision");
            std::fs::write(marker(&dir, "done", &id), []).expect("write done barrier");
        }
        other => panic!("unknown fixture kind: {other}"),
    }
}

#[test]
fn refresh_and_dismissal_contend_after_the_same_starting_revision() {
    let dir = TempDir::new();
    let before = starting_cache(None);
    store(dir.path(), &before);

    run_contending_writers(dir.path(), "refresh-new", "dismiss-old", &before);

    assert_eq!(
        load(dir.path()),
        Some(Cache {
            spotlight: Some(NEW.to_vec()),
            spotlight_retrieved_at_unix: Some(70),
            ..before
        }),
        "contending writers retain both complete intents"
    );
}

#[test]
fn independent_refreshes_contend_after_the_same_starting_revision_without_loss() {
    let dir = TempDir::new();
    let before = starting_cache(None);
    store(dir.path(), &before);

    run_contending_writers(dir.path(), "refresh-new", "refresh-release", &before);

    assert_eq!(
        load(dir.path()),
        Some(Cache {
            last_check_unix: 90,
            latest_seen: Some("1.3.0".into()),
            release_details: None,
            spotlight: Some(NEW.to_vec()),
            spotlight_retrieved_at_unix: Some(70),
            ..before
        }),
        "each contender must reread after exclusivity so independent fields both survive"
    );
}

#[test]
fn refresh_then_dismissal_rereads_the_complete_revision_after_exclusivity() {
    let dir = TempDir::new();
    let before = starting_cache(None);
    store(dir.path(), &before);

    run_ordered_writers(dir.path(), "refresh-new", "dismiss-old", &before);

    assert_eq!(
        load(dir.path()),
        Some(Cache {
            spotlight: Some(NEW.to_vec()),
            spotlight_retrieved_at_unix: Some(70),
            ..before
        }),
        "the stale dismissal applies to the newly reread cache, not its old snapshot"
    );
}

#[test]
fn dismissal_then_refresh_preserves_the_new_document_in_the_opposite_order() {
    let dir = TempDir::new();
    let before = starting_cache(None);
    store(dir.path(), &before);

    run_ordered_writers(dir.path(), "dismiss-old", "refresh-new", &before);

    assert_eq!(
        load(dir.path()),
        Some(Cache {
            spotlight: Some(NEW.to_vec()),
            spotlight_retrieved_at_unix: Some(70),
            ..before
        }),
        "a changed document clears a dismissal of the old exact identity"
    );
}

#[test]
fn dismissal_then_withdrawal_retains_the_dismissed_exact_identity() {
    let dir = TempDir::new();
    let before = starting_cache(None);
    store(dir.path(), &before);

    run_ordered_writers(dir.path(), "dismiss-old", "withdraw", &before);

    assert_eq!(
        load(dir.path()),
        Some(Cache {
            spotlight: None,
            spotlight_retrieved_at_unix: Some(80),
            dismissed_spotlight_identity: Some(OLD.to_vec()),
            ..before
        }),
        "withdrawal clears content but does not erase the completed dismissal intent"
    );
}

#[test]
fn withdrawal_then_dismissal_does_not_dismiss_content_that_is_already_gone() {
    let dir = TempDir::new();
    let before = starting_cache(None);
    store(dir.path(), &before);

    run_ordered_writers(dir.path(), "withdraw", "dismiss-old", &before);

    assert_eq!(
        load(dir.path()),
        Some(Cache {
            spotlight: None,
            spotlight_retrieved_at_unix: Some(80),
            ..before
        }),
        "the stale dismissal is checked against the reread withdrawn revision"
    );
}

#[test]
fn changed_identity_does_not_carry_a_prior_dismissal_into_the_new_document() {
    let dir = TempDir::new();
    let before = starting_cache(Some(OLD.to_vec()));
    store(dir.path(), &before);

    run_ordered_writers(dir.path(), "dismiss-old", "refresh-new", &before);

    assert_eq!(
        load(dir.path()),
        Some(Cache {
            spotlight: Some(NEW.to_vec()),
            spotlight_retrieved_at_unix: Some(70),
            dismissed_spotlight_identity: None,
            ..before
        }),
        "only an equal identity carries a dismissal forward"
    );
}

#[test]
fn reader_sees_only_complete_revisions_while_a_same_directory_stage_waits_to_publish() {
    let dir = TempDir::new();
    let before = starting_cache(None);
    store(dir.path(), &before);
    let mut stager = spawn_fixture(dir.path(), "stage", "stager", None);

    wait_for(&marker(dir.path(), "ready", "stager"));
    assert_eq!(
        load(dir.path()),
        Some(before),
        "a complete sibling staging file cannot change what a reader sees"
    );

    release(dir.path(), "stager");
    finish_fixture(dir.path(), "stager", &mut stager);
    assert_eq!(
        load(dir.path()).and_then(|cache| cache.spotlight),
        Some(NEW.to_vec()),
        "the rename publishes the already-complete staged revision"
    );
}

fn starting_cache(dismissed_spotlight_identity: Option<Vec<u8>>) -> Cache {
    Cache {
        last_check_unix: 10,
        latest_seen: Some("1.2.0".into()),
        release_details: Some(PersistedReleaseDetails {
            release: "1.2.0".into(),
            details: "exact release details".into(),
        }),
        spotlight: Some(OLD.to_vec()),
        spotlight_retrieved_at_unix: Some(20),
        dismissed_spotlight_identity,
        ..Cache::default()
    }
}

fn fixture_delta(action: &str) -> CacheDelta {
    match action {
        "refresh-new" => CacheDelta::RefreshSpotlight {
            spotlight: NEW.to_vec(),
            retrieved_at_unix: 70,
        },
        "refresh-release" => CacheDelta::RefreshRelease {
            checked_at_unix: 90,
            detected_release: Some("1.3.0".into()),
        },
        "dismiss-old" => CacheDelta::DismissSpotlight {
            identity: OLD.to_vec(),
        },
        "withdraw" => CacheDelta::WithdrawSpotlight {
            retrieved_at_unix: 80,
        },
        other => panic!("unknown fixture action: {other}"),
    }
}

fn run_contending_writers(dir: &Path, first_action: &str, second_action: &str, before: &Cache) {
    let mut holder = spawn_fixture(dir, "lock-holder", "holder", None);
    wait_for(&marker(dir, "ready", "holder"));
    let mut first = spawn_fixture(dir, "writer", "first", Some(first_action));
    let mut second = spawn_fixture(dir, "writer", "second", Some(second_action));

    assert_ready_snapshots(dir, before);
    release(dir, "first");
    release(dir, "second");
    wait_for(&marker(dir, "attempt", "first"));
    wait_for(&marker(dir, "attempt", "second"));
    // Both writers have crossed their release barriers while the separate cache lock is held.
    std::thread::sleep(Duration::from_millis(25));
    assert!(
        !marker(dir, "done", "first").exists() && !marker(dir, "done", "second").exists(),
        "writers must wait for the held separate cache lock"
    );
    release(dir, "holder");
    finish_fixture(dir, "holder", &mut holder);
    finish_fixture(dir, "first", &mut first);
    finish_fixture(dir, "second", &mut second);
}

fn run_ordered_writers(dir: &Path, first_action: &str, second_action: &str, before: &Cache) {
    let mut first = spawn_fixture(dir, "writer", "first", Some(first_action));
    let mut second = spawn_fixture(dir, "writer", "second", Some(second_action));

    assert_ready_snapshots(dir, before);

    release(dir, "first");
    finish_fixture(dir, "first", &mut first);
    release(dir, "second");
    finish_fixture(dir, "second", &mut second);
}

fn assert_ready_snapshots(dir: &Path, before: &Cache) {
    for id in ["first", "second"] {
        wait_for(&marker(dir, "ready", id));
        let snapshot: Cache = serde_json::from_slice(
            &std::fs::read(marker(dir, "ready", id)).expect("read ready snapshot"),
        )
        .expect("decode ready snapshot");
        assert_eq!(
            snapshot, *before,
            "both child processes must start from the same stale revision"
        );
    }
}

fn spawn_fixture(dir: &Path, kind: &str, id: &str, action: Option<&str>) -> Child {
    let mut command = Command::new(std::env::current_exe().expect("integration test executable"));
    command
        .args(["--exact", "cache_writer_fixture"])
        .env(FIXTURE_ENV, kind)
        .env(FIXTURE_DIR_ENV, dir)
        .env(FIXTURE_ID_ENV, id)
        .env_remove(FIXTURE_ACTION_ENV);
    if let Some(action) = action {
        command.env(FIXTURE_ACTION_ENV, action);
    }
    command.spawn().expect("spawn child fixture")
}

fn finish_fixture(dir: &Path, id: &str, child: &mut Child) {
    wait_for(&marker(dir, "done", id));
    assert!(child.wait().expect("wait for child fixture").success());
}

fn release(dir: &Path, id: &str) {
    std::fs::write(marker(dir, "release", id), []).expect("write release barrier");
}

fn marker(dir: &Path, phase: &str, id: &str) -> PathBuf {
    dir.join(format!("{phase}-{id}"))
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
