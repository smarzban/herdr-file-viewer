//! Tests for the Match Highlighter (AC-9, AC-11).
//!
//! We build small `Line`s from `Span`s with known styles and assert on the
//! output spans' content + style produced by `highlight::apply`.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use herdr_file_viewer::highlight::{CURRENT_HIGHLIGHT, HIGHLIGHT, apply};
use herdr_file_viewer::search::Match;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Collect span content strings from a line.
fn span_texts<'a>(line: &'a Line<'static>) -> Vec<&'a str> {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Collect span styles from a line.
fn span_styles(line: &Line<'static>) -> Vec<Style> {
    line.spans.iter().map(|s| s.style).collect()
}

/// Build a `Line` with a single unstyled span.
fn raw_line(text: &'static str) -> Line<'static> {
    Line::from(vec![Span::raw(text)])
}

/// Build a `Span` with a specific style.
fn styled_span(text: &'static str, style: Style) -> Span<'static> {
    Span::styled(text, style)
}

// ── AC-9: exact match range highlighted ──────────────────────────────────────

/// A single match mid-span → before / highlighted / after.
#[test]
fn single_match_mid_span() {
    // "Hello, world!" — match "world" → bytes 7..12
    let lines = vec![raw_line("Hello, world!")];
    let matches = vec![Match {
        line: 0,
        start: 7,
        end: 12,
    }];

    let out = apply(&lines, &matches, 0);
    assert_eq!(out.len(), 1);

    let texts = span_texts(&out[0]);
    assert_eq!(texts, vec!["Hello, ", "world", "!"]);

    let styles = span_styles(&out[0]);
    // "Hello, " and "!" keep the original (default) style
    assert_eq!(styles[0], Style::default());
    assert_eq!(styles[2], Style::default());
    // "world" is the current match (index 0)
    assert_eq!(styles[1], Style::default().patch(CURRENT_HIGHLIGHT));
}

/// The char immediately before start and the char at end keep the original style (AC-9).
#[test]
fn highlight_covers_exactly_match_bytes() {
    // "abcde" — match "bcd" → bytes 1..4
    let lines = vec![raw_line("abcde")];
    let matches = vec![Match {
        line: 0,
        start: 1,
        end: 4,
    }];

    let out = apply(&lines, &matches, 0);
    let texts = span_texts(&out[0]);
    assert_eq!(texts, vec!["a", "bcd", "e"]);

    let styles = span_styles(&out[0]);
    assert_eq!(styles[0], Style::default()); // 'a' unchanged
    assert_eq!(styles[2], Style::default()); // 'e' unchanged
    assert_eq!(styles[1], Style::default().patch(CURRENT_HIGHLIGHT));
}

// ── AC-11: current match is visually distinct ─────────────────────────────────

/// Two matches on the same line: the current one (index 1) gets CURRENT_HIGHLIGHT;
/// the other gets plain HIGHLIGHT.
#[test]
fn current_match_distinct_from_other_matches() {
    // "foo bar foo" — two matches of "foo": 0..3 and 8..11
    let lines = vec![raw_line("foo bar foo")];
    let matches = vec![
        Match {
            line: 0,
            start: 0,
            end: 3,
        },
        Match {
            line: 0,
            start: 8,
            end: 11,
        },
    ];
    let current = 1; // second "foo" is current

    let out = apply(&lines, &matches, current);
    let texts = span_texts(&out[0]);
    assert_eq!(texts, vec!["foo", " bar ", "foo"]);

    let styles = span_styles(&out[0]);
    // first "foo" → non-current highlight
    assert_eq!(styles[0], Style::default().patch(HIGHLIGHT));
    // " bar " → unchanged
    assert_eq!(styles[1], Style::default());
    // second "foo" → current highlight
    assert_eq!(styles[2], Style::default().patch(CURRENT_HIGHLIGHT));

    // The two highlight styles must be distinct.
    assert_ne!(HIGHLIGHT, CURRENT_HIGHLIGHT);
}

// ── match spanning a span boundary ───────────────────────────────────────────

/// A match that crosses two adjacent spans highlights across both.
#[test]
fn match_spanning_span_boundary() {
    let base = Style::new().fg(Color::Green);
    // Line: ["Hello" (green), " world"] — match "o w" → bytes 4..7
    let lines = vec![Line::from(vec![
        styled_span("Hello", base),
        Span::raw(" world"),
    ])];
    let matches = vec![Match {
        line: 0,
        start: 4,
        end: 7,
    }];

    let out = apply(&lines, &matches, 0);
    let texts = span_texts(&out[0]);
    // "Hell" | "o" (in span1) | " w" (in span2) | "orld"
    assert_eq!(texts, vec!["Hell", "o", " w", "orld"]);

    let styles = span_styles(&out[0]);
    assert_eq!(styles[0], base); // "Hell" — original span1 style, no highlight
    assert_eq!(styles[1], base.patch(CURRENT_HIGHLIGHT)); // "o" — highlighted, keeps green base
    assert_eq!(styles[2], Style::default().patch(CURRENT_HIGHLIGHT)); // " w" — highlighted
    assert_eq!(styles[3], Style::default()); // "orld" — original span2 style
}

// ── a line with no matches is unchanged ──────────────────────────────────────

#[test]
fn line_with_no_matches_unchanged() {
    let original = vec![
        Line::from(vec![Span::raw("no match here")]),
        raw_line("also fine"),
    ];
    // Match targets only line 1 (index 1); line 0 has no matches.
    let matches = vec![Match {
        line: 1,
        start: 0,
        end: 4,
    }];
    let out = apply(&original, &matches, 0);

    // Line 0 is unchanged.
    assert_eq!(out[0].spans, original[0].spans);
    assert_eq!(out[0].style, original[0].style);
    assert_eq!(out[0].alignment, original[0].alignment);
}

// ── out-of-range match is skipped without panic ───────────────────────────────

/// Match.line beyond the slice length → skipped (no panic).
#[test]
fn match_line_out_of_bounds_skipped() {
    let lines = vec![raw_line("hello")];
    let matches = vec![Match {
        line: 99,
        start: 0,
        end: 5,
    }];
    let out = apply(&lines, &matches, 0);
    // Line 0 unchanged (the out-of-bounds match is simply ignored).
    let texts = span_texts(&out[0]);
    assert_eq!(texts, vec!["hello"]);
}

/// Match range [start, end) extends beyond the line's total byte length → skipped.
#[test]
fn match_range_past_line_length_skipped() {
    let lines = vec![raw_line("hi")];
    // "hi" is 2 bytes; match claims bytes 0..100 → out of range → skip.
    let matches = vec![Match {
        line: 0,
        start: 0,
        end: 100,
    }];
    let out = apply(&lines, &matches, 0);
    let texts = span_texts(&out[0]);
    assert_eq!(texts, vec!["hi"]);
}

/// Match whose start is past line text length → skipped.
#[test]
fn match_start_past_line_length_skipped() {
    let lines = vec![raw_line("abc")];
    let matches = vec![Match {
        line: 0,
        start: 10,
        end: 15,
    }];
    let out = apply(&lines, &matches, 0);
    let texts = span_texts(&out[0]);
    assert_eq!(texts, vec!["abc"]);
}

// ── line-level style and alignment are preserved ──────────────────────────────

#[test]
fn line_style_and_alignment_preserved() {
    use ratatui::layout::Alignment;
    let line_style = Style::new().fg(Color::Blue);
    let mut line = raw_line("test");
    line.style = line_style;
    line.alignment = Some(Alignment::Center);

    let lines = vec![line];
    // No matches — line is cloned unchanged.
    let out = apply(&lines, &[], 0);
    assert_eq!(out[0].style, line_style);
    assert_eq!(out[0].alignment, Some(Alignment::Center));
}

// ── multi-line: matches on different lines ────────────────────────────────────

#[test]
fn matches_on_different_lines() {
    let lines = vec![raw_line("first line"), raw_line("second line")];
    let matches = vec![
        Match {
            line: 0,
            start: 0,
            end: 5,
        }, // "first"
        Match {
            line: 1,
            start: 7,
            end: 11,
        }, // "line" in line 1
    ];

    let out = apply(&lines, &matches, 1); // line 1's match is current

    // Line 0: "first" highlighted (non-current), " line" plain.
    let texts0 = span_texts(&out[0]);
    assert_eq!(texts0, vec!["first", " line"]);
    assert_eq!(out[0].spans[0].style, Style::default().patch(HIGHLIGHT));
    assert_eq!(out[0].spans[1].style, Style::default());

    // Line 1: "second " plain, "line" current-highlighted.
    let texts1 = span_texts(&out[1]);
    assert_eq!(texts1, vec!["second ", "line"]);
    assert_eq!(out[1].spans[0].style, Style::default());
    assert_eq!(
        out[1].spans[1].style,
        Style::default().patch(CURRENT_HIGHLIGHT)
    );
}
