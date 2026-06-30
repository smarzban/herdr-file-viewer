//! Match Highlighter — overlay highlight styling onto content-pane spans (AC-9, AC-11).
//!
//! `apply` re-segments each `Line`'s spans at match boundaries and patches highlight
//! styles onto the sub-spans that fall within a match's `[start, end)` byte range.
//! The current match (`matches[current]`) gets a visually distinct style.
//!
//! Invariants upheld:
//! - **Pure & read-only**: allocates a new `Vec<Line>`; never mutates the input.
//! - **Zero new Cargo deps**: ratatui + std only.
//! - **Skip, never panic**: stale or out-of-range matches are dropped silently.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::search::Match;

// ── public style constants ────────────────────────────────────────────────────

/// Style applied to every non-current highlighted match (black text on cyan).
pub const HIGHLIGHT: Style = Style::new().fg(Color::Black).bg(Color::Cyan);

/// Style applied to the **current** match — visually distinct from `HIGHLIGHT` AND distinguishable
/// with color stripped: `REVERSED` inverts whatever the terminal theme is (so the active
/// position still reads on a non-default palette or to a colorblind user) and `BOLD` adds a weight
/// cue on top. Previously this was `Black` on `Yellow` — color-only, so a non-default theme or a
/// colorblind user could lose the "which match am I on" signal entirely. `REVERSED` is theme-relative:
/// it never picks a hardcoded fg/bg that could clash with the theme.
pub const CURRENT_HIGHLIGHT: Style = Style::new()
    .add_modifier(Modifier::REVERSED)
    .add_modifier(Modifier::BOLD);

// ── public API ────────────────────────────────────────────────────────────────

