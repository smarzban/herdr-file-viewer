//! Shared text helpers, neutral to the view/controller split.
//!
//! `wrapped_rows` lives here (not on the Session Controller) so the Presenter can measure wrapped
//! body heights without importing from the controller — the view layer must not depend on the
//! orchestration layer. Both the content-pane scroll clamp (controller) and the help-overlay body
//! measurement (presenter) call this single helper, so their wrapped-row counts can never drift.
//! `sanitize_control` lives here for the same reason: the Presenter (every displayed string) and
//! the controller (the clipboard path) share one AC-27 neutralizer rather than two copies.

use ratatui::text::Line;

/// The 0-based char index at which each wrapped display row of `text` begins under ratatui's
/// word wrapper (`Wrap{trim:false}`). Row 0 always starts at char 0, so the result is never
/// empty and its `len()` IS the wrapped row count ([`wrapped_rows`] is defined as exactly that,
/// so the two can never drift).
///
/// This is a faithful char-unit port of ratatui's `WordWrapper::process_input` (the `reflow`
/// module is private, so it cannot be called directly): whitespace runs are breakable and
/// OCCUPY row space (`trim: false` — leading/interior spaces pack into the row, which a naive
/// space-split simulation gets wrong on gutter/indented code lines), whitespace at a break is
/// dropped only up to the finished row's remaining width (the excess carries to the next row),
/// and a word wider than a row breaks at full-row boundaries. The
/// `wrap_row_starts_match_a_real_ratatui_render` test cross-checks this port against an actual
/// `Paragraph` render, so it cannot silently drift from the widget.
///
/// This is what lets a mouse position be mapped to a character under wrap: display row *r* of
/// the line covers chars `[starts[r], starts[r+1])` (the last row runs to the line's end), so
/// `caret = starts[r] + column`, clamped into the row's span. Chars stand in for display width
/// (1 char = 1 column) — the documented wide-glyph caveat every consumer shares.
pub(crate) fn wrap_row_starts(text: &str, width: usize) -> Vec<usize> {
    if width == 0 {
        return vec![0];
    }
    let max = width;
    let mut starts = vec![0usize];
    // The port tracks char COUNTS where ratatui buffers graphemes; text order is preserved
    // (pending whitespace precedes the pending word, which precedes the current char), so at
    // any moment the pending whitespace occupies chars `[pos - word - ws, pos - word)` and the
    // pending word `[pos - word, pos)` — that is what lets a row start be computed at each break.
    let mut committed = 0usize; // chars committed to the current (not yet emitted) row
    let mut word = 0usize; // pending (uncommitted) word chars
    let mut ws = 0usize; // pending (uncommitted) whitespace chars
    let mut prev_non_ws = false;
    for (pos, c) in text.chars().enumerate() {
        let is_ws = c.is_whitespace();
        // A finished word (word→whitespace edge), or a first segment that alone overflows the
        // empty row: commit the pending whitespace + word to the row (trim:false keeps the
        // whitespace — this is where leading/indent spaces pack into the row).
        if (prev_non_ws && is_ws) || (committed == 0 && word + ws + 1 > max) {
            committed += ws + word;
            ws = 0;
            word = 0;
        }
        // The row is full, or the still-growing word would overflow it: emit the row and start
        // the next one.
        if committed >= max || committed + ws + word >= max {
            let remaining = max - committed.min(max);
            // Whitespace that would have fit on the finished row is dropped; the excess carries.
            let dropped = ws.min(remaining);
            ws -= dropped;
            committed = 0;
            // Where the next row begins: the first still-pending char — or past the current
            // char when it is whitespace landing right at the break with nothing pending
            // (ratatui skips it: "don't count first whitespace toward next word").
            let skip_current = is_ws && ws == 0;
            let next_start = if ws + word > 0 {
                pos - word - ws
            } else {
                pos + usize::from(skip_current)
            };
            starts.push(next_start);
            if skip_current {
                prev_non_ws = false;
                continue;
            }
        }
        if is_ws {
            ws += 1;
        } else {
            word += 1;
        }
        prev_non_ws = !is_ws;
    }
    // Tail: the remaining whitespace + word form the final row (trim:false appends both). If
    // nothing remains, the last pushed start was for a row ratatui never emits — drop it (but
    // never row 0: an empty text is still one empty row).
    if committed + ws + word == 0 && starts.len() > 1 {
        starts.pop();
    }
    starts
}

