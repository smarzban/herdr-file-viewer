//! Content Renderer: delegate to external renderers + fallback + notice
//! (AC-8, AC-9, AC-10, AC-24, AC-25), with binary placeholder (AC-12) reinforced.
//!
//! Renderer commands are injected: `cat` echoes stdin (a working renderer), a nonexistent
//! program simulates a missing one — so the tests never depend on glow/delta/bat.

use herdr_file_viewer::render::{Caps, Prepared, Renderers, render};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn cat() -> Renderers {
    Renderers {
        markdown: vec!["cat".into()],
        diff: vec!["cat".into()],
        full_diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
        timeout: Duration::from_secs(5),
    }
}

fn flatten(t: &Text) -> String {
    t.lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect()
}

#[test]
fn syntax_mode_invokes_the_syntax_renderer_and_ingests_output() {
    let prepared = Prepared::Full {
        text: "fn main() {}".into(),
    };
    let (text, notice) = render(
        &cat(),
        &prepared,
        ViewMode::SyntaxContent,
        None,
        None,
        Caps::default(),
    );
    assert!(flatten(&text).contains("fn main"), "AC-10");
    assert!(notice.is_none());
}

#[test]
fn markdown_mode_invokes_the_markdown_renderer() {
    let prepared = Prepared::Full {
        text: "# Title".into(),
    };
    let (text, _) = render(
        &cat(),
        &prepared,
        ViewMode::RenderedMarkdown,
        None,
        None,
        Caps::default(),
    );
    assert!(flatten(&text).contains("# Title"), "AC-8");
}

#[test]
fn diff_mode_renders_the_supplied_raw_diff() {
    let prepared = Prepared::Full {
        text: "ignored".into(),
    };
    let (text, _) = render(
        &cat(),
        &prepared,
        ViewMode::Diff,
        Some("@@ -1 +1 @@\n+new line"),
        None,
        Caps::default(),
    );
    assert!(flatten(&text).contains("+new line"), "AC-9");
}

#[test]
fn an_oversized_diff_is_truncated_with_a_notice() {
    // Cap-relative so the test can't rot when the default line cap changes.
    let cap = Caps::default().max_lines;
    let huge = "+line\n".repeat(cap + 1000); // over the line cap
    let prepared = Prepared::Full { text: "x".into() };
    let (text, notice) = render(
        &cat(),
        &prepared,
        ViewMode::Diff,
        Some(&huge),
        None,
        Caps::default(),
    );
    assert!(
        notice.unwrap().to_lowercase().contains("truncated"),
        "AC-13: large diff bounded"
    );
    assert!(text.lines.len() <= cap, "diff preview is line-bounded");
}

#[test]
fn diff_mode_renders_even_for_a_binary_or_deleted_file() {
    // A deleted file classifies as Binary (gone from disk), but its diff comes from git,
    // so Diff mode must still show the diff — not the binary placeholder (AC-9).
    let (text, _) = render(
        &cat(),
        &Prepared::Binary,
        ViewMode::Diff,
        Some("@@ -1 +0 @@\n-removed"),
        None,
        Caps::default(),
    );
    let s = flatten(&text);
    assert!(s.contains("-removed"), "deletion diff is shown");
    assert!(
        !s.to_lowercase().contains("binary file"),
        "no binary placeholder in diff mode"
    );
}

#[test]
fn missing_renderer_falls_back_to_plain_text_with_a_notice() {
    let renderers = Renderers {
        markdown: vec!["herdr-no-such-binary-xyz".into()],
        diff: vec!["cat".into()],
        full_diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
        timeout: Duration::from_secs(5),
    };
    let prepared = Prepared::Full {
        text: "# Title".into(),
    };
    let (text, notice) = render(
        &renderers,
        &prepared,
        ViewMode::RenderedMarkdown,
        None,
        None,
        Caps::default(),
    );
    assert!(
        flatten(&text).contains("# Title"),
        "AC-24: plain-text fallback, not empty/crash"
    );
    let notice = notice.expect("AC-25: a non-fatal fallback notice");
    assert!(
        notice.to_lowercase().contains("markdown"),
        "AC-25: notice names the missing capability: {notice}"
    );
    // the missing-renderer notice names the binary, points to remediation, and never
    // leaks a raw OS errno ("os error 2") or io::Error Debug string.
    assert!(
        notice.contains("herdr-no-such-binary-xyz"),
        "notice names the missing binary: {notice}"
    );
    assert!(
        notice.contains("not found"),
        "notice states the renderer was not found: {notice}"
    );
    assert!(
        notice.contains("docs/renderers.md"),
        "notice points to remediation: {notice}"
    );
    assert!(
        !notice.contains("os error"),
        "no raw OS errno in the notice: {notice}"
    );
    assert!(
        !notice.contains("unavailable ("),
        "no raw error detail leaked in the notice: {notice}"
    );
}

