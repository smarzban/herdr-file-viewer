//! Content Renderer — produce the content-pane text for a file, with safety guards.
//!
//! The primary trust boundary: all file bytes are untrusted. This module bounds size
//! (AC-13), refuses to emit raw bytes for binary files (AC-12), neutralizes control/escape
//! sequences (AC-27), and delegates styling to external CLIs with a plain-text fallback
//! (AC-24/25). Reads only, never writes (AC-N1).

use crate::document::{Converter, DocKind};
use crate::view_policy::ViewMode;
use ansi_to_tui::IntoText;
use ratatui::text::Text;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    /// The per-kind converters for binary documents (docx/odt/pdf/pptx/xlsx). Held here (not
    /// hard-coded in [`render_document`]) so tests inject stubs, the same hermeticity rule the
    /// stdin renderers above follow. Each is optional at runtime: a missing converter tool
    /// degrades to a notice, never a crash.
    pub documents: crate::document::DocConverters,
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
        // Documents don't flow through `classify`/`Prepared` (they're binary); they render via
        // the separate path-based [`render_document`], dispatched before this by the caller.
        ViewMode::RenderedDocument => {
            unreachable!("documents render via render_document, not render")
        }
    }
}

/// Render a binary document by converting it to text and rendering that like markdown.
///
/// Unlike the stdin-fed renderers, the converter is handed the file **path** (pandoc / pdftotext /
/// LibreOffice can't take a binary office file on stdin). The path is already confined to the tree
/// root by the caller (`classify`'s root check governs the same selection), and the converter's
/// output is re-neutralized by [`to_text`] before display — so the trust boundary holds. A missing
/// converter tool, or a conversion failure/timeout, degrades to a short notice (never a crash or an
/// empty pane), exactly like the markdown/diff/syntax delegates (AC-24/25).
pub fn render_document(
    renderers: &Renderers,
    path: &Path,
    kind: DocKind,
    caps: Caps,
) -> (Text<'static>, Option<String>) {
    // 1. Convert the document to intermediate text: markdown (pandoc) or plain text / CSV / PDF
    //    (pdftotext, LibreOffice). `{path}`/`{outdir}`/`{tmpfile}` tokens get real paths. The
    //    converter runs under a more generous bound than the interactive delegates below — a
    //    LibreOffice cold start alone can outlast glow's few-second timeout.
    let converted = match renderers.documents.for_kind(kind) {
        // Stdout converters (pandoc, pdftotext): the path is an argv arg, nothing on stdin.
        Converter::Stdout(argv) => run_renderer(&subst_path(argv, path), "", DOC_CONVERT_TIMEOUT),
        // Temp-file converters (LibreOffice): write into a private temp dir; either read the
        // produced file back (`then` None) or run a second extractor over it (`then` Some).
        Converter::TempFile {
            argv,
            out_ext,
            then,
        } => run_tempfile(argv, out_ext, then.as_deref(), path, DOC_CONVERT_TIMEOUT),
    };
    let intermediate = match converted {
        Ok(text) => text,
        Err(err) => {
            let cap = format!("{} ({})", kind.label(), kind.tool());
            return (
                Text::raw(format!("[{}: could not render]", kind.label())),
                Some(err.notice(&cap)),
            );
        }
    };
    // 2. Bound the converted text to the size caps (a huge doc can't blow up the pane, AC-13).
    let (bounded, over) = cap_preview(&intermediate, caps);
    let trunc = over.map(|_| "⚠ Truncated: converted document exceeds the size cap.".to_string());
    // 3. Render the intermediate through the markdown delegate (glow) — headings/lists/tables for
    //    pandoc output, readable plain text otherwise. A missing glow degrades to plain text, and
    //    either way `to_text` neutralizes the converter output (AC-27). The interactive delegate
    //    keeps the normal (shorter) renderer timeout.
    delegate(
        &renderers.markdown,
        &bounded,
        ViewMode::RenderedMarkdown,
        renderers.timeout,
        trunc,
    )
}