/// How many rows one rendered line occupies under ratatui's word wrapper (`Wrap{trim:false}`)
/// at `width` columns. A plain `ceil(width/col)` undercounts this (words rarely pack flush to
/// the column), which is what would make the bottom of wrapped prose unreachable via the scroll
/// clamp. Defined as the row count of [`wrap_row_starts`] — one source of truth, so the scroll
/// math and the mouse caret mapping can never disagree about where rows break. The caller floors
/// with the all-columns char-wrap so it never undershoots.
pub(crate) fn wrapped_rows(text: &str, width: usize) -> usize {
    wrap_row_starts(text, width).len()
}

/// Wrapped rows a single rendered [`Line`] occupies at `width` columns: flatten its spans to text
/// and run the [`wrapped_rows`] port. `width` is clamped to ≥ 1 so a zero width can't
/// divide-by-zero. The content-pane scroll clamp, the mouse caret mapping, and the help-overlay
/// body measurement all call this, so their per-line row counts can never drift.
///
/// No unconditional char-wrap floor: the port is exact for 1-column text (cross-checked against
/// a real render), and ratatui DROPS whitespace at row breaks — a real line can occupy FEWER
/// rows than `ceil(chars/width)` (e.g. two 40-char words joined by one space render as exactly
/// two rows at width 40), so flooring by the char-wrap would overcount, shifting every line
/// below it up by a row (mouse selections then landed on the line ABOVE). The floor survives
/// only for lines whose display width exceeds their char count (wide CJK/emoji glyphs, where the
/// 1-char=1-col port undercounts): there it keeps the scroll clamp able to reach the bottom, at
/// the cost of the already-documented wide-glyph mapping caveat.
pub(crate) fn line_wrapped_rows(line: &Line, width: usize) -> usize {
    let width = width.max(1);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let rows = wrapped_rows(&text, width);
    let display_width = line.width();
    if display_width > text.chars().count() {
        return rows.max(display_width.div_ceil(width));
    }
    rows
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
    use super::{sanitize_control, wrap_row_starts, wrapped_rows};

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
    fn wrap_row_starts_marks_word_and_char_breaks() {
        // Word wrap: four width-6 words in a 10-col pane pack one per row. Each break lands on
        // the next WORD's first char — the separator space stays behind on the ending row.
        // "aaaaaa aaaaaa aaaaaa aaaaaa": words start at chars 0, 7, 14, 21.
        assert_eq!(
            wrap_row_starts("aaaaaa aaaaaa aaaaaa aaaaaa", 10),
            vec![0, 7, 14, 21]
        );
        // A single over-long word breaks at full-row boundaries, like char wrapping.
        assert_eq!(wrap_row_starts(&"x".repeat(100), 25), vec![0, 25, 50, 75]);
        // Words that pack flush share one row; short/empty/zero-width are a single row at 0.
        assert_eq!(wrap_row_starts("ab cd ef", 8), vec![0]);
        assert_eq!(wrap_row_starts("hello", 80), vec![0]);
        assert_eq!(wrap_row_starts("", 80), vec![0]);
        assert_eq!(wrap_row_starts("anything", 0), vec![0]);
        // A mixed line: a 50-char word, then a 40-char word at width 80 → the second word
        // doesn't fit (50+1+40 > 80) and starts row 1 at char 51 (index 50 is the space).
        let line = format!("{} {}", "a".repeat(50), "b".repeat(40));
        assert_eq!(wrap_row_starts(&line, 80), vec![0, 51]);
        // trim:false packs leading whitespace INTO the row: a 4-space indent + a 100-char word
        // at width 80 fills row 0 with the 4 spaces + 76 word chars, so row 1 begins at char 80
        // — NOT at 84 as a simulation that discards leading whitespace would claim. This is the
        // indented-code case that made wrapped selection land on the text below.
        let line = format!("    {}", "x".repeat(100));
        assert_eq!(wrap_row_starts(&line, 80), vec![0, 80]);
    }

    /// The port must agree with the REAL widget: render each corpus line through a ratatui
    /// `Paragraph` + `Wrap{trim:false}` (the exact configuration the content pane draws with)
    /// and check row-by-row that the buffer's text equals the slice `[starts[r], starts[r+1])`
    /// of the source — trailing-whitespace-insensitively, since break-dropped whitespace lives
    /// between a row's content and the next row's start, and the backend pads rows with spaces.
    /// This pins `wrap_row_starts` to ratatui's actual behavior, so a silent drift (a ratatui
    /// upgrade, a port bug) fails here instead of mis-mapping mouse selections.
    #[test]
    fn wrap_row_starts_match_a_real_ratatui_render() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::widgets::{Paragraph, Wrap};

        let corpus: Vec<String> = vec![
            // bat-style gutter + indented code (the reported mis-selection case)
            format!("   1     let selected = tree.selected().map(|n| n.path.clone()); {}", "x".repeat(40)),
            // deep indentation + long tokens
            format!("        {}", "long_function_name(arg_one, arg_two, arg_three, arg_four);".repeat(3)),
            // prose with normal word lengths
            "The quick brown fox jumps over the lazy dog and keeps on running far past the fence line".to_string(),
            // multi-space interior runs
            format!("a  b   c    d     e {}", "word ".repeat(30)),
            // one giant unbroken word
            "y".repeat(250),
            // leading whitespace + giant word
            format!("      {}", "z".repeat(200)),
            // trailing whitespace
            format!("{}      ", "tail ".repeat(20)),
            // exact-fit rows
            format!("{} {}", "q".repeat(39), "r".repeat(40)),
            // two exact-width words joined by one space: the break DROPS the space, so the line
            // renders in 2 rows — fewer than ceil(81/40) = 3. The case that proved the char-wrap
            // floor wrong (it shifted wrapped code selections onto the line above).
            format!("{} {}", "x".repeat(40), "y".repeat(40)),
        ];

        const W: u16 = 40; // a narrow pane exercises many breaks per line
        for text in &corpus {
            let starts = wrap_row_starts(text, W as usize);
            let chars: Vec<char> = text.chars().collect();

            // Render the same text through the real widget at the same width.
            let rows_needed = starts.len() as u16 + 2;
            let backend = TestBackend::new(W, rows_needed);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| {
                f.render_widget(
                    Paragraph::new(text.as_str()).wrap(Wrap { trim: false }),
                    f.area(),
                );
            })
            .unwrap();
            let buffer = term.backend().buffer().clone();

            for (r, &start) in starts.iter().enumerate() {
                let end = starts.get(r + 1).copied().unwrap_or(chars.len());
                let expected: String = chars[start..end].iter().collect();
                let rendered: String = (0..W)
                    .map(|x| {
                        buffer
                            .cell(ratatui::layout::Position::new(x, r as u16))
                            .unwrap()
                            .symbol()
                            .chars()
                            .next()
                            .unwrap_or(' ')
                    })
                    .collect();
                assert_eq!(
                    rendered.trim_end(),
                    expected.trim_end(),
                    "row {r} of {text:?} at width {W}: port says chars [{start}, {end}), widget drew {rendered:?}"
                );
            }
            // And no extra rendered rows beyond the port's count: the row past the last must be blank.
            let past: String = (0..W)
                .map(|x| {
                    buffer
                        .cell(ratatui::layout::Position::new(x, starts.len() as u16))
                        .unwrap()
                        .symbol()
                        .chars()
                        .next()
                        .unwrap_or(' ')
                })
                .collect();
            assert_eq!(
                past.trim_end(),
                "",
                "the widget drew more rows than the port counted for {text:?}"
            );
        }
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
