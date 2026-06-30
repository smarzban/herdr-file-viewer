//! Shared text helpers, neutral to the view/controller split.
//!
//! `wrapped_rows` lives here (not on the Session Controller) so the Presenter can measure wrapped
//! body heights without importing from the controller — the view layer must not depend on the
//! orchestration layer. Both the content-pane scroll clamp (controller) and the help-overlay body
//! measurement (presenter) call this single helper, so their wrapped-row counts can never drift.
//! `sanitize_control` lives here for the same reason: the Presenter (every displayed string) and
//! the controller (the clipboard path) share one AC-27 neutralizer rather than two copies.

use ratatui::text::Line;

/// How many rows one rendered line occupies under ratatui's word wrapper (`Wrap{trim:false}`)
/// at `width` columns: greedy word packing — fill the row with space-separated words until the
/// next one doesn't fit, then wrap; a word wider than the row is broken across rows. A plain
/// `ceil(width/col)` undercounts this (words rarely pack flush to the column), which is what
/// would make the bottom of wrapped prose unreachable via the scroll clamp. Char counts stand
/// in for display width — close enough for the clamp, and the caller floors with the
/// all-columns char-wrap so it never undershoots.
pub(crate) fn wrapped_rows(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut rows = 1usize;
    let mut col = 0usize;
    for (i, word) in text.split(' ').enumerate() {
        let wl = word.chars().count();
        let sep = usize::from(i > 0);
        if col != 0 && col + sep + wl > width {
            rows += 1; // doesn't fit → start a new row
            col = 0;
        }
        if col == 0 {
            // word starts a fresh row; a word wider than the row breaks across full rows
            let extra = wl.saturating_sub(1) / width;
            rows += extra;
            col = wl - extra * width;
        } else {
            col += sep + wl;
        }
    }
    rows
}

/// Wrapped rows a single rendered [`Line`] occupies at `width` columns: flatten its spans to text,
/// run the word-wrap simulation, and floor by the all-columns char-wrap so the simulation can never
/// undershoot (a leading/interior over-long word still costs its char-wrapped rows). `width` is
/// clamped to ≥ 1 so a zero width can't divide-by-zero. The content-pane scroll clamp and the
/// help-overlay body measurement both call this, so their per-line row counts can never drift.
pub(crate) fn line_wrapped_rows(line: &Line, width: usize) -> usize {
    let width = width.max(1);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    wrapped_rows(&text, width).max(line.width().max(1).div_ceil(width))
}

/// Neutralize a string for display (a label/title) or for the clipboard: drop control characters
/// (C0, DEL, and C1 — `char::is_control`), so a repo-controlled file name carrying ESC/CSI bytes
/// cannot move the cursor, clear the screen, spoof the UI, or paste-inject once copied (AC-27,
/// defense-in-depth). ratatui's renderer also drops control graphemes, but the viewer's security
/// guarantee must not rest on that internal — and the clipboard path never touches a renderer at
/// all. Neutral to the view/controller split, so both layers share one definition.
pub(crate) fn sanitize_control(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::{sanitize_control, wrapped_rows};

    #[test]
    fn sanitize_control_strips_control_bytes_keeps_printable() {
        // ESC + CSI + C0 controls removed; the printable remainder (incl. unicode) survives.
        assert_eq!(
            sanitize_control("evil\u{1b}[2J\u{1b}[10;10Hpwned"),
            "evil[2J[10;10Hpwned"
        );
        assert_eq!(sanitize_control("a\u{07}\u{08}\rb\tc"), "abc");
        assert_eq!(sanitize_control("plain_name.rs"), "plain_name.rs");
        assert_eq!(sanitize_control("café—ok"), "café—ok");
        // C1 controls (U+0080..U+009F) are also dropped.
        assert_eq!(sanitize_control("x\u{0090}y"), "xy");
        // No control codepoint survives, ever.
        assert!(
            !sanitize_control("\u{1b}\u{07}\u{7f}\u{9b}z")
                .chars()
                .any(|c| c.is_control())
        );
    }

    #[test]
    fn wrapped_rows_counts_word_wrapping_not_just_char_wrapping() {
        // Four width-6 words in a 10-col pane pack one per row → 4 rows, even though the
        // 27-column line char-wraps to only 3. The scroll clamp must use the larger count.
        assert_eq!(wrapped_rows("aaaaaa aaaaaa aaaaaa aaaaaa", 10), 4);
        // A single over-long word is broken like char wrapping.
        assert_eq!(wrapped_rows(&"x".repeat(100), 25), 4);
        // Words that pack flush share rows.
        assert_eq!(wrapped_rows("ab cd ef", 8), 1); // "ab cd ef" = 8 cols, fits exactly
        // Short / empty / zero-width are one row.
        assert_eq!(wrapped_rows("hello", 80), 1);
        assert_eq!(wrapped_rows("", 80), 1);
        assert_eq!(wrapped_rows("anything", 0), 1);
    }
}
