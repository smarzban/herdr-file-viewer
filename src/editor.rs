//! Editor Launcher — hand the selected file off to an external editor (AC-19).
//!
//! Pure hand-off: it either spawns the user's configured editor on the file, or asks the
//! host to open the file in a **new herdr pane** (whose split→run sequence the Host Adapter
//! owns, T-17). It never reads, writes, or otherwise mutates the file (AC-N1) — it only
//! launches another process. External effects go through the injected [`Spawner`] so tests
//! stay hermetic (nothing is really launched).

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

/// Where to hand the file off to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The user's configured editor, launched in place of / over the viewer.
    Editor,
    /// A new herdr pane that edits the file (Host Adapter performs the split→run, T-17).
    NewPane,
}

/// The external-effect seam. The real implementation runs processes / drives the herdr CLI;
/// tests substitute a recorder so no editor or pane is actually launched.
pub trait Spawner {
    /// Run a local command for the editor hand-off (`argv[0]` is the program).
    fn spawn(&mut self, argv: &[OsString]) -> io::Result<()>;
    /// Open `file` in a new herdr pane, editing it there with `editor`.
    fn open_pane(&mut self, editor: &OsStr, file: &Path) -> io::Result<()>;
}

/// Hands a file off to an external editor, holding the configured editor command.
pub struct EditorLauncher {
    editor: OsString,
}

impl EditorLauncher {
    /// Create a launcher for the given editor command (e.g. from `$EDITOR`).
    pub fn new(editor: impl Into<OsString>) -> Self {
        Self { editor: editor.into() }
    }

    /// Hand `file` off per `target` — spawn the configured editor on it, or ask the host to
    /// open it in a new herdr pane. Returns the launch result; a failure is an `Err` the
    /// caller surfaces as a non-fatal notice (never a panic). Performs no file I/O (AC-N1).
    pub fn open(&self, file: &Path, target: Target, spawner: &mut impl Spawner) -> io::Result<()> {
        match target {
            Target::Editor => spawner.spawn(&self.editor_argv(file)),
            Target::NewPane => spawner.open_pane(&self.editor, file),
        }
    }

    /// Build the local-spawn argv: the configured editor split into program + arguments
    /// (so `$EDITOR` values like `"code --wait"` launch correctly), with the selected file
    /// appended as the final argument. Whitespace-split is the conventional `$EDITOR`
    /// reading; an editor path containing spaces is not supported on this path (use the
    /// new-pane path, which runs the editor as shell text). An empty/whitespace-only editor
    /// falls back to the raw value so the launch fails loudly rather than exec-ing the file.
    fn editor_argv(&self, file: &Path) -> Vec<OsString> {
        let mut argv: Vec<OsString> =
            self.editor.to_string_lossy().split_whitespace().map(OsString::from).collect();
        if argv.is_empty() {
            argv.push(self.editor.clone());
        }
        argv.push(file.as_os_str().to_owned());
        argv
    }
}
