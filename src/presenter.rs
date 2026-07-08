//! Presenter — draw the two-column viewer UI (the terminal view layer).
//!
//! Left column: the file tree (recursive indentation, AC-3; per-file git-status markers,
//! AC-7). Right column: the content pane (rendered text) with a notices strip above it for
//! truncation (AC-13) and renderer-fallback (AC-25) messages. The focused column is
//! highlighted. All content is clipped to the frame region — defense-in-depth for AC-27.
//!
//! Pure view: takes a [`ViewState`] and draws it; holds no state and performs no I/O.

use crate::git::Status;
use crate::text_layout::sanitize_control;
use crate::tree::{Node, NodeKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

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
    /// Vertical scroll offset of the content pane, in lines (wrapped lines when `wrap`,
    /// raw lines otherwise — matching ratatui's `Paragraph::scroll` semantics).
    pub content_scroll: u16,
    /// Horizontal scroll offset of the content pane, in columns. Only meaningful when not
    /// wrapping (ratatui ignores it under wrap); lets long code/diff lines be read sideways.
    pub content_hscroll: u16,
    /// The tree's vertical scroll offset from the LAST drawn frame (first visible node index),
    /// carried back via [`PaneGeometry::tree_scroll`]. The Presenter scrolls *minimally* from it
    /// so selecting a row already in view (e.g. a mouse click) never jumps the viewport (#45). `0`
    /// on the first frame and whenever every node fits.
    pub tree_scroll: u16,
    /// The tree's horizontal scroll offset, in columns — so a deeply-nested or long file name can
    /// be read sideways when it overflows the tree column. Driven by the `H`/`L` keys, the
    /// horizontal wheel, and by dragging the tree's horizontal scrollbar (the `←`/`→` keys are
    /// expand/collapse in the tree). The Presenter clamps it to the widest row at draw, so it can
    /// never over-scroll.
    pub tree_hscroll: u16,
    /// The content's total RENDERED row count — wrapped rows under `wrap`, raw lines otherwise (the
    /// controller's wrapped-aware count). The content vertical scrollbar sizes/positions against
    /// this so the thumb is correct under wrap, where raw `content.lines.len()` undercounts.
    pub content_rows: u16,
    /// Wrap long content lines (prose: markdown / plain text) instead of truncating them.
    /// Off for diffs and code, whose column alignment must be preserved.
    pub wrap: bool,
    /// The tree column's share of the width, as a percentage (the content pane takes the
    /// rest). Adjustable from the keyboard; used only in the wide two-column layout.
    pub split_pct: u16,
    /// Hide the tree and let the content pane fill the whole frame (the `z` zoom toggle).
    /// Overrides the split — and the narrow-layout focus rule — to draw content only.
    pub zoomed: bool,
    /// When `Some`, a one-row "update available" status line is drawn across the bottom of the
    /// frame (the columns take the remaining rows). `None` ⇒ no footer, layout unchanged.
    pub update_banner: Option<String>,
    /// When `Some`, the worktree picker overlay is drawn on top of the two columns (AC-1, AC-5).
    /// `None` ⇒ no overlay.
    pub picker: Option<PickerView>,
    /// When `Some`, the go-to-file finder overlay is drawn on top of the columns (AC-1).
    /// `None` ⇒ no overlay.
    pub finder: Option<FinderView>,
    /// When `Some`, a one-row prompt is drawn across the very bottom of the viewer showing the active
    /// in-file-nav prompt (e.g. `Go to line: 42`). `None` ⇒ no prompt open. The Controller builds the
    /// display string (label + buffer) so the Presenter stays mode-agnostic. (AC-1)
    pub prompt: Option<String>,
    /// The tree root's directory basename (e.g. `"herdr-plugin"`), shown as the tree column's
    /// top-border title so the user can see *which* directory the tree is rooted at — mirroring
    /// how the content pane titles itself from the selected node. Truncated with an ellipsis if
    /// it would overflow the column; the Presenter falls back to "Files" when it is empty.
    pub root_name: String,
    /// The current git branch (e.g. `"main"`, `"feat/x"`), shown on the tree column's bottom
    /// border. `None` outside a git repo or on a detached HEAD — in which case the bottom title is
    /// omitted entirely rather than showing a blank/placeholder branch (degrade gracefully).
    pub branch: Option<String>,
    /// The content pane's border title, derived from the displayed content's file path (not the
    /// live tree cursor), so the title switches in lockstep with the body — it never shows a
    /// freshly-selected file's name before that file's content arrives. `None` while no
    /// file's content has landed yet (launch, a re-root, or a directory/empty selection); the
    /// Presenter then falls back to the selected node's name (a directory) or "Content" — unless
    /// [`content_rendering`](Self::content_rendering) is set, in which case it uses a neutral
    /// "Content" label so the title doesn't jump to the still-loading selection.
    pub content_title: Option<String>,
    /// True while an off-thread render for a file is in flight. The Presenter uses this to pick a
    /// neutral title while the body shows the loading placeholder.
    pub content_rendering: bool,
    /// When `Some`, the content pane is drawn through [`crate::highlight::apply`] to overlay
    /// match highlights on top of the rendered text (AC-9, AC-11). `None` ⇒ draw the content
    /// as-is (byte-identical to today — the `None` arm is just `state.content.clone()`).
    pub search: Option<ContentSearch>,
    /// When `Some`, the copy-line-reference modal is active: the Presenter overlays a marker +
    /// selection highlight on the content pane (AC-1, AC-7), mirroring the [`search`](Self::search)
    /// overlay. `None` ⇒ draw the content as-is (byte-identical to today — the `None` arm leaves the
    /// content path untouched, so no other snapshot moves).
    pub line_select: Option<LineSelectView>,
    /// When `Some`, the in-app help overlay is drawn on top of everything else (AC-1, AC-5).
    /// `None` ⇒ no overlay. Drawn last in [`draw`] so it sits above the picker and finder.
    pub help: Option<HelpView>,
}

/// The worktree picker's draw model (an owned snapshot of the controller's picker state, so
/// the Presenter stays borrow-free). Built by the Session Controller's `view_state()`.
pub struct PickerView {
    /// The worktree rows, in display order.
    pub rows: Vec<PickerRowView>,
    /// Index into `rows` of the highlighted row.
    pub cursor: usize,
    /// Raw horizontal scroll offset (columns) carried from the controller. The Presenter clamps
    /// it to the live inner width at draw, so it can never over-scroll past the widest row.
    pub hscroll: u16,
}

/// One worktree row in the picker overlay.
pub struct PickerRowView {
    /// The worktree's path (displayed, sanitized for control bytes — AC-27).
    pub path: String,
    /// The branch name, or `None` when HEAD is detached.
    pub branch: Option<String>,
    /// `true` when HEAD is detached (no branch) — shown as a detached marker, never an empty
    /// branch (AC-2).
    pub detached: bool,
    /// `true` when this is the worktree the viewer is currently rooted at — rendered as a leading
    /// "current" marker, distinct from the selection cursor (AC-18).
    pub is_current: bool,
    /// The hosting agent's status (e.g. `"working"`), or `None` when the worktree's workspace
    /// hosts no real agent. Rendered as a small trailing badge, colored by status (AC-19).
    pub agent: Option<String>,
}

/// Search-highlight overlay for the content pane: the matches in the displayed content and which
/// one is current. Carried in [`ViewState`] so the Presenter overlays them via
/// [`crate::highlight::apply`]. Built by the Session Controller's `view_state()` from
/// `self.search: Option<SearchState>`.
///
/// The Presenter uses this purely for rendering — it overlays the highlight onto `content` lines
/// at draw time and never mutates any state (AC-N1, constitution read-only).
pub struct ContentSearch {
    /// The matches to highlight, in document order.
    pub matches: Vec<crate::search::Match>,
    /// Index into `matches` of the current (most recently navigated) match — rendered with
    /// [`crate::highlight::CURRENT_HIGHLIGHT`] rather than the regular [`crate::highlight::HIGHLIGHT`].
    pub current: usize,
}

/// Line-select overlay for the content pane: the marker line + the selection range to emphasize
/// while the copy-line-reference modal is active (AC-1, AC-7). Carried in [`ViewState`] so the
/// Presenter overlays a per-LINE style (mirroring [`ContentSearch`]), never mutating the content
/// text. Built by the Session Controller's `view_state()` from `self.modal.line_select()`.
///
/// All three fields are **1-based source-line indices**. The Presenter draws a marker caret +
/// [`crate::highlight::CURRENT_HIGHLIGHT`] on the `marker` line and a selection bar +
/// [`crate::highlight::HIGHLIGHT`] across `[start, end]`; lines scrolled off-screen are simply not
/// drawn (the `Paragraph` scroll offset clips them — the state stays whole).
pub struct LineSelectView {
    /// The marker (cursor) line — where `Enter` will anchor the reference. Rendered with a distinct
    /// caret + the stronger current-match emphasis so the user sees exactly which line is active.
    pub marker: usize,
    /// The ascending selection start (inclusive), 1-based.
    pub start: usize,
    /// The ascending selection end (inclusive), 1-based.
    pub end: usize,
    /// The character-granular selection (a mouse drag), or `None` for a whole-line (keyboard)
    /// selection. When `Some`, the overlay highlights only the selected characters on the boundary
    /// lines (and the full code of any interior line) instead of the whole `[start, end]` rows.
    pub char_sel: Option<CharSelView>,
}

/// A character-granular selection for the line-select overlay. `*_col` are char carets into the
/// displayed line (gutter included), ordered ascending by `(line, col)`; `gutter` is the leading
/// gutter width so continuation lines start their highlight at the code, not the line number.
pub struct CharSelView {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub gutter: usize,
}

/// The finder overlay's draw model (an owned snapshot of the controller's finder state).
/// Built by the Session Controller's `view_state()`.
pub struct FinderView {
    /// The current query text drawn on the input line.
    pub query: String,
    /// Matched root-relative paths, ranked best-first. Empty when the query is empty (AC-2).
    pub matches: Vec<String>,
    /// Index into `matches` of the highlighted row.
    pub cursor: usize,
    /// Raw horizontal scroll offset (columns) for the result rows. The Presenter clamps it to
    /// `max_row_width − inner_width` at draw so it can never over-scroll. The query line is
    /// NOT scrolled; this affects only the match rows.
    pub hscroll: u16,
}

/// The help overlay's draw model — an owned, borrow-free snapshot of the controller's
/// [`crate::help::HelpState`], so the Presenter holds no reference into the controller (exactly
/// like [`FinderView`]/[`PickerView`]). Built by the Session Controller's `view_state()`.
pub struct HelpView {
    /// The index of the active section in `labels` — rendered with the active-tab indicator (AC-5).
    pub active: usize,
    /// The section labels (the tab row), in order. v1: `["What's New", "About"]`.
    pub labels: Vec<String>,
    /// The active section's prerendered body (an owned clone, so the Presenter stays borrow-free).
    /// Drawn via [`Paragraph::scroll`] so a tall changelog can be read with the scrollbar.
    pub body: Text<'static>,
    /// The active section's vertical scroll offset, in **rendered (wrapped) rows** — the body draws
    /// with `Paragraph::wrap`, so `scroll_by` advances rendered rows and `clamp_scroll` bounds the
    /// offset against the wrapped `help_body_rows` (equalling raw lines only when the body doesn't
    /// wrap). The Presenter draws from it; the controller re-clamps the stored offset to the live
    /// measured body height each frame (AC-9).
    pub scroll: u16,
    /// The self-operating key-hints footer (AC-11) — at minimum how to switch sections and how to
    /// close (e.g. `"Tab/←→ switch · Esc/q close"`). Built by the controller so the Presenter stays
    /// mode-agnostic.
    pub hint: String,
    /// Center-align the active body? Set by the controller's `help_view()`: `true` only for the
    /// About section (its short identity lines read better centered), `false` for What's New (whose
    /// rendered changelog stays left-aligned). The Presenter applies it via `Paragraph::alignment`.
    pub center: bool,
}

