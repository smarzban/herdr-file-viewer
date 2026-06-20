//! Presenter — draw the two-column viewer UI (the terminal view layer).
//!
//! Left column: the file tree (recursive indentation, AC-3; per-file git-status markers,
//! AC-7). Right column: the content pane (rendered text) with a notices strip above it for
//! truncation (AC-13) and renderer-fallback (AC-25) messages. The focused column is
//! highlighted. All content is clipped to the frame region — defense-in-depth for AC-27.
//!
//! Pure view: takes a [`ViewState`] and draws it; holds no state and performs no I/O.

use crate::git::Status;
use crate::tree::{Node, NodeKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};

/// Which column currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Content,
}

/// Everything the Presenter needs to draw one frame. Built by the Session Controller from
/// the Tree Model (nodes + selection), Content Renderer (content + notices), and session
/// focus/width. `width` is the pane width the controller observed (the narrow-split input
/// for AC-21); geometry is taken from the live frame area.
pub struct ViewState {
    /// Visible tree rows, in display order.
    pub nodes: Vec<Node>,
    /// Index into `nodes` of the selected row.
    pub selected: usize,
    /// The content-pane text (already sanitized/ingested by the Content Renderer).
    pub content: Text<'static>,
    /// Non-fatal notices to surface (truncation AC-13, renderer fallback AC-25).
    pub notices: Vec<String>,
    /// Which column has focus.
    pub focus: Focus,
    /// The pane width the controller last observed (session state — e.g. for tracking the
    /// narrow-split flag). The Presenter lays out from the live frame width, not this, so
    /// the two can never disagree; it is carried for the controller's own use.
    pub width: u16,
}

/// The single-character git-status marker shown beside a tree row (AC-7).
fn status_marker(status: Option<Status>) -> char {
    match status {
        Some(Status::Modified) => 'M',
        Some(Status::Added) => 'A',
        Some(Status::Deleted) => 'D',
        Some(Status::Untracked) => '?',
        None => ' ',
    }
}

/// Neutralize a string for display as a label/title: drop control characters (C0, DEL, and
/// C1 — `char::is_control`), so a repo-controlled file name carrying ESC/CSI bytes cannot
/// move the cursor, clear the screen, or spoof the UI (AC-27, defense-in-depth). ratatui's
/// own renderer also drops control graphemes, but the viewer's security guarantee must not
/// rest on that internal — this makes it explicit.
fn sanitize_label(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// The display name of a node — its final path component, or the whole path for a root.
fn node_name(node: &Node) -> String {
    node.path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| node.path.to_string_lossy().into_owned())
}

/// Render one tree row: `<marker> <indent><glyph><name>`. Indentation grows with depth so
/// the recursion is visible (AC-3); a directory carries an expand/collapse glyph.
fn tree_row(node: &Node, selected: bool) -> Line<'static> {
    let glyph = match node.kind {
        NodeKind::Dir if node.expanded => "▾ ",
        NodeKind::Dir => "▸ ",
        NodeKind::File => "",
    };
    let text = format!(
        "{} {}{}{}",
        status_marker(node.status),
        "  ".repeat(node.depth),
        glyph,
        sanitize_label(&node_name(node)),
    );
    let style = if selected {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    };
    Line::from(Span::styled(text, style))
}

/// Border style for a column — highlighted when it holds focus.
fn border_style(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

/// Draw the left column: the bordered file tree.
fn draw_tree(frame: &mut Frame, area: Rect, state: &ViewState) {
    let block = Block::bordered()
        .title("Files")
        .border_style(border_style(state.focus == Focus::Tree));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows: Vec<Line> = state
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| tree_row(node, i == state.selected))
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

/// Draw the right column: a notices strip (if any) above the content pane.
fn draw_content(frame: &mut Frame, area: Rect, state: &ViewState) {
    let title = state
        .nodes
        .get(state.selected)
        .map(|n| sanitize_label(&node_name(n)))
        .unwrap_or_else(|| "Content".to_string());
    let block = Block::bordered()
        .title(title)
        .border_style(border_style(state.focus == Focus::Content));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // A notice strip (truncation AC-13, fallback AC-25) sits above the content, bounded so
    // it can never crowd out the file itself.
    let max_notices = (inner.height.saturating_sub(1)).min(state.notices.len() as u16);
    let parts = Layout::vertical([Constraint::Length(max_notices), Constraint::Min(0)]).split(inner);

    if max_notices > 0 {
        let notice_lines: Vec<Line> = state
            .notices
            .iter()
            .map(|n| Line::styled(n.clone(), Style::new().fg(Color::Yellow)))
            .collect();
        frame.render_widget(Paragraph::new(notice_lines), parts[0]);
    }
    frame.render_widget(Paragraph::new(state.content.clone()), parts[1]);
}

/// Below this pane width the viewer drops to a single, focused column (AC-21).
const NARROW_SPLIT: u16 = 80;

/// Draw the viewer for the given state.
///
/// At ≥ 80 columns both columns are shown side by side. Narrower than that, only the
/// focused column is drawn — full width — so the active content stays readable (AC-21).
/// The decision is taken from the **live frame width**, so the split can never disagree
/// with the geometry it is drawn into (a stale `state.width` cannot desync the layout).
pub fn draw(frame: &mut Frame, state: &ViewState) {
    let area = frame.area();
    if area.width < NARROW_SPLIT {
        match state.focus {
            Focus::Tree => draw_tree(frame, area, state),
            Focus::Content => draw_content(frame, area, state),
        }
        return;
    }
    let cols =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(area);
    draw_tree(frame, cols[0], state);
    draw_content(frame, cols[1], state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_label_strips_control_bytes_keeps_printable() {
        // ESC + CSI + C0 controls removed; the printable remainder (incl. unicode) survives.
        assert_eq!(sanitize_label("evil\u{1b}[2J\u{1b}[10;10Hpwned"), "evil[2J[10;10Hpwned");
        assert_eq!(sanitize_label("a\u{07}\u{08}\rb\tc"), "abc");
        assert_eq!(sanitize_label("plain_name.rs"), "plain_name.rs");
        assert_eq!(sanitize_label("café—ok"), "café—ok");
        // C1 controls (U+0080..U+009F) are also dropped.
        assert_eq!(sanitize_label("x\u{0090}y"), "xy");
        // No control codepoint survives, ever.
        assert!(!sanitize_label("\u{1b}\u{07}\u{7f}\u{9b}z").chars().any(|c| c.is_control()));
    }
}
