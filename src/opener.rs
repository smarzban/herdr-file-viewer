//! Opener — the read-only OS-opener argv builder (AC-9, AC-10).
//!
//! The core of the Opener component: given an [`OsKind`], an [`OpenAction`], and a path,
//! [`opener_argv`] produces the exact argv to hand a file/dir off to the OS default app or file
//! manager, and the [`Opener`] seam ([`CommandOpener`]) spawns it through the injected editor
//! [`Spawner`]. It never mutates the file (AC-N1). The target OS is an explicit parameter (not
//! `cfg!(target_os)`) so all three platforms' argv are unit-testable on any host, and the path is
//! always carried as a single, un-shell-split argv element to keep spaces and metacharacters
//! literal (AC-9). The one non-pure input is reading `%SystemRoot%` on Windows to resolve
//! Explorer's absolute path (a security fix, not a hijackable bare name — see
//! [`windows_explorer_program`]).

use crate::editor::{SpawnError, Spawner};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
/// select-in-file-manager); a path with no parent (e.g. `/`) falls back to itself. On Windows
/// the Explorer program is resolved to its **absolute** `%SystemRoot%\explorer.exe` (not a
/// hijackable bare name — see [`windows_explorer_program`]); this is the one non-pure input
/// (reading `%SystemRoot%`), unset on unix so the mapping stays host-testable.
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
            vec![
                windows_explorer_program(system_root().as_deref()),
                path.as_os_str().to_owned(),
            ]
        }
        (OsKind::Windows, OpenAction::Reveal) => {
            let mut s = OsString::from("/select,");
            s.push(path);
            vec![windows_explorer_program(system_root().as_deref()), s]
        }
    }
}

/// A short display label for the built-in OS opener (no concrete path), used by the Settings
/// help tab when `open` / `reveal` are unset. Mirrors [`opener_argv`]'s program (+ fixed flags).
pub fn default_opener_display(os: OsKind, action: OpenAction) -> String {
    match (os, action) {
        (OsKind::Mac, OpenAction::Open) => "open".to_owned(),
        (OsKind::Mac, OpenAction::Reveal) => "open -R".to_owned(),
        (OsKind::Linux, OpenAction::Open | OpenAction::Reveal) => "xdg-open".to_owned(),
        (OsKind::Windows, OpenAction::Open) => windows_explorer_program(system_root().as_deref())
            .to_string_lossy()
            .into_owned(),
        (OsKind::Windows, OpenAction::Reveal) => format!(
            "{} /select,<path>",
            windows_explorer_program(system_root().as_deref()).to_string_lossy()
        ),
    }
}

/// The current process's `%SystemRoot%`, if set and non-empty. Only meaningful on Windows;
/// unset on unix (so the Windows-arm tests resolve the bare-name fallback on a unix CI host).
fn system_root() -> Option<PathBuf> {
    std::env::var_os("SystemRoot")
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
}

