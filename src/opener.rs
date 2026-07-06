//! Opener — the read-only OS-opener argv builder (AC-9, AC-10).
//!
//! The pure core of the Opener component: given an [`OsKind`], an [`OpenAction`], and a
//! path, it produces the exact argv to hand a file/dir off to the OS default app or file
//! manager. It is pure — no process spawning, no I/O, no trait — and never mutates the file
//! (AC-N1); a later task wires the spawn. The target OS is an explicit parameter (not
//! `cfg!(target_os)`) so all three platforms are unit-testable on any host, and the path is
//! always carried as a single, un-shell-split argv element to keep spaces and metacharacters
//! literal (AC-9).

use crate::editor::{SpawnError, Spawner};
use std::ffi::OsString;
use std::path::Path;

/// The target operating system whose opener convention to build for. An explicit parameter
/// (rather than compile-time `cfg!`) so every platform's argv is testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Mac,
    Linux,
    Windows,
}

/// What to do with the path: open it in the default app, or reveal it in a file manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAction {
    Open,
    Reveal,
}

/// Build the per-OS argv (argv[0] = program, rest = args) to open or reveal `path`.
///
/// The path is always placed as ONE argv element, never shell-split, so spaces and shell
/// metacharacters stay literal (AC-9). `OsString`s are built directly so non-UTF-8 paths are
/// preserved. On Linux, "reveal" opens the containing folder (there is no universal
/// select-in-file-manager); a path with no parent (e.g. `/`) falls back to itself.
pub fn opener_argv(os: OsKind, action: OpenAction, path: &Path) -> Vec<OsString> {
    match (os, action) {
        (OsKind::Mac, OpenAction::Open) => {
            vec![OsString::from("open"), path.as_os_str().to_owned()]
        }
        (OsKind::Mac, OpenAction::Reveal) => vec![
            OsString::from("open"),
            OsString::from("-R"),
            path.as_os_str().to_owned(),
        ],
        (OsKind::Linux, OpenAction::Open) => {
            vec![OsString::from("xdg-open"), path.as_os_str().to_owned()]
        }
        (OsKind::Linux, OpenAction::Reveal) => {
            let target = path.parent().unwrap_or(path);
            vec![OsString::from("xdg-open"), target.as_os_str().to_owned()]
        }
        (OsKind::Windows, OpenAction::Open) => {
            vec![OsString::from("explorer"), path.as_os_str().to_owned()]
        }
        (OsKind::Windows, OpenAction::Reveal) => {
            let mut s = OsString::from("/select,");
            s.push(path);
            vec![OsString::from("explorer"), s]
        }
    }
}

/// The result of an OS hand-off attempt through the [`Opener`] seam. Mirrors the editor's
/// [`SpawnError`] split so the controller can word its notice accurately:
/// [`OpenerOutcome::NotLaunched`] means nothing ran (e.g. `xdg-open` missing) — "could not
/// open"; [`OpenerOutcome::NonZeroExit`] means the opener ran and returned a failing status.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenerOutcome {
    /// The opener was spawned successfully (non-blocking success — the TUI keeps running).
    Launched,
    /// The opener process could not be started at all; the carried string is a human-readable
    /// reason (from the underlying `io::Error`).
    NotLaunched(String),
    /// The opener launched but exited non-zero; the carried string is a human-readable detail.
    NonZeroExit(String),
}

/// The read-only OS hand-off seam: open a path with its default app, or reveal it in the
/// file manager. Unlike the editor hand-off, these are **non-blocking** — they do not suspend
/// or take over the TUI's terminal (AC-1, AC-2, AC-7, AC-8). Never mutates the file (AC-N1).
pub trait Opener {
    /// Open `path` with the OS default application (non-blocking). (AC-1)
    fn open(&mut self, path: &Path) -> OpenerOutcome;
    /// Reveal `path` in the OS file manager (non-blocking). (AC-2)
    fn reveal(&mut self, path: &Path) -> OpenerOutcome;
}

/// The concrete [`Opener`]: builds the per-OS argv via [`opener_argv`] and hands it to the
/// injected [`Spawner`] (the same low-level seam the editor launcher reuses), so tests stay
/// hermetic — no real process is ever spawned here.
pub struct CommandOpener {
    os: OsKind,
    spawner: Box<dyn Spawner>,
}

