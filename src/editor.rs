//! Editor Launcher — hand the selected file off to an external editor (AC-19).
//!
//! Pure hand-off: it spawns the user's configured editor on the file. It never reads,
//! writes, or otherwise mutates the file (AC-N1) — it only launches another process. The
//! spawn goes through the injected [`Spawner`] so tests stay hermetic (nothing is really
//! launched).

use std::ffi::OsString;
use std::io;
use std::path::Path;

/// The external-effect seam. The real implementation runs the editor process; tests
/// substitute a recorder so no editor is actually launched.
pub trait Spawner {
    /// Run a local command for the editor hand-off (`argv[0]` is the program).
    fn spawn(&mut self, argv: &[OsString]) -> io::Result<()>;
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
    pub fn open(&self, file: &Path, spawner: &mut impl Spawner) -> io::Result<()> {
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