/// The single-character git-status marker shown beside a tree row (AC-7).
///
/// Files carry their git status letter (`M`/`A`/`D`/`?`); a directory containing any change carries
/// `●` so the "dirty directory" state is distinguishable with color stripped — previously it was
/// color-only (LightRed) and lost to a colorblind user or a non-default theme. A clean directory
/// and a clean file both show a blank, so the column stays aligned across clean and dirty rows.
fn status_marker(node: &Node) -> char {
    match node.kind {
        NodeKind::Dir if node.dir_dirty => '●',
        NodeKind::File => match node.status {
            Some(Status::Modified) => 'M',
            Some(Status::Added) => 'A',
            Some(Status::Deleted) => 'D',
            Some(Status::Untracked) => '?',
            None => ' ',
        },
        _ => ' ',
    }
}

/// The display name of a node — its final path component, or the whole path for a root.
fn node_name(node: &Node) -> String {
    node.path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| node.path.to_string_lossy().into_owned())
}

/// Truncate a border title to fit a bordered block of outer width `area_width`, replacing the
/// tail with an ellipsis (`…`) when it would overflow. The interior is the outer width minus the
/// two border columns; we keep one further column of slack so the title never butts flush against
/// the corner glyph and risk pushing the border out. A title that already fits is returned
/// unchanged; a degenerate (tiny) width yields an empty string rather than a broken border.
fn truncate_title(s: &str, area_width: u16) -> String {
    // Interior width inside the two borders, minus a one-column slack so the title can't reach the
    // far corner. Saturating throughout so a 0/1/2-wide area can never underflow.
    let budget = area_width.saturating_sub(2).saturating_sub(1) as usize;
    if budget == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= budget {
        return s.to_string();
    }
    // Reserve one column for the ellipsis, so the visible result is `budget` columns total.
    let keep = budget.saturating_sub(1);
    let mut out: String = chars[..keep].iter().collect();
    out.push('…');
    out
}

/// Truncate a border title to fit a bordered block of outer width `area_width`, replacing the
/// MIDDLE with an ellipsis so BOTH ends stay visible (e.g. `fix/tree…r-hscroll`). Used for the
/// branch, where the distinctive parts are the `prefix/` and the trailing feature name — tail
/// truncation (`fix/tree-and-pi…`) would hide the latter. Same budget rule as [`truncate_title`]:
/// the interior width minus the two borders and a one-column slack. The tail gets the extra column
/// on an odd budget (the trailing feature name is usually the most distinctive part of a branch).
fn truncate_middle(s: &str, area_width: u16) -> String {
    let budget = area_width.saturating_sub(2).saturating_sub(1) as usize;
    if budget == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= budget {
        return s.to_string();
    }
    // Reserve one column for the ellipsis; split the remainder head/tail, tail favored on an odd budget.
    let keep = budget.saturating_sub(1);
    let head_len = keep / 2;
    let tail_len = keep - head_len;
    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[chars.len() - tail_len..].iter().collect();
    format!("{head}…{tail}")
}

/// The status color for a tree row: changes (modified / deleted) are light red, new files
/// (added / untracked) light green, and a directory containing any change is light red.
/// Clean rows take the default foreground.
fn row_color(node: &Node) -> Option<Color> {
    match node.kind {
        NodeKind::Dir => node.dir_dirty.then_some(Color::LightRed),
        NodeKind::File => match node.status {
            Some(Status::Modified | Status::Deleted) => Some(Color::LightRed),
            Some(Status::Added | Status::Untracked) => Some(Color::LightGreen),
            None => None,
        },
    }
}

/// The widest visible tree row, in display columns — drives the tree's horizontal scrollbar and
/// the controller's horizontal-scroll clamp. Computed from the same [`tree_row`] the tree draws
/// (selection-independent: the REVERSED highlight doesn't change a row's width), so the drawn
/// rows and the hit-test/clamp can never disagree.
fn tree_rows_max_width(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| tree_row(n, false).width())
        .max()
        .unwrap_or(0)
}

/// Render one tree row: `<marker> <indent><glyph><name>`. Indentation grows with depth so
/// the recursion is visible (AC-3); a directory carries an expand/collapse glyph; the row is
/// tinted by git status (AC-7).
fn tree_row(node: &Node, selected: bool) -> Line<'static> {
    let glyph = match node.kind {
        NodeKind::Dir if node.expanded => "▾ ",
        NodeKind::Dir => "▸ ",
        NodeKind::File => "",
    };
    let text = format!(
        "{} {}{}{}",
        status_marker(node),
        "  ".repeat(node.depth),
        glyph,
        sanitize_control(&node_name(node)),
    );
    let mut style = Style::new();
    if let Some(color) = row_color(node) {
        style = style.fg(color);
    }
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Line::from(Span::styled(text, style))
}

/// Build a [`ScrollbarState`] that places the thumb correctly for a **scroll offset** (not a list
/// selection). ratatui's thumb reaches the track end only at `position == content_length - 1`,
/// while a scroll offset maxes at `total - viewport` — so model the scroll range as its
/// `(max_scroll + 1)` distinct offsets (`content_length = total - viewport + 1`, positions
/// `0..=max_scroll`) and set `viewport_content_length` so the thumb is sized to the visible
/// fraction (`viewport / total`). The thumb then sits at the top at offset 0 and reaches the
/// bottom at the last offset (fixes the "thumb never reaches the end" misreport). Caller guarantees
/// `total > viewport`.
fn scrollbar_state(total: usize, pos: usize, viewport: usize) -> ScrollbarState {
    let content_length = total - viewport + 1;
    ScrollbarState::new(content_length)
        .position(pos.min(content_length - 1))
        .viewport_content_length(viewport)
}

// The scrollbars sit INSIDE the pane (a reserved gutter column / row, one cell off the text — see
// `bar_layout`). They are THUMB-ONLY (no track line): a half-block thumb (`▐` vertical, `▄`
// horizontal) floats in an otherwise-blank gutter, so there's no extra line running beside the
// border / above the row.
const VSCROLL_THUMB: &str = "▐";
const HSCROLL_THUMB: &str = "▄";

/// Lay out a pane interior `inner` with the scrollbars drawn *inside* it (not on the border):
/// reserve the rightmost column for a vertical bar with a one-column gap before it, and/or the
/// bottom row for a horizontal bar with a one-row gap above it. Returns the (shrunk) text rect and
/// each bar's 1-cell track. The gap keeps the bar off the text; the two bars never overlap (the
/// vbar spans only the text rows, the hbar only the text columns), leaving the inner corner blank.
fn bar_layout(inner: Rect, vbar: bool, hbar: bool) -> (Rect, Option<Rect>, Option<Rect>) {
    let text = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width.saturating_sub(if vbar { 2 } else { 0 }),
        height: inner.height.saturating_sub(if hbar { 2 } else { 0 }),
    };
    let vbar_rect = (vbar && inner.width > 0).then(|| Rect {
        x: inner.x + inner.width - 1,
        y: inner.y,
        width: 1,
        height: text.height.max(1),
    });
    let hbar_rect = (hbar && inner.height > 0).then(|| Rect {
        x: inner.x,
        y: inner.y + inner.height - 1,
        width: text.width.max(1),
        height: 1,
    });
    (text, vbar_rect, hbar_rect)
}

/// Draw a vertical scrollbar into `track` (a 1-column rect inside the pane), only when the content
/// overflows (`total > viewport`). Thumb-only: no track line or arrow glyphs, just the thumb in an
/// otherwise-blank gutter. No-op when everything fits — "a scrollbar only where there is something
/// to be moved".
fn draw_vscrollbar(frame: &mut Frame, track: Rect, total: usize, pos: usize, viewport: usize) {
    if viewport == 0 || total <= viewport {
        return;
    }
    let mut sb = scrollbar_state(total, pos, viewport);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol(VSCROLL_THUMB),
        track,
        &mut sb,
    );
}

/// Draw a horizontal scrollbar into `track` (a 1-row rect inside the pane), only when the content
/// is wider than it (`total > viewport`). Thumb-only: no track line or arrow glyphs.
fn draw_hscrollbar(frame: &mut Frame, track: Rect, total: usize, pos: usize, viewport: usize) {
    if viewport == 0 || total <= viewport {
        return;
    }
    let mut sb = scrollbar_state(total, pos, viewport);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol(HSCROLL_THUMB),
        track,
        &mut sb,
    );
}

/// The tree interior split into the text rect + optional in-pane scrollbar tracks. Overflow is
/// decided against the *reserved* space — a vbar steals width that can then require an hbar, and an
/// hbar steals height that can require a vbar — so a two-pass check settles which bars are needed.
/// Shared by [`draw_tree`] and [`geometry`] so the drawn layout and the hit-test geometry agree.
fn tree_bars(
    inner: Rect,
    nodes_len: usize,
    max_row_width: usize,
) -> (Rect, Option<Rect>, Option<Rect>) {
    let v0 = nodes_len > inner.height as usize;
    let h0 = max_row_width > inner.width as usize;
    let needs_v = nodes_len > inner.height.saturating_sub(if h0 { 2 } else { 0 }) as usize;
    let needs_h = max_row_width > inner.width.saturating_sub(if v0 { 2 } else { 0 }) as usize;
    bar_layout(inner, needs_v, needs_h)
}

/// Split the content block interior into the notices strip (top) and the content area (below it,
/// where the file + its scrollbars are drawn). Shared by [`draw_content`] and [`geometry`].
fn content_notice_split(inner: Rect, notices_len: usize) -> (Rect, Rect) {
    let max_notices = inner.height.saturating_sub(1).min(notices_len as u16);
    let parts =
        Layout::vertical([Constraint::Length(max_notices), Constraint::Min(0)]).split(inner);
    (parts[0], parts[1])
}

/// The content area split into the text rect + optional in-pane scrollbar tracks. `total_rows` is
/// the RENDERED row count (wrapped rows under `wrap`, raw lines otherwise) — so the vertical bar
/// appears whenever the file truly overflows, including a few long lines that wrap past the
/// viewport. `max_line_width` is the widest raw line; under `wrap` there is no horizontal overflow.
/// Two-pass like [`tree_bars`]. Shared by [`draw_content`] and [`geometry`].
fn content_bars(
    content_area: Rect,
    total_rows: usize,
    max_line_width: usize,
    wrap: bool,
) -> (Rect, Option<Rect>, Option<Rect>) {
    let max_w = if wrap { 0 } else { max_line_width };
    let v0 = total_rows > content_area.height as usize;
    let h0 = max_w > content_area.width as usize;
    let needs_v = total_rows > content_area.height.saturating_sub(if h0 { 2 } else { 0 }) as usize;
    let needs_h = max_w > content_area.width.saturating_sub(if v0 { 2 } else { 0 }) as usize;
    bar_layout(content_area, needs_v, needs_h)
}

/// The widest raw content line, in display columns (for the content area's horizontal overflow).
fn content_max_line_width(content: &Text<'static>) -> usize {
    content.lines.iter().map(|l| l.width()).max().unwrap_or(0)
}

