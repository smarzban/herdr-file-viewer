//! Host Adapter — the herdr boundary: parse the injected launch context and issue
//! open-pane requests to herdr (AC-19, AC-26).
//!
//! Two trust boundaries meet here:
//!  * **herdr-injected context** (`HERDR_PLUGIN_CONTEXT_JSON`) is parsed defensively —
//!    malformed/missing input degrades to a minimal `{ cwd }`, never a panic (AC-26).
//!  * **the herdr CLI hand-off** shells out to open a new pane. The untrusted file name is
//!    single-quoted into the run command so it cannot inject, a relative path that begins
//!    with `-` is prefixed `./` so the editor can't read it as a flag, and every pane id
//!    used as an argv element is validated so a flag-like id (`--foo`) cannot option-inject.
//!
//! The CLI is reached through the injected [`HerdrCli`] seam so tests stay hermetic.

use crate::context::LaunchContext;
use serde::Deserialize;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

/// The seam for invoking the herdr CLI (`$HERDR_BIN_PATH`). Returns captured stdout so the
/// new pane id can be parsed from the `pane split` result. Injected for hermetic tests.
pub trait HerdrCli {
    /// Run a herdr subcommand (the implementation prepends `$HERDR_BIN_PATH`); capture stdout.
    fn run(&mut self, args: &[OsString]) -> io::Result<String>;
}

/// The shape of `HERDR_PLUGIN_CONTEXT_JSON`. Every field is optional so a partial or absent
/// object degrades gracefully rather than failing to parse.
#[derive(Deserialize, Default)]
struct RawContext {
    cwd: Option<String>,
    worktree_root: Option<String>,
    base_branch: Option<String>,
    #[serde(default)]
    is_worktree: bool,
    pane_id: Option<String>,
}

/// The `pane split` result envelope we read the new pane id from (`result.pane.pane_id`).
#[derive(Deserialize)]
struct SplitResult {
    result: SplitResultBody,
}
#[derive(Deserialize)]
struct SplitResultBody {
    pane: SplitPane,
}
#[derive(Deserialize)]
struct SplitPane {
    pane_id: String,
}

/// Build a `LaunchContext` from the process environment: the injected context JSON, falling
/// back to the process working directory. Never panics (AC-26).
pub fn from_env() -> LaunchContext {
    let json = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok();
    let cwd = std::env::current_dir().unwrap_or_default();
    parse_context(json.as_deref(), cwd)
}

/// Pure parser behind [`from_env`] (testable without touching process env). Missing or
/// malformed JSON yields a minimal `{ cwd: fallback_cwd }` context (AC-26).
pub fn parse_context(json: Option<&str>, fallback_cwd: PathBuf) -> LaunchContext {
    let raw: RawContext = json
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    LaunchContext {
        cwd: raw.cwd.map(PathBuf::from).unwrap_or(fallback_cwd),
        worktree_root: raw.worktree_root.map(PathBuf::from),
        base_branch: raw.base_branch,
        is_worktree: raw.is_worktree,
    }
}

/// The current pane id from the injected context, if present and well-formed. A malformed
/// JSON blob or a syntactically unsafe id yields `None` (so it is never used as an argv).
pub fn current_pane_id(json: Option<&str>) -> Option<String> {
    let raw: RawContext = json.and_then(|s| serde_json::from_str(s).ok())?;
    raw.pane_id.filter(|id| is_safe_pane_id(id))
}

/// Open `file` in a new herdr pane, editing it there with `editor`, via the two-step herdr
/// CLI sequence:
///   1. `pane split <current_pane_id> --direction right --no-focus` → capture JSON,
///   2. parse `result.pane.pane_id`,
///   3. `pane run <new_pane_id> "<editor> <quoted-file>"`.
///
/// Both pane ids are validated before use (option-injection guard) and the file is
/// shell-escaped (command-injection guard). Any failure is an `Err` the caller surfaces as
/// a non-fatal notice; it performs no file I/O (AC-N1).
pub fn open_pane(
    file: &Path,
    editor: &OsStr,
    current_pane_id: &str,
    cli: &mut impl HerdrCli,
) -> io::Result<()> {
    if !is_safe_pane_id(current_pane_id) {
        return Err(invalid(format!("unsafe current pane id: {current_pane_id:?}")));
    }

    let split_out = cli.run(&osv([
        "pane",
        "split",
        current_pane_id,
        "--direction",
        "right",
        "--no-focus",
    ]))?;

    let parsed: SplitResult = serde_json::from_str(&split_out)
        .map_err(|e| invalid(format!("could not parse pane split result: {e}")))?;
    let new_pane = parsed.result.pane.pane_id;
    if !is_safe_pane_id(&new_pane) {
        return Err(invalid(format!("unsafe new pane id from split: {new_pane:?}")));
    }

    // Trust boundary: `editor` is application configuration ($EDITOR / user config), treated
    // as trusted shell text and left unquoted so editors-with-flags work (as git does with
    // GIT_EDITOR). `file` is the untrusted, repo-controlled input — it is single-quoted by
    // `shell_quote_path`, so it is inert data inside the command. (A file name that is not
    // valid UTF-8 is rendered lossily here — a known v1 limitation on the rare non-UTF-8
    // path; the herdr `pane run` command is a text string and cannot carry raw bytes.)
    let command = format!("{} {}", editor.to_string_lossy(), shell_quote_path(file));
    cli.run(&[
        OsString::from("pane"),
        OsString::from("run"),
        OsString::from(new_pane),
        OsString::from(command),
    ])?;
    Ok(())
}

/// Build an argv as `OsString`s from string literals.
fn osv<const N: usize>(parts: [&str; N]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

/// An `io::Error` for a rejected/garbled host response.
fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// A pane id is safe to use as an argv element iff it is a non-empty token of
/// `[A-Za-z0-9_-]` that does not start with `-` (which would option-inject).
fn is_safe_pane_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => {}
        _ => return false,
    }
    id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Render `file` for the `pane run` shell command: POSIX single-quoted so any metacharacter
/// is literal, and `./`-prefixed when a relative path would otherwise begin with `-`.
fn shell_quote_path(file: &Path) -> String {
    let raw = file.to_string_lossy();
    let flag_safe = if raw.starts_with('-') {
        format!("./{raw}")
    } else {
        raw.into_owned()
    };
    let mut out = String::with_capacity(flag_safe.len() + 2);
    out.push('\'');
    for ch in flag_safe.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}