/// Resolve the Windows Explorer program to an **absolute** `%SystemRoot%\explorer.exe`.
///
/// A bare `explorer` program name is subject to Windows' executable search order, which can
/// include the process working directory — here an *untrusted* browsed repo. An `explorer.exe`
/// planted in the repo could then be spawned when the user presses `O`/`R`, executing repo code
/// and breaking the read-only / no-repo-code boundary (constitution §1). Resolving via
/// `%SystemRoot%` closes that hole, mirroring the editor's Notepad default. Falls back to the
/// bare name only when `SystemRoot` is unset/empty (effectively never on real Windows).
fn windows_explorer_program(system_root: Option<&Path>) -> OsString {
    match system_root {
        Some(root) if !root.as_os_str().is_empty() => root.join("explorer.exe").into_os_string(),
        _ => OsString::from("explorer"),
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
    open_override: Option<Vec<OsString>>,
    reveal_override: Option<Vec<OsString>>,
}

impl CommandOpener {
    /// Create an opener for the given target OS, spawning through `spawner`. Uses the per-OS
    /// default argv for both actions; see [`Self::with_overrides`] to configure a replacement.
    pub fn new(os: OsKind, spawner: Box<dyn Spawner>) -> Self {
        Self {
            os,
            spawner,
            open_override: None,
            reveal_override: None,
        }
    }

    /// Configure optional per-action override argv (e.g. from user config). When `Some(prefix)`,
    /// the corresponding action spawns `prefix` with `path` appended as one trailing argv
    /// element instead of the per-OS default from [`opener_argv`] (AC-8). `None` leaves that
    /// action's default behavior unchanged (AC-12: no shell — the path is never concatenated).
    pub fn with_overrides(
        mut self,
        open: Option<Vec<OsString>>,
        reveal: Option<Vec<OsString>>,
    ) -> Self {
        self.open_override = open;
        self.reveal_override = reveal;
        self
    }

    /// Build the argv for `action`/`path` — the configured override (with `path` appended as
    /// one trailing element) if set, else the per-OS default — spawn it, and map the spawn
    /// result onto an [`OpenerOutcome`]. The only external effect goes through the injected
    /// [`Spawner`].
    fn run(&mut self, action: OpenAction, path: &Path) -> OpenerOutcome {
        let override_prefix = match action {
            OpenAction::Open => &self.open_override,
            OpenAction::Reveal => &self.reveal_override,
        };
        let argv = match override_prefix {
            Some(prefix) if !prefix.is_empty() => {
                let mut argv = prefix.clone();
                argv.push(path.as_os_str().to_os_string());
                argv
            }
            // An empty override (e.g. `open = ""`, or whitespace-only, tokenizes to no args) is
            // meaningless — building `argv = [path]` would spawn the SELECTED FILE itself as a
            // program. Fall through to the per-OS default instead of executing repo content.
            _ => opener_argv(self.os, action, path),
        };
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
        // argv[0] is the resolved Explorer program — bare `explorer` when `%SystemRoot%` is
        // unset (a unix CI host), the absolute `explorer.exe` on real Windows. Compute the
        // expected program the same way so the assertion is correct on either host.
        let prog = windows_explorer_program(system_root().as_deref());
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Windows, OpenAction::Open, path),
            vec![prog, OsString::from("/abs/dir/file.rs")]
        );
    }

    #[test]
    fn windows_reveal_argv_is_select_prefix() {
        let prog = windows_explorer_program(system_root().as_deref());
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Windows, OpenAction::Reveal, path),
            vec![prog, OsString::from("/select,/abs/dir/file.rs")]
        );
    }

    #[test]
    fn windows_explorer_program_absolute_from_system_root() {
        // With `%SystemRoot%` set, Explorer resolves under that root (its `explorer.exe`) — not
        // a hijackable bare name that Windows' search order could resolve from an untrusted repo
        // dir. Built via `join`, so the assertion is host-correct (the real `\` separation only
        // happens on Windows, where this actually runs); the point is that the root is applied,
        // not the bare fallback.
        let root = Path::new("C:\\Windows");
        let prog = windows_explorer_program(Some(root));
        assert_eq!(prog, root.join("explorer.exe").into_os_string());
        assert_ne!(
            prog,
            OsString::from("explorer"),
            "must not be the bare name"
        );
    }

    #[test]
    fn windows_explorer_program_falls_back_to_bare_when_unset_or_empty() {
        assert_eq!(windows_explorer_program(None), OsString::from("explorer"));
        assert_eq!(
            windows_explorer_program(Some(Path::new(""))),
            OsString::from("explorer")
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

    #[test]
    fn with_overrides_open_replaces_default_argv() {
        // AC-8: a configured open override replaces the per-OS default entirely.
        let (recorder, calls) = RecordingSpawner::new(SpawnResult::Ok);
        let mut opener = CommandOpener::new(OsKind::Linux, Box::new(recorder)).with_overrides(
            Some(vec![OsString::from("myopen"), OsString::from("-x")]),
            None,
        );
        let outcome = opener.open(Path::new("/a/b.txt"));
        assert_eq!(outcome, OpenerOutcome::Launched);
        assert_eq!(
            calls.borrow()[0],
            vec![
                OsString::from("myopen"),
                OsString::from("-x"),
                OsString::from("/a/b.txt"),
            ]
        );
    }

    #[test]
    fn with_overrides_none_keeps_per_os_default() {
        // AC-8: `with_overrides(None, None)` (and plain `new`) must still produce the
        // existing per-OS default argv, unchanged.
        let (recorder, calls) = RecordingSpawner::new(SpawnResult::Ok);
        let mut opener =
            CommandOpener::new(OsKind::Linux, Box::new(recorder)).with_overrides(None, None);
        let outcome = opener.reveal(Path::new("/a/b.txt"));
        assert_eq!(outcome, OpenerOutcome::Launched);
        assert_eq!(
            calls.borrow()[0],
            opener_argv(OsKind::Linux, OpenAction::Reveal, Path::new("/a/b.txt"))
        );
    }

    #[test]
    fn with_overrides_path_is_distinct_trailing_element() {
        // AC-12: the override prefix is used verbatim (program untouched) and the path is
        // appended as one distinct trailing argv element — never concatenated or shell-split.
        let (recorder, calls) = RecordingSpawner::new(SpawnResult::Ok);
        let mut opener = CommandOpener::new(OsKind::Linux, Box::new(recorder))
            .with_overrides(Some(vec![OsString::from("my open")]), None);
        let outcome = opener.open(Path::new("/a/b.txt"));
        assert_eq!(outcome, OpenerOutcome::Launched);
        let calls = calls.borrow();
        assert_eq!(calls[0][0], OsString::from("my open"), "program untouched");
        assert_eq!(
            calls[0].last().unwrap(),
            &OsString::from("/a/b.txt"),
            "path is the last, distinct argv element"
        );
    }

    #[test]
    fn with_overrides_reveal_replaces_default_argv() {
        // AC-8: a configured `reveal` override reaches the spawn seam too — each of {open, reveal}
        // is covered, not just `open`.
        let (recorder, calls) = RecordingSpawner::new(SpawnResult::Ok);
        let mut opener = CommandOpener::new(OsKind::Linux, Box::new(recorder))
            .with_overrides(None, Some(vec![OsString::from("nautilus")]));
        let outcome = opener.reveal(Path::new("/a/b.txt"));
        assert_eq!(outcome, OpenerOutcome::Launched);
        assert_eq!(
            calls.borrow()[0],
            vec![OsString::from("nautilus"), OsString::from("/a/b.txt")],
        );
    }

    #[test]
    fn with_overrides_empty_prefix_falls_through_to_the_per_os_default() {
        // An empty override (e.g. `open = ""` tokenizes to no args) must NOT spawn the selected
        // file itself as a program — it falls through to the per-OS default argv.
        let (recorder, calls) = RecordingSpawner::new(SpawnResult::Ok);
        let mut opener = CommandOpener::new(OsKind::Linux, Box::new(recorder))
            .with_overrides(Some(vec![]), Some(vec![]));
        let outcome = opener.open(Path::new("/a/b.txt"));
        assert_eq!(outcome, OpenerOutcome::Launched);
        assert_eq!(
            calls.borrow()[0],
            opener_argv(OsKind::Linux, OpenAction::Open, Path::new("/a/b.txt")),
            "empty override falls through to the default, never [path] alone",
        );
    }
}