/// The gutter glyph on the marker (cursor) line — a caret so the active line is visible even with
/// color stripped (the row also carries [`crate::highlight::CURRENT_HIGHLIGHT`]).
const LINE_SELECT_MARKER: char = '▶';
/// The gutter glyph on the selection rows other than the marker — a bar so the extent of the
/// selection is visible in text (the rows also carry [`crate::highlight::HIGHLIGHT`]).
const LINE_SELECT_BAR: char = '│';

/// Overlay the line-select marker + selection styling onto the content lines (read-only, AC-1/AC-7).
///
/// Mirrors [`crate::highlight::apply`]: for each source line (its 1-based index is `i + 1`), prepend
/// a one-column gutter glyph and patch a per-LINE highlight style onto the row so the marker and the
/// selection extent read both with color (the [`crate::highlight`] theme styles) and with color
/// stripped (the caret/bar glyph). The marker line gets the caret + the stronger
/// [`crate::highlight::CURRENT_HIGHLIGHT`]; the other selection rows get the bar +
/// [`crate::highlight::HIGHLIGHT`]; every other row gets a blank gutter and no style so the columns
/// stay aligned. The content text itself is never mutated — spans are cloned, only their style is
/// patched, and lines scrolled off-screen are clipped by the `Paragraph` offset (clamp-to-visible).
fn apply_line_select(lines: &[Line<'static>], ls: &LineSelectView) -> Vec<Line<'static>> {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let src = i + 1; // 1-based source line
            let is_marker = src == ls.marker;
            let in_range = src >= ls.start && src <= ls.end;
            let (glyph, style) = if is_marker {
                (LINE_SELECT_MARKER, crate::highlight::CURRENT_HIGHLIGHT)
            } else if in_range {
                (LINE_SELECT_BAR, crate::highlight::HIGHLIGHT)
            } else {
                (' ', Style::new())
            };
            let mut spans = Vec::with_capacity(line.spans.len() + 1);
            spans.push(Span::styled(glyph.to_string(), style));
            match &ls.char_sel {
                // Character selection: highlight only the selected chars on this row (the boundary
                // lines get a partial range; interior lines get all of their code). The gutter is
                // never highlighted — continuation lines start at `cs.gutter`.
                Some(cs) if in_range => {
                    let lo = if src == cs.start_line {
                        cs.start_col.max(cs.gutter)
                    } else {
                        cs.gutter
                    };
                    let hi = if src == cs.end_line {
                        cs.end_col
                    } else {
                        usize::MAX // to end of line for an interior row
                    };
                    spans.extend(patch_char_range(
                        &line.spans,
                        lo,
                        hi,
                        crate::highlight::HIGHLIGHT,
                    ));
                }
                // Whole-line (keyboard) selection, or a row outside the selection: the original
                // per-line style patch.
                _ => {
                    for s in &line.spans {
                        let patched = if is_marker || in_range {
                            s.style.patch(style)
                        } else {
                            s.style
                        };
                        spans.push(Span {
                            content: s.content.clone(),
                            style: patched,
                        });
                    }
                }
            }
            Line {
                spans,
                style: line.style,
                alignment: line.alignment,
            }
        })
        .collect()
}

/// Rebuild `spans` so the chars at 0-based char indices `[lo, hi)` carry `style` (patched onto
/// their existing style) and every other char keeps its own. Splits spans at the range boundaries,
/// grouping consecutive same-selectedness chars so the output stays compact. Char-indexed (not
/// byte- or column-indexed) to match the caret coordinates the controller produces.
fn patch_char_range(
    spans: &[Span<'static>],
    lo: usize,
    hi: usize,
    style: Style,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut idx = 0usize;
    for s in spans {
        let mut buf = String::new();
        let mut buf_selected: Option<bool> = None;
        for ch in s.content.chars() {
            let selected = idx >= lo && idx < hi;
            match buf_selected {
                Some(b) if b == selected => buf.push(ch),
                Some(b) => {
                    out.push(styled_run(&buf, s.style, b, style));
                    buf.clear();
                    buf.push(ch);
                    buf_selected = Some(selected);
                }
                None => {
                    buf.push(ch);
                    buf_selected = Some(selected);
                }
            }
            idx += 1;
        }
        if let Some(b) = buf_selected {
            out.push(styled_run(&buf, s.style, b, style));
        }
    }
    out
}

/// A span for one run of chars: the base style, patched with `style` when the run is `selected`.
fn styled_run(text: &str, base: Style, selected: bool, style: Style) -> Span<'static> {
    let s = if selected { base.patch(style) } else { base };
    Span::styled(text.to_string(), s)
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
    // Top title = the root directory basename (mirroring how the content pane titles itself from
    // the selected node), sanitized (a repo dir name is untrusted, AC-27) and truncated to the
    // column so a long name can't break the border. Fall back to "Files" only when it is empty.
    let name = sanitize_control(&state.root_name);
    let title = if name.is_empty() {
        "Files".to_string()
    } else {
        truncate_title(&name, area.width)
    };
    let mut block = Block::bordered()
        .title(title)
        .border_style(border_style(state.focus == Focus::Tree));
    // Bottom title = the current git branch, when in a repo on a real branch. Omitted entirely
    // (no `title_bottom`) outside a repo or on a detached HEAD, so the border degrades cleanly
    // rather than showing a blank/placeholder branch. Sanitized + truncated like the top title.
    if let Some(branch) = &state.branch {
        // Middle-ellipsis (not tail) so a long branch keeps both its `prefix/` and trailing feature
        // name visible when the tree column is narrow.
        block = block.title_bottom(truncate_middle(&sanitize_control(branch), area.width));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows: Vec<Line> = state
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| tree_row(node, i == state.selected))
        .collect();
    // Reserve an in-pane gutter for whichever scrollbars are needed, then render the rows into the
    // (possibly shrunk) text rect. The vertical offset scrolls minimally from last frame's offset
    // (#45) so selecting a row already in view doesn't jump the viewport; the horizontal offset
    // lets long / deeply-nested rows be read sideways (`H`/`L` scroll the tree; ←/→ are
    // expand/collapse in the tree).
    // `geometry` recomputes the SAME layout + offset, so hit-testing agrees with what is drawn.
    let max_width = tree_rows_max_width(&state.nodes);
    let (text, vbar, hbar) = tree_bars(inner, state.nodes.len(), max_width);
    let offset = sticky_scroll_offset(
        state.selected,
        state.nodes.len(),
        text.height as usize,
        state.tree_scroll as usize,
    );
    let hoff = (state.tree_hscroll as usize).min(max_width.saturating_sub(text.width as usize));
    frame.render_widget(
        Paragraph::new(rows).scroll((
            offset.min(u16::MAX as usize) as u16,
            hoff.min(u16::MAX as usize) as u16,
        )),
        text,
    );
    if let Some(track) = vbar {
        // The tree's vertical thumb tracks the CURSOR (selected index), not the viewport offset —
        // the tree has no independent vertical scroll (its position follows the selection, #45), so
        // dragging the bar scrubs the selection and the thumb must follow it. (The content vbar,
        // which has a real offset, uses that — see `draw_content`.)
        draw_vscrollbar(
            frame,
            track,
            state.nodes.len(),
            state.selected,
            text.height as usize,
        );
    }
    if let Some(track) = hbar {
        draw_hscrollbar(frame, track, max_width, hoff, text.width as usize);
    }
}

/// Draw the right column: a notices strip (if any) above the content pane. Returns the
/// content viewport `(width, height)` so the controller can clamp scrolling to it.
fn draw_content(frame: &mut Frame, area: Rect, state: &ViewState) -> (u16, u16) {
    // the title is derived from the DISPLAYED content's file (`content_title`), not the
    // live tree cursor, so it switches in lockstep with the body — the pane never shows a newly-
    // selected file's name over the previous file's body while the new render is in flight.
    // `content_title` is `None` before any file's content has landed (launch, re-root, or a
    // directory/empty selection); in that case fall back to the selected node's name (a directory)
    // or "Content" — but only when NO render is in flight, otherwise the fallback would pick up
    // the still-loading selection's name and re-introduce the title-ahead-of-body bug.
    let title = if let Some(name) = &state.content_title {
        sanitize_control(name)
    } else if !state.content_rendering {
        state
            .nodes
            .get(state.selected)
            .map(|n| sanitize_control(&node_name(n)))
            .unwrap_or_else(|| "Content".to_string())
    } else {
        "Content".to_string()
    };
    // A persistent `? help` hint rides the content block's bottom border, right-aligned —
    // one short segment, sanitized (AC-27) like the other border titles, so a new user
    // discovers the help overlay without opening it first. It shares the border
    // row (not the layout), so it never crowds the content or steals a row.
    let hint =
        Line::styled(sanitize_control(HELP_HINT), Style::new().fg(Color::Reset)).right_aligned();
    let block = Block::bordered()
        .title(title)
        .title_bottom(hint)
        .border_style(border_style(state.focus == Focus::Content));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // A notice strip (truncation AC-13, fallback AC-25) sits above the content, bounded so
    // it can never crowd out the file itself; the file + its scrollbars fill the area below it.
    let (notices_rect, content_area) = content_notice_split(inner, state.notices.len());
    if notices_rect.height > 0 {
        let notice_lines: Vec<Line> = state
            .notices
            .iter()
            .map(|n| Line::styled(sanitize_control(n), Style::new().fg(Color::Yellow)))
            .collect();
        frame.render_widget(Paragraph::new(notice_lines), notices_rect);
    }

    // Reserve an in-pane gutter for whichever scrollbars overflow, then render the file into the
    // (possibly shrunk) text rect. The VERTICAL extent is the controller's rendered row count
    // (`content_rows`) — wrapped rows under wrap, raw lines otherwise — so the bar is correct even
    // for a few long lines that wrap past the viewport. The horizontal bar uses the widest raw line
    // and is suppressed under wrap, where there is nothing to scroll sideways.
    let total_rows = state.content_rows as usize;
    let max_width = content_max_line_width(&state.content);
    let (text, vbar, hbar) = content_bars(content_area, total_rows, max_width, state.wrap);

    // Overlay the line-select marker/selection first (it is a modal — search cannot be committed
    // while it is open), then a committed search, else the content as-is. Each overlay returns the
    // same line count, so `content_rows` (computed above from `state.content`) stays valid. When
    // both are `None`, cloning `state.content` is byte-identical to the prior path, so existing
    // snapshots are unaffected (AC zero-churn invariant).
    let content_text = if let Some(ls) = &state.line_select {
        ratatui::text::Text::from(apply_line_select(&state.content.lines, ls))
    } else if let Some(cs) = &state.search {
        ratatui::text::Text::from(crate::highlight::apply(
            &state.content.lines,
            &cs.matches,
            cs.current,
        ))
    } else {
        state.content.clone()
    };
    let mut content =
        Paragraph::new(content_text).scroll((state.content_scroll, state.content_hscroll));
    if state.wrap {
        content = content.wrap(Wrap { trim: false });
    }
    frame.render_widget(content, text);

    if let Some(track) = vbar {
        draw_vscrollbar(
            frame,
            track,
            total_rows,
            state.content_scroll as usize,
            text.height as usize,
        );
    }
    if let Some(track) = hbar {
        draw_hscrollbar(
            frame,
            track,
            max_width,
            state.content_hscroll as usize,
            text.width as usize,
        );
    }
    (text.width, text.height)
}