impl CommandOpener {
    /// Create an opener for the given target OS, spawning through `spawner`.
    pub fn new(os: OsKind, spawner: Box<dyn Spawner>) -> Self {
        Self { os, spawner }
    }

    /// Build the argv for `action`/`path`, spawn it, and map the spawn result onto an
    /// [`OpenerOutcome`]. The only external effect goes through the injected [`Spawner`].
    fn run(&mut self, action: OpenAction, path: &Path) -> OpenerOutcome {
        let argv = opener_argv(self.os, action, path);
        match self.spawner.spawn(&argv) {
            Ok(()) => OpenerOutcome::Launched,
            Err(SpawnError::NotLaunched(e)) => OpenerOutcome::NotLaunched(e.to_string()),
            Err(SpawnError::NonZeroExit(d)) => OpenerOutcome::NonZeroExit(d),
        }
    }
}

impl Opener for CommandOpener {
    fn open(&mut self, path: &Path) -> OpenerOutcome {
        self.run(OpenAction::Open, path)
    }

    fn reveal(&mut self, path: &Path) -> OpenerOutcome {
        self.run(OpenAction::Reveal, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{SpawnError, Spawner};
    use std::cell::RefCell;
    use std::io;
    use std::rc::Rc;

    /// Which result the recorder returns for each `spawn` call.
    #[derive(Clone, Copy)]
    enum SpawnResult {
        Ok,
        NotLaunched,
        NonZeroExit,
    }

    /// A test double for [`Spawner`] that records every argv it is handed (into a shared
    /// `Vec` the test still holds after boxing) and returns a preconfigured result — the only
    /// spawn path, so no real process is ever launched (AC-13 hermeticity).
    struct RecordingSpawner {
        calls: Rc<RefCell<Vec<Vec<OsString>>>>,
        result: SpawnResult,
    }

    impl RecordingSpawner {
        fn new(result: SpawnResult) -> (Self, Rc<RefCell<Vec<Vec<OsString>>>>) {
            let calls = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    calls: Rc::clone(&calls),
                    result,
                },
                calls,
            )
        }
    }

    impl Spawner for RecordingSpawner {
        fn spawn(&mut self, argv: &[OsString]) -> Result<(), SpawnError> {
            self.calls.borrow_mut().push(argv.to_vec());
            match self.result {
                SpawnResult::Ok => Ok(()),
                SpawnResult::NotLaunched => {
                    Err(SpawnError::NotLaunched(io::Error::other("missing")))
                }
                SpawnResult::NonZeroExit => Err(SpawnError::NonZeroExit("code 1".into())),
            }
        }
    }

    #[test]
    fn command_opener_open_spawns_open_argv() {
        let (recorder, calls) = RecordingSpawner::new(SpawnResult::Ok);
        let mut opener = CommandOpener::new(OsKind::Linux, Box::new(recorder));
        let outcome = opener.open(Path::new("/a/b.txt"));
        assert_eq!(outcome, OpenerOutcome::Launched);
        let calls = calls.borrow();
        assert_eq!(calls.len(), 1, "exactly one spawn call");
        assert_eq!(
            calls[0],
            opener_argv(OsKind::Linux, OpenAction::Open, Path::new("/a/b.txt"))
        );
        assert_eq!(
            calls[0],
            vec![OsString::from("xdg-open"), OsString::from("/a/b.txt")]
        );
    }

    #[test]
    fn command_opener_reveal_spawns_reveal_argv() {
        let (recorder, calls) = RecordingSpawner::new(SpawnResult::Ok);
        let mut opener = CommandOpener::new(OsKind::Linux, Box::new(recorder));
        let outcome = opener.reveal(Path::new("/a/b.txt"));
        assert_eq!(outcome, OpenerOutcome::Launched);
        let calls = calls.borrow();
        assert_eq!(calls.len(), 1, "exactly one spawn call");
        assert_eq!(
            calls[0],
            opener_argv(OsKind::Linux, OpenAction::Reveal, Path::new("/a/b.txt"))
        );
        // Linux reveal opens the parent directory.
        assert_eq!(
            calls[0],
            vec![OsString::from("xdg-open"), OsString::from("/a")]
        );
    }

