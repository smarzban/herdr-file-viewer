//! Content Renderer — produce the content-pane text for a file, with safety guards.
//!
//! The primary trust boundary: all file bytes are untrusted. This module bounds size
//! (AC-13), refuses to emit raw bytes for binary files (AC-12), and (in later tasks)
//! neutralizes control/escape sequences (AC-27) and delegates styling to external CLIs
//! with a plain-text fallback (AC-24/25). Reads only, never writes (AC-N1).

use crate::view_policy::ViewMode;
use ansi_to_tui::IntoText;
use ratatui::text::Text;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// The size cap: files at or above this many bytes are previewed, not shown whole (AC-13).
const MAX_BYTES: u64 = 1024 * 1024; // 1 MB
/// The size cap by line count (AC-13).
const MAX_LINES: usize = 5000;
/// Cap on bytes captured from a renderer's stdout, bounding memory if it spews output.
const MAX_RENDER_OUTPUT: u64 = 16 * 1024 * 1024; // 16 MB

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
/// `MAX_BYTES` from disk, so a huge or hostile file can never be slurped whole (AC-N1).
///
/// Refuses to read anything that does not resolve to a **regular file inside `root`**:
/// a symlink (or `..`) escaping the root cannot leak out-of-root content into the pane
/// (AC-N5), and a FIFO/device/dir is never opened (no hang, no garbage). Such paths
/// return `Binary` (a placeholder, no bytes).
pub fn classify(root: &Path, path: &Path) -> Prepared {
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
    // Bounded read: at most MAX_BYTES, so a giant/hostile file is never slurped whole.
    let mut buf = Vec::new();
    if file.take(MAX_BYTES).read_to_end(&mut buf).is_err() {
        return Prepared::Full {
            text: String::new(),
        };
    }

    // Binary: a NUL byte anywhere in the (bounded) content. No raw bytes are emitted.
    if buf.contains(&0) {
        return Prepared::Binary;
    }

    let over_bytes = byte_len >= MAX_BYTES;
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
    let over_lines = line_count >= MAX_LINES;
    if over_bytes || over_lines {
        let preview: String = text.lines().take(MAX_LINES).collect::<Vec<_>>().join("\n");
        let cap = if over_bytes { "1 MB size" } else { "5000-line" };
        let notice = format!(
            "⚠ Truncated preview — showing {} lines ({} of {} bytes); file exceeds the {} cap.",
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
        let (diff, notice) = cap_preview(raw_diff.unwrap_or(""));
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
        Prepared::Binary => return (Text::raw("[binary file — preview not shown]"), None),
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
    }
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
fn cap_preview(text: &str) -> (String, Option<String>) {
    let over = text.lines().count() >= MAX_LINES || text.len() as u64 >= MAX_BYTES;
    if !over {
        return (text.to_string(), None);
    }
    let mut preview: String = text.lines().take(MAX_LINES).collect::<Vec<_>>().join("\n");
    if preview.len() as u64 > MAX_BYTES {
        let end = preview
            .char_indices()
            .take_while(|(i, _)| (*i as u64) < MAX_BYTES)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        preview.truncate(end);
    }
    (
        preview,
        Some("⚠ Truncated diff preview — diff exceeds the size cap.".into()),
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
        Err(reason) => {
            let fallback = format!(
                "{} renderer unavailable ({reason}); showing plain text.",
                capability(mode)
            );
            let notice = match base_notice {
                Some(prev) => format!("{prev}\n{fallback}"),
                None => fallback,
            };
            (to_text(input), Some(notice))
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
fn run_renderer(command: &[String], input: &str, timeout: Duration) -> Result<String, String> {
    let prog = command.first().cloned().ok_or("empty renderer command")?;
    let mut child = renderer_command(command)?
        .spawn()
        .map_err(|e| format!("{prog}: {e}"))?;

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

    match rx.recv_timeout(timeout) {
        Ok(buf) => {
            // stdout closed; the process should exit promptly. Bound that wait too, so a
            // renderer that closes stdout then hangs is still killed (no indefinite block).
            match wait_bounded(&mut child, timeout) {
                Some(status) if status.success() => Ok(String::from_utf8_lossy(&buf).into_owned()),
                Some(status) => Err(format!("{prog} exited with {status}")),
                None => Err(format!("{prog} did not exit")),
            }
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!("{prog} timed out"))
        }
    }
}

/// Wait for a child to exit within `grace`, polling; kill and reap it if it overruns.
fn wait_bounded(
    child: &mut std::process::Child,
    grace: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
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
        assert_eq!(classify(&std::env::temp_dir(), &p), Prepared::Binary); // AC-12
        fs::remove_file(&p).ok();
    }

    #[test]
    fn small_text_file_is_returned_in_full() {
        let p = tmp("small.txt", b"hello\nworld\n");
        match classify(&std::env::temp_dir(), &p) {
            Prepared::Full { text } => assert!(text.contains("hello")),
            other => panic!("expected Full, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn file_over_one_megabyte_is_truncated_with_a_notice() {
        let big = vec![b'a'; (MAX_BYTES as usize) + 100];
        let p = tmp("big.txt", &big);
        match classify(&std::env::temp_dir(), &p) {
            Prepared::Truncated { text, notice } => {
                assert!(!notice.is_empty(), "AC-13: a visible truncation notice");
                assert!(text.len() as u64 <= MAX_BYTES, "AC-13: preview is bounded");
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn file_over_five_thousand_lines_is_truncated() {
        let many = "x\n".repeat(6000);
        let p = tmp("many.txt", many.as_bytes());
        match classify(&std::env::temp_dir(), &p) {
            Prepared::Truncated { text, notice } => {
                assert!(
                    text.lines().count() <= MAX_LINES,
                    "AC-13: preview line-bounded"
                );
                assert!(notice.contains("line"), "notice describes the line cap");
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn classify_does_not_modify_the_file() {
        let p = tmp("ro.txt", b"unchanged\n");
        let before = fs::read(&p).unwrap();
        let _ = classify(&std::env::temp_dir(), &p);
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

    #[test]
    fn refuses_a_symlink_whose_target_escapes_the_root() {
        use std::os::unix::fs::symlink;
        let root = unique_dir("root");
        let outside = tmp("secret", b"TOPSECRET"); // lives in temp_dir, outside `root`
        let link = root.join("link.txt");
        symlink(&outside, &link).unwrap();
        assert_eq!(
            classify(&root, &link),
            Prepared::Binary,
            "AC-N5: no out-of-root read"
        );
        fs::remove_dir_all(&root).ok();
        fs::remove_file(&outside).ok();
    }

    #[test]
    fn follows_a_symlink_that_stays_within_the_root() {
        use std::os::unix::fs::symlink;
        let root = unique_dir("root");
        let real = root.join("real.txt");
        fs::write(&real, "hello inside").unwrap();
        let link = root.join("link.txt");
        symlink(&real, &link).unwrap();
        match classify(&root, &link) {
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
        assert_eq!(classify(&root, &sub), Prepared::Binary);
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
