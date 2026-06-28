//! Shared text-layout helpers, neutral to the view/controller split.
//!
//! `wrapped_rows` lives here (not on the Session Controller) so the Presenter can measure wrapped
//! body heights without importing from the controller — the view layer must not depend on the
//! orchestration layer. Both the content-pane scroll clamp (controller) and the help-overlay body
//! measurement (presenter) call this single helper, so their wrapped-row counts can never drift.

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

#[cfg(test)]
mod tests {
    use super::wrapped_rows;

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