/// Split the frame into the body (the two columns) and an optional one-row footer. The footer
/// is present exactly when an update banner is to be shown (and the frame is tall enough to
/// spare a row). Shared by [`draw`] and [`geometry`] so the drawn layout and the hit-test
/// geometry carve the same body rect — a mouse click is never mapped against stale geometry.
fn body_and_footer(area: Rect, state: &ViewState) -> (Rect, Option<Rect>) {
    if state.update_banner.is_none() || area.height < 2 {
        return (area, None);
    }
    let parts = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    (parts[0], Some(parts[1]))
}

/// Draw the one-row "update available" status line. Reversed (theme-relative) so it reads
/// as a status bar on any terminal palette — previously `Black`-on-`Cyan`, which ignored the theme
/// and could be invisible on a cyan-heavy palette. Sanitized (defense-in-depth, AC-27) and clipped
/// to its row by ratatui.
fn draw_update_footer(frame: &mut Frame, area: Rect, banner: &str) {
    let line = Line::styled(
        sanitize_control(banner),
        Style::new()
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(Paragraph::new(line), area);
}

/// Carve the optional one-row bottom prompt off the very bottom, then the optional update-banner
/// footer off what remains (so the prompt sits below the banner). Reuses [`body_and_footer`] for
/// the banner so a frame with NO prompt lays out exactly as before. Shared by [`draw`] and
/// [`geometry`] so the body rect they use can never disagree.
fn body_footer_prompt(area: Rect, state: &ViewState) -> (Rect, Option<Rect>, Option<Rect>) {
    let (above_prompt, prompt) = if state.prompt.is_some() && area.height >= 2 {
        let parts = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
        (parts[0], Some(parts[1]))
    } else {
        (area, None)
    };
    let (body, banner) = body_and_footer(above_prompt, state);
    (body, banner, prompt)
}

/// Draw the one-row bottom prompt (`Go to line: 42` / later search). Reversed (theme-relative)
/// so it reads as a prompt bar on any palette — previously `Black`-on-`Gray`, which
/// ignored the terminal theme. Sanitized (AC-27), clipped to its row.
fn draw_prompt_line(frame: &mut Frame, area: Rect, prompt: &str) {
    let line = Line::styled(
        sanitize_control(prompt),
        Style::new().add_modifier(Modifier::REVERSED),
    );
    frame.render_widget(Paragraph::new(line), area);
}

/// Below this pane width the viewer drops to a single, focused column (AC-21).
const NARROW_SPLIT: u16 = 80;

/// The column split for the current frame: `(tree_area, content_area, divider_x)`. A column
/// is `None` when not drawn (narrow layout shows only the focused one). Shared by [`draw`] and
/// [`geometry`] so the drawn layout and the hit-test geometry can never disagree.
fn columns(area: Rect, state: &ViewState) -> (Option<Rect>, Option<Rect>, Option<u16>) {
    // Zoom hides the tree entirely: the content pane fills the frame regardless of width or
    // focus, so there is no tree interior and no divider to hit-test.
    if state.zoomed {
        return (None, Some(area), None);
    }
    if area.width < NARROW_SPLIT {
        return match state.focus {
            Focus::Tree => (Some(area), None, None),
            Focus::Content => (None, Some(area), None),
        };
    }
    let tree_pct = state.split_pct.clamp(10, 90);
    let cols = Layout::horizontal([
        Constraint::Percentage(tree_pct),
        Constraint::Percentage(100 - tree_pct),
    ])
    .split(area);
    // The divider is the boundary column where the tree's right border meets the content's
    // left border (the two bordered blocks abut here).
    (Some(cols[0]), Some(cols[1]), Some(cols[1].x))
}

/// Hit-test geometry for mouse input, derived from the same split [`draw`] renders.
/// `tree_inner` is the interior where tree rows are drawn — the visible node at screen row
/// `tree_inner.y + r` is index `r + tree_scroll` (the tree scrolls to keep the selection in
/// view, #45). `content_inner` is the content column interior. `divider_x` is the draggable
/// boundary column (wide layout only).
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct PaneGeometry {
    pub area_x: u16,
    pub area_width: u16,
    pub tree_inner: Option<Rect>,
    /// The tree's vertical scroll offset (first visible node index) on the last drawn frame —
    /// the same value [`draw_tree`] scrolled by. Hit-testing adds it to map a screen row to the
    /// node actually drawn there. `0` when every node fits.
    pub tree_scroll: u16,
    /// The widest visible tree row, in columns — so the controller can clamp the tree's horizontal
    /// scroll and map a drag on the tree's horizontal scrollbar to an offset. `0` when no tree.
    pub tree_content_width: u16,
    /// The tree's in-pane vertical / horizontal scrollbar tracks (1-cell rects), present only when
    /// that bar is drawn. Hit-testing maps a press/drag on them to a scroll.
    pub tree_vbar: Option<Rect>,
    pub tree_hbar: Option<Rect>,
    /// The content text interior (below the notices strip, minus any reserved scrollbar gutter).
    pub content_inner: Option<Rect>,
    /// The content pane's in-pane scrollbar tracks (1-cell rects), present only when drawn.
    pub content_vbar: Option<Rect>,
    pub content_hbar: Option<Rect>,
    pub divider_x: Option<u16>,
    /// The screen rect where finder result rows are drawn, `None` when the finder is closed or
    /// has no rows (empty query or zero matches). Used by the controller to map a mouse click to
    /// a result row index: `row - finder_rows.y + finder_scroll` gives the match list index.
    pub finder_rows: Option<Rect>,
    /// The finder's scroll offset into the match list — the index of the first visible result row.
    /// `0` when the finder is closed or when all rows fit. Added to a click's screen-row delta to
    /// produce the absolute match-list index.
    pub finder_scroll: u16,
    /// The maximum useful HORIZONTAL scroll for the finder result rows, in columns (widest match
    /// row minus the rows-area width; `0` when rows fit or the finder is closed). Fed back so the
    /// controller clamps the *stored* `hscroll` in state each frame — without it, over-scrolling
    /// right parks the offset past the real maximum and the first few left presses appear to do
    /// nothing while it burns back down.
    pub finder_max_hscroll: u16,
    /// The finder's vertical scrollbar track rect (1-cell gutter right of the rows), present only
    /// when the match rows overflow. `None` when the finder is closed or every row fits. Lets the
    /// controller map a press/drag on the bar to a selection position (click-drag scroll).
    pub finder_vbar: Option<Rect>,
    /// The maximum useful HORIZONTAL scroll for the worktree picker rows, in columns (widest row
    /// minus the inner width; `0` when rows fit or the picker is closed). Fed back so the controller
    /// clamps the *stored* `hscroll` in state each frame — without it, over-scrolling right (Expand)
    /// parks the offset past the real maximum and the first few Collapse presses appear to do
    /// nothing while it burns back down (the same fix as `finder_max_hscroll`).
    pub picker_max_hscroll: u16,
    /// The screen rect where the help overlay's active body is drawn, `None` when the overlay is
    /// closed. Exposed for next-frame hit-testing (the tab regions are layered on top).
    pub help_body: Option<Rect>,
    /// The help body's visible viewport HEIGHT, in rows (`0` when the overlay is closed). Fed back
    /// so the controller re-clamps the stored scroll to `[0, help_body_rows − this]` each frame —
    /// the bottom bound enforced against the live measured height (AC-9).
    pub help_body_height: u16,
    /// The help body's total height in **wrapped (rendered) rows** at the body draw width (`0` when
    /// the overlay is closed). The body is drawn with `Paragraph::wrap`, so its scroll offset is in
    /// wrapped rows — fed back as the scroll's upper extent so the controller clamps against the
    /// wrapped total (not raw `lines.len()`, which would leave a long changelog's tail unreachable —
    /// AC-8/AC-9). Mirrors how the content pane clamps against its wrapped `rendered_line_count_for`.
    pub help_body_rows: u16,
    /// The help body's vertical scrollbar track rect (1-cell gutter right of the body), present only
    /// when the body overflows. `None` when the overlay is closed or the body fits.
    pub help_vbar: Option<Rect>,
    /// The screen rect of each section tab in the help overlay's top-border tab row, paired with its
    /// section index — `(index, cell_rect)`. Computed inside [`help_overlay_layout`] from the SAME
    /// widths [`draw_help_overlay`] uses (the `"Help: "` prefix + cumulative `sanitize_control(label)`
    /// widths + `HELP_TAB_SEP`), so a click maps to the tab actually drawn. Empty when the overlay is
    /// closed. The controller hit-tests a left-click against these to switch sections (AC-10).
    pub help_tabs: Vec<(usize, Rect)>,
}

/// Compute the [`PaneGeometry`] for hit-testing the current frame — the same layout [`draw`]
/// renders, so a click is never mapped against stale geometry. The interior of a bordered
/// block is its area inset by one cell on each side (the title does not change it).
pub fn geometry(area: Rect, state: &ViewState) -> PaneGeometry {
    let (body, _footer, _prompt) = body_footer_prompt(area, state);
    let (tree, content, divider_x) = columns(body, state);
    let inner = |r: Rect| Block::bordered().inner(r);

    // Tree: the SAME layout `draw_tree` computes (text rect + in-pane bar tracks), so a click maps
    // to the row actually drawn and a press lands on the bar actually shown. The scroll offset is
    // derived identically (over the reduced text height + last frame's offset). Saturating casts:
    // an absurd >65535 value clamps instead of wrapping.
    let max_width = tree_rows_max_width(&state.nodes);
    let (tree_inner, tree_vbar, tree_hbar) = match tree.map(inner) {
        Some(ti) => {
            let (text, v, h) = tree_bars(ti, state.nodes.len(), max_width);
            (Some(text), v, h)
        }
        None => (None, None, None),
    };
    let tree_scroll = tree_inner.map_or(0, |t| {
        sticky_scroll_offset(
            state.selected,
            state.nodes.len(),
            t.height as usize,
            state.tree_scroll as usize,
        )
        .min(u16::MAX as usize) as u16
    });
    let tree_content_width = if tree_inner.is_some() {
        max_width.min(u16::MAX as usize) as u16
    } else {
        0
    };

    // Content: notices split, then the same bar layout `draw_content` computes.
    let (content_inner, content_vbar, content_hbar) = match content.map(inner) {
        Some(ci) => {
            let (_notices, content_area) = content_notice_split(ci, state.notices.len());
            let (text, v, h) = content_bars(
                content_area,
                state.content_rows as usize,
                content_max_line_width(&state.content),
                state.wrap,
            );
            (Some(text), v, h)
        }
        None => (None, None, None),
    };

    // Finder: if the finder overlay is open, compute its layout with the same helper
    // `draw_finder_overlay` uses (same `area` = `frame.area()` = the full terminal rect),
    // so the hit-test geometry agrees with what is drawn.
    let (finder_rows, finder_scroll, finder_max_hscroll, finder_vbar) = match &state.finder {
        Some(finder) => {
            let fl = finder_overlay_layout(area, finder);
            (
                fl.rows_area,
                fl.offset.min(u16::MAX as usize) as u16,
                fl.max_hscroll,
                fl.vbar,
            )
        }
        None => (None, 0, 0, None),
    };

    // Picker: the SAME helper `draw_picker_overlay` uses, so the fed-back `max_hscroll` matches what
    // is drawn — the controller clamps the stored picker hscroll to it each frame. `0`
    // when the picker is closed.
    let picker_max_hscroll = match &state.picker {
        Some(picker) => picker_overlay_layout(area, picker).max_hscroll,
        None => 0,
    };

    // Help: the SAME helper `draw_help_overlay` uses, so the fed-back body HEIGHT matches what is
    // drawn — the controller clamps the stored scroll to `[0, body_lines − height]` each frame, the
    // bottom bound enforced against the live measured height (AC-9). All `None`/`0` when the overlay is closed.
    let (help_body, help_body_height, help_body_rows, help_vbar, help_tabs) = match &state.help {
        Some(help) => {
            let hl = help_overlay_layout(area, help);
            let height = hl.body.map_or(0, |b| b.height);
            (hl.body, height, hl.body_rows, hl.vbar, hl.tabs)
        }
        None => (None, 0, 0, None, Vec::new()),
    };

    PaneGeometry {
        area_x: body.x,
        area_width: body.width,
        tree_inner,
        tree_scroll,
        tree_content_width,
        tree_vbar,
        tree_hbar,
        content_inner,
        content_vbar,
        content_hbar,
        divider_x,
        finder_rows,
        finder_scroll,
        finder_max_hscroll,
        finder_vbar,
        picker_max_hscroll,
        help_body,
        help_body_height,
        help_body_rows,
        help_vbar,
        help_tabs,
    }
}

/// Draw the viewer for the given state, returning the content viewport `(width, height)`
/// the content pane was drawn into — `(0, 0)` when the content pane is not visible (narrow
/// layout with the tree focused). The controller uses it to clamp content scrolling.
///
/// At ≥ 80 columns both columns are shown side by side. Narrower than that, only the focused
/// column is drawn — full width — so the active content stays readable (AC-21). The split is
/// taken from the **live frame width** (via [`columns`]), so it can never disagree with the
/// geometry it is drawn into (a stale `state.width` cannot desync the layout).
pub fn draw(frame: &mut Frame, state: &ViewState) -> (u16, u16) {
    let (body, footer, prompt_area) = body_footer_prompt(frame.area(), state);
    if let (Some(area), Some(banner)) = (footer, state.update_banner.as_deref()) {
        draw_update_footer(frame, area, banner);
    }
    if let (Some(area), Some(prompt)) = (prompt_area, state.prompt.as_deref()) {
        draw_prompt_line(frame, area, prompt);
    }
    let (tree, content, _divider) = columns(body, state);
    if let Some(area) = tree {
        draw_tree(frame, area, state);
    }
    let dims = match content {
        Some(area) => draw_content(frame, area, state),
        None => (0, 0),
    };
    // The worktree picker is a modal overlay: drawn last, on TOP of whatever columns are
    // visible (AC-1, AC-5), so it is never obscured by the layout beneath it.
    if let Some(picker) = &state.picker {
        draw_picker_overlay(frame, frame.area(), picker);
    }
    // The go-to-file finder is also a modal overlay (AC-1). Only one modal is ever open, but
    // an independent check is correct — if both are somehow set, both draw (last wins).
    if let Some(finder) = &state.finder {
        draw_finder_overlay(frame, frame.area(), finder);
    }
    // The in-app help overlay draws LAST — on top of the picker/finder (AC-1, AC-5).
    if let Some(help) = &state.help {
        draw_help_overlay(frame, frame.area(), help);
    }
    dims
}

/// A `Rect` of outer size `w` × `h`, centered in `area` and clamped so it never exceeds `area`.
/// Used to size the picker overlay to its content (caller passes content + borders), capped at
/// the pane. All math is saturating, so a frame smaller than the requested box never panics —
/// the box simply shrinks to fit (down to a zero-area rect at a degenerate frame). Centering
/// rounds the leftover margin down, biasing the box toward the top-left by at most one cell.
fn centered_rect_sized(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// The base frame every modal overlay (picker, finder, help) is drawn in: a bordered block with
/// uniform [`PICKER_PADDING`]. The `*_overlay_layout` fns measure `inner` from it; the draw fns
/// start from it and add the titles + [`modal_border_style`] on top. Titles render on the
/// already-reserved border rows and the accent is style-only, so neither changes the interior
/// geometry — the measured interior and the drawn interior therefore always agree.
fn modal_frame() -> Block<'static> {
    Block::bordered().padding(Padding::uniform(PICKER_PADDING))
}

/// The shared modal border accent — blue + bold — so the picker, finder, and help overlays read
/// as the same kind of surface and the accent is defined once.
fn modal_border_style() -> Style {
    Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD)
}

