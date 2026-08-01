//! What's New composition from already-projected local state.
//!
//! This module owns the Help-open deadline and document ordering only. Its renderer boundary is
//! deliberately one document at a time, so a failed or timed-out document cannot affect another.

use super::NoticeSnapshot;
use ratatui::text::{Line, Text};
use std::time::{Duration, Instant};

/// The whole synchronous What's New composition budget, measured once when Help opens.
pub const WHATS_NEW_COMPOSE_TIMEOUT: Duration = Duration::from_millis(200);

/// Render one already-selected Markdown document under the Help-open absolute deadline.
///
/// `fallback` is terminal-safe plain text prepared before the renderer receives any budget. An
/// implementation must return it unchanged when the deadline has expired or rendering fails. The
/// production adapter is [`crate::render::render_markdown_section`]; this small boundary keeps
/// composer tests hermetic.
pub trait MarkdownSectionRenderer {
    fn render(
        &mut self,
        document: &str,
        fallback: Text<'static>,
        width: u16,
        deadline: Instant,
    ) -> Text<'static>;
}

impl<F> MarkdownSectionRenderer for F
where
    F: FnMut(&str, Text<'static>, u16, Instant) -> Text<'static>,
{
    fn render(
        &mut self,
        document: &str,
        fallback: Text<'static>,
        width: u16,
        deadline: Instant,
    ) -> Text<'static> {
        self(document, fallback, width, deadline)
    }
}

/// Fixed local install guidance, derived only from the official repository slug.
///
/// It is display copy, never an action: composition passes it to the Markdown renderer as text.
pub fn install_guidance() -> String {
    format!(
        "To install this update, run:\n\n    herdr plugin install {}",
        super::repo_slug()
    )
}

/// Compose the What's New body under one absolute Help-open deadline.
///
/// Documents are independent and always ordered as Available updates, the accepted spotlight body,
/// then the full embedded released history. Available updates combines exact release details when
/// present with local install guidance for a detected release. Once the deadline is exhausted,
/// remaining documents use the shared safe plain-text fallback without invoking `renderer`.
pub fn compose_whats_new(
    snapshot: &NoticeSnapshot,
    embedded_changelog: &str,
    install_copy: &str,
    opened_at: Instant,
    width: u16,
    renderer: &mut impl MarkdownSectionRenderer,
) -> Text<'static> {
    let deadline = opened_at + WHATS_NEW_COMPOSE_TIMEOUT;
    let embedded_releases = crate::help::released_changelog(embedded_changelog);
    let spotlight_body = snapshot
        .spotlight
        .whats_new_body()
        .and_then(|body| std::str::from_utf8(body).ok());
    let mut documents = Vec::with_capacity(3);
    if snapshot.detected_release.is_some() {
        let mut available_updates = snapshot
            .release_details
            .as_ref()
            .map(|details| details.details.clone())
            .unwrap_or_default();
        if !available_updates.is_empty() {
            if !available_updates.ends_with('\n') {
                available_updates.push('\n');
            }
            available_updates.push('\n');
        }
        available_updates.push_str(install_copy);
        documents.push(available_updates);
    }
    if let Some(body) = spotlight_body {
        documents.push(body.to_owned());
    }
    if !embedded_releases.is_empty() {
        documents.push(embedded_releases);
    }

    // Do this before delegating even the first document. Once the deadline expires, these owned
    // values are appended directly, so an expired later document never re-neutralizes up to MiB of
    // remote content on the input thread.
    let documents = documents
        .into_iter()
        .map(|document| {
            let fallback = crate::render::to_text(&document);
            (document, fallback)
        })
        .collect::<Vec<_>>();

    let mut lines = Vec::<Line<'static>>::new();
    for (index, (document, fallback)) in documents.into_iter().enumerate() {
        let rendered = if deadline.saturating_duration_since(Instant::now()).is_zero() {
            fallback
        } else {
            renderer.render(&document, fallback, width, deadline)
        };
        if index > 0 {
            lines.push(Line::raw(""));
        }
        lines.extend(rendered.lines);
    }
    Text::from(lines)
}