/// Re-segment `lines` at match boundaries and overlay highlight styles.
///
/// For each line in `lines`:
/// - If no match targets that line, the line is cloned unchanged.
/// - Otherwise the line's spans are split at every match `[start, end)` byte
///   boundary and the resulting sub-spans inside a match get the highlight style
///   patched onto the original span style. Sub-spans inside `matches[current]`
///   get `CURRENT_HIGHLIGHT`; all other matched sub-spans get `HIGHLIGHT`.
///
/// Out-of-range matches (line index beyond `lines.len()`, byte range past the
/// line's plain-text length, or a range not on a UTF-8 char boundary) are
/// silently skipped.
pub fn apply(lines: &[Line<'static>], matches: &[Match], current: usize) -> Vec<Line<'static>> {
    // Group matches by line in a single O(total_matches) pass, recording each
    // match's global index so we can identify the current match in O(1).
    let mut by_line: Vec<Vec<(usize, &Match)>> = vec![Vec::new(); lines.len()];
    for (global_idx, m) in matches.iter().enumerate() {
        if m.line < lines.len() {
            by_line[m.line].push((global_idx, m));
        }
    }

    lines
        .iter()
        .enumerate()
        .map(|(line_idx, line)| {
            let line_matches = &by_line[line_idx];

            if line_matches.is_empty() {
                return line.clone();
            }

            // Reconstruct the plain text ONCE per line for boundary validation.
            let plain: String = {
                let cap: usize = line.spans.iter().map(|s| s.content.len()).sum();
                let mut s = String::with_capacity(cap);
                for span in &line.spans {
                    s.push_str(&span.content);
                }
                s
            };

            // Validate each match using the pre-built plain text; drop silently
            // if out-of-range or not on a UTF-8 char boundary.
            let validated: Vec<(usize, usize, bool)> = line_matches
                .iter()
                .filter_map(|&(global_idx, m)| {
                    validate_match_plain(&plain, m).map(|(s, e)| {
                        let is_current = global_idx == current;
                        (s, e, is_current)
                    })
                })
                .collect();

            if validated.is_empty() {
                return line.clone();
            }

            // Re-segment the spans.
            let new_spans = resegment(&line.spans, &validated);
            Line {
                spans: new_spans,
                style: line.style,
                alignment: line.alignment,
            }
        })
        .collect()
}

// ── internals ─────────────────────────────────────────────────────────────────

/// Validate a match against the pre-built plain text of a line.
///
/// Accepts the line's already-concatenated plain text (built once per line in
/// `apply`) so validation costs O(1) per match instead of O(spans) per match.
/// Returns `Some((start, end))` if the match is usable, `None` to skip.
fn validate_match_plain(plain: &str, m: &Match) -> Option<(usize, usize)> {
    if m.start > m.end || m.end > plain.len() {
        return None;
    }
    if m.start == m.end {
        // Zero-length match — nothing to highlight, skip.
        return None;
    }
    if !plain.is_char_boundary(m.start) || !plain.is_char_boundary(m.end) {
        return None;
    }
    Some((m.start, m.end))
}

/// Split `spans` at the boundaries implied by `intervals` (sorted `(start, end, is_current)`)
/// and return the resulting sub-spans with highlight styles patched in.
///
/// `intervals` must all be valid (validated by `validate_match`).
fn resegment(spans: &[Span<'static>], intervals: &[(usize, usize, bool)]) -> Vec<Span<'static>> {
    // Collect all boundary points from the intervals and sort them.
    let mut boundaries: Vec<usize> = Vec::with_capacity(intervals.len() * 2);
    for (s, e, _) in intervals {
        boundaries.push(*s);
        boundaries.push(*e);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    // Walk spans, splitting at each boundary.
    let mut result: Vec<Span<'static>> = Vec::new();
    let mut byte_cursor: usize = 0; // global byte offset in the plain text

    for span in spans {
        let span_text = span.content.as_ref();
        let span_len = span_text.len();
        let span_lo = byte_cursor;
        let span_hi = byte_cursor + span_len;

        // Find boundaries that fall strictly inside this span.
        let cuts: Vec<usize> = boundaries
            .iter()
            .copied()
            .filter(|&b| b > span_lo && b < span_hi)
            .collect();

        if cuts.is_empty() {
            // No split needed — emit the whole span with the appropriate style.
            let style = style_for_offset(span_lo, span_hi, span.style, intervals);
            result.push(Span {
                content: span.content.clone(),
                style,
            });
        } else {
            // Split the span at each cut point.
            let mut pos = span_lo;
            for &cut in &cuts {
                if cut > pos {
                    let sub = &span_text[(pos - span_lo)..(cut - span_lo)];
                    let style = style_for_offset(pos, cut, span.style, intervals);
                    result.push(Span {
                        content: std::borrow::Cow::Owned(sub.to_owned()),
                        style,
                    });
                }
                pos = cut;
            }
            // Remaining part after last cut.
            if pos < span_hi {
                let sub = &span_text[(pos - span_lo)..(span_hi - span_lo)];
                let style = style_for_offset(pos, span_hi, span.style, intervals);
                result.push(Span {
                    content: std::borrow::Cow::Owned(sub.to_owned()),
                    style,
                });
            }
        }

        byte_cursor = span_hi;
    }

    result
}

/// Determine the effective style for the byte range `[lo, hi)` of plain text.
///
/// If the range is fully covered by an interval, patch the highlight onto the
/// original span style. Otherwise keep the original style.
///
/// The range `[lo, hi)` is always a sub-segment of a single original span, so
/// it cannot partially overlap an interval — it is either fully inside or fully
/// outside every interval (boundaries were inserted at interval edges).
fn style_for_offset(
    lo: usize,
    hi: usize,
    original_style: Style,
    intervals: &[(usize, usize, bool)],
) -> Style {
    for &(start, end, is_current) in intervals {
        if lo >= start && hi <= end {
            let highlight = if is_current {
                CURRENT_HIGHLIGHT
            } else {
                HIGHLIGHT
            };
            return original_style.patch(highlight);
        }
    }
    original_style
}