/// Uniform inner padding (cells on every side) between the picker rows and the box border, so the
/// content reads with a little breathing room — matching herdr's indented popup content. Applied
/// both horizontally (a column gutter each side) and vertically (a blank row above the first row
/// and below the last), so the rows never sit flush against the border or the title/footer chrome.
const PICKER_PADDING: u16 = 1;
/// The picker's top-left title (the box label).
const PICKER_TITLE: &str = "Switch worktree";
/// The herdr-style `esc close` chip on the top border (right-aligned, dim chrome).
const PICKER_ESC_CLOSE: &str = "esc close";
/// The herdr-style key-hint footer on the bottom border — the picker's real bindings, with
/// herdr's ` · ` separator. Up/Down move the cursor, Left/Right horizontal-scroll, Enter
/// confirms the switch, Esc cancels. Static (not repo-derived), so no sanitization is needed.
const PICKER_FOOTER_HINT: &str = "↑↓ move · ←→ scroll · ⏎ switch · esc cancel";

/// The computed layout geometry of the worktree picker overlay, shared between
/// [`draw_picker_overlay`] and [`geometry`] so neither can drift from the other — mirroring
/// [`FinderLayout`]. Both functions call [`picker_overlay_layout`] and operate on these rects.
struct PickerLayout {
    /// The full popup outer rect (after centering + clamping to `area`). Used by `draw` to
    /// `Clear` the region and render the bordered block.
    popup: Rect,
    /// The padded interior the rows are drawn into (the popup minus borders + uniform padding).
    inner: Rect,
    /// The scroll offset: the index of the first visible row (keeps the cursor in view). `0` when
    /// every row fits.
    offset: usize,
    /// The maximum useful HORIZONTAL scroll for the rows, in columns: the widest row minus the
    /// inner width (`0` when every row fits). The single source of truth for the clamp —
    /// [`draw_picker_overlay`] clamps the displayed offset to it AND [`geometry`] feeds it back so
    /// the controller clamps the *stored* `hscroll` to the same value, so the two can never
    /// disagree (which is what made an over-scroll-right need several left presses to undo).
    max_hscroll: u16,
}

/// Compute the worktree picker overlay's layout geometry for the given frame `area` and `picker`
/// draw model. This is the **single authoritative place** for the picker's sizing + centering +
/// scroll math — both [`draw_picker_overlay`] and [`geometry`] call it, so the drawn rects and
/// the hit-test / clamp geometry are guaranteed to agree (mirrors [`finder_overlay_layout`]).
fn picker_overlay_layout(area: Rect, picker: &PickerView) -> PickerLayout {
    // Build every row once to measure widths (size-to-content), exactly as draw does.
    let rows: Vec<Line> = picker
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| picker_row(row, i == picker.cursor))
        .collect();

    // Chrome widths (same as draw): the title + `esc close` chip on top, the key-hint footer below.
    let hint_style = Style::new().fg(Color::Reset);
    let top_left = Line::from(PICKER_TITLE);
    let top_right = Line::styled(PICKER_ESC_CLOSE, hint_style).right_aligned();
    let footer = Line::styled(PICKER_FOOTER_HINT, hint_style).centered();

    let max_row_width = rows.iter().map(Line::width).max().unwrap_or(0);
    let min_top = top_left.width() + 1 + top_right.width();
    let min_bottom = footer.width();
    let desired_inner_w = max_row_width
        .max(min_top)
        .max(min_bottom)
        .min(u16::MAX as usize) as u16;
    let desired_inner_h = (rows.len().min(u16::MAX as usize) as u16).max(1);
    let want_w = desired_inner_w
        .saturating_add(2)
        .saturating_add(PICKER_PADDING * 2);
    let want_h = desired_inner_h
        .saturating_add(2)
        .saturating_add(PICKER_PADDING * 2);
    let cap_w = area.width.saturating_sub(2);
    let cap_h = area.height.saturating_sub(2);
    let popup = centered_rect_sized(want_w.min(cap_w), want_h.min(cap_h), area);

    let block = modal_frame();
    let inner = block.inner(popup);

    let visible = inner.height as usize;
    let offset = scroll_offset(picker.cursor, picker.rows.len(), visible);

    // Max useful horizontal scroll = widest ROW minus the inner width. NOT against `desired_inner_w`,
    // which is inflated by the title/footer chrome — clamping there would let scroll-right push the
    // rows off-screen on a narrow pane even when every row fits. Saturating, so a narrow box never
    // underflows.
    let max_hscroll = (max_row_width.min(u16::MAX as usize) as u16).saturating_sub(inner.width);

    PickerLayout {
        popup,
        inner,
        offset,
        max_hscroll,
    }
}

/// Draw the worktree picker as a centered, bordered list overlay on top of the columns (AC-1,
/// AC-5). Each row is `<path> [branch]`, or `<path> (detached)` when HEAD is detached — never
/// an empty branch (AC-2). The `cursor` row is highlighted (`REVERSED`, the same
/// idiom `tree_row` uses for the tree selection). The path and branch are both run through
/// `sanitize_control` first, so a worktree path or branch name carrying control bytes cannot
/// move the cursor or spoof the UI (AC-27, defense-in-depth — exactly as the tree does).
///
/// The box is **sized to its content** (the widest rendered row × the row count, plus the
/// border, title, and uniform inner padding), then **clamped to the frame** with a small margin
/// and **centered** — so a
/// few short worktrees draw a tidy box and many long paths grow up to the pane, then cap. It is
/// recomputed every draw, so it is fully responsive to a pane resize.
///
/// When there are more rows than the popup interior is tall (herdr's multi-agent repos have
/// many worktrees), the row window scrolls so the `cursor` row is always visible (AC-5). When a
/// row is wider than the (capped) interior, `picker.hscroll` shifts the rows horizontally so a
/// long path can be read sideways; it is clamped here to `[0, max_row_width - inner_width]`, so
/// it is a no-op while everything fits and can never over-scroll past the widest row.
///
/// herdr-style chrome surrounds the box: a dim `esc close` chip on the top border (right) and a
/// dim `·`-separated footer of the picker's real keys on the bottom border (AC discoverability).
/// Both are Block titles, never inner rows, so the rows area / scroll are untouched; the
/// size-to-content calc widens the box to fit them so short rows don't clip the chrome.
fn draw_picker_overlay(frame: &mut Frame, area: Rect, picker: &PickerView) {
    // Delegate all sizing + centering + scroll math to the shared layout helper, so this function
    // and `geometry()` can never drift from each other (mirrors `draw_finder_overlay`).
    let layout = picker_overlay_layout(area, picker);

    // Re-build every row for rendering (the layout helper built them only for measurement;
    // `Line` is not `Copy`, so it can't return them).
    let rows: Vec<Line> = picker
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| picker_row(row, i == picker.cursor))
        .collect();

    // herdr-style chrome: an `esc close` chip on the top border (right) and a key-hint footer on
    // the bottom border. These are static affordances (not repo-derived), rendered as Block titles
    // — never inner rows — so the rows area / scroll above are untouched. The two hint strings are
    // drawn in the default/terminal foreground — the SAME color as the worktree path text in the
    // rows. We set `Color::Reset` explicitly: title spans inherit the Block's (blue) border style,
    // so without an override they would tint blue rather than match the un-styled path text. (The
    // current-marker cyan and the agent badges keep their own colors — only the hints change.)
    let hint_style = Style::new().fg(Color::Reset);
    let top_left = Line::from(PICKER_TITLE);
    let top_right = Line::styled(PICKER_ESC_CLOSE, hint_style).right_aligned();
    let footer = Line::styled(PICKER_FOOTER_HINT, hint_style).centered();

    // Clear whatever the columns drew beneath the popup so it reads as a true modal.
    frame.render_widget(Clear, layout.popup);

    // The shared modal frame already carries the 1-cell uniform gutter (so the rows aren't flush
    // against the border or the title/footer chrome on the border rows); `inner()` subtracts it
    // all round, so the rows, cursor highlight, current marker, agent badge, and scrollbars all
    // flow from the padded interior below.
    let block = modal_frame()
        .title_top(top_left)
        .title_top(top_right)
        .title_bottom(footer)
        .border_style(modal_border_style());
    frame.render_widget(block, layout.popup);

    let visible = layout.inner.height as usize;
    // Clamp the displayed hscroll to `layout.max_hscroll` — the SAME value the controller clamps the
    // stored offset to (via geometry feedback), so display and state never disagree. A no-op when
    // every row fits, and never scrolls past the widest row.
    let hscroll = picker.hscroll.min(layout.max_hscroll);

    let window: Vec<Line> = rows.into_iter().skip(layout.offset).take(visible).collect();
    // `Paragraph::scroll((y, x))` clips the leading `x` columns off each line — the horizontal
    // read for long paths. The vertical window is already applied by skip/take, so y stays 0.
    frame.render_widget(Paragraph::new(window).scroll((0, hscroll)), layout.inner);
}

