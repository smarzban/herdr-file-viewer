//! Content Renderer — produce the content-pane text for a file, with safety guards.
//!
//! The primary trust boundary: all file bytes are untrusted. This module bounds size
//! (AC-13), refuses to emit raw bytes for binary files (AC-12), neutralizes control/escape
//! sequences (AC-27), and delegates styling to external CLIs with a plain-text fallback
//! (AC-24/25). Reads only, never writes (AC-N1).

use crate::view_policy::ViewMode;
use ansi_to_tui::IntoText;
use ratatui::text::Text;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The default preview line cap — mirror of [`crate::config::DEFAULT_PREVIEW_MAX_LINES`]. Used by
/// [`Caps::default`] so a config-absent run behaves exactly as before; a config test keeps the two
/// in lockstep.
const DEFAULT_MAX_LINES: usize = 10000;
/// The default preview size cap (1 MiB) — mirror of [`crate::config::DEFAULT_PREVIEW_MAX_KIB`].
const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;
/// Cap on bytes captured from a renderer's stdout, bounding memory if it spews output.
const MAX_RENDER_OUTPUT: u64 = 16 * 1024 * 1024; // 16 MB

/// The Content Renderer's size caps: past `max_lines` lines **or** `max_bytes` bytes a file (or a
/// large diff) is shown as a truncated preview plus a visible notice (AC-13), and `max_bytes` also
/// bounds the actual file read so a giant/hostile file is never slurped whole (AC-N1). Injected
/// (from the `preview_max_lines` / `preview_max_kib` config keys) so the caps are configurable while
/// tests stay hermetic. `Copy` — it is two integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// Truncate the preview past this many lines.
    pub max_lines: usize,
    /// Truncate the preview past this many bytes; also the bounded-read ceiling.
    pub max_bytes: u64,
}