/// Converters (especially a LibreOffice cold start that spins up a fresh user profile) routinely
/// outlast the interactive `RENDER_TIMEOUT` used for glow/delta/bat. Give document conversion its
/// own, more generous wall-clock bound; the render of the *converted* text still uses the normal
/// renderer timeout.
const DOC_CONVERT_TIMEOUT: Duration = Duration::from_secs(25);

/// Substitute the `{path}` token in a converter argv with the (absolute, root-confined) file path.
fn subst_path(argv: &[String], path: &Path) -> Vec<String> {
    let p = path.to_string_lossy();
    argv.iter().map(|a| a.replace("{path}", &p)).collect()
}

/// Run a temp-file converter (LibreOffice): substitute `{path}`/`{outdir}`, run it in a fresh
/// private temp dir, and produce the intermediate text. If `then` is `None` the `<stem>.<out_ext>`
/// file it wrote is read back directly (Calc → CSV); if `then` is `Some`, that extractor is run
/// over the produced file (`{tmpfile}`) and its stdout is the result (Impress → PDF → pdftotext).
/// The temp dir is removed best-effort afterward. LibreOffice exits `0` even when it writes nothing
/// (e.g. "no export filter"), so a missing output file is treated as a conversion failure rather
/// than trusting the exit status.
fn run_tempfile(
    argv: &[String],
    out_ext: &str,
    then: Option<&[String]>,
    path: &Path,
    timeout: Duration,
) -> Result<String, RendererError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("hfv-doc-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).map_err(|e| RendererError::Failed {
        detail: format!("temp dir: {e}"),
    })?;
    let p = path.to_string_lossy();
    let out = dir.to_string_lossy();
    let cmd: Vec<String> = argv
        .iter()
        .map(|a| a.replace("{path}", &p).replace("{outdir}", &out))
        .collect();
    let result = run_renderer(&cmd, "", timeout).and_then(|_stdout| {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
        let produced = dir.join(format!("{stem}.{out_ext}"));
        // The converter exited 0 but may have written nothing (LibreOffice does this on a missing
        // export filter). Surface that as a failure, not a silent empty pane.
        if !produced.is_file() {
            return Err(RendererError::Failed {
                detail: format!("converter produced no .{out_ext} output"),
            });
        }
        match then {
            // Read the produced text file directly.
            None => std::fs::read_to_string(&produced).map_err(|e| RendererError::Failed {
                detail: format!("read converted output: {e}"),
            }),
            // Run the second-stage extractor over the produced file (e.g. pdftotext on the PDF).
            Some(extractor) => {
                let tf = produced.to_string_lossy();
                let cmd2: Vec<String> = extractor
                    .iter()
                    .map(|a| a.replace("{tmpfile}", &tf))
                    .collect();
                run_renderer(&cmd2, "", timeout)
            }
        }
    });
    let _ = std::fs::remove_dir_all(&dir); // best-effort cleanup, regardless of outcome
    result
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

