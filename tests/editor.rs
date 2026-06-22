//! T-16 — Editor Launcher: hand-off to an external editor (AC-19, AC-N1).
//! The spawner is injected and records requests; nothing is really launched, and the file
//! on disk is never written by the hand-off.

mod common;

use common::TempDir;
use herdr_file_viewer::editor::{EditorLauncher, Spawner};
use std::ffi::OsString;
use std::fs;
use std::io;

#[derive(Default)]
struct FakeSpawner {
    spawned: Vec<Vec<OsString>>,
    fail: bool,
}

impl Spawner for FakeSpawner {
    fn spawn(&mut self, argv: &[OsString]) -> io::Result<()> {
        if self.fail {
            return Err(io::Error::new(io::ErrorKind::NotFound, "editor not found"));
        }
        self.spawned.push(argv.to_vec());
        Ok(())
    }
}

#[test]
fn open_spawns_configured_editor_with_the_selected_file() {
    let dir = TempDir::new();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "content").unwrap();

    let launcher = EditorLauncher::new("vim");
    let mut sp = FakeSpawner::default();
    launcher.open(&file, &mut sp).unwrap();

    // AC-19: the configured editor is launched on exactly the selected file.
    assert_eq!(
        sp.spawned,
        vec![vec![OsString::from("vim"), file.clone().into_os_string()]]
    );
    // AC-N1: the hand-off never writes the file.
    assert_eq!(fs::read_to_string(&file).unwrap(), "content");
}

#[test]
fn open_splits_a_configured_editor_with_flags() {
    // AC-19: $EDITOR commonly carries flags (e.g. "code --wait"); the local spawn must
    // launch the program with its args, not treat the whole string as one executable name.
    let dir = TempDir::new();
    let file = dir.path().join("f.rs");
    fs::write(&file, "x").unwrap();

    let launcher = EditorLauncher::new("code --wait");
    let mut sp = FakeSpawner::default();
    launcher.open(&file, &mut sp).unwrap();

    assert_eq!(
        sp.spawned,
        vec![vec![
            OsString::from("code"),
            OsString::from("--wait"),
            file.clone().into_os_string(),
        ]],
    );
}

#[test]
fn launch_failure_is_an_error_not_a_panic_and_leaves_the_file_intact() {
    // A failed launch surfaces as Err (the controller turns it into a non-fatal notice),
    // never a panic — and the file is untouched (AC-N1).
    let dir = TempDir::new();
    let file = dir.path().join("x.txt");
    fs::write(&file, "x").unwrap();

    let launcher = EditorLauncher::new("editor");
    let mut sp = FakeSpawner {
        fail: true,
        ..Default::default()
    };
    assert!(launcher.open(&file, &mut sp).is_err());
    assert_eq!(fs::read_to_string(&file).unwrap(), "x");
}
