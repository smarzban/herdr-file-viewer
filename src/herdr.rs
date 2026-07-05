//! Host Adapter — herdr query seam (AC-3, AC-15).
//!
//! Provides the [`HerdrCli`] trait as the substitution point for all herdr JSON queries,
//! and [`LiveHerdr`] as the real implementation. Tests inject a fake [`CommandRunner`] so
//! nothing is ever really spawned (hermetic).
//!
//! **Read-only:** this seam only runs read-only herdr subcommands. The args are passed by
//! callers; nothing in this module constructs a mutating command.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
use std::process::{Command, Output};

// ---------------------------------------------------------------------------
// Public trait: the substitution point the rest of the app depends on
// ---------------------------------------------------------------------------

/// Run read-only herdr subcommands and return their JSON stdout.
///
/// Callers pass the subcommand args; this seam executes and returns the output.
/// On a non-zero exit the call returns `Err` so the caller can degrade (AC-15).
pub trait HerdrCli {
    /// Run a read-only herdr subcommand expected to emit JSON on stdout.
    fn run_json(&self, args: &[&str]) -> io::Result<String>;
}

// ---------------------------------------------------------------------------
// Inner seam: CommandRunner — lets tests assert argv without real spawning
// ---------------------------------------------------------------------------

/// The inner command-execution seam. The real implementation shells out via
/// [`std::process::Command`]; tests substitute a recorder.
pub trait CommandRunner {
    fn run(&self, program: &OsStr, args: &[&str]) -> io::Result<Output>;
}

/// The real [`CommandRunner`]: delegates to [`std::process::Command`].
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, program: &OsStr, args: &[&str]) -> io::Result<Output> {
        Command::new(program).args(args).output()
    }
}

// ---------------------------------------------------------------------------
// LiveHerdr: the real HerdrCli implementation
// ---------------------------------------------------------------------------

/// Resolves and invokes the herdr binary via an injected [`CommandRunner`].
///
/// Construct with [`LiveHerdr::from_env`] for production use, or
/// [`LiveHerdr::with_runner`] for tests.
pub struct LiveHerdr<R: CommandRunner = RealRunner> {
    program: OsString,
    runner: R,
}

impl LiveHerdr<RealRunner> {
    /// Resolve the herdr binary: `$HERDR_BIN_PATH` if set and non-empty,
    /// otherwise `"herdr"` (expected on `$PATH`).
    pub fn from_env() -> Self {
        let program = resolve_program(std::env::var("HERDR_BIN_PATH").ok());
        Self {
            program,
            runner: RealRunner,
        }
    }
}

impl<R: CommandRunner> LiveHerdr<R> {
    /// Construct with an explicit program name and runner (for tests).
    pub fn with_runner(program: impl Into<OsString>, runner: R) -> Self {
        Self {
            program: program.into(),
            runner,
        }
    }
}