impl Default for Caps {
    /// The built-in caps (10000 lines / 1 MiB). Must equal what [`crate::config::resolve`] produces
    /// for an empty config; `crate::config`'s `render_caps_default_matches_config_defaults` test
    /// pins that so the two constants can never drift apart.
    fn default() -> Self {
        Caps {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// Render a cap as a short human label for a truncation notice (`1 MB`, `512 KB`). Values come from
/// a KiB config knob, so they are whole kibibytes; MiB-round values read as `N MB` (matching the
/// historical "1 MB" wording), everything else as `N KB`.
fn human_bytes(n: u64) -> String {
    let kib = n / 1024;
    if kib >= 1024 && kib.is_multiple_of(1024) {
        format!("{} MB", kib / 1024)
    } else {
        format!("{kib} KB")
    }
}

/// Truncate `s` in place to at most `max_bytes` bytes, cutting on a UTF-8 char boundary so a
/// multi-byte character is never split. Shared by [`classify`] and [`cap_preview`] so the byte cap
/// bounds the *displayed* preview, not only the disk read: `from_utf8_lossy` can expand invalid
/// bytes (each becomes a 3-byte U+FFFD), so a line-bounded-only preview of a hostile file could
/// otherwise exceed the cap by up to ~3× before rendering.
fn truncate_to_bytes(s: &mut String, max_bytes: u64) {
    let max = max_bytes.min(s.len() as u64) as usize;
    if max == s.len() {
        return; // already within the cap — no allocation, no scan
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// The guarded result of reading a file's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    /// A binary file: a placeholder is shown, never the raw bytes (AC-12).
    Binary,
    /// A file at/above the size cap: a bounded preview plus a visible notice (AC-13).
    Truncated { text: String, notice: String },
    /// A normal text file shown in full.
    Full { text: String },
}

/// Classify a file for display: binary vs. truncated-preview vs. full text. Reads at most
/// `caps.max_bytes` from disk, so a huge or hostile file can never be slurped whole (AC-N1).
///
/// Refuses to read anything that does not resolve to a **regular file inside `root`**:
/// a symlink (or `..`) escaping the root cannot leak out-of-root content into the pane
/// (AC-N5), and a FIFO/device/dir is never opened (no hang, no garbage). Such paths
/// return `Binary` (a placeholder, no bytes).
pub fn classify(root: &Path, path: &Path, caps: Caps) -> Prepared {
    let (Ok(canonical), Ok(canon_root)) = (path.canonicalize(), root.canonicalize()) else {
        return Prepared::Binary; // unresolvable / missing
    };
    if !canonical.starts_with(&canon_root) {
        return Prepared::Binary; // escapes the root (AC-N5)
    }
    match std::fs::metadata(&canonical) {
        Ok(m) if m.is_file() => {}
        _ => return Prepared::Binary, // dir / FIFO / device / gone
    }

    let byte_len = std::fs::metadata(&canonical).map(|m| m.len()).unwrap_or(0);
    let Ok(file) = File::open(&canonical) else {
        return Prepared::Binary; // unreadable (e.g. permissions) → placeholder, not a misleading empty pane
    };
    // Bounded read: at most caps.max_bytes, so a giant/hostile file is never slurped whole. The
    // config resolver clamps the cap to a finite ceiling, so even a configured value keeps this
    // guarantee (AC-N1).
    let mut buf = Vec::new();
    if file.take(caps.max_bytes).read_to_end(&mut buf).is_err() {
        return Prepared::Full {
            text: String::new(),
        };
    }

    // Binary: a NUL byte anywhere in the (bounded) content. No raw bytes are emitted.
    if buf.contains(&0) {
        return Prepared::Binary;
    }

    let over_bytes = byte_len >= caps.max_bytes;
    // If the file fit under the cap, invalid UTF-8 means binary. If it was capped, the
    // read may have split a multi-byte char, so decode lossily rather than misclassify.
    let text = if over_bytes {
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        match String::from_utf8(buf) {
            Ok(t) => t,
            Err(_) => return Prepared::Binary,
        }
    };

    let line_count = text.lines().count();
    let over_lines = line_count >= caps.max_lines;
    if over_bytes || over_lines {
        let mut preview: String = text
            .lines()
            .take(caps.max_lines)
            .collect::<Vec<_>>()
            .join("\n");
        // Byte-bound the (possibly lossy-expanded) preview so the byte cap bounds what is *shown*,
        // not only the disk read — matching cap_preview's guarantee for diffs.
        truncate_to_bytes(&mut preview, caps.max_bytes);
        let cap = if over_bytes {
            format!("{} size", human_bytes(caps.max_bytes))
        } else {
            format!("{}-line", caps.max_lines)
        };
        let notice = format!(
            "⚠ Truncated preview: showing {} lines ({} of {} bytes); file exceeds the {} cap.",
            preview.lines().count(),
            preview.len(),
            byte_len,
            cap
        );
        return Prepared::Truncated {
            text: preview,
            notice,
        };
    }
    Prepared::Full { text }
}

/// The external renderer commands (program + args) per view mode. Injected so tests stay
/// hermetic and so a real deployment points these at glow / delta / bat.
#[derive(Debug, Clone)]
pub struct Renderers {
    pub markdown: Vec<String>,
    pub diff: Vec<String>,
    /// Renders a full-context diff (whole file) — same delegate as `diff` but configured to
    /// show a line-number gutter, so the file's lines are numbered with the diff shown inline.
    pub full_diff: Vec<String>,
    pub syntax: Vec<String>,
    /// Converts a non-PNG image file (fed on **stdin** as raw bytes) to PNG on stdout, for the
    /// Media view (defaults to ffmpeg). Absent ⇒ the Media view shows its placeholder + notice.
    pub image: Vec<String>,
    /// Extracts video frames as PNG. A template: `{start}` / `{width}` / `{height}` / `{fps}`
    /// are substituted before use (the file path is passed as an argv element — you cannot
    /// seek a pipe, the one narrowing of the stdin trust boundary; see ARCHITECTURE.md).
    /// Absent ⇒ video shows its placeholder + notice.
    pub video: Vec<String>,
    /// Reports a video's codec and duration for the Media info line (defaults to ffprobe).
    /// `{name}` is substituted with the file path. Purely informational: an empty vec, a missing
    /// binary, or an unparsable answer just omits those fields — it never blocks playback.
    pub probe: Vec<String>,
    /// Per-invocation wall-clock bound; a renderer exceeding it is killed and the plain-
    /// text fallback is used, so a wedged delegate can never hang rendering.
    pub timeout: Duration,
}

/// Produce the content-pane text for a prepared file in a given view mode, delegating to
/// the external renderer for that mode. Untrusted content is fed on **stdin** (never as an
/// argument) to the trusted, configured renderer; its output is re-neutralized by
/// [`to_text`]. A missing/failed renderer falls back to plain text plus a notice naming
/// the missing capability (AC-24, AC-25). Returns the text and an optional notice.
pub fn render(
    renderers: &Renderers,
    prepared: &Prepared,
    mode: ViewMode,
    raw_diff: Option<&str>,
    file_name: Option<&str>,
    caps: Caps,
) -> (Text<'static>, Option<String>) {
    let name = sanitize_name(file_name.unwrap_or(""));
    let name = name.as_str();
    // A diff is derived from git, not from the file's bytes, so it renders even for a
    // deleted or binary file (AC-9) — never short-circuit it to the binary placeholder. Both
    // the compact diff and the full-context diff render from the git diff text on `raw_diff`;
    // they differ only in the diff git produced (default vs. whole-file context) and the
    // delegate used (the full-context one numbers lines).
    if mode == ViewMode::Diff || mode == ViewMode::FullDiff {
        let cmd = if mode == ViewMode::FullDiff {
            &renderers.full_diff
        } else {
            &renderers.diff
        };
        let (diff, notice) = cap_preview(raw_diff.unwrap_or(""), caps);
        return delegate(
            &with_name(cmd, name),
            &diff,
            mode,
            renderers.timeout,
            notice,
        );
    }

    // Content modes: a binary file shows a placeholder, never raw bytes (AC-12).
    let (content, base_notice) = match prepared {
        Prepared::Binary => return (Text::raw("[binary file: preview not shown]"), None),
        Prepared::Full { text } => (text.as_str(), None),
        Prepared::Truncated { text, notice } => (text.as_str(), Some(notice.clone())),
    };

    match mode {
        ViewMode::RenderedMarkdown => delegate(
            &with_name(&renderers.markdown, name),
            content,
            mode,
            renderers.timeout,
            base_notice,
        ),
        ViewMode::SyntaxContent => delegate(
            &with_name(&renderers.syntax, name),
            content,
            mode,
            renderers.timeout,
            base_notice,
        ),
        ViewMode::Diff | ViewMode::FullDiff => unreachable!("handled above"),
        ViewMode::Media => unreachable!("media is rendered by render_media, not render"),
    }
}

/// Return a bounded, escape-neutralized raw diff without invoking an external renderer. This is
/// the `D` cycle's plain-text state: the diff still comes from git, but no formatter is required.
pub fn render_raw_diff(raw_diff: Option<&str>, caps: Caps) -> (Text<'static>, Option<String>) {
    let (diff, notice) = cap_preview(raw_diff.unwrap_or(""), caps);
    (to_text(&diff), notice)
}

/// Return a copy of a markdown renderer command (e.g. glow) with its wrap width set to `width`:
/// replace the argument following the `-w` flag, or append `-w <width>` if absent. Used by the help
/// overlay's What's New render so glow wraps the changelog to the fixed help-box body width (with its
/// own hanging indents) instead of the default `-w 0` (no wrap → the Presenter's flat re-wrap loses
/// the indents). The base command (and its `{name}`/`-` args) is otherwise unchanged.
pub(crate) fn with_wrap_width(command: &[String], width: u16) -> Vec<String> {
    let mut out = command.to_vec();
    let w = width.to_string();
    match out.iter().position(|a| a == "-w") {
        // Replace the value after `-w`; if `-w` is the trailing arg with no value, append one.
        Some(i) => match out.get_mut(i + 1) {
            Some(v) => *v = w,
            None => out.push(w),
        },
        // No `-w` at all: append the flag + value (kept ahead of any trailing positional is not
        // required — glow accepts flags after `-`, but we insert before the final `-` if present
        // for tidiness).
        None => {
            let insert_at = out.iter().rposition(|a| a == "-").unwrap_or(out.len());
            out.insert(insert_at, w);
            out.insert(insert_at, "-w".to_string());
        }
    }
    out
}

/// Render exactly one untrusted Markdown document for a Help section using the injected command.
///
/// `deadline` is the one Help-open absolute deadline, supplied by the composer. `fallback` was
/// terminal-neutralized before that budget was spent, so expiration never scans the document again.
/// The normal path still applies the width rewrite, output cap, ANSI neutralizer, and existing
/// capability-specific fallback notice.
pub fn render_markdown_section(
    markdown_command: &[String],
    document: &str,
    fallback: Text<'static>,
    width: u16,
    deadline: Instant,
) -> (Text<'static>, Option<String>) {
    if deadline.saturating_duration_since(Instant::now()).is_zero() {
        return markdown_section_timeout(fallback);
    }
    let command = with_wrap_width(markdown_command, width);
    delegate_markdown_section(&command, document, fallback, deadline)
}

fn markdown_section_timeout(fallback: Text<'static>) -> (Text<'static>, Option<String>) {
    (
        fallback,
        Some(RendererError::Timeout.notice(capability(ViewMode::RenderedMarkdown))),
    )
}

/// Substitute the `{name}` placeholder in a renderer command with the selected file name,
/// so a stdin-fed renderer (e.g. `bat --file-name={name}`) can still infer the language —
/// keeping the secure stdin design while enabling syntax highlighting (AC-10).
pub(crate) fn with_name(command: &[String], name: &str) -> Vec<String> {
    command
        .iter()
        .map(|arg| arg.replace("{name}", name))
        .collect()
}

/// Substitute the `{name}` (the file PATH, `'{}`-canonicalized in-root) into the video frame
/// decoder's argv template — in addition to the player's `{start}/{fps}/{width}/{height}`
/// substitution. This is the one deliberate narrowing of the stdin trust boundary: you cannot
/// `-ss`-seek a pipe, so the canonicalized in-root path is passed as its own argv element, no
/// shell. `name` must already be the sanitized basename-or-path the caller controls.
pub(crate) fn with_video_name(command: &[String], name: &str) -> Vec<String> {
    with_name(command, name)
}

/// Bound a text block to the size cap, returning a preview plus a truncation notice when
/// it exceeds it. Used for diff text (AC-13's bound applied to large diffs, keeping the
/// UI path responsive regardless of how big a changed file's diff is).
fn cap_preview(text: &str, caps: Caps) -> (String, Option<String>) {
    let over = text.lines().count() >= caps.max_lines || text.len() as u64 >= caps.max_bytes;
    if !over {
        return (text.to_string(), None);
    }
    let mut preview: String = text
        .lines()
        .take(caps.max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    truncate_to_bytes(&mut preview, caps.max_bytes);
    (
        preview,
        Some("⚠ Truncated diff preview: diff exceeds the size cap.".into()),
    )
}

/// Reduce an untrusted file name to a safe basename — directory parts stripped, only
/// `[A-Za-z0-9._-]` kept (others → `_`). The extension survives (for language detection),
/// but the value is safe to interpolate even into a shell-wrapper renderer command, so a
/// repo-controlled file name cannot inject shell metacharacters via `{name}`.
fn sanitize_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let safe: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // A leading '-' would be parsed as an option by a renderer (e.g. `bat -rf.rs`); prefix
    // it so the value is always treated as a file name.
    if safe.starts_with('-') {
        format!("_{safe}")
    } else {
        safe
    }
}

/// Run a renderer over `input`, ingesting its output; on missing/failed/timed-out renderer
/// fall back to plain text plus a capability-naming notice (AC-24/25), preserving any
/// pre-existing `base_notice` (e.g. a truncation notice).
fn delegate(
    command: &[String],
    input: &str,
    mode: ViewMode,
    timeout: Duration,
    base_notice: Option<String>,
) -> (Text<'static>, Option<String>) {
    match run_renderer(command, input, timeout) {
        Ok(out) => (to_text(&out), base_notice),
        Err(err) => (
            to_text(input),
            Some(fallback_notice(err, mode, base_notice)),
        ),
    }
}

/// The Help-only adapter receives a fallback prepared by the composer before it lets a renderer
/// consume time. Unlike [`delegate`], this failure path must not scan the source document again.
fn delegate_markdown_section(
    command: &[String],
    input: &str,
    fallback: Text<'static>,
    deadline: Instant,
) -> (Text<'static>, Option<String>) {
    match run_renderer_until(command, input, deadline) {
        Ok(out) => (to_text(&out), None),
        Err(err) => (
            fallback,
            Some(fallback_notice(err, ViewMode::RenderedMarkdown, None)),
        ),
    }
}

/// Map a typed failure to the existing short, actionable user-facing notice, preserving a prior
/// truncation notice when a general file/diff render already has one.
fn fallback_notice(err: RendererError, mode: ViewMode, base_notice: Option<String>) -> String {
    // The raw OS errno / io::Error detail is retained by `RendererError`, never shown here: a user
    // can act on the capability and remediation, not "No such file or directory (os error 2)".
    let fallback = err.notice(capability(mode));
    match base_notice {
        Some(prev) => format!("{prev}\n{fallback}"),
        None => fallback,
    }
}

/// A typed renderer failure, so the fallback notice can branch on the failure *kind* rather
/// than string-matching a raw error. The raw detail is retained for a future
/// debug/verbose path but is kept out of the user-facing notice.
#[derive(Debug)]
#[allow(dead_code)] // `detail` is retained for a future debug/verbose path.
enum RendererError {
    /// The renderer binary could not be found (spawn returned `ErrorKind::NotFound`).
    NotFound { prog: String, detail: String },
    /// The renderer exceeded its wall-clock bound and was killed.
    Timeout,
    /// The renderer spawned but failed otherwise (non-zero exit, IO error, no exit). The detail
    /// is the raw underlying message (kept off the default notice).
    Failed { detail: String },
}

impl RendererError {
    /// Build the user-facing fallback notice for this failure kind, naming the capability
    /// (`cap`) the renderer was meant to provide. Never includes a raw OS errno or
    /// `io::Error` Debug string.
    fn notice(&self, cap: &str) -> String {
        match self {
            RendererError::NotFound { prog, .. } => format!(
                "{cap} renderer ({prog}) not found; showing plain text. \
                 Install it or see docs/renderers.md."
            ),
            RendererError::Timeout => format!("{cap} renderer timed out; showing plain text."),
            RendererError::Failed { .. } => format!("{cap} renderer failed; showing plain text."),
        }
    }
}

/// A human name for the renderer a mode delegates to (for fallback notices).
fn capability(mode: ViewMode) -> &'static str {
    match mode {
        ViewMode::Diff => "Diff",
        ViewMode::FullDiff => "Full-file diff",
        ViewMode::RenderedMarkdown => "Markdown",
        ViewMode::SyntaxContent => "Syntax",
        ViewMode::Media => "Media",
    }
}

/// Build the (trusted, operator-configured) renderer subprocess: program + args, color forced
/// for the pipe, stdin/stdout piped, stderr discarded. `CLICOLOR_FORCE=1` stops termenv-based
/// tools (glow/glamour) from dropping to a no-color profile when stdout is not a TTY — as it
/// always is here — which would strip all markdown color (headings, inline code, code-block
/// highlighting). Harmless to delta/bat, which force color via their own flags.
fn renderer_command(command: &[String]) -> Result<Command, String> {
    let (prog, args) = command.split_first().ok_or("empty renderer command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .env("CLICOLOR_FORCE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(cmd)
}

/// General file/diff rendering: the wall-clock bound is measured from this call and spans spawn,
/// capture, and exit — one deadline, never stacked phase budgets.
fn run_renderer(
    command: &[String],
    input: &str,
    timeout: Duration,
) -> Result<String, RendererError> {
    run_renderer_until(command, input, Instant::now() + timeout)
}

/// A binary-in, binary-out renderer call (the media converters — `image`/`video` commands). The
/// input is fed on stdin as raw bytes and stdout is returned raw, so a PNG pipeline is never
/// round-tripped through lossy UTF-8. Same deadline / output-cap / kill-and-reap guarantees as
/// [`run_renderer_until`]; the caller owns the byte cap (the media size cap, not the text one).
pub(crate) fn run_renderer_bytes(
    command: &[String],
    input: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    run_renderer_bytes_until(command, input, Instant::now() + timeout)
        .map_err(|e| e.notice(capability(ViewMode::Media)))
}

/// Spawn a renderer, feed `input` on stdin (writer thread, avoiding a pipe deadlock), then capture
/// stdout on a reader thread through the caller's absolute deadline.
///
/// The deadline bounds the wait for useful output only: on overrun the child is killed and reaped
/// **unconditionally** (see [`crate::proc::terminate_and_reap`]), so the call may briefly outlive
/// the deadline rather than ever leaking a zombie. Killing the child closes both pipes, which
/// releases the stdin writer and stdout reader threads.
fn run_renderer_until(
    command: &[String],
    input: &str,
    deadline: Instant,
) -> Result<String, RendererError> {
    run_renderer_bytes_until(command, input.as_bytes(), deadline)
        .map(|buf| String::from_utf8_lossy(&buf).into_owned())
}

/// The byte-oriented core shared by [`run_renderer_until`] (text, lossy-decoded) and
/// [`run_renderer_bytes`] (binary pipelines): spawn, stdin write on a writer thread, stdout
/// capture on a reader thread, one deadline, unconditional kill-and-reap on overrun.
fn run_renderer_bytes_until(
    command: &[String],
    input: &[u8],
    deadline: Instant,
) -> Result<Vec<u8>, RendererError> {
    let prog = command
        .first()
        .cloned()
        .ok_or_else(|| RendererError::Failed {
            detail: "empty renderer command".to_string(),
        })?;
    if deadline.saturating_duration_since(Instant::now()).is_zero() {
        return Err(RendererError::Timeout);
    }
    let mut child = renderer_command(command)
        .map_err(|e| RendererError::Failed { detail: e })?
        .spawn()
        .map_err(|e| {
            // A spawn failure is almost always "binary not installed" — branch on the OS error
            // kind so the notice can name the binary and point to remediation, instead of
            // leaking the raw "No such file or directory (os error 2)".
            if e.kind() == std::io::ErrorKind::NotFound {
                RendererError::NotFound {
                    prog: prog.clone(),
                    detail: e.to_string(),
                }
            } else {
                RendererError::Failed {
                    detail: e.to_string(),
                }
            }
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        let owned = input.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&owned); // ignore a closed pipe
        });
    }

    let stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let buf = stdout.map(capture_renderer_output).unwrap_or_default();
        let _ = tx.send(buf);
    });

    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(buf) => match crate::proc::wait_until(&mut child, deadline) {
            Some(status) if status.success() => Ok(buf),
            Some(status) => Err(RendererError::Failed {
                detail: format!("exited with {status}"),
            }),
            // `wait_until` killed and reaped the child on overrun.
            None => Err(RendererError::Timeout),
        },
        Err(_) => {
            let _ = crate::proc::terminate_and_reap(&mut child);
            Err(RendererError::Timeout)
        }
    }
}