/// The first row index to render so the `cursor` row stays within a window of `visible` rows,
/// **anchoring** the window to the cursor (cursor in the first page ⇒ offset 0). Returns 0 when
/// every row fits (or the window is degenerate), and never scrolls past the end. Used by the
/// worktree picker, whose cursor only ever moves by keyboard (no jump to worry about).
fn scroll_offset(cursor: usize, len: usize, visible: usize) -> usize {
    if visible == 0 || len <= visible {
        return 0;
    }
    // Keep the cursor in view: if it sits below the window, scroll down just enough; if above,
    // scroll up to it. Clamp so the last window ends at the final row.
    let max_offset = len - visible;
    if cursor < visible {
        0
    } else {
        (cursor + 1 - visible).min(max_offset)
    }
}

/// Like [`scroll_offset`] but scrolls **minimally** from the `current` offset: if the cursor is
/// already inside `[current, current + visible)` the offset does not move. The file tree uses this
/// so selecting a row that is already on screen — e.g. a mouse click — never jumps the viewport
/// (a jump would also make a double-click land on the wrong row). Off-screen, it scrolls just
/// enough to bring the cursor to the nearest edge. Clamped to `[0, len - visible]`.
fn sticky_scroll_offset(cursor: usize, len: usize, visible: usize, current: usize) -> usize {
    if visible == 0 || len <= visible {
        return 0;
    }
    let max_offset = len - visible;
    let current = current.min(max_offset);
    let offset = if cursor < current {
        cursor // above the window → bring the cursor to the top edge
    } else if cursor >= current + visible {
        cursor + 1 - visible // below the window → bring it to the bottom edge
    } else {
        current // already visible → don't move (no jump on a click)
    };
    offset.min(max_offset)
}

/// The color for an agent-status badge: `working`/`done` green, `idle` blue, `blocked` red,
/// anything else (incl. `unknown`) gray. Keeps the badge legible at a glance without overloading
/// the row's meaning.
fn agent_badge_color(status: &str) -> Color {
    match status {
        "working" | "done" => Color::Green,
        "idle" => Color::Blue,
        "blocked" => Color::Red,
        _ => Color::Gray,
    }
}

/// Render one picker row as `<current-marker> <path> [branch]|(detached) <(current)> <agent-badge>`:
///
/// - a leading **current marker** (`●` in cyan) when the row is the worktree the viewer is rooted
///   at, else a blank — visually distinct from the selection cursor, which stays `REVERSED` on the
///   highlighted row (AC-18). A row can be current without being selected and vice versa.
/// - the path + branch (or `(detached)` when HEAD is detached, AC-2), both sanitized (AC-27).
/// - a trailing **`(current)` text label** on the current row, so the "current worktree"
///   state is distinguishable with color stripped — previously the `●` was color-only (cyan) and a
///   colorblind user or a non-default theme could miss it. The label rides after the path/branch.
/// - a trailing **agent badge** (`● <status>`, colored by status) when the worktree's workspace
///   hosts a real agent, else nothing (AC-19). The status is sanitized too (defense-in-depth).
///
/// The whole row is `REVERSED` when it is the cursor row (the same idiom `tree_row` uses).
fn picker_row(row: &PickerRowView, selected: bool) -> Line<'static> {
    let path = sanitize_control(&row.path);
    let suffix = match &row.branch {
        Some(branch) => format!(" [{}]", sanitize_control(branch)),
        None if row.detached => " (detached)".to_string(),
        // No branch and not detached: show nothing rather than an empty `[]` (defensive).
        None => String::new(),
    };

    // Row-wide style: the REVERSED cursor highlight applies to every span on the selected row.
    let base = if selected {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    // Leading current marker (AC-18): a cyan ● when current, two spaces otherwise so the path
    // column stays aligned across current and non-current rows. The ● is a glyph cue; the
    // trailing `(current)` label (below) is the color-stripped cue.
    if row.is_current {
        spans.push(Span::styled("● ", base.fg(Color::Cyan)));
    } else {
        spans.push(Span::styled("  ", base));
    }
    spans.push(Span::styled(format!("{path}{suffix}"), base));
    // Trailing `(current)` text label: a non-color cue on the current row so the
    // "current worktree" state survives a colorblind palette or a non-default theme. Rendered in
    // the row's base style (so it picks up the REVERSED cursor highlight when selected).
    if row.is_current {
        spans.push(Span::styled(" (current)", base));
    }
    // Trailing agent badge (AC-19): colored by status, sanitized (AC-27).
    if let Some(status) = &row.agent {
        let status = sanitize_control(status);
        spans.push(Span::styled(
            format!("  ● {status}"),
            base.fg(agent_badge_color(&status)),
        ));
    }
    Line::from(spans)
}

/// The finder overlay's top-left title (the box label).
const FINDER_TITLE: &str = "Go to file";
/// The herdr-style key-hint footer on the bottom border — the finder's real bindings, including
/// the horizontal scroll keys (←→) added alongside the result-row hscroll feature.
const FINDER_FOOTER_HINT: &str = "↑↓ move · ←→ scroll · ⏎ open · esc cancel";
/// The prompt prefix shown on the query-input line.
const FINDER_PROMPT: &str = "> ";
/// The placeholder shown on the query-input line when the query is empty (AC-2).
const FINDER_PLACEHOLDER: &str = "> type to find a file…";

/// The help overlay's top-left title (the box label).
const HELP_TITLE: &str = "Help";
/// The persistent discoverability hint shown on the content pane's bottom border — a
/// right-aligned one-segment affordance that `?` opens help, visible on the default screen
/// without opening any modal. Static (first-party), so no sanitization is needed
/// beyond the defense-in-depth `sanitize_control` applied at the call site (AC-27).
const HELP_HINT: &str = "? help";
/// The help overlay's desired interior WIDTH (columns) before clamping to the frame. A generous
/// fixed size (the changelog/about bodies are unbounded — the box does NOT size to content like the
/// finder; it clamps to the frame and the body scrolls).
const HELP_WANT_INNER_W: u16 = 72;
/// The help overlay's desired interior HEIGHT (rows) before clamping to the frame.
const HELP_WANT_INNER_H: u16 = 20;
/// The separator between section tabs in the help overlay's top border.
const HELP_TAB_SEP: &str = "  ";
/// The leading marker prepended to the ACTIVE help tab so the active section is distinguishable
/// with color stripped — previously it was REVERSED+BOLD only, which a colorblind user
/// or a non-default theme could lose. Inactive tabs carry no marker, so the active one stands out
/// by glyph alone. Drawn in the SAME `Color::Reset` style as the tab label, and counted in
/// [`help_tab_rects`] so the hit-test rect tracks the drawn width.
const HELP_ACTIVE_MARKER: &str = "▶ ";

/// The columns [`bar_layout`] reserves for a vertical scrollbar when the body overflows: a 1-cell
/// gap before the bar + the 1-cell bar itself (see `bar_layout`'s `saturating_sub(2)`).
const VSCROLL_GUTTER_W: u16 = 2;

/// The help overlay's body **text width** (columns) on a standard terminal — the unclamped interior
/// ([`HELP_WANT_INNER_W`]) minus the vertical-scrollbar gutter that [`bar_layout`] reserves. The
/// changelog body always overflows the fixed-height box, so the vbar is always present and the body
/// is always drawn this narrow. This is the SINGLE source the help layout AND the What's New glow
/// `-w` both derive from, so glow wraps its markdown to exactly the width the body is drawn at — the
/// Presenter's `Paragraph::wrap` then becomes a no-op and glow's hanging indents survive (the box is
/// fixed-width, so this never changes on resize for a terminal ≥ ~76 cols; `Paragraph::wrap` is the
/// graceful fallback for narrower frames where the box clamps below this width).
pub(crate) const fn help_body_text_width() -> u16 {
    HELP_WANT_INNER_W.saturating_sub(VSCROLL_GUTTER_W)
}

/// The computed layout geometry of the finder overlay, shared between [`draw_finder_overlay`]
/// and [`geometry`] so neither can drift from the other. Both functions call
/// [`finder_overlay_layout`] and operate on the returned rects.
struct FinderLayout {
    /// The full popup outer rect (after centering + clamping to `area`). Used by `draw` to
    /// `Clear` the region and render the bordered block.
    popup: Rect,
    /// The single-row rect for the query-input line (first row of the block interior).
    query_area: Rect,
    /// The rect where result rows are rendered (the interior below the query line), or `None`
    /// when the interior has no room for rows or when there are no match rows.
    rows_area: Option<Rect>,
    /// The scroll offset: the index of the first visible match row. `0` when all rows fit.
    offset: usize,
    /// The maximum useful HORIZONTAL scroll for the result rows, in columns: the widest match row
    /// minus the rows-area width (`0` when every row fits or there are none). The single source of
    /// truth for the clamp — `draw_finder_overlay` clamps the displayed offset to it AND `geometry`
    /// feeds it back so the controller clamps the *stored* offset to the same value, so the two can
    /// never disagree (which is what made an over-scroll-right need several left presses to undo).
    max_hscroll: u16,
    /// The vertical scrollbar track rect (the 1-cell gutter column right of the rows), present only
    /// when the match rows overflow the visible height. The SAME rect `draw_finder_overlay` renders
    /// the scrollbar into and `geometry` feeds back, so a press/drag on it hit-tests where it's drawn.
    vbar: Option<Rect>,
}