#[test]
fn binary_shows_a_placeholder_not_raw_bytes() {
    let (text, _) = render(
        &cat(),
        &Prepared::Binary,
        ViewMode::SyntaxContent,
        None,
        None,
        Caps::default(),
    );
    assert!(
        flatten(&text).to_lowercase().contains("binary"),
        "AC-12 placeholder"
    );
}

#[test]
fn syntax_renderer_receives_the_file_name_via_placeholder() {
    // A renderer echoing its args proves the {name} substitution that lets a stdin-fed
    // highlighter (e.g. bat --file-name={name}) infer the language (AC-10).
    let renderers = Renderers {
        markdown: vec!["cat".into()],
        diff: vec!["cat".into()],
        full_diff: vec!["cat".into()],
        syntax: vec!["sh".into(), "-c".into(), "echo {name}".into()],
        timeout: Duration::from_secs(5),
    };
    let prepared = Prepared::Full {
        text: "code".into(),
    };
    let (text, _) = render(
        &renderers,
        &prepared,
        ViewMode::SyntaxContent,
        None,
        Some("main.rs"),
        Caps::default(),
    );
    assert!(
        flatten(&text).contains("main.rs"),
        "file name passed to the syntax renderer"
    );
}

#[test]
fn a_malicious_file_name_cannot_inject_via_the_placeholder() {
    // Even with a shell-wrapper renderer, a repo-controlled file name is sanitized to a
    // safe basename, so command substitution / metacharacters cannot execute.
    let marker = std::env::temp_dir().join(format!("HFV-PWN-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let renderers = Renderers {
        markdown: vec!["cat".into()],
        diff: vec!["cat".into()],
        full_diff: vec!["cat".into()],
        syntax: vec!["sh".into(), "-c".into(), "echo {name}".into()],
        timeout: Duration::from_secs(5),
    };
    let prepared = Prepared::Full {
        text: "code".into(),
    };
    let evil = format!("$(touch {}).rs", marker.display());
    let (text, _) = render(
        &renderers,
        &prepared,
        ViewMode::SyntaxContent,
        None,
        Some(&evil),
        Caps::default(),
    );
    assert!(!marker.exists(), "command substitution must not execute");
    let out = flatten(&text);
    assert!(
        !out.contains('$') && !out.contains('('),
        "metacharacters sanitized: {out}"
    );
}

#[test]
fn full_diff_mode_renders_the_diff_text_via_the_full_diff_renderer() {
    // FullDiff renders from git's (full-context) diff text on raw_diff — not the file bytes —
    // and delegates to the dedicated `full_diff` renderer, NOT the compact `diff` one. A
    // renderer that fails for `diff` but succeeds for `full_diff` proves the right one is used.
    let renderers = Renderers {
        markdown: vec!["cat".into()],
        diff: vec!["herdr-no-such-binary-xyz".into()], // would fail if FullDiff used it
        full_diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
        timeout: Duration::from_secs(5),
    };
    let full = "@@ -1,2 +1,2 @@\n fn main() {\n-    old();\n+    new();\n }";
    // Prepared::Binary on purpose: like Diff, FullDiff renders from git, so a deleted/binary
    // file still shows its diff (AC-9) rather than the binary placeholder.
    let (text, notice) = render(
        &renderers,
        &Prepared::Binary,
        ViewMode::FullDiff,
        Some(full),
        None,
        Caps::default(),
    );
    let s = flatten(&text);
    assert!(
        s.contains("fn main()") && s.contains("new()"),
        "full-context diff is shown: {s}"
    );
    assert!(
        !s.to_lowercase().contains("binary file"),
        "no binary placeholder in full-diff mode"
    );
    assert!(
        notice.is_none(),
        "the full_diff renderer succeeded → no fallback notice"
    );
}

#[test]
fn an_oversized_full_diff_is_truncated_with_a_notice() {
    // AC-13 on the FullDiff path: a full-context diff of a large file is bounded with a visible
    // truncation notice before it is rendered, so it can't blow up the content pane.
    // Cap-relative so the test can't rot when the default line cap changes.
    let cap = Caps::default().max_lines;
    let big_diff = "+line\n".repeat(cap + 1000); // over the line cap
    let (text, notice) = render(
        &cat(),
        &Prepared::Binary,
        ViewMode::FullDiff,
        Some(&big_diff),
        None,
        Caps::default(),
    );
    let n = notice.expect("AC-13: an oversized full-context diff gets a truncation notice");
    assert!(
        n.to_lowercase().contains("truncat"),
        "the notice names the truncation: {n}"
    );
    assert!(
        flatten(&text).lines().count() <= cap,
        "the rendered diff is line-bounded (AC-13)"
    );
}

#[test]
fn a_hanging_renderer_times_out_and_falls_back() {
    let renderers = Renderers {
        markdown: vec!["sleep".into(), "30".into()],
        diff: vec!["cat".into()],
        full_diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
        timeout: Duration::from_millis(150),
    };
    let prepared = Prepared::Full {
        text: "# Title".into(),
    };
    let start = Instant::now();
    let (text, notice) = render(
        &renderers,
        &prepared,
        ViewMode::RenderedMarkdown,
        None,
        None,
        Caps::default(),
    );
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "must not block on a wedged renderer"
    );
    assert!(
        flatten(&text).contains("# Title"),
        "AC-24: plain-text fallback after timeout"
    );
    assert!(
        notice.unwrap().to_lowercase().contains("timed out"),
        "notice reports the timeout"
    );
}