/// Capture no more than [`MAX_RENDER_OUTPUT`] bytes. A blocked read is released when the child is
/// killed at the deadline and its stdout pipe closes.
fn capture_renderer_output(stdout: impl Read) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = stdout.take(MAX_RENDER_OUTPUT).read_to_end(&mut buf);
    buf
}

// ---------------------------------------------------------------------------
// Media: the still-preview payload for the Media view mode
// ---------------------------------------------------------------------------

/// The default media size cap, in bytes (8 MiB) — mirror of `crate::config::DEFAULT_MEDIA_MAX_KIB`
/// (8192). Far larger than the 1 MiB text-preview budget: images are naturally big, and the
/// byte-bound here only gates the disk read so a giant/hostile file is never slurped whole.
pub const DEFAULT_MEDIA_MAX_BYTES: u64 = 8192 * 1024;

/// The still preview for a media file, ready to hand to the graphics host.
///
/// Carries **raw PNG bytes**; base64 happens in `graphics.rs` at send time so the payload stays
/// bytes. Dimensions are parsed at placement time via [`crate::media::png_dimensions`], so a
/// re-encode (the PNG fast-path guard) can decide from the actual bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPayload {
    pub kind: crate::media::MediaKind,
    pub png: Vec<u8>,
    /// The source's OWN pixel size, before any resample for display or for the host's byte cap.
    ///
    /// The placement maths must not use `png`'s dimensions: those may have been shrunk purely to
    /// get under the cap, and `fit`'s never-upscale rule would then render a large photo smaller
    /// than the pane just because it had to travel small. The "is this image smaller than the
    /// box?" question is about the SOURCE, so it is answered with this.
    pub natural: (u32, u32),
}

