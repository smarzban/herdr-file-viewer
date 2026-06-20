//! T-16 — Editor Launcher: hand-off to editor / new pane (AC-19, AC-N1).
//! The spawner is injected and records requests; nothing is really launched, and the file
//! on disk is never written by the hand-off.

mod common;

use common::TempDir;
use herdr_file_viewer::editor::{EditorLauncher, Spawner, Target};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct FakeSpawner {
    spawned: Vec<Vec<OsString>>,
    panes: Vec<(OsString, PathBuf)>,
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
    fn open_pane(&mut self, editor: &OsStr, file: &Path) -> io::Result<()> {
        if self.fail {
            return Err(io::Error::new(io::ErrorKind::NotFound, "herdr not found"));
        }
        self.panes.push((editor.to_owned(), file.to_path_buf()));
        Ok(())
    }
}

#[test]
fn editor_target_spawns_configured_editor_with_the_selected_file() {
    let dir = TempDir::new();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "content").unwrap();

    let launcher = EditorLauncher::new("vim");
    let mut sp = FakeSpawner::default();
    launcher.open(&file, Target::Editor, &mut sp).unwrap();

    // AC-19: the configured editor is launched on exactly the selected file.
    assert_eq!(sp.spawned, vec![vec![OsString::from("vim"), file.clone().into_os_string()]]);
    assert!(sp.panes.is_empty(), "the editor path must not open a herdr pane");
    // AC-N1: the hand-off never writes the file.
    assert_eq!(fs::read_to_string(&file).unwrap(), "content");
}

#[test]
fn new_pane_target_requests_a_herdr_pane_carrying_the_file() {
    let dir = TempDir::new();
    let file = dir.path().join("main.rs");
    fs::write(&file, "fn main() {}").unwrap();

    let launcher = EditorLauncher::new("nano");
    let mut sp = FakeSpawner::default();
    launcher.open(&file, Target::NewPane, &mut sp).unwrap();

    // AC-19: a new-pane request carries the exact file (and the editor to run there).
    assert_eq!(sp.panes, vec![(OsString::from("nano"), file.clone())]);
    assert!(sp.spawned.is_empty(), "the new-pane path must not spawn a local editor");
    // AC-N1: still no write.
    assert_eq!(fs::read_to_string(&file).unwrap(), "fn main() {}");
}

#[test]
fn launch_failure_is_an_error_not_a_panic_and_leaves_the_file_intact() {
    // A failed launch surfaces as Err (the controller turns it into a non-fatal notice),
    // never a panic — and the file is untouched (AC-N1).
    let dir = TempDir::new();
    let file = dir.path().join("x.txt");
    fs::write(&file, "x").unwrap();

    let launcher = EditorLauncher::new("editor");
    let mut sp = FakeSpawner { fail: true, ..Default::default() };
    assert!(launcher.open(&file, Target::Editor, &mut sp).is_err());
    assert_eq!(fs::read_to_string(&file).unwrap(), "x");
}