/// Compute the finder overlay's layout geometry for the given frame `area` and `finder` draw
/// model. This is the **single authoritative place** for all the sizing + centering + scroll
/// math — both [`draw_finder_overlay`] and [`geometry`] call it, so the drawn rects and the
/// hit-test geometry are guaranteed to agree.
fn finder_overlay_layout(area: Rect, finder: &FinderView) -> FinderLayout {
    // Build the query line for width measurement (same logic as draw).
    let query_line: Line<'static> = if finder.query.is_empty() {
        Line::styled(
            FINDER_PLACEHOLDER.to_string(),
            Style::new().add_modifier(Modifier::DIM),
        )
    } else {
        let display_query = sanitize_control(&finder.query);
        Line::from(format!("{FINDER_PROMPT}{display_query}"))
    };

    // Build match lines for width measurement.
    let match_lines: Vec<Line<'static>> = finder
        .matches
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let text = sanitize_control(path);
            let style = if i == finder.cursor {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            Line::styled(text, style)
        })
        .collect();

    // Chrome widths (same as draw_finder_overlay). No top-right chip — the footer is the single
    // home for all key hints.
    let hint_style = Style::new().fg(Color::Reset);
    let top_left = Line::from(FINDER_TITLE);
    let footer = Line::styled(FINDER_FOOTER_HINT, hint_style).centered();

    let query_w = query_line.width();
    let max_row_w = match_lines.iter().map(Line::width).max().unwrap_or(0);
    let min_top = top_left.width();
    let min_bottom = footer.width();
    let desired_inner_w = query_w
        .max(max_row_w)
        .max(min_top)
        .max(min_bottom)
        .min(u16::MAX as usize) as u16;
    let desired_inner_h = (1 + match_lines.len().min(u16::MAX as usize) as u16).max(1);
    let want_w = desired_inner_w
        .saturating_add(2)
        .saturating_add(PICKER_PADDING * 2);
    let want_h = desired_inner_h
        .saturating_add(2)
        .saturating_add(PICKER_PADDING * 2);
    let cap_w = area.width.saturating_sub(2);
    let cap_h = area.height.saturating_sub(2);
    let popup = centered_rect_sized(want_w.min(cap_w), want_h.min(cap_h), area);

    let block = modal_frame();
    let inner = block.inner(popup);

    // The query line always occupies the first row of the interior (when it fits).
    let (rows_area, offset) = if inner.height == 0 {
        (None, 0)
    } else {
        let query_area_height = 1u16;
        let remaining = inner.height.saturating_sub(query_area_height);
        if remaining == 0 || match_lines.is_empty() {
            (None, 0)
        } else {
            let ra = Rect {
                x: inner.x,
                y: inner.y + query_area_height,
                width: inner.width,
                height: remaining,
            };
            let visible = ra.height as usize;
            let off = scroll_offset(finder.cursor, match_lines.len(), visible);
            (Some(ra), off)
        }
    };

    let query_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: if inner.height > 0 { 1 } else { 0 },
    };

    // Max useful horizontal scroll = widest match row (over ALL matches, `max_row_w`) minus the
    // rows-area width. `0` when there are no rows or everything fits.
    let max_hscroll = match rows_area {
        Some(ra) => (max_row_w.min(u16::MAX as usize) as u16).saturating_sub(ra.width),
        None => 0,
    };

    // Vertical scrollbar track (the gutter column right of the rows), present only when the match
    // rows overflow the visible height — the SAME rect draw renders into and geometry feeds back.
    let vbar = match rows_area {
        Some(ra) if match_lines.len() > ra.height as usize => Some(Rect {
            x: ra.x + ra.width,
            y: ra.y,
            width: 1,
            height: ra.height,
        }),
        _ => None,
    };

    FinderLayout {
        popup,
        query_area,
        rows_area,
        offset,
        max_hscroll,
        vbar,
    }
}

/// Draw the go-to-file finder as a centered, bordered overlay on top of the columns (AC-1).
///
/// The interior (top to bottom) is:
///   1. A **query-input line**: `"> "` + the current query text (both through `sanitize_control`
///      for AC-27 parity). When the query is empty a dim placeholder replaces the prompt.
///   2. **Match rows**: each matched root-relative path run through `sanitize_control` (AC-5, AC-27);
///      the `cursor` row is highlighted with REVERSED — the same idiom the picker uses. When
///      `matches` is empty (empty query or no hit) no rows are drawn (AC-2).
///
/// Reuses [`centered_rect_sized`], [`scroll_offset`], `PICKER_PADDING`, and the Scrollbar/
/// Block primitives from the picker overlay — no duplication of their internals.
fn draw_finder_overlay(frame: &mut Frame, area: Rect, finder: &FinderView) {
    // Delegate all sizing + centering + scroll math to the shared layout helper, so this
    // function and `geometry()` can never drift from each other.
    let layout = finder_overlay_layout(area, finder);

    // Build the query line for rendering (same logic as the layout helper, which built it only
    // for measurement). Re-built here because `Line` is not `Copy` and the helper doesn't need
    // to return it.
    let query_line: Line<'static> = if finder.query.is_empty() {
        // Empty query: dim placeholder (AC-2).
        Line::styled(
            FINDER_PLACEHOLDER.to_string(),
            Style::new().add_modifier(Modifier::DIM),
        )
    } else {
        let display_query = sanitize_control(&finder.query);
        Line::from(format!("{FINDER_PROMPT}{display_query}"))
    };

    // Build match rows for rendering (AC-5, AC-27).
    let match_lines: Vec<Line<'static>> = finder
        .matches
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let text = sanitize_control(path);
            let style = if i == finder.cursor {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            Line::styled(text, style)
        })
        .collect();

    // Chrome: static strings, no sanitization needed. Only FINDER_TITLE on the top border —
    // the `esc cancel` chip has been removed so it does not duplicate the footer hint.
    let hint_style = Style::new().fg(Color::Reset);
    let top_left = Line::from(FINDER_TITLE);
    let footer = Line::styled(FINDER_FOOTER_HINT, hint_style).centered();

    // Clear whatever the columns drew beneath the popup so it reads as a true modal.
    frame.render_widget(Clear, layout.popup);

    let block = modal_frame()
        .title_top(top_left)
        .title_bottom(footer)
        .border_style(modal_border_style());
    frame.render_widget(block, layout.popup);

    // Render the query line if the interior is tall enough.
    if layout.query_area.height > 0 {
        frame.render_widget(Paragraph::new(query_line), layout.query_area);
    }

    // Render match rows if the layout allocated space for them.
    if let Some(rows_area) = layout.rows_area {
        let visible = rows_area.height as usize;
        let offset = layout.offset;
        let window: Vec<Line<'static>> =
            match_lines.into_iter().skip(offset).take(visible).collect();

        // Clamp the displayed hscroll to `layout.max_hscroll` — the SAME value the controller
        // clamps the stored offset to (via geometry feedback), so display and state never disagree.
        // A no-op when every row fits, and never scrolls past the widest match row.
        // `Paragraph::scroll((0, x))` clips the leading `x` columns off each line so long paths
        // can be read sideways.
        let hscroll = finder.hscroll.min(layout.max_hscroll);
        frame.render_widget(Paragraph::new(window).scroll((0, hscroll)), rows_area);

        // Vertical scrollbar when match rows overflow. The track rect is `layout.vbar` — the same
        // rect `geometry` feeds back so a press/drag on it hit-tests where it is drawn. It tracks the
        // cursor position (not the viewport offset) so it follows the selection, like the tree bar.
        if let Some(sb_area) = layout.vbar {
            let sb_state = scrollbar_state(finder.matches.len(), finder.cursor, visible);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .thumb_symbol("▐")
                    .track_symbol(None)
                    .begin_symbol(None)
                    .end_symbol(None),
                sb_area,
                &mut sb_state.clone(),
            );
        }
    }
}

/// The computed layout geometry of the help overlay, shared between [`draw_help_overlay`] and
/// [`geometry`] so neither can drift from the other — mirroring [`FinderLayout`]/[`PickerLayout`].
/// Both functions call [`help_overlay_layout`] and operate on the returned rects.
struct HelpLayout {
    /// The full popup outer rect (after centering + clamping to `area`). `draw` clears it + draws
    /// the bordered block.
    popup: Rect,
    /// The padded interior where the body is drawn (the popup minus borders + uniform padding,
    /// minus any reserved scrollbar gutter). `None` when the interior is degenerate (too small).
    body: Option<Rect>,
    /// The vertical scrollbar track rect (1-cell gutter right of the body), present only when the
    /// body overflows the visible height. The SAME rect `draw` renders into and `geometry` feeds back.
    vbar: Option<Rect>,
    /// The body's total height in **wrapped (rendered) rows** at the ACTUAL drawn body width (i.e.
    /// post scrollbar-gutter — `text.width`, not the full `inner.width`) — the extent the scroll
    /// offset and scrollbar must be measured against, since the body is drawn with `Paragraph::wrap`
    /// (raw `lines.len()` undercounts and leaves a long changelog's tail unreachable). `0` when the
    /// interior is degenerate. Fed back via `PaneGeometry::help_body_rows`.
    body_rows: u16,
    /// Each section tab's screen rect in the top-border tab row, paired with its section index —
    /// `(index, cell_rect)`. Derived from the SAME widths `draw_help_overlay` renders the tab Line
    /// with (the `"Help: "` prefix + cumulative `sanitize_control(label)` widths + `HELP_TAB_SEP`), so
    /// the drawn tabs and the hit-test rects can never drift. Each rect is the tab label's own cells
    /// (1 row tall, at `popup.y`); a tab clipped past the popup's right edge is dropped.
    tabs: Vec<(usize, Rect)>,
}

/// Compute the help overlay's layout geometry for the given frame `area` and `help` draw model.
/// This is the **single authoritative place** for the help overlay's sizing + centering — both
/// [`draw_help_overlay`] and [`geometry`] call it, so the drawn rects and the hit-test / clamp
/// geometry are guaranteed to agree (mirrors [`finder_overlay_layout`]).
///
/// Unlike the finder/picker (size-to-content), the help overlay is a **fixed centered box** clamped
/// to the frame — the changelog body is unbounded, so the box does not grow with it; the body
/// scrolls instead.
fn help_overlay_layout(area: Rect, help: &HelpView) -> HelpLayout {
    // Fixed desired size (+ border + uniform padding), then clamp to the frame with a 1-cell margin.
    let want_w = HELP_WANT_INNER_W
        .saturating_add(2)
        .saturating_add(PICKER_PADDING * 2);
    let want_h = HELP_WANT_INNER_H
        .saturating_add(2)
        .saturating_add(PICKER_PADDING * 2);
    let cap_w = area.width.saturating_sub(2);
    let cap_h = area.height.saturating_sub(2);
    let popup = centered_rect_sized(want_w.min(cap_w), want_h.min(cap_h), area);

    let block = modal_frame();
    let inner = block.inner(popup);

    // Section-tab rects in the top-border tab row, derived from the SAME span widths the draw path
    // lays out: a left-aligned `title_top` Line begins at the first interior border column
    // (`popup.x + 1`), starting with the `"Help: "` prefix; each tab is `sanitize_control(label)`
    // wide, separated by `HELP_TAB_SEP`. We walk those widths to place each tab's cell rect, so a
    // click maps to the tab actually drawn (the whole point of the shared helper). Rects fully past
    // the popup's right border are dropped (clipped off-screen, not clickable).
    let tabs = help_tab_rects(popup, &help.labels, help.active);

    // The body fills the whole interior (tabs + footer ride the border, not inner rows). Reserve a
    // 1-cell vertical scrollbar gutter (with a 1-cell gap) only when the body overflows — there is
    // no horizontal overflow because the body wraps. A degenerate (zero-size) interior yields no body.
    if inner.width == 0 || inner.height == 0 {
        return HelpLayout {
            popup,
            body: None,
            vbar: None,
            body_rows: 0,
            tabs,
        };
    }
    // The body wraps (prose), so its height in rendered rows — not raw lines — is what the scroll
    // offset and scrollbar must be measured against. Sum the per-line WRAPPED rows with the EXACT
    // helper the content pane uses (`text_layout::line_wrapped_rows`), so the help clamp and the
    // content clamp can never drift. A long changelog otherwise can't scroll to its last entry
    // (AC-8/AC-9).
    //
    // Two-pass, mirroring the content pane (which measures at its live text width): the wrapped count
    // depends on the width the body is ACTUALLY drawn into, and reserving the 1-col vbar gutter
    // shrinks that width — so a count taken at the full `inner.width` UNDER-counts when the bar shows,
    // leaving the changelog's tail unreachable (FIX-A). Pass 1 estimates at `inner.width` to decide
    // whether the bar is needed; pass 2 recomputes against the post-gutter `text.width` actually drawn.
    let body_rows_at = |w: u16| -> u16 {
        let w = w as usize;
        let rows: usize = help
            .body
            .lines
            .iter()
            .map(|line| crate::text_layout::line_wrapped_rows(line, w))
            .sum();
        rows.min(u16::MAX as usize) as u16
    };
    let needs_v = body_rows_at(inner.width) > inner.height;
    let (text, vbar, _hbar) = bar_layout(inner, needs_v, false);
    // When the box is NOT clamped (terminal ≥ ~76 cols, the common case) AND the body overflows — the
    // real changelog always does, so the vbar is always present — the body is drawn at exactly
    // `help_body_text_width()`, the width the What's New glow render wraps to. Pin that agreement so
    // the shared constant can never silently drift from the `bar_layout` math it models. (A short body
    // with no vbar is drawn at the full interior, which is still ≥ the glow wrap width — fits fine.)
    debug_assert!(
        inner.width < HELP_WANT_INNER_W || vbar.is_none() || text.width == help_body_text_width(),
        "help body text width {} must match help_body_text_width() {} when unclamped + overflowing",
        text.width,
        help_body_text_width(),
    );
    // Recompute against the width the body is genuinely drawn into (post-gutter), so the scroll clamp
    // reaches the true last wrapped row — this now genuinely matches how the content pane measures.
    let body_rows = body_rows_at(text.width);
    HelpLayout {
        popup,
        body: Some(text),
        vbar,
        body_rows,
        tabs,
    }
}