#[test]
fn truncation_notice_is_preserved_through_rendering() {
    let prepared = Prepared::Truncated {
        text: "head".into(),
        notice: "truncated-preview".into(),
    };
    let (_, notice) = render(
        &cat(),
        &prepared,
        ViewMode::SyntaxContent,
        None,
        None,
        Caps::default(),
    );
    assert!(
        notice.unwrap().contains("truncated-preview"),
        "AC-13 notice survives"
    );
}

/// Whether a real `glow` is installed — the wrap-width behavioral test below needs the actual
/// renderer (its table layout / line padding is glow's own behavior, not something we mock), so it
/// skips cleanly when glow is absent (e.g. a CI image without the runtime renderers).
fn glow_available() -> bool {
    Command::new("glow")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The table fix, end-to-end through the REAL markdown delegate: with glow pointed at the bundled
/// (margin-0) style and `-w <width>`, every rendered line is `<= width`. That is the property the
/// content pane relies on — the Presenter's `Paragraph::wrap` becomes a no-op (no line overflows to
/// a blank "gap" row), and a table wider than the pane at natural `-w 0` layout is instead sized to
/// fit with its borders intact rather than shattered by the re-wrap. Skips if glow is not installed.
#[test]
fn glow_markdown_wrapped_to_width_never_exceeds_it() {
    if !glow_available() {
        eprintln!("skipping glow_markdown_wrapped_to_width_never_exceeds_it: glow not installed");
        return;
    }
    let style = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/markdown-style.json");
    let width: u16 = 74;
    let renderers = Renderers {
        // The real default markdown command shape, pinned to the bundled style and this wrap width
        // (what `LiveContent::render_at_width` builds for a markdown render at a known pane width).
        markdown: vec![
            "glow".into(),
            "-s".into(),
            style.into(),
            "-w".into(),
            width.to_string(),
            "-".into(),
        ],
        diff: vec!["cat".into()],
        full_diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
        timeout: Duration::from_secs(5),
    };
    // A table far wider than `width` at natural layout, plus a long prose paragraph — both must be
    // brought within `width` by glow.
    let md = "\
| Model | Roles | Raised | Notes |\n\
|---|---|---|---|\n\
| gpt-5.5 | holistic, lens-security | 5 | The security sniper: sole catch of the grid-resize DoS, unbounded dims to OOM, nobody else saw it. |\n\
| opus-4-8 | holistic, lens-contracts | 7 | Co-caught the scrollback HIGH; a strong advisory set spanning cold-start and snapshot perms. |\n\
\n\
This is a long prose paragraph that at its natural width would be wider than the pane and so must be wrapped by glow to fit within the requested width without ever overflowing it.\n";
    let prepared = Prepared::Full { text: md.into() };
    let (text, _) = render(
        &renderers,
        &prepared,
        ViewMode::RenderedMarkdown,
        None,
        None,
        Caps::default(),
    );
    for line in &text.lines {
        assert!(
            line.width() as u16 <= width,
            "rendered line exceeds the wrap width {width} (would force a Presenter re-wrap / gap): \
             width={} content={:?}",
            line.width(),
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        );
    }
    // A table WAS rendered (box-drawing borders survived), not degraded to bare pipes.
    let flat = flatten(&text);
    assert!(
        flat.contains('│') || flat.contains('┼') || flat.contains('─'),
        "table borders present in the rendered output: {flat:?}"
    );
}
