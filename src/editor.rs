//! Editor Launcher — hand the selected file off to an external editor (AC-19).
//!
//! Pure hand-off: it spawns the user's configured editor on the file. It never reads,
//! writes, or otherwise mutates the file (AC-N1) — it only launches another process. The
//! spawn goes through the injected [`Spawner`] so tests stay hermetic (nothing is really
//! launched).

use std::ffi::OsString;
use std::io;
use std::path::Path;

/// The reason a [`Spawner::spawn`] call failed. Distinguishing the two cases lets the
/// controller word its user-facing notice correctly: a [`SpawnError::NotLaunched`] means
/// the editor never ran (e.g. the binary is not on `PATH`) — "could not open editor"; a
/// [`SpawnError::NonZeroExit`] means the editor *did* run and returned a failing status —
/// "editor exited with …" (a non-zero vim exit is often benign, so it must not be reported
/// as a launch failure).
#[derive(Debug)]
pub enum SpawnError {
    /// The editor process could not be started at all (binary missing, permission denied,
    /// empty argv, …). Nothing ran; the terminal was not handed over to an editor.
    NotLaunched(io::Error),
    /// The editor launched and ran, then exited with a non-zero status. The hand-off took
    /// place (the editor owned the terminal); only its exit code signals a problem.
    NonZeroExit(String),
}

impl From<SpawnError> for io::Error {
    fn from(e: SpawnError) -> Self {
        match e {
            SpawnError::NotLaunched(e) => e,
            SpawnError::NonZeroExit(msg) => io::Error::other(msg),
        }
    }
}

/// The external-effect seam. The real implementation runs the editor process; tests
/// substitute a recorder so no editor is actually launched.
pub trait Spawner {
    /// Run a local command for the editor hand-off (`argv[0]` is the program). The result
    /// distinguishes a launch failure ([`SpawnError::NotLaunched`]) from a successful launch
    /// that exited non-zero ([`SpawnError::NonZeroExit`]) so the caller can report each case
    /// accurately.
    fn spawn(&mut self, argv: &[OsString]) -> Result<(), SpawnError>;
}

/// Hands a file off to an external editor, holding the configured editor command.
pub struct EditorLauncher {
    editor: OsString,
}

impl EditorLauncher {
    /// Create a launcher for the given editor command (e.g. from `$EDITOR`).
    pub fn new(editor: impl Into<OsString>) -> Self {
        Self {
            editor: editor.into(),
        }
    }

    /// Spawn the configured editor on `file`. Returns the launch result; a failure is an
    /// `Err` the caller surfaces as a non-fatal notice (never a panic). Performs no file I/O
    /// (AC-N1).
    pub fn open(&self, file: &Path, spawner: &mut impl Spawner) -> Result<(), SpawnError> {
        spawner.spawn(&self.editor_argv(file))
    }

    /// Build the local-spawn argv: the configured editor split into program + arguments
    /// (so `$EDITOR` values like `"code --wait"` launch correctly), with the selected file
    /// appended as the final argument. Whitespace-split is the conventional `$EDITOR`
    /// reading; an editor path containing spaces is not supported. An empty/whitespace-only
    /// editor falls back to the raw value so the launch fails loudly rather than exec-ing the
    /// file.
    fn editor_argv(&self, file: &Path) -> Vec<OsString> {
        let mut argv: Vec<OsString> = self
            .editor
            .to_string_lossy()
            .split_whitespace()
            .map(OsString::from)
            .collect();
        if argv.is_empty() {
            argv.push(self.editor.clone());
        }
        argv.push(file.as_os_str().to_owned());
        argv
    }
}
