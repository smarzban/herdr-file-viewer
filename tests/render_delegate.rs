//! T-11 — Content Renderer: delegate to external renderers + fallback + notice
//! (AC-8, AC-9, AC-10, AC-24, AC-25), with binary placeholder (AC-12) reinforced.
//!
//! Renderer commands are injected: `cat` echoes stdin (a working renderer), a nonexistent
//! program simulates a missing one — so the tests never depend on glow/delta/bat.

use herdr_file_viewer::render::{render, Prepared, Renderers};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::time::{Duration, Instant};

fn cat() -> Renderers {
    Renderers {
        markdown: vec!["cat".into()],
        diff: vec!["cat".into()],
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
    let prepared = Prepared::Full { text: "fn main() {}".into() };
    let (text, notice) = render(&cat(), &prepared, ViewMode::SyntaxContent, None, None);
    assert!(flatten(&text).contains("fn main"), "AC-10");
    assert!(notice.is_none());
}

#[test]
fn markdown_mode_invokes_the_markdown_renderer() {
    let prepared = Prepared::Full { text: "# Title".into() };
    let (text, _) = render(&cat(), &prepared, ViewMode::RenderedMarkdown, None, None);
    assert!(flatten(&text).contains("# Title"), "AC-8");
}

#[test]
fn diff_mode_renders_the_supplied_raw_diff() {
    let prepared = Prepared::Full { text: "ignored".into() };
    let (text, _) = render(&cat(), &prepared, ViewMode::Diff, Some("@@ -1 +1 @@\n+new line"), None);
    assert!(flatten(&text).contains("+new line"), "AC-9");
}

#[test]
fn diff_mode_renders_even_for_a_binary_or_deleted_file() {
    // A deleted file classifies as Binary (gone from disk), but its diff comes from git,
    // so Diff mode must still show the diff — not the binary placeholder (AC-9).
    let (text, _) = render(&cat(), &Prepared::Binary, ViewMode::Diff, Some("@@ -1 +0 @@\n-removed"), None);
    let s = flatten(&text);
    assert!(s.contains("-removed"), "deletion diff is shown");
    assert!(!s.to_lowercase().contains("binary file"), "no binary placeholder in diff mode");
}

#[test]
fn missing_renderer_falls_back_to_plain_text_with_a_notice() {
    let renderers = Renderers {
        markdown: vec!["herdr-no-such-binary-xyz".into()],
        diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
        timeout: Duration::from_secs(5),
    };
    let prepared = Prepared::Full { text: "# Title".into() };
    let (text, notice) = render(&renderers, &prepared, ViewMode::RenderedMarkdown, None, None);
    assert!(flatten(&text).contains("# Title"), "AC-24: plain-text fallback, not empty/crash");
    let notice = notice.expect("AC-25: a non-fatal fallback notice");
    assert!(
        notice.to_lowercase().contains("markdown") || notice.to_lowercase().contains("unavailable"),
        "AC-25: notice names the missing capability: {notice}"
    );
}

#[test]
fn binary_shows_a_placeholder_not_raw_bytes() {
    let (text, _) = render(&cat(), &Prepared::Binary, ViewMode::SyntaxContent, None, None);
    assert!(flatten(&text).to_lowercase().contains("binary"), "AC-12 placeholder");
}

#[test]
fn syntax_renderer_receives_the_file_name_via_placeholder() {
    // A renderer echoing its args proves the {name} substitution that lets a stdin-fed
    // highlighter (e.g. bat --file-name={name}) infer the language (AC-10).
    let renderers = Renderers {
        markdown: vec!["cat".into()],
        diff: vec!["cat".into()],
        syntax: vec!["sh".into(), "-c".into(), "echo {name}".into()],
        timeout: Duration::from_secs(5),
    };
    let prepared = Prepared::Full { text: "code".into() };
    let (text, _) = render(&renderers, &prepared, ViewMode::SyntaxContent, None, Some("main.rs"));
    assert!(flatten(&text).contains("main.rs"), "file name passed to the syntax renderer");
}

#[test]
fn raw_content_mode_does_not_invoke_a_renderer() {
    let renderers = Renderers {
        markdown: vec!["nope-xyz".into()],
        diff: vec!["nope-xyz".into()],
        syntax: vec!["nope-xyz".into()],
        timeout: Duration::from_secs(5),
    };
    let prepared = Prepared::Full { text: "plain text here".into() };
    let (text, notice) = render(&renderers, &prepared, ViewMode::RawContent, None, None);
    assert!(flatten(&text).contains("plain text here"));
    assert!(notice.is_none(), "raw content needs no renderer → no fallback notice");
}

#[test]
fn a_hanging_renderer_times_out_and_falls_back() {
    let renderers = Renderers {
        markdown: vec!["sleep".into(), "30".into()],
        diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
        timeout: Duration::from_millis(150),
    };
    let prepared = Prepared::Full { text: "# Title".into() };
    let start = Instant::now();
    let (text, notice) = render(&renderers, &prepared, ViewMode::RenderedMarkdown, None, None);
    assert!(start.elapsed() < Duration::from_secs(3), "must not block on a wedged renderer");
    assert!(flatten(&text).contains("# Title"), "AC-24: plain-text fallback after timeout");
    assert!(notice.unwrap().to_lowercase().contains("timed out"), "notice reports the timeout");
}

#[test]
fn truncation_notice_is_preserved_through_rendering() {
    let prepared = Prepared::Truncated {
        text: "head".into(),
        notice: "truncated-preview".into(),
    };
    let (_, notice) = render(&cat(), &prepared, ViewMode::SyntaxContent, None, None);
    assert!(notice.unwrap().contains("truncated-preview"), "AC-13 notice survives");
}