/// Produce the Media view's content for a media file: a text line (so the pane is never blank —
/// the no-graphics degradation is automatic) plus, when a PNG was obtained, the payload.
///
/// `media_max_bytes` is the dedicated media size cap (the byte-bound on the disk read and on the
/// captured output); it is deliberately separate from the text-preview cap. A missing converter,
/// an over-cap file, or a malformed result degrades to the text line plus a notice — never a
/// crash. `Png` needs no converter: the file's own bytes are the payload (the hosting layer still
/// applies the PNG fast-path guard at placement time). `Video` decodes frame 0 only here; playback
/// is the controller's, elsewhere.
pub fn render_media(
    renderers: &Renderers,
    path: &Path,
    kind: crate::media::MediaKind,
    media_max_bytes: u64,
    media_box: Option<(u32, u32)>,
) -> (Text<'static>, Option<String>, Option<MediaPayload>) {
    match kind {
        crate::media::MediaKind::Png => {
            let bytes = read_media_bytes(path, media_max_bytes);
            match bytes.and_then(|b| crate::media::png_dimensions(&b).map(|(w, h)| (b, w, h))) {
                // The fast path is only fast when the bytes are actually sendable: a PNG over the
                // host's cap is re-encoded smaller rather than silently dropped. The text line
                // keeps reporting the file's TRUE dimensions — the downscale is a transport
                // detail, not something the user asked for.
                Some((png, w, h)) => {
                    let colour = crate::media::png_colour(&png);
                    let bytes = png.len() as u64;
                    // Resample to the size the pane will actually show BEFORE worrying about
                    // bytes. Downscaling to the display box is free visually (those pixels can
                    // never be seen) and usually lands under the cap on its own, so the picture
                    // is resampled once, with a good filter, instead of being squeezed by a
                    // byte-ratio guess that ignores how large the pane is.
                    let png = to_display_box(renderers, png, (w, h), media_box);
                    match fit_under_cap(renderers, png, (w, h)) {
                        Some(fitted) => (
                            info_line(
                                "image",
                                (w, h),
                                Some("PNG"),
                                colour.as_deref(),
                                bytes,
                                None,
                                fitted.rescaled_to,
                            ),
                            None,
                            Some(MediaPayload {
                                kind,
                                png: fitted.png,
                                natural: (w, h),
                            }),
                        ),
                        None => (
                            info_line(
                                "image",
                                (w, h),
                                Some("PNG"),
                                colour.as_deref(),
                                bytes,
                                None,
                                None,
                            ),
                            Some(OVERSIZED_NOTICE.into()),
                            None,
                        ),
                    }
                }
                None => (
                    Text::raw("[image: preview not shown]"),
                    Some("⚠ Image too large or unreadable.".into()),
                    None,
                ),
            }
        }
        crate::media::MediaKind::Image => {
            let bytes = read_media_bytes(path, media_max_bytes);
            // The conversion is also the first downscale opportunity: bounding it to a generous
            // box here means a 12-megapixel JPEG usually lands under the cap in one pass instead
            // of converting at full size and then needing a second re-encode.
            let (box_w, box_h) = media_box.unwrap_or((DEFAULT_IMAGE_BOX, DEFAULT_IMAGE_BOX));
            let convert = with_image_size(&renderers.image, box_w, box_h, "lanczos");
            let on_disk = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let source_format = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_uppercase());
            let Ok(png) =
                run_renderer_bytes(&convert, &bytes.unwrap_or_default(), renderers.timeout)
            else {
                return (
                    Text::raw("[image: preview not shown]"),
                    Some("⚠ The image converter is unavailable; see docs/renderers.md.".into()),
                    None,
                );
            };
            match crate::media::png_dimensions(&png) {
                Some((w, h)) => match fit_under_cap(renderers, png, (w, h)) {
                    Some(fitted) => (
                        info_line(
                            "image",
                            (w, h),
                            source_format.as_deref(),
                            None,
                            on_disk,
                            None,
                            fitted.rescaled_to,
                        ),
                        None,
                        Some(MediaPayload {
                            kind,
                            png: fitted.png,
                            natural: (w, h),
                        }),
                    ),
                    None => (
                        info_line(
                            "image",
                            (w, h),
                            source_format.as_deref(),
                            None,
                            on_disk,
                            None,
                            None,
                        ),
                        Some(OVERSIZED_NOTICE.into()),
                        None,
                    ),
                },
                None => (
                    Text::raw("[image: preview not shown]"),
                    Some("⚠ The image converter returned no image.".into()),
                    None,
                ),
            }
        }
        crate::media::MediaKind::Video => {
            // Frame 0 only, for the still preview. Runs the video command with a fixed start; the
            // width/height default to a conservative budget because the render worker has no pane
            // geometry yet (playback — the decoder thread — sizes to the pane at tick time).
            // The file path is substituted as its own argv element (no shell), so a hostile
            // filename cannot inject — the canonicalized in-root path from `classify`'s caller.
            let on_disk = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let probe = probe_video(renderers, path);
            let command = with_video_name(&renderers.video, &path.to_string_lossy());
            // Identical to what `MediaPlayer` will ask the decoder for, so the poster frame and
            // the first played frame are the same size — previously the still was hardcoded to
            // 640x360 while playback used the pane budget, and the video jumped on play.
            let (vw, vh) = crate::media::clamp_pixels_to_cap(
                media_box.unwrap_or((DEFAULT_VIDEO_BOX.0, DEFAULT_VIDEO_BOX.1)),
            );
            let command = crate::media::player::substitute(
                &command,
                "0",
                "8",
                &vw.to_string(),
                &vh.to_string(),
            );
            // The template streams by design, so the still preview adds a single-frame limit —
            // and it MUST be inserted before the output URL, never appended. ffmpeg applies
            // output options to the output that FOLLOWS them, so a trailing `-frames:v 1` is
            // silently inert: measured on a real .m4v, appending produced 8 MB of concatenated
            // frames (and on a longer video it simply ran until the renderer timeout, surfacing
            // to the user as "the video decoder is unavailable"), while inserting produced one
            // 84 KB frame.
            let mut command = command;
            let before_output = command.len().saturating_sub(1);
            command.splice(
                before_output..before_output,
                ["-frames:v".to_string(), "1".to_string()],
            );
            match run_renderer_bytes(&command, &[], renderers.timeout) {
                Ok(png) => match crate::media::png_dimensions(&png) {
                    // A frame from a high-resolution source can still exceed the host's cap, so
                    // it goes through the same downscale ladder as a still image rather than
                    // being dropped at send time.
                    Some((w, h)) => match fit_under_cap(renderers, png, (w, h)) {
                        Some(fitted) => (
                            // `p`, not Space: Space is already `page_down` in the registry, so
                            // the caption must name the key that actually plays. The size shown
                            // is the video's own, not the downscaled poster frame's.
                            info_line(
                                "video",
                                probe.native.unwrap_or((w, h)),
                                probe.codec.as_deref(),
                                None,
                                on_disk,
                                probe.duration_s,
                                fitted.rescaled_to,
                            ),
                            None,
                            Some(MediaPayload {
                                kind,
                                png: fitted.png,
                                // The VIDEO's own resolution, not the poster frame's. The frame is
                                // decoded small to stay under the host's byte cap, and `fit` never
                                // upscales — so using the frame's size here pinned video to a
                                // fraction of the pane while images, which report their true size,
                                // filled it.
                                natural: probe.native.unwrap_or((w, h)),
                            }),
                        ),
                        None => (
                            info_line(
                                "video",
                                probe.native.unwrap_or((w, h)),
                                probe.codec.as_deref(),
                                None,
                                on_disk,
                                probe.duration_s,
                                None,
                            ),
                            Some(OVERSIZED_NOTICE.into()),
                            None,
                        ),
                    },
                    None => (
                        Text::raw("[video: preview not shown]"),
                        Some("⚠ The video decoder returned no frame.".into()),
                        None,
                    ),
                },
                Err(_) => (
                    Text::raw("[video: preview not shown]"),
                    Some("⚠ The video decoder is unavailable; see docs/renderers.md.".into()),
                    None,
                ),
            }
        }
    }
}

/// The box a non-PNG image is converted into, per edge, when the pane size is not yet known
/// (the very first render, before a draw has measured the layout).
const DEFAULT_IMAGE_BOX: u32 = 1920;

/// The same fallback for video, as a 16:9 box.
const DEFAULT_VIDEO_BOX: (u32, u32) = (1280, 720);

/// Resample a PNG down to the size the pane will actually display, with a high-quality filter.
///
/// This is the step that answers "why does a big image look worse than a small one": a picture
/// larger than the pane must be resampled *somewhere*, and doing it here — once, to the display
/// box, with `lanczos` — beats letting the byte-cap ladder shrink it by a blind ratio with a
/// hard-edged filter. Pixels the pane cannot show are not quality, so this loses nothing visible.
///
/// Returns the input untouched when the pane size is unknown, when the image already fits the box,
/// or when the converter fails — every one of which is better served by the original bytes than by
/// no picture.
fn to_display_box(
    renderers: &Renderers,
    png: Vec<u8>,
    dimensions: (u32, u32),
    media_box: Option<(u32, u32)>,
) -> Vec<u8> {
    let Some((box_w, box_h)) = media_box else {
        return png;
    };
    if box_w == 0 || box_h == 0 || (dimensions.0 <= box_w && dimensions.1 <= box_h) {
        return png; // already no larger than the pane shows — the original IS the best version
    }
    let command = with_image_size(&renderers.image, box_w, box_h, "lanczos");
    match run_renderer_bytes(&command, &png, renderers.timeout) {
        Ok(smaller) if crate::media::png_dimensions(&smaller).is_some() => smaller,
        _ => png,
    }
}

/// What `ffprobe` told us about a video. Every field is optional: the probe is a convenience, and
/// its absence must never stop the frame from being shown.
#[derive(Default)]
struct VideoProbe {
    codec: Option<String>,
    duration_s: Option<f64>,
    /// The video's OWN resolution. Reported in the caption in place of the decoded preview's
    /// size, so a video states its real dimensions exactly as an image does.
    native: Option<(u32, u32)>,
}

/// Ask the `probe` command for a video's codec and duration.
///
/// Best-effort by construction — a missing ffprobe, a malformed answer, or a container it cannot
/// read all yield an empty [`VideoProbe`] and simply omit those fields from the info line. The
/// path is passed as its own argv element via `{name}` (no shell), the same narrowing of the
/// stdin trust boundary the `video` command already documents.
fn probe_video(renderers: &Renderers, path: &Path) -> VideoProbe {
    if renderers.probe.is_empty() {
        return VideoProbe::default();
    }
    let command = with_video_name(&renderers.probe, &path.to_string_lossy());
    let Ok(out) = run_renderer_bytes(&command, &[], renderers.timeout) else {
        return VideoProbe::default();
    };
    let text = String::from_utf8_lossy(&out);
    let field = |name: &str| {
        text.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == name)
            .map(|(_, v)| v.trim().to_string())
    };
    let width = field("width").and_then(|v| v.parse::<u32>().ok());
    let height = field("height").and_then(|v| v.parse::<u32>().ok());
    VideoProbe {
        codec: field("codec_name").map(|c| c.to_ascii_uppercase()),
        duration_s: field("duration").and_then(|v| v.parse::<f64>().ok()),
        native: width.zip(height).filter(|&(w, h)| w > 0 && h > 0),
    }
}