/// Compute each section tab's screen rect in the help overlay's top-border tab row, paired with its
/// section index. This is the single place the tab x-positions are derived; both the draw path (via
/// the tab `Line` it builds with the same spans) and the hit-test (via [`PaneGeometry::help_tabs`])
/// flow from it, so a click can never map to a different tab than the one drawn.
///
/// Layout mirrors [`draw_help_overlay`]'s `title_top`: a left-aligned title begins at the first
/// interior border column (`popup.x + 1`) with the `"{HELP_TITLE}: "` prefix, then each label —
/// `sanitize_control(label)` wide — separated by [`HELP_TAB_SEP`]. A tab whose cells fall entirely
/// past the popup's right border is dropped (ratatui clips it off-screen, so it isn't clickable).
fn help_tab_rects(popup: Rect, labels: &[String], active: usize) -> Vec<(usize, Rect)> {
    // The title row is the popup's top border; left-aligned titles start one cell in from the corner.
    let row = popup.y;
    let mut x = popup.x.saturating_add(1).saturating_add(prefix_width());
    // The rightmost interior column (exclusive of the right border corner): popup.x + width - 1.
    let right_edge = popup.x.saturating_add(popup.width.saturating_sub(1));
    let active_marker_w = HELP_ACTIVE_MARKER.chars().count() as u16;
    let mut out = Vec::with_capacity(labels.len());
    for (i, label) in labels.iter().enumerate() {
        if i > 0 {
            x = x.saturating_add(HELP_TAB_SEP.chars().count() as u16);
        }
        // The active tab is drawn with a leading `▶ ` marker; include it in the tab's
        // hit-test rect (a click on the glyph still switches the right section) and advance past
        // it before placing the label rect.
        let marker_w = if i == active { active_marker_w } else { 0 };
        let label_w = sanitize_control(label).chars().count() as u16;
        let total_w = marker_w.saturating_add(label_w);
        // Keep only a tab that begins before the right border — its visible cells are clickable.
        if total_w > 0 && x < right_edge {
            // Clip the tab's width to what fits before the right border (covers marker + label).
            let visible_w = total_w.min(right_edge.saturating_sub(x));
            out.push((
                i,
                Rect {
                    x,
                    y: row,
                    width: visible_w,
                    height: 1,
                },
            ));
        }
        x = x.saturating_add(total_w);
    }
    out
}

/// The display width (columns) of the help tab row's leading `"{HELP_TITLE}: "` prefix.
fn prefix_width() -> u16 {
    format!("{HELP_TITLE}: ").chars().count() as u16
}

/// Draw the in-app help overlay as a centered, bordered, fixed-size modal over everything else
/// (AC-1, AC-5, AC-11). The interior is the active section's body, scrollable; a vertical
/// scrollbar appears when it overflows. The chrome rides the border:
///   - **Top border:** the section tabs (`HELP_TITLE` is the box label; each label is a tab, the
///     ACTIVE one REVERSED so it is visibly indicated — AC-5).
///   - **Bottom border:** the self-operating key-hints footer (switch + close — AC-11).
///
/// Reuses [`centered_rect_sized`], `PICKER_PADDING`, and the `Clear`/`Block`/`Scrollbar` primitives
/// — no new layout abstraction. The body `Text` is already produced by the controller; this
/// function only lays it out (delegate-rendering, constitution #2).
fn draw_help_overlay(frame: &mut Frame, area: Rect, help: &HelpView) {
    // Delegate all sizing + centering to the shared layout helper, so this and `geometry()` agree.
    let layout = help_overlay_layout(area, help);

    // Top border: the box label followed by the section tabs, the active one REVERSED (AC-5). The
    // hints/labels are static or first-party (no untrusted repo input crosses into the overlay), but
    // `Color::Reset` is set so the tab spans match the body text rather than inheriting the (blue)
    // border tint — the same idiom the picker chrome uses.
    let mut tab_spans: Vec<Span<'static>> = vec![Span::styled(
        format!("{HELP_TITLE}: "),
        Style::new().fg(Color::Reset),
    )];
    for (i, label) in help.labels.iter().enumerate() {
        if i > 0 {
            tab_spans.push(Span::styled(HELP_TAB_SEP, Style::new().fg(Color::Reset)));
        }
        let mut style = Style::new().fg(Color::Reset);
        if i == help.active {
            // The active tab is REVERSED — the visible active-section indicator (AC-5) — AND carries
            // a leading `▶ ` marker so it stays distinguishable with color stripped.
            style = style
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD);
            tab_spans.push(Span::styled(HELP_ACTIVE_MARKER, style));
        }
        tab_spans.push(Span::styled(sanitize_control(label), style));
    }
    let tabs = Line::from(tab_spans);

    // Bottom border: the self-operating key-hints footer (AC-11), centered, in the body's color.
    let footer =
        Line::styled(sanitize_control(&help.hint), Style::new().fg(Color::Reset)).centered();

    // Clear whatever is beneath the popup so it reads as a true modal (on top of the picker/finder).
    frame.render_widget(Clear, layout.popup);

    let block = modal_frame()
        .title_top(tabs)
        .title_bottom(footer)
        .border_style(modal_border_style());
    frame.render_widget(block, layout.popup);

    if let Some(body_area) = layout.body {
        // The body wraps (prose: the rendered changelog / about text) and scrolls vertically.
        // About is center-aligned (`help.center`); What's New stays left as today.
        let alignment = if help.center {
            ratatui::layout::Alignment::Center
        } else {
            ratatui::layout::Alignment::Left
        };
        frame.render_widget(
            Paragraph::new(help.body.clone())
                .alignment(alignment)
                .wrap(Wrap { trim: false })
                .scroll((help.scroll, 0)),
            body_area,
        );
        // Vertical scrollbar when the body overflows — the same `layout.vbar` rect `geometry` feeds
        // back, so a press/drag on it hit-tests where it is drawn. Tracks the scroll OFFSET
        // (the body has a real offset, like the content pane — unlike the cursor-tracking tree bar).
        // Sized against the WRAPPED row total (`layout.body_rows`), not raw `lines.len()`, so the
        // thumb matches the offset extent the scroll clamp uses (the body is drawn with `wrap`).
        if let Some(track) = layout.vbar {
            draw_vscrollbar(
                frame,
                track,
                layout.body_rows as usize,
                help.scroll as usize,
                body_area.height as usize,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a line's spans to plain text (drops styling) so a test can read the result of
    /// `patch_char_range` back as a string.
    fn plain(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The chars a `patch_char_range` result marked selected, in order (the run(s) that carry the
    /// patched style), so a test can assert exactly which characters were highlighted.
    fn selected_text(spans: &[Span<'static>], style: Style) -> String {
        spans
            .iter()
            .filter(|s| s.style == Style::new().patch(style))
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn patch_char_range_highlights_only_the_range_across_spans() {
        let hl = crate::highlight::HIGHLIGHT;
        // Two spans "fn " + "main" → chars 0..7; highlight [3, 7) == "main".
        let spans = vec![Span::raw("fn "), Span::raw("main")];
        let out = patch_char_range(&spans, 3, 7, hl);
        assert_eq!(plain(&out), "fn main", "no characters are lost or reordered");
        assert_eq!(selected_text(&out, hl), "main", "exactly [3,7) is highlighted");
    }

    #[test]
    fn patch_char_range_clamps_open_end_and_empty_range() {
        let hl = crate::highlight::HIGHLIGHT;
        let spans = vec![Span::raw("hello")];
        // Open-ended (usize::MAX) highlights to end of line.
        assert_eq!(selected_text(&patch_char_range(&spans, 2, usize::MAX, hl), hl), "llo");
        // Empty range highlights nothing.
        assert_eq!(selected_text(&patch_char_range(&spans, 3, 3, hl), hl), "");
        assert_eq!(plain(&patch_char_range(&spans, 3, 3, hl)), "hello");
    }

    #[test]
    fn help_body_text_width_is_the_interior_minus_the_scrollbar_gutter() {
        // The glow `-w` target: the unclamped interior minus the 1-gap + 1-bar vbar gutter.
        assert_eq!(help_body_text_width(), HELP_WANT_INNER_W - 2);
        assert_eq!(help_body_text_width(), 70);
        assert!(
            help_body_text_width() < HELP_WANT_INNER_W,
            "the body text width must be narrower than the full interior (gutter reserved)"
        );
        assert!(
            help_body_text_width() > 0,
            "the body text width must be a usable positive column count"
        );
    }

    #[test]
    fn help_body_text_width_matches_the_drawn_body_on_a_wide_frame() {
        use ratatui::text::{Line, Text};
        // A body tall enough to overflow the fixed box → the vbar is present, so the drawn body
        // width equals `help_body_text_width()`. This pins the shared constant to the SAME width
        // `help_overlay_layout` actually draws (and that the glow `-w` is given), preventing drift.
        let body = Text::from(
            (0..200)
                .map(|i| Line::from(format!("line {i}")))
                .collect::<Vec<_>>(),
        );
        let help = HelpView {
            active: 0,
            labels: vec!["What's New".to_string(), "About".to_string()],
            body,
            scroll: 0,
            hint: "x".to_string(),
            center: false,
        };
        // A frame comfortably wider than the fixed box so it is NOT clamped.
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 40,
        };
        let layout = help_overlay_layout(area, &help);
        let body_rect = layout.body.expect("a non-degenerate body");
        assert!(layout.vbar.is_some(), "an overflowing body shows the vbar");
        assert_eq!(
            body_rect.width,
            help_body_text_width(),
            "the drawn body width must equal the shared help_body_text_width()"
        );
    }
}
