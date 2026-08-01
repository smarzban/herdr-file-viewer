//! What's New composition from already-projected local state.
//!
//! This module owns the Help-open deadline and document ordering only. Its renderer boundary is
//! deliberately one document at a time, so a failed or timed-out document cannot affect another.

use super::NoticeSnapshot;
use ratatui::text::{Line, Text};
use std::time::{Duration, Instant};

/// The whole synchronous What's New composition budget, measured once when Help opens.
pub const WHATS_NEW_COMPOSE_TIMEOUT: Duration = Duration::from_millis(200);

/// Render one already-selected Markdown document under the caller's remaining budget.
///
/// Implementations must return terminal-safe [`Text`]. The production adapter is T-16's
/// [`crate::render::render_markdown_section`]; this small boundary keeps composer tests hermetic.
pub trait MarkdownSectionRenderer {
    fn render(&mut self, document: &str, width: u16, remaining: Duration) -> Text<'static>;
}

impl<F> MarkdownSectionRenderer for F
where
    F: FnMut(&str, u16, Duration) -> Text<'static>,
{
    fn render(&mut self, document: &str, width: u16, remaining: Duration) -> Text<'static> {
        self(document, width, remaining)
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
/// Documents are independent and always ordered as the accepted spotlight body, Available updates,
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
    let embedded_releases = crate::help::released_changelog(embedded_changelog);
    let spotlight_body = snapshot
        .spotlight
        .whats_new_body()
        .and_then(|body| std::str::from_utf8(body).ok());
    let mut documents = Vec::with_capacity(3);
    if let Some(body) = spotlight_body {
        documents.push(body.to_owned());
    }
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
    if !embedded_releases.is_empty() {
        documents.push(embedded_releases);
    }

    let deadline = opened_at + WHATS_NEW_COMPOSE_TIMEOUT;
    let mut lines = Vec::<Line<'static>>::new();
    for (index, document) in documents.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let rendered = if remaining.is_zero() {
            crate::render::to_text(document)
        } else {
            renderer.render(document, width, remaining)
        };
        if index > 0 {
            lines.push(Line::raw(""));
        }
        lines.extend(rendered.lines);
    }
    Text::from(lines)
}