/// The caption above a media file: what it is, at a glance.
///
/// Reads as `[image: 3008×1546 · PNG · 8-bit RGBA · 655 KiB · shown at 2259×1161]`. The trailing
/// clause appears only when the host's byte cap forced a re-encode, which is the answer to "why
/// does this large file look softer than that small one" — without it the degradation is invisible
/// and looks like a bug.
#[allow(clippy::too_many_arguments)]
fn info_line(
    label: &str,
    dimensions: (u32, u32),
    format: Option<&str>,
    colour: Option<&str>,
    on_disk: u64,
    duration_s: Option<f64>,
    rescaled_to: Option<(u32, u32)>,
) -> Text<'static> {
    let (w, h) = dimensions;
    let mut parts = vec![format!("{w}×{h}")];
    if let Some(d) = duration_s {
        parts.push(crate::media::human_duration(d));
    }
    if let Some(f) = format {
        parts.push(f.to_string());
    }
    if let Some(c) = colour {
        parts.push(c.to_string());
    }
    if on_disk > 0 {
        parts.push(crate::media::human_size(on_disk));
    }
    // Only worth saying when the size actually changed: a converter that returned the same
    // dimensions (or a re-encode that only shrank bytes) would otherwise print a confusing
    // "shown at" clause identical to the size right before it.
    if let Some((rw, rh)) = rescaled_to.filter(|&r| r != dimensions) {
        parts.push(format!("shown at {rw}×{rh}"));
    }
    if label == "video" {
        parts.push("p to play".to_string());
    }
    Text::raw(format!("[{label}: {}]", parts.join(" · ")))
}

/// Shown when an image cannot be squeezed under the host's cap — the pane still shows the text
/// line, so this explains why no picture accompanies it.
const OVERSIZED_NOTICE: &str = "⚠ Image too large for the terminal to display; install ffmpeg so it can be scaled down. \
     See docs/renderers.md.";

/// Substitute `{width}` / `{height}` / `{scaler}` in an image-converter command.
///
/// Mirrors [`crate::media::player::substitute`] for the video template. Applied on **every**
/// invocation, so the placeholders are never passed through to ffmpeg literally.
pub(crate) fn with_image_size(
    command: &[String],
    width: u32,
    height: u32,
    scaler: &str,
) -> Vec<String> {
    let (w, h) = (width.to_string(), height.to_string());
    command
        .iter()
        .map(|arg| {
            arg.replace("{width}", &w)
                .replace("{height}", &h)
                .replace("{scaler}", scaler)
        })
        .collect()
}

/// The re-encode ladder, best quality first: `(scaler, size multiplier)`.
///
/// The goal is the **best-looking image that herdr will accept**, not merely one that fits. So we
/// start with the highest-fidelity resampler at the largest size the byte estimate allows and only
/// trade quality away when the host's cap forces it.
///
/// Why the second rung changes filter rather than size: measured on this repo's 3008x1546
/// screenshots, `lanczos` at the byte-target produced 794 KiB and `neighbor` at the *same* size
/// produced 350 KiB. For screenshots, diagrams, and UI captures, dropping the smoothing filter
/// costs nothing visually — it keeps text crisp — and buys more than halving the size, which is
/// far better than keeping a smooth filter and shrinking the picture. Photographs are the inverse
/// case, and they simply take the first rung when they fit.
const QUALITY_LADDER: &[(&str, f64)] = &[("lanczos", 1.0), ("neighbor", 1.0), ("neighbor", 0.7)];

/// The outcome of squeezing an image under the host's byte cap.
pub struct Fitted {
    pub png: Vec<u8>,
    /// `Some(dimensions)` when the bytes had to be re-encoded smaller, so the caller can say so in
    /// the info line rather than letting the user wonder why a 3008px file looks softer than a
    /// 900px one. `None` means the original bytes were sent untouched.
    pub rescaled_to: Option<(u32, u32)>,
}

/// Produce the best-quality version of `png` that herdr will accept.
///
/// Returns the original bytes untouched when they already fit — the zero-subprocess fast path, and
/// the only path that is bit-for-bit pristine. Otherwise it walks [`QUALITY_LADDER`], stopping at
/// the first rung that lands under the cap, so the picture is only degraded as far as the host's
/// 512 KiB limit actually forces.
///
/// `None` means the converter is missing, produced garbage, or no rung fit; the caller then shows
/// the caption plus [`OVERSIZED_NOTICE`] rather than sending a payload herdr would reject with
/// `image_too_large`.
fn fit_under_cap(renderers: &Renderers, png: Vec<u8>, dimensions: (u32, u32)) -> Option<Fitted> {
    if png.len() <= crate::graphics::MAX_IMAGE_BYTES {
        return Some(Fitted {
            png,
            rescaled_to: None,
        });
    }
    let (base_w, base_h) =
        crate::media::downscale_target(dimensions, png.len(), crate::graphics::MAX_IMAGE_BYTES);
    for (scaler, factor) in QUALITY_LADDER {
        let w = ((base_w as f64 * factor).round() as u32).max(1);
        let h = ((base_h as f64 * factor).round() as u32).max(1);
        let command = with_image_size(&renderers.image, w, h, scaler);
        // A failed rung is not fatal on its own — but a *missing converter* fails identically on
        // every rung, so bailing out here avoids three pointless spawns.
        let Ok(candidate) = run_renderer_bytes(&command, &png, renderers.timeout) else {
            return None;
        };
        let dims = crate::media::png_dimensions(&candidate)?;
        if candidate.len() <= crate::graphics::MAX_IMAGE_BYTES {
            return Some(Fitted {
                png: candidate,
                rescaled_to: Some(dims),
            });
        }
    }
    None
}

/// Read a media file's bytes, bounded by the media size cap (`None` if missing, unreadable, or
/// over the cap — the pane then shows the placeholder text rather than vomiting bytes).
fn read_media_bytes(path: &Path, media_max_bytes: u64) -> Option<Vec<u8>> {
    let m = std::fs::metadata(path).ok()?;
    if m.len() > media_max_bytes {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut buf = Vec::with_capacity(m.len() as usize);
    file.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Ingest (possibly untrusted) content into ratatui `Text`. Cursor-movement and
/// screen-control escape sequences are stripped regardless of source; only SGR styling is
/// kept and mapped into spans by `ansi-to-tui` (AC-27). The result can only ever paint the
/// viewer's own region — it carries no terminal-control operations.
pub fn to_text(raw: &str) -> Text<'static> {
    // The shared scanner keeps SGR only for this styled content path; status titles use the same
    // scanner through `neutralize_plain_text`, where SGR is dropped with every other control.
    let cleaned = neutralize_terminal_control(raw, ControlMode::Styled);
    cleaned.clone().into_text().unwrap_or_else(|_| {
        // If ANSI parsing fails, remove retained SGR while preserving the content's line structure.
        plain_text_with_line_breaks(&cleaned)
    })
}

/// Return one safe visible plain-text line from hostile input by dropping every terminal control,
/// including SGR styling, C0/C1 bytes, and cursor/screen-control escape sequences. Unicode text is
/// otherwise retained verbatim. Used for remote status titles; content rendering uses the same
/// scanner with SGR retained for ratatui styling.
pub fn neutralize_plain_text(raw: &str) -> String {
    neutralize_terminal_control(raw, ControlMode::OneLine)
}

fn plain_text_with_line_breaks(raw: &str) -> Text<'static> {
    Text::raw(neutralize_terminal_control(raw, ControlMode::Plain))
}

#[derive(Clone, Copy)]
enum ControlMode {
    /// Keep SGR for `ansi-to-tui` and preserve content line structure.
    Styled,
    /// Drop all controls including SGR, preserving content line structure.
    Plain,
    /// Drop all controls and line separators for status titles.
    OneLine,
}

