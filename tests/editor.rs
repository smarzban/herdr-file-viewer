//! Editor Launcher: hand-off to an external editor (AC-19, AC-N1).
//! The spawner is injected and records requests; nothing is really launched, and the file
//! on disk is never written by the hand-off.

mod common;

use common::TempDir;
use herdr_file_viewer::editor::{EditorLauncher, SpawnError, Spawner};
use std::ffi::OsString;
use std::fs;
use std::io;

#[derive(Default)]
struct FakeSpawner {
    spawned: Vec<Vec<OsString>>,
    fail: bool,
    /// When set, the spawn "launches" but exits with this non-zero status string.
    non_zero_exit: Option<String>,
}

impl Spawner for FakeSpawner {
    fn spawn(&mut self, argv: &[OsString]) -> Result<(), SpawnError> {
        if self.fail {
            return Err(SpawnError::NotLaunched(io::Error::new(
                io::ErrorKind::NotFound,
                "editor not found",
            )));
        }
        self.spawned.push(argv.to_vec());
        if let Some(detail) = self.non_zero_exit.clone() {
            return Err(SpawnError::NonZeroExit(detail));
        }
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
fn open_keeps_a_double_quoted_program_path_with_spaces_as_one_argv0() {
    // AC-8: a Windows-style `$EDITOR` value quotes a program path containing spaces (e.g.
    // "C:\Program Files\...\Code.exe" --wait); the quote-aware tokenizer must keep that whole
    // quoted path as argv[0], not split it on the embedded spaces.
    let dir = TempDir::new();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "content").unwrap();

    let launcher = EditorLauncher::new(r#""C:\Program Files\Code\Code.exe" --wait"#);
    let mut sp = FakeSpawner::default();
    launcher.open(&file, &mut sp).unwrap();

    assert_eq!(
        sp.spawned,
        vec![vec![
            OsString::from(r"C:\Program Files\Code\Code.exe"),
            OsString::from("--wait"),
            file.clone().into_os_string(),
        ]],
    );
}

#[test]
fn open_with_a_bare_unquoted_editor_launches_it_with_just_the_file() {
    let dir = TempDir::new();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "content").unwrap();

    let launcher = EditorLauncher::new("vi");
    let mut sp = FakeSpawner::default();
    launcher.open(&file, &mut sp).unwrap();

    assert_eq!(
        sp.spawned,
        vec![vec![OsString::from("vi"), file.clone().into_os_string()]]
    );
}

#[test]
fn open_with_an_empty_editor_still_attempts_a_launch_loudly() {
    // An empty/whitespace-only `$EDITOR` must not silently exec the file as the program: the
    // raw (empty) value is still passed through so the launch fails loudly.
    let dir = TempDir::new();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "content").unwrap();

    let launcher = EditorLauncher::new("");
    let mut sp = FakeSpawner::default();
    launcher.open(&file, &mut sp).unwrap();

    assert_eq!(
        sp.spawned,
        vec![vec![OsString::from(""), file.clone().into_os_string()]]
    );
}

#[test]
fn launch_failure_is_an_error_not_a_panic_and_leaves_the_file_intact() {
    // A failed launch surfaces as `SpawnError::NotLaunched` (the controller turns it into a
    // non-fatal "could not open editor" notice), never a panic — and the file is untouched
    // (AC-N1).
    let dir = TempDir::new();
    let file = dir.path().join("x.txt");
    fs::write(&file, "x").unwrap();

    let launcher = EditorLauncher::new("editor");
    let mut sp = FakeSpawner {
        fail: true,
        ..Default::default()
    };
    match launcher.open(&file, &mut sp) {
        Err(SpawnError::NotLaunched(_)) => {}
        other => panic!("expected NotLaunched, got {other:?}"),
    }
    assert_eq!(fs::read_to_string(&file).unwrap(), "x");
}

#[test]
fn a_non_zero_editor_exit_is_distinguished_from_a_launch_failure() {
    // a successful launch that exits non-zero must surface as
    // `SpawnError::NonZeroExit` (so the controller can say "editor exited with …"), NOT as a
    // `NotLaunched` launch failure — the editor DID run.
    let dir = TempDir::new();
    let file = dir.path().join("x.txt");
    fs::write(&file, "x").unwrap();

    let launcher = EditorLauncher::new("vim");
    let mut sp = FakeSpawner {
        non_zero_exit: Some("exit status: 1".into()),
        ..Default::default()
    };
    // The argv was still recorded (the editor was launched)…
    match launcher.open(&file, &mut sp) {
        Err(SpawnError::NonZeroExit(d)) => assert_eq!(d, "exit status: 1"),
        other => panic!("expected NonZeroExit, got {other:?}"),
    }
    assert_eq!(sp.spawned.len(), 1, "the editor was actually launched");
    // …and the file is untouched (AC-N1).
    assert_eq!(fs::read_to_string(&file).unwrap(), "x");
}