impl<R: CommandRunner> HerdrCli for LiveHerdr<R> {
    fn run_json(&self, args: &[&str]) -> io::Result<String> {
        let out = self.runner.run(&self.program, args)?;
        if !out.status.success() {
            return Err(io::Error::other("herdr exited non-zero"));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

// ---------------------------------------------------------------------------
// Pure helper — factored out so tests can cover the env-resolution logic
// without touching the real environment.
// ---------------------------------------------------------------------------

/// Resolve the herdr binary path from the optional env-var value.
///
/// - `Some(non-empty)` → use that path (with the Windows `.exe`-suffix seam below applied).
/// - `None` or `Some("")` → fall back to `"herdr"`.
pub fn resolve_program(var: Option<String>) -> OsString {
    resolve_program_with(var, Path::exists)
}

/// [`resolve_program`]'s logic, factored out so the Windows `.exe`-suffix seam is testable
/// from an injected exists-predicate (no real filesystem probing needed in tests).
fn resolve_program_with(var: Option<String>, exists: impl Fn(&Path) -> bool) -> OsString {
    match var {
        Some(v) if !v.is_empty() => with_exe_suffix(v, exists),
        _ => OsString::from("herdr"),
    }
}

/// unix: an explicit path/name is used exactly as configured (unchanged — AC-3).
#[cfg(not(windows))]
fn with_exe_suffix(v: String, _exists: impl Fn(&Path) -> bool) -> OsString {
    OsString::from(v)
}

/// Windows: a *bare* name (no path separator) defers to the OS's own `PATH`/`PATHEXT` search —
/// untouched, since the OS already tries `.exe`/`.cmd`/… there. An *explicit* path that lacks
/// an extension and is not found as given resolves to its `.exe` form, so a configured value
/// like `C:\tools\herdr` (commonly typed without the suffix) still locates the real binary
/// (AC-9).
#[cfg(windows)]
fn with_exe_suffix(v: String, exists: impl Fn(&Path) -> bool) -> OsString {
    let path = Path::new(&v);
    let is_explicit_path = v.contains('/') || v.contains('\\');
    if is_explicit_path && path.extension().is_none() && !exists(path) {
        return OsString::from(format!("{v}.exe"));
    }
    OsString::from(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- with_exe_suffix: Windows executable-suffix resolution (AC-9, T-6) -----

    /// unix: any explicit path/name is returned exactly as configured, regardless of what the
    /// (injected, never consulted) exists-predicate would say — AC-3.
    #[cfg(not(windows))]
    #[test]
    fn with_exe_suffix_unix_returns_the_value_unchanged() {
        let got = resolve_program_with(Some("/custom/herdr".to_string()), |_| false);
        assert_eq!(got, OsString::from("/custom/herdr"));
    }

    /// Windows: an explicit path lacking an extension and not found as given resolves to its
    /// `.exe` form.
    #[cfg(windows)]
    #[test]
    fn with_exe_suffix_windows_explicit_path_without_extension_gets_exe_suffix() {
        let got = resolve_program_with(Some(r"C:\tools\herdr".to_string()), |_| false);
        assert_eq!(got, OsString::from(r"C:\tools\herdr.exe"));
    }

    /// Windows: a bare name (no path separator) defers to the OS's own `PATH`/`PATHEXT`
    /// search — left untouched even though it "doesn't exist" by the injected predicate.
    #[cfg(windows)]
    #[test]
    fn with_exe_suffix_windows_bare_name_defers_to_path_search() {
        let got = resolve_program_with(Some("herdr".to_string()), |_| false);
        assert_eq!(got, OsString::from("herdr"));
    }

    /// Windows: an explicit path that already exists as given (e.g. a `.bat` shim, or any
    /// extension-less file that really is the binary) is left untouched.
    #[cfg(windows)]
    #[test]
    fn with_exe_suffix_windows_explicit_path_that_exists_is_untouched() {
        let got = resolve_program_with(Some(r"C:\tools\herdr".to_string()), |_| true);
        assert_eq!(got, OsString::from(r"C:\tools\herdr"));
    }

    /// Windows: an explicit path that already carries an extension (e.g. `.exe`, `.cmd`) is
    /// left untouched, even when it does not exist as given.
    #[cfg(windows)]
    #[test]
    fn with_exe_suffix_windows_explicit_path_with_extension_is_untouched() {
        let got = resolve_program_with(Some(r"C:\tools\herdr.cmd".to_string()), |_| false);
        assert_eq!(got, OsString::from(r"C:\tools\herdr.cmd"));
    }

    /// `None`/empty still fall back to `"herdr"` on every platform (unchanged).
    #[test]
    fn resolve_program_with_none_or_empty_falls_back_to_herdr() {
        assert_eq!(
            resolve_program_with(None, |_| false),
            OsString::from("herdr")
        );
        assert_eq!(
            resolve_program_with(Some(String::new()), |_| false),
            OsString::from("herdr")
        );
    }
}