/// Substitute the `{name}` placeholder in a renderer command with the selected file name,
/// so a stdin-fed renderer (e.g. `bat --file-name={name}`) can still infer the language —
/// keeping the secure stdin design while enabling syntax highlighting (AC-10).
fn with_name(command: &[String], name: &str) -> Vec<String> {
    command
        .iter()
        .map(|arg| arg.replace("{name}", name))
        .collect()
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
        Err(err) => {
            // Map the typed failure to a short, actionable notice. The raw OS errno / io::Error
            // detail is kept on the error but NOT surfaced in the default notice — a user can't
            // act on "No such file or directory (os error 2)", but can act on "renderer (glow)
            // not found; install it or see docs/renderers.md" (AC-24/25).
            let fallback = err.notice(capability(mode));
            let notice = match base_notice {
                Some(prev) => format!("{prev}\n{fallback}"),
                None => fallback,
            };
            (to_text(input), Some(notice))
        }
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
        ViewMode::RenderedDocument => "Document",
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

/// Spawn a renderer, feed `input` on stdin (writer thread, avoids a pipe deadlock), read
/// stdout (reader thread), and bound the wait by `timeout` — a wedged renderer is killed
/// and reported as failed so the plain-text fallback kicks in. `Err` on a missing program,
/// non-zero exit, or timeout. The command is trusted (operator-configured); only the stdin
/// content is untrusted, so there is no argument injection.
fn run_renderer(
    command: &[String],
    input: &str,
    timeout: Duration,
) -> Result<String, RendererError> {
    let prog = command
        .first()
        .cloned()
        .ok_or_else(|| RendererError::Failed {
            detail: "empty renderer command".to_string(),
        })?;
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
            let _ = stdin.write_all(owned.as_bytes()); // ignore a closed pipe
        });
    }

    let stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Cap the captured output so a renderer spewing unbounded data can't exhaust memory.
        let mut buf = Vec::new();
        if let Some(out) = stdout {
            let _ = out.take(MAX_RENDER_OUTPUT).read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });

    // A SINGLE combined wall-clock deadline for the whole invocation (the doc'd "per-invocation
    // wall-clock bound"), NOT `timeout` applied twice. The stdout phase below waits up to
    // `timeout`, then the Ok-path exit-wait gets only the REMAINING budget — so a renderer that
    // closes stdout late and then lingers on exit can't burn ~2× the timeout (which, for the
    // synchronous help render, would blow AC-22's responsiveness budget). See item 1 / AC-22.
    let deadline = Instant::now() + timeout;
    match rx.recv_timeout(timeout) {
        Ok(buf) => {
            // stdout closed; the process should exit promptly. Bound that wait by what's LEFT of
            // the single deadline, so a renderer that closes stdout then hangs is still killed and
            // the TOTAL never exceeds `timeout` (no indefinite block, no doubled budget).
            match crate::proc::wait_bounded(
                &mut child,
                deadline.saturating_duration_since(Instant::now()),
            ) {
                Some(status) if status.success() => Ok(String::from_utf8_lossy(&buf).into_owned()),
                Some(status) => Err(RendererError::Failed {
                    detail: format!("exited with {status}"),
                }),
                None => Err(RendererError::Failed {
                    detail: "did not exit".to_string(),
                }),
            }
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(RendererError::Timeout)
        }
    }
}

/// Ingest (possibly untrusted) content into ratatui `Text`. Cursor-movement and
/// screen-control escape sequences are stripped regardless of source; only SGR styling is
/// kept and mapped into spans by `ansi-to-tui` (AC-27). The result can only ever paint the
/// viewer's own region — it carries no terminal-control operations.
pub fn to_text(raw: &str) -> Text<'static> {
    let cleaned = strip_terminal_control(raw);
    cleaned.clone().into_text().unwrap_or_else(|_| {
        // If ANSI parsing fails, the kept SGR runs still contain raw ESC bytes — strip
        // ALL ESC on the fallback so no control byte ever reaches the terminal (AC-27).
        Text::raw(cleaned.replace('\u{1b}', ""))
    })
}

/// Remove cursor/screen-control escape sequences, keeping only SGR (`…m`) styling so it
/// can be mapped to ratatui styles downstream. Operates on bytes (control sequences are
/// ASCII) and preserves all other (UTF-8) content verbatim.
fn strip_terminal_control(raw: &str) -> String {
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
                    if j < bytes.len() && bytes[j] == b'm' {
                        out.extend_from_slice(&bytes[start..=j]); // keep SGR styling
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
            // Drop other C0 control bytes (BEL/BS/CR/FF/VT/…) and DEL, which can still
            // ring the bell, backspace, or carriage-return to overwrite/spoof a line.
            // Keep only newline and tab.
            let b = bytes[i];
            let is_c0_control = b < 0x20 && b != b'\n' && b != b'\t';
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
    fn run_renderer_bounds_total_wall_clock_to_a_single_timeout_on_slow_exit() {
        // R3 item 1 / AC-22: `run_renderer` must enforce a SINGLE combined wall-clock deadline,
        // not apply `timeout` twice (once waiting for stdout, again waiting for exit). This
        // exercises the Ok→wait_bounded slow-exit path: `cat` echoes stdin then closes stdout
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
        // `wait_bounded` path. Under the 2× bug: ~0.8×+1.0× ≈ 1.8×. Under the single combined
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
}
