//! T-11 — Content Renderer: delegate to external renderers + fallback + notice
//! (AC-8, AC-9, AC-10, AC-24, AC-25), with binary placeholder (AC-12) reinforced.
//!
//! Renderer commands are injected: `cat` echoes stdin (a working renderer), a nonexistent
//! program simulates a missing one — so the tests never depend on glow/delta/bat.

use herdr_file_viewer::render::{render, Prepared, Renderers};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;

fn cat() -> Renderers {
    Renderers {
        markdown: vec!["cat".into()],
        diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
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
    let (text, notice) = render(&cat(), &prepared, ViewMode::SyntaxContent, None);
    assert!(flatten(&text).contains("fn main"), "AC-10");
    assert!(notice.is_none());
}

#[test]
fn markdown_mode_invokes_the_markdown_renderer() {
    let prepared = Prepared::Full { text: "# Title".into() };
    let (text, _) = render(&cat(), &prepared, ViewMode::RenderedMarkdown, None);
    assert!(flatten(&text).contains("# Title"), "AC-8");
}

#[test]
fn diff_mode_renders_the_supplied_raw_diff() {
    let prepared = Prepared::Full { text: "ignored".into() };
    let (text, _) = render(&cat(), &prepared, ViewMode::Diff, Some("@@ -1 +1 @@\n+new line"));
    assert!(flatten(&text).contains("+new line"), "AC-9");
}

#[test]
fn missing_renderer_falls_back_to_plain_text_with_a_notice() {
    let renderers = Renderers {
        markdown: vec!["herdr-no-such-binary-xyz".into()],
        diff: vec!["cat".into()],
        syntax: vec!["cat".into()],
    };
    let prepared = Prepared::Full { text: "# Title".into() };
    let (text, notice) = render(&renderers, &prepared, ViewMode::RenderedMarkdown, None);
    assert!(flatten(&text).contains("# Title"), "AC-24: plain-text fallback, not empty/crash");
    let notice = notice.expect("AC-25: a non-fatal fallback notice");
    assert!(
        notice.to_lowercase().contains("markdown") || notice.to_lowercase().contains("unavailable"),
        "AC-25: notice names the missing capability: {notice}"
    );
}

#[test]
fn binary_shows_a_placeholder_not_raw_bytes() {
    let (text, _) = render(&cat(), &Prepared::Binary, ViewMode::SyntaxContent, None);
    assert!(flatten(&text).to_lowercase().contains("binary"), "AC-12 placeholder");
}

#[test]
fn raw_content_mode_does_not_invoke_a_renderer() {
    let renderers = Renderers {
        markdown: vec!["nope-xyz".into()],
        diff: vec!["nope-xyz".into()],
        syntax: vec!["nope-xyz".into()],
    };
    let prepared = Prepared::Full { text: "plain text here".into() };
    let (text, notice) = render(&renderers, &prepared, ViewMode::RawContent, None);
    assert!(flatten(&text).contains("plain text here"));
    assert!(notice.is_none(), "raw content needs no renderer → no fallback notice");
}

#[test]
fn truncation_notice_is_preserved_through_rendering() {
    let prepared = Prepared::Truncated {
        text: "head".into(),
        notice: "truncated-preview".into(),
    };
    let (_, notice) = render(&cat(), &prepared, ViewMode::SyntaxContent, None);
    assert!(notice.unwrap().contains("truncated-preview"), "AC-13 notice survives");
}
