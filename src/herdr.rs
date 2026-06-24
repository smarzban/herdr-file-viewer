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
/// - `Some(non-empty)` → use that path.
/// - `None` or `Some("")` → fall back to `"herdr"`.
pub fn resolve_program(var: Option<String>) -> OsString {
    match var {
        Some(v) if !v.is_empty() => OsString::from(v),
        _ => OsString::from("herdr"),
    }
}