/// Remove terminal-control escape sequences according to the destination's presentation needs.
/// Operates on bytes (control sequences are ASCII) and preserves all other UTF-8 content verbatim.
fn neutralize_terminal_control(raw: &str, mode: ControlMode) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            match bytes.get(i + 1) {
                Some(b'[') => {
                    // CSI: params/intermediates until a final byte (0x40..=0x7e).
                    let start = i;
                    let mut j = i + 2;
                    while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                        j += 1;
                    }
                    if matches!(mode, ControlMode::Styled) && j < bytes.len() && bytes[j] == b'm' {
                        out.extend_from_slice(&bytes[start..=j]); // keep SGR styling for `to_text`
                    }
                    // else: drop the whole control sequence (cursor move, erase, …)
                    i = if j < bytes.len() { j + 1 } else { j };
                }
                Some(b']') => {
                    // OSC: drop through BEL or ST (ESC \).
                    let mut j = i + 2;
                    while j < bytes.len() {
                        if bytes[j] == 0x07 {
                            j += 1;
                            break;
                        }
                        if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                            j += 2;
                            break;
                        }
                        j += 1;
                    }
                    i = j;
                }
                Some(_) => i += 2, // ESC + single (e.g. ESC c reset) → drop both
                None => i += 1,    // lone trailing ESC → drop
            }
        } else if bytes[i] == 0xc2 && matches!(bytes.get(i + 1), Some(0x80..=0x9f)) {
            // A C1 control codepoint (U+0080–U+009F, e.g. U+009B = CSI) encoded in UTF-8;
            // some terminals act on these, so drop the whole 2-byte sequence.
            i += 2;
        } else {
            // Drop C0 controls and DEL, which can ring the bell, backspace, or overwrite/spoof
            // a line. Content modes keep newline/tab; a status title keeps neither.
            let b = bytes[i];
            let preserves_line_structure = !matches!(mode, ControlMode::OneLine);
            let is_c0_control =
                b < 0x20 && (!preserves_line_structure || (b != b'\n' && b != b'\t'));
            if !is_c0_control && b != 0x7f {
                out.push(b);
            }
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);
    const RENDERER_FIXTURE_NAME: &str = "render::tests::renderer_fixture";
    const RENDERER_FIXTURE_ARG: &str = "--hfv-render-fixture=";

    fn renderer_fixture_command(mode: &str) -> Vec<String> {
        vec![
            std::env::current_exe()
                .expect("test binary path")
                .display()
                .to_string(),
            "--exact".to_string(),
            RENDERER_FIXTURE_NAME.to_string(),
            "--".to_string(),
            format!("{RENDERER_FIXTURE_ARG}{mode}"),
        ]
    }

    #[test]
    fn renderer_fixture() {
        let mode = std::env::args().find_map(|argument| {
            argument
                .strip_prefix(RENDERER_FIXTURE_ARG)
                .map(str::to_owned)
        });
        if mode.as_deref() == Some("stall") {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    fn tmp(name: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "hfv-render-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn nul_bytes_classify_as_binary_without_emitting_raw_bytes() {
        let p = tmp("bin", &[0x00, 0x01, 0x02, b'h', b'i']);
        assert_eq!(
            classify(&std::env::temp_dir(), &p, Caps::default()),
            Prepared::Binary
        ); // AC-12
        fs::remove_file(&p).ok();
    }

    #[test]
    fn raw_diff_is_bounded_and_neutralized_without_a_renderer_process() {
        let (text, notice) = render_raw_diff(Some("- old\n+ new\n"), Caps::default());
        assert_eq!(notice, None);
        assert_eq!(text.lines.len(), 2);
        assert_eq!(text.lines[0].spans[0].content.as_ref(), "- old");
        assert_eq!(text.lines[1].spans[0].content.as_ref(), "+ new");
    }

    #[test]
    fn with_wrap_width_replaces_the_w_value_without_disturbing_the_rest() {
        // The default markdown command: glow with `-w 0` (no wrap). The help overlay rewrites the
        // 0 to the box body width so glow wraps with its own hanging indents.
        let base: Vec<String> = ["glow", "-s", "dark", "-w", "0", "-"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = with_wrap_width(&base, 70);
        assert_eq!(got, ["glow", "-s", "dark", "-w", "70", "-"]);
        // The wrap value is non-zero (the whole point — `-w 0` disables wrapping → flat re-wrap).
        let i = got.iter().position(|a| a == "-w").expect("-w present");
        assert_ne!(got[i + 1], "0", "the help render must use a non-zero -w");
    }

    #[test]
    fn with_wrap_width_inserts_the_flag_when_absent() {
        let base: Vec<String> = ["glow", "-"].iter().map(|s| s.to_string()).collect();
        let got = with_wrap_width(&base, 70);
        let i = got.iter().position(|a| a == "-w").expect("-w inserted");
        assert_eq!(got[i + 1], "70");
        // The trailing stdin positional is preserved at the end.
        assert_eq!(got.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn with_wrap_width_appends_the_value_when_w_is_the_trailing_arg() {
        // `-w` present but with no value after it (the `out.get_mut(i + 1)` is `None` branch): the
        // width is appended rather than replacing a following token.
        let base: Vec<String> = ["glow", "-w"].iter().map(|s| s.to_string()).collect();
        let got = with_wrap_width(&base, 70);
        assert_eq!(got, ["glow", "-w", "70"]);
    }

    #[test]
    fn small_text_file_is_returned_in_full() {
        let p = tmp("small.txt", b"hello\nworld\n");
        match classify(&std::env::temp_dir(), &p, Caps::default()) {
            Prepared::Full { text } => assert!(text.contains("hello")),
            other => panic!("expected Full, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn file_over_one_megabyte_is_truncated_with_a_notice() {
        let caps = Caps::default();
        let big = vec![b'a'; (caps.max_bytes as usize) + 100];
        let p = tmp("big.txt", &big);
        match classify(&std::env::temp_dir(), &p, caps) {
            Prepared::Truncated { text, notice } => {
                assert!(!notice.is_empty(), "AC-13: a visible truncation notice");
                assert!(
                    text.len() as u64 <= caps.max_bytes,
                    "AC-13: preview is bounded"
                );
                // Exercises human_bytes' MB branch on the default 1 MiB cap (the common path).
                assert!(
                    notice.contains("1 MB"),
                    "notice names the default 1 MB size cap: {notice}"
                );
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn file_over_the_default_line_cap_is_truncated() {
        let caps = Caps::default();
        let many = "x\n".repeat(caps.max_lines + 1000);
        let p = tmp("many.txt", many.as_bytes());
        match classify(&std::env::temp_dir(), &p, caps) {
            Prepared::Truncated { text, notice } => {
                assert!(
                    text.lines().count() <= caps.max_lines,
                    "AC-13: preview line-bounded"
                );
                assert!(notice.contains("line"), "notice describes the line cap");
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn a_configured_smaller_line_cap_truncates_a_file_the_default_would_show_whole() {
        // 200 lines is well under the default line cap (would be `Full`), but a caller-supplied
        // 100-line cap must truncate it to a bounded preview — proving the cap is injected, not fixed.
        let text = "line\n".repeat(200);
        let p = tmp("cfg-lines.txt", text.as_bytes());
        let caps = Caps {
            max_lines: 100,
            max_bytes: DEFAULT_MAX_BYTES,
        };
        match classify(&std::env::temp_dir(), &p, caps) {
            Prepared::Truncated { text, notice } => {
                assert!(
                    text.lines().count() <= 100,
                    "preview honors the configured line cap"
                );
                assert!(
                    notice.contains("100-line"),
                    "notice names the configured cap: {notice}"
                );
            }
            other => panic!("expected Truncated at a 100-line cap, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn a_configured_smaller_byte_cap_truncates_and_names_the_size_in_the_notice() {
        // 200 KiB is under the 1 MiB default, but a 64 KiB cap must truncate it and label the size.
        let text = vec![b'a'; 200 * 1024];
        let p = tmp("cfg-bytes.txt", &text);
        let caps = Caps {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: 64 * 1024,
        };
        match classify(&std::env::temp_dir(), &p, caps) {
            Prepared::Truncated { text, notice } => {
                assert!(
                    text.len() as u64 <= caps.max_bytes,
                    "preview honors the configured byte cap"
                );
                assert!(
                    notice.contains("64 KB"),
                    "notice names the configured size: {notice}"
                );
            }
            other => panic!("expected Truncated at a 64 KiB cap, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn classify_byte_bounds_a_lossy_expanded_single_line_preview() {
        // A hostile over-cap file that is ONE long line of INVALID UTF-8: the line cap never trips
        // (1 line), and from_utf8_lossy expands each 0xFF into a 3-byte U+FFFD — so a line-bounded-only
        // preview would balloon past the cap. The byte-bound must hold the shown preview at <= the cap.
        let cap = 64 * 1024u64;
        let raw = vec![0xFFu8; (cap as usize) + 4096]; // over the cap, no NUL, no newline
        let p = tmp("lossy.bin", &raw);
        let caps = Caps {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: cap,
        };
        match classify(&std::env::temp_dir(), &p, caps) {
            Prepared::Truncated { text, .. } => {
                assert!(
                    text.len() as u64 <= cap,
                    "lossy-expanded preview must still honor the byte cap: {} > {cap}",
                    text.len()
                );
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn truncate_to_bytes_respects_char_boundaries_and_the_cap() {
        // Never split a multi-byte char, always land <= cap, and be a no-op under the cap.
        let mut s = "aé…z".to_string(); // 'a'(1) 'é'(2) '…'(3) 'z'(1) = 7 bytes
        truncate_to_bytes(&mut s, 4); // cap lands mid-'…' (bytes 3..6) → must cut back to "aé"
        assert_eq!(s, "aé");
        let mut whole = "hello".to_string();
        truncate_to_bytes(&mut whole, 100); // over-cap: unchanged
        assert_eq!(whole, "hello");
        let mut empty = "hello".to_string();
        truncate_to_bytes(&mut empty, 0); // zero cap: empty, no panic
        assert_eq!(empty, "");
    }

    #[test]
    fn cap_preview_byte_bounds_a_long_line_diff_under_the_line_cap() {
        // A diff of few lines but many BYTES trips cap_preview's byte cap (not its line cap): the
        // returned preview must be byte-bounded and carry a notice.
        let caps = Caps {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: 8 * 1024,
        };
        let long_line = "+".to_string() + &"x".repeat(32 * 1024); // one line, > 8 KiB
        let (preview, notice) = cap_preview(&long_line, caps);
        assert!(
            preview.len() as u64 <= caps.max_bytes,
            "cap_preview must byte-bound a long-line diff: {}",
            preview.len()
        );
        assert!(
            notice.unwrap().to_lowercase().contains("truncated"),
            "a byte-over diff gets a truncation notice"
        );
    }

    #[test]
    fn classify_does_not_modify_the_file() {
        let p = tmp("ro.txt", b"unchanged\n");
        let before = fs::read(&p).unwrap();
        let _ = classify(&std::env::temp_dir(), &p, Caps::default());
        assert_eq!(fs::read(&p).unwrap(), before); // AC-N1
        fs::remove_file(&p).ok();
    }

    fn unique_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hfv-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    // Creating a symlink reliably without elevated privilege is a unix assumption (Windows
    // symlink creation needs Developer Mode or admin rights, not guaranteed on a CI runner);
    // the escape-via-symlink guard these exercise is platform-agnostic path canonicalization.
    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_whose_target_escapes_the_root() {
        use std::os::unix::fs::symlink;
        let root = unique_dir("root");
        let outside = tmp("secret", b"TOPSECRET"); // lives in temp_dir, outside `root`
        let link = root.join("link.txt");
        symlink(&outside, &link).unwrap();
        assert_eq!(
            classify(&root, &link, Caps::default()),
            Prepared::Binary,
            "AC-N5: no out-of-root read"
        );
        fs::remove_dir_all(&root).ok();
        fs::remove_file(&outside).ok();
    }

    #[cfg(unix)]
    #[test]
    fn follows_a_symlink_that_stays_within_the_root() {
        use std::os::unix::fs::symlink;
        let root = unique_dir("root");
        let real = root.join("real.txt");
        fs::write(&real, "hello inside").unwrap();
        let link = root.join("link.txt");
        symlink(&real, &link).unwrap();
        match classify(&root, &link, Caps::default()) {
            Prepared::Full { text } => assert!(text.contains("hello inside")),
            other => panic!("expected Full, got {other:?}"),
        }
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_a_non_regular_file() {
        let root = unique_dir("root");
        // a directory is not a regular file
        let sub = root.join("subdir");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(classify(&root, &sub, Caps::default()), Prepared::Binary);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn renderer_command_forces_color_so_piped_renderers_keep_styling() {
        // glow/glamour drops to a no-color profile when stdout is a pipe (always, here), so
        // every renderer subprocess is spawned with CLICOLOR_FORCE=1 — without it markdown
        // loses all color (headings, inline code, code-block highlighting). Harmless to
        // delta/bat, which force color via flags already.
        use std::ffi::OsStr;
        let cmd = renderer_command(&["glow".into(), "-".into()]).unwrap();
        let forced = cmd
            .get_envs()
            .any(|(k, v)| k == OsStr::new("CLICOLOR_FORCE") && v == Some(OsStr::new("1")));
        assert!(
            forced,
            "CLICOLOR_FORCE=1 must be set on the renderer subprocess"
        );
    }

    #[test]
    fn renderer_never_spawns_after_an_expired_deadline() {
        // The nonexistent program would return NotFound if a spawn were attempted; Timeout
        // proves the expired deadline is checked before any process is created.
        let result = run_renderer_until(
            &["renderer-must-not-run".to_string()],
            "input",
            Instant::now() - Duration::from_millis(1),
        );

        assert!(
            matches!(result, Err(RendererError::Timeout)),
            "an already-expired deadline must fail before spawning"
        );
    }

    #[test]
    fn general_renderer_timeout_has_one_bounded_reap_tail() {
        let started = Instant::now();
        let result = run_renderer(
            &renderer_fixture_command("stall"),
            "input",
            Duration::from_millis(100),
        );

        assert!(matches!(result, Err(RendererError::Timeout)));
        // The claim is BOUNDED vs UNBOUNDED, and the fixture stalls for 60s. 2s is 20x the
        // requested timeout — ample for a loaded runner (the flakes this replaced were 314-340ms)
        // while still rejecting a multi-second reap tail, which a 5s bound let through. It is
        // deliberately NOT `timeout + small slack`: that shape (100ms asserted under 250ms) flaked
        // on a loaded macOS runner and blocked an unrelated PR. The timeout's exact value is the
        // caller's argument above, not a stopwatch's job. See AGENTS.md.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the full capture window plus bounded reap tail must not become an unbounded wait: {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_renderer_bounds_total_wall_clock_to_a_single_timeout_on_slow_exit() {
        // R3 item 1 / AC-22: `run_renderer` must enforce a SINGLE combined wall-clock deadline,
        // not apply `timeout` twice (once waiting for stdout, again waiting for exit). This
        // exercises the Ok→wait_until slow-exit path: `cat` echoes stdin then closes stdout
        // (fast EOF → the recv_timeout(stdout) phase returns promptly), but the shell then sleeps
        // 2s before exiting — so the exit-wait is what would burn a second full `timeout` under
        // the old code. The combined deadline caps the TOTAL at roughly one `timeout`.
        // A generous 1s timeout so the (roughly fixed, ~100ms) process-spawn/scheduling overhead on a
        // loaded CI runner is a SMALL fraction of it — a tight bound on a small timeout flaked here
        // (a 200ms timeout + ~120ms overhead blew a 1.4× bound on a busy macOS runner).
        let timeout = Duration::from_millis(1000);
        // Two phases, each timed to expose the double-bound: the renderer holds stdout open for
        // ~0.8× the timeout (so the `recv_timeout(stdout)` phase nearly burns a full timeout, but
        // still returns Ok), THEN lingers ~2s before exiting (so the Ok-path exit-wait would burn
        // a SECOND full timeout under the bug). `exec 1>&-` closes stdout precisely at the phase
        // boundary so the reader thread sees EOF and `recv_timeout` returns Ok → the slow-exit
        // `wait_until` path. Under the 2× bug: ~0.8×+1.0× ≈ 1.8×. Under the single combined
        // deadline: ~0.8× + remaining(~0.2×) ≈ 1.0×.
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "cat >/dev/null; sleep 0.8; exec 1>&-; sleep 3".to_string(),
        ];
        let start = std::time::Instant::now();
        let _ = run_renderer(&cmd, "hello", timeout);
        let elapsed = start.elapsed();
        // The bug applies `timeout` twice → ~2×. A single combined deadline keeps it ~1×; allow
        // slack for the 10ms poll + scheduling, but well under the ~1.8× the bug produces here.
        // Single combined deadline → total ≈ 1× the timeout (+overhead). The 2× bug here ≈ 1.8×
        // (0.8× recv + a fresh 1.0× exit-wait). Assert < 1.5×: comfortably above 1×+CI-overhead
        // (~380ms headroom), comfortably below the bug's ~1.8× (~300ms margin).
        assert!(
            elapsed < timeout.mul_f32(1.5),
            "run_renderer must bound TOTAL wall-clock to a single timeout (~{timeout:?}); \
             took {elapsed:?} (the 2× bug would take ~{:?})",
            timeout.mul_f32(1.8)
        );
    }

    #[test]
    fn ansi_parse_fallback_preserves_content_line_structure() {
        let text = plain_text_with_line_breaks("\x1b[31mfirst\nsecond\tcolumn");
        assert_eq!(text.lines.len(), 2, "fallback preserves line breaks");
        assert_eq!(text.lines[0].spans[0].content, "first");
        assert_eq!(text.lines[1].spans[0].content, "second\tcolumn");
    }

    #[test]
    fn named_ansi_color_survives_to_text_as_a_named_color() {
        // The markdown palette feature relies on glow's named ANSI colors (e.g. `\e[34m`)
        // surviving `to_text` as ratatui *named* colors, so the terminal/herdr theme re-themes
        // them — rather than being flattened to fixed RGB.
        use ratatui::style::Color;
        let t = to_text("\u{1b}[34mhi\u{1b}[0m");
        let fg = t
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find_map(|s| s.style.fg);
        assert_eq!(
            fg,
            Some(Color::Blue),
            "SGR 34 must map to the named Blue, not RGB"
        );
    }

    // -- media --------------------------------------------------------------

    /// A minimal PNG header (the 24-byte IHDR prefix) — enough for `png_dimensions` / the
    /// fast-path payload without a full encoder.
    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b
    }

    #[test]
    fn media_png_uses_the_files_own_bytes_as_the_payload() {
        let p = tmp("media.png", &png_bytes(64, 48));
        let renderers = Renderers {
            image: vec!["herdr-no-such-converter".into()], // must not be reached for PNG
            ..cat_like()
        };
        let (text, notice, media) = render_media(
            &renderers,
            &p,
            crate::media::MediaKind::Png,
            DEFAULT_MEDIA_MAX_BYTES,
            None,
        );
        assert_eq!(notice, None);
        let line: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        // The caption is now an info line: dimensions, format, and on-disk size.
        assert_eq!(line, "[image: 64×48 · PNG · 24 B]");
        let payload = media.expect("a PNG payload");
        assert_eq!(payload.kind, crate::media::MediaKind::Png);
        assert_eq!(
            payload.png,
            png_bytes(64, 48),
            "native bytes, no conversion"
        );
        fs::remove_file(&p).ok();
    }

    /// An over-cap PNG padded past herdr's 512 KiB limit. Sized from the real regression: the
    /// repo's own `assets/File-Viewer-FS.png` is 655 KiB and rendered nothing at all.
    fn oversized_png() -> Vec<u8> {
        let mut b = png_bytes(3008, 1546);
        b.resize(crate::graphics::MAX_IMAGE_BYTES + 1024, 0u8);
        b
    }

    #[cfg(unix)]
    #[test]
    fn an_oversized_png_is_downscaled_instead_of_silently_dropped() {
        // THE REGRESSION: an over-cap PNG used to be skipped at send time with no picture, no
        // notice, and `media_shown` still claiming it was displayed — which is exactly why
        // Markdown-view.png (501 KiB) rendered while File-viewer.png (549 KiB) never did.
        // `head -c` stands in for ffmpeg: it consumes stdin and emits a strictly smaller stream
        // whose PNG header (and therefore parsed dimensions) survives intact.
        let p = tmp("oversized.png", &oversized_png());
        let renderers = Renderers {
            image: vec!["sh".into(), "-c".into(), "head -c 1000".into()],
            ..cat_like()
        };
        let (text, notice, media) = render_media(
            &renderers,
            &p,
            crate::media::MediaKind::Png,
            DEFAULT_MEDIA_MAX_BYTES,
            None,
        );

        assert_eq!(
            notice, None,
            "a successful downscale is not a problem to report"
        );
        let payload = media.expect("an over-cap PNG must still produce a payload");
        assert!(
            payload.png.len() <= crate::graphics::MAX_IMAGE_BYTES,
            "the payload must fit the host's cap, got {} bytes",
            payload.png.len()
        );
        let line: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            line.starts_with("[image: 3008×1546 ·"),
            "the caption reports the file's TRUE size; the downscale is a transport detail: {line}"
        );
        fs::remove_file(&p).ok();
    }

    #[test]
    fn an_oversized_png_without_a_converter_says_so_rather_than_vanishing() {
        // Degradation, not silence: with no ffmpeg there is nothing to downscale with, so the
        // user gets the text line AND an explanation pointing at the fix.
        let p = tmp("oversized-noconv.png", &oversized_png());
        let renderers = Renderers {
            image: vec!["herdr-no-such-converter".into()],
            ..cat_like()
        };
        let (_, notice, media) = render_media(
            &renderers,
            &p,
            crate::media::MediaKind::Png,
            DEFAULT_MEDIA_MAX_BYTES,
            None,
        );
        assert!(media.is_none(), "nothing sendable was produced");
        assert_eq!(notice.as_deref(), Some(OVERSIZED_NOTICE));
        fs::remove_file(&p).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_converter_that_ignores_the_size_request_gives_up_instead_of_looping() {
        // `cat` echoes its input unchanged, so it never gets under the cap. Without the
        // "did it actually shrink?" guard this would burn the whole attempt budget on identical
        // subprocess calls; with it, the first pass concludes the converter is useless.
        let p = tmp("oversized-noop.png", &oversized_png());
        let renderers = Renderers {
            image: vec!["cat".into()],
            ..cat_like()
        };
        let (_, notice, media) = render_media(
            &renderers,
            &p,
            crate::media::MediaKind::Png,
            DEFAULT_MEDIA_MAX_BYTES,
            None,
        );
        assert!(media.is_none());
        assert_eq!(notice.as_deref(), Some(OVERSIZED_NOTICE));
        fs::remove_file(&p).ok();
    }

    #[cfg(unix)]
    #[test]
    fn the_video_still_limits_frames_before_the_output_not_after_it() {
        // THE REGRESSION: `-frames:v 1` appended AFTER the output URL is inert — ffmpeg applies
        // output options to the output that follows them. That made every still preview decode
        // the WHOLE video (8 MB of frames on a short clip; a timeout reported as "the video
        // decoder is unavailable" on a long one). The stub echoes its own argv so the ordering
        // is asserted directly, without needing ffmpeg.
        let renderers = Renderers {
            video: vec![
                "sh".into(),
                "-c".into(),
                // Emit the argv we were handed, so the test sees the real command shape.
                "printf '%s\\n' \"$@\" >&2; printf ''".into(),
                "argv0".into(),
                "-i".into(),
                "{name}".into(),
                "-f".into(),
                "image2pipe".into(),
                "-".into(),
            ],
            ..cat_like()
        };
        let command = with_video_name(&renderers.video, "/tmp/clip.mp4");
        let command = crate::media::player::substitute(&command, "0", "8", "640", "360");
        let mut command = command;
        let before_output = command.len().saturating_sub(1);
        command.splice(
            before_output..before_output,
            ["-frames:v".to_string(), "1".to_string()],
        );

        let frames_at = command
            .iter()
            .position(|a| a == "-frames:v")
            .expect("present");
        let output_at = command.len() - 1;
        assert_eq!(command[output_at], "-", "the output URL stays last");
        assert!(
            frames_at < output_at,
            "the single-frame limit must precede the output URL, else ffmpeg ignores it: {command:?}"
        );
        assert_eq!(command[frames_at + 1], "1");
    }

    #[test]
    fn image_size_substitution_leaves_no_placeholder_behind() {
        // The default converter carries `{width}`/`{height}`; if substitution were ever skipped,
        // ffmpeg would receive the literal braces and fail on every image.
        let command: Vec<String> = ["ffmpeg", "-vf", "scale={width}:{height}:x", "pipe:1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = with_image_size(&command, 640, 480, "lanczos");
        assert_eq!(got[2], "scale=640:480:x");
        assert_eq!(got[1], "-vf");
        assert!(
            !got.iter().any(|a| a.contains('{')),
            "no placeholder may survive: {got:?}"
        );
    }

    #[test]
    fn media_image_routes_through_the_configured_converter() {
        // The converter is fed the raw bytes on stdin and its stdout is returned raw. A
        // `cat` converter round-trips them, so a JPEG's bytes are not validated as PNG all the
        // way down — the semantic is "capture the converter's stdout as the payload".
        let p = tmp("media.jpg", b"\xff\xd8ff fake jpeg bytes");
        let renderers = Renderers {
            image: vec!["cat".into()],
            ..cat_like()
        };
        let (text, _notice, media) = render_media(
            &renderers,
            &p,
            crate::media::MediaKind::Image,
            DEFAULT_MEDIA_MAX_BYTES,
            None,
        );
        let line: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            line, "[image: preview not shown]",
            "non-PNG converter output degrades"
        );
        assert_eq!(media, None);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn media_image_converted_to_png_carries_the_png_bytes() {
        // The `image` renderer fixture emits a well-formed 24-byte PNG header to stdout in
        // response to any stdin, so its output is a parseable PNG payload.
        let esc = escape_octal(&png_bytes(100, 60));
        #[cfg(unix)]
        let renderers = Renderers {
            image: vec![
                "sh".into(),
                "-c".into(),
                format!("cat >/dev/null; printf '{esc}'"),
            ],
            ..cat_like()
        };
        #[cfg(not(unix))]
        let renderers = cat_like();
        #[cfg(unix)]
        {
            let p = tmp("media-image-png", b"fake jpeg bytes");
            let (_, _notice, media) = render_media(
                &renderers,
                &p,
                crate::media::MediaKind::Image,
                DEFAULT_MEDIA_MAX_BYTES,
                None,
            );
            let payload = media.expect("a PNG payload from a valid converter output");
            assert_eq!(payload.png, png_bytes(100, 60));
            fs::remove_file(&p).ok();
        }
    }

    /// `\NNN` octal escapes for `sh` `printf` (PNG magic includes bytes `printf` would otherwise
    /// interpret, e.g. `\r\n` — escaping by hand avoids a second interpreter layer).
    fn escape_octal(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("\\{:03o}", b))
            .collect::<String>()
    }

    #[test]
    fn media_fallback_degrades_cleanly_when_over_the_cap_or_unreadable() {
        // Over the media cap → placeholder text + notice, no bytes read.
        let p = tmp("big.png", &png_bytes(64, 48));
        let renderers = cat_like();
        let (_, notice, media) =
            render_media(&renderers, &p, crate::media::MediaKind::Png, 1, None);
        assert!(notice.is_some(), "over-cap PNG must produce a notice");
        assert_eq!(media, None);
        // Missing file → same graceful placeholder.
        let (text, notice, media) = render_media(
            &renderers,
            Path::new("/nonexistent/hfv-media.jpg"),
            crate::media::MediaKind::Image,
            DEFAULT_MEDIA_MAX_BYTES,
            None,
        );
        let line: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(!line.is_empty(), "the pane is never blank");
        assert!(notice.is_some());
        assert_eq!(media, None);
        fs::remove_file(&p).ok();
    }

    fn cat_like() -> Renderers {
        Renderers {
            markdown: vec!["cat".into()],
            diff: vec!["cat".into()],
            full_diff: vec!["cat".into()],
            syntax: vec!["cat".into()],
            image: vec!["cat".into()],
            video: vec!["cat".into()],
            probe: Vec::new(),
            timeout: Duration::from_secs(5),
        }
    }
}