    #[test]
    fn command_opener_maps_not_launched() {
        let (recorder, _calls) = RecordingSpawner::new(SpawnResult::NotLaunched);
        let mut opener = CommandOpener::new(OsKind::Linux, Box::new(recorder));
        match opener.open(Path::new("/a/b.txt")) {
            OpenerOutcome::NotLaunched(msg) => assert!(
                msg.contains("missing"),
                "message should carry the io::Error detail, got {msg:?}"
            ),
            other => panic!("expected NotLaunched, got {other:?}"),
        }
    }

    #[test]
    fn command_opener_maps_non_zero_exit() {
        let (recorder, _calls) = RecordingSpawner::new(SpawnResult::NonZeroExit);
        let mut opener = CommandOpener::new(OsKind::Linux, Box::new(recorder));
        assert_eq!(
            opener.open(Path::new("/a/b.txt")),
            OpenerOutcome::NonZeroExit("code 1".into())
        );
    }

    #[test]
    fn command_opener_uses_only_injected_spawner() {
        // The injected recorder is the sole spawn path: it captures the call and no real
        // process runs (AC-13 hermeticity).
        let (recorder, calls) = RecordingSpawner::new(SpawnResult::Ok);
        let mut opener = CommandOpener::new(OsKind::Linux, Box::new(recorder));
        let outcome = opener.open(Path::new("/a/b.txt"));
        assert_eq!(outcome, OpenerOutcome::Launched);
        assert_eq!(
            calls.borrow().len(),
            1,
            "the recorder captured the call; nothing else spawned"
        );
    }

    #[test]
    fn mac_open_argv() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Mac, OpenAction::Open, path),
            vec![OsString::from("open"), OsString::from("/abs/dir/file.rs")]
        );
    }

    #[test]
    fn mac_reveal_argv() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Mac, OpenAction::Reveal, path),
            vec![
                OsString::from("open"),
                OsString::from("-R"),
                OsString::from("/abs/dir/file.rs"),
            ]
        );
    }

    #[test]
    fn linux_open_argv() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Linux, OpenAction::Open, path),
            vec![
                OsString::from("xdg-open"),
                OsString::from("/abs/dir/file.rs")
            ]
        );
    }

    #[test]
    fn linux_reveal_argv_is_parent() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Linux, OpenAction::Reveal, path),
            vec![OsString::from("xdg-open"), OsString::from("/abs/dir")]
        );
    }

    #[test]
    fn linux_reveal_parent_fallback_is_self() {
        let path = Path::new("/");
        assert_eq!(
            opener_argv(OsKind::Linux, OpenAction::Reveal, path),
            vec![OsString::from("xdg-open"), OsString::from("/")]
        );
    }

    #[test]
    fn windows_open_argv() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Windows, OpenAction::Open, path),
            vec![
                OsString::from("explorer"),
                OsString::from("/abs/dir/file.rs")
            ]
        );
    }

    #[test]
    fn windows_reveal_argv_is_select_prefix() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Windows, OpenAction::Reveal, path),
            vec![
                OsString::from("explorer"),
                OsString::from("/select,/abs/dir/file.rs"),
            ]
        );
    }

    #[test]
    fn path_is_single_unmodified_element_open() {
        // Spaces, a leading-dash-looking component, and a shell metacharacter must stay
        // literal and un-split (AC-9). The path is absolute.
        let path = Path::new("/tmp/a dir/-weird;name.txt");
        let expected = path.as_os_str().to_owned();

        for os in [OsKind::Mac, OsKind::Linux, OsKind::Windows] {
            let argv = opener_argv(os, OpenAction::Open, path);
            assert_eq!(argv.len(), 2, "{os:?} Open argv should be [prog, path]");
            assert_eq!(
                argv[1], expected,
                "{os:?} path must be one unmodified element"
            );
        }
    }

    #[test]
    fn windows_reveal_path_stays_single_element() {
        let path = Path::new("/tmp/a dir/-weird;name.txt");
        let argv = opener_argv(OsKind::Windows, OpenAction::Reveal, path);
        assert_eq!(argv.len(), 2);
        assert_eq!(
            argv[1],
            OsString::from("/select,/tmp/a dir/-weird;name.txt")
        );
    }
}
