//! T-10 — Content Renderer: escape-sequence neutralization (AC-27).

use herdr_file_viewer::render::to_text;
use ratatui::text::Text;

fn flatten(text: &Text) -> String {
    text.lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn cursor_and_screen_control_sequences_are_neutralized() {
    // \x1b[2J clears the screen; \x1b[10;10H moves the cursor.
    let hostile = "before\x1b[2Jmid\x1b[10;10Hafter";
    let text = to_text(hostile);
    let rendered = flatten(&text);

    assert!(
        !rendered.contains('\u{1b}'),
        "AC-27: no ESC byte survives ingestion"
    );
    assert!(
        !rendered.contains("[2J"),
        "AC-27: screen-clear not reproduced"
    );
    assert!(
        !rendered.contains("[10;10H"),
        "AC-27: cursor-move not reproduced"
    );
    // The actual textual content is preserved.
    assert!(rendered.contains("before") && rendered.contains("mid") && rendered.contains("after"));
}

#[test]
fn c0_control_bytes_are_stripped() {
    // BEL, backspace, carriage-return, form-feed, vertical-tab can ring the bell or
    // overwrite/spoof a line; only newline and tab survive (AC-27).
    let hostile = "a\x07b\x08c\rd\x0ce\x0bf\ttab\nnext";
    let text = to_text(hostile);
    let rendered = flatten(&text);
    for ctl in ['\u{07}', '\u{08}', '\r', '\u{0c}', '\u{0b}'] {
        assert!(
            !rendered.contains(ctl),
            "control {:#x} must be stripped",
            ctl as u32
        );
    }
    assert!(rendered.contains('\t'), "tab is kept");
    assert_eq!(
        text.lines.len(),
        2,
        "newline is kept as a line break, not stripped"
    );
    assert!(rendered.contains("tab") && rendered.contains("next"));
}

#[test]
fn sgr_styling_is_mapped_to_style_not_left_as_raw_codes() {
    let styled = "\x1b[31mRED\x1b[0m"; // red foreground
    let text = to_text(styled);
    let rendered = flatten(&text);

    assert!(
        !rendered.contains('\u{1b}'),
        "SGR codes are consumed, not emitted as bytes"
    );
    assert!(rendered.contains("RED"));
    let has_color = text
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .any(|s| s.style.fg.is_some());
    assert!(has_color, "SGR color is applied as a ratatui style");
}
