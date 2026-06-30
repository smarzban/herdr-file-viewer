//! Content Renderer: escape-sequence neutralization (AC-27).

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

#[test]
fn osc_sequences_are_dropped_through_both_terminators() {
    // OSC (ESC ]) can set the window title, define hyperlinks, etc. It must be dropped whole,
    // through either terminator: BEL (\x07) or ST (ESC \). (AC-27)
    let bel = "before\x1b]0;hijack the title\x07after"; // OSC ... BEL
    let st = "x\x1b]8;;http://evil/\x1b\\y"; // OSC-8 hyperlink ... ST (ESC \)
    for hostile in [bel, st] {
        let rendered = flatten(&to_text(hostile));
        assert!(
            !rendered.contains('\u{1b}'),
            "no ESC survives: {rendered:?}"
        );
        assert!(
            !rendered.contains("hijack"),
            "OSC payload not reproduced: {rendered:?}"
        );
        assert!(
            !rendered.contains("http://evil"),
            "OSC-8 target not reproduced: {rendered:?}"
        );
    }
    // The surrounding plain text on each side is preserved.
    assert!(flatten(&to_text(bel)).contains("before") && flatten(&to_text(bel)).contains("after"));
    assert!(flatten(&to_text(st)).contains('x') && flatten(&to_text(st)).contains('y'));
}

#[test]
fn osc_52_clipboard_exfiltration_is_neutralized() {
    // OSC 52 ; clipboard c ; base64 payload ; BEL terminator. A clipboard-exfiltration vector
    // named by AC-27 — deserves its own case on the content-renderer path. Neither the OSC
    // sequence, the `52;c;` parameter, nor the base64 payload may survive into the spans.
    let payload = base64_clipboard_payload();
    let hostile = format!("before\x1b]52;c;{payload}\x07after");
    let rendered = flatten(&to_text(&hostile));

    assert!(
        !rendered.contains('\u{1b}'),
        "AC-27: no ESC byte survives: {rendered:?}"
    );
    assert!(
        !rendered.contains("52;c;"),
        "AC-27: OSC-52 parameter not reproduced: {rendered:?}"
    );
    assert!(
        !rendered.contains(&payload),
        "AC-27: OSC-52 base64 clipboard payload not reproduced: {rendered:?}"
    );
    assert!(
        rendered.contains("before") && rendered.contains("after"),
        "surrounding text preserved: {rendered:?}"
    );
}

fn base64_clipboard_payload() -> String {
    // "stolen" base64-encoded — a plausible clipboard payload that must never reach the spans.
    "c3RvbGVu".to_string()
}

#[test]
fn c1_control_codepoints_are_dropped() {
    // C1 controls (U+0080–U+009F, e.g. U+009B = a single-byte CSI introducer) are acted on by
    // some terminals, so each is dropped. (AC-27)
    let hostile = "a\u{9b}b\u{85}c"; // U+009B (CSI), U+0085 (NEL)
    let rendered = flatten(&to_text(hostile));
    assert!(!rendered.contains('\u{9b}'), "C1 CSI dropped: {rendered:?}");
    assert!(!rendered.contains('\u{85}'), "C1 NEL dropped: {rendered:?}");
    assert!(rendered.contains('a') && rendered.contains('b') && rendered.contains('c'));
}

#[test]
fn a_lone_trailing_esc_is_dropped() {
    // A bare ESC at the end of input (no following byte to form a sequence) must still be
    // dropped, never emitted. (AC-27)
    let rendered = flatten(&to_text("text\x1b"));
    assert!(
        !rendered.contains('\u{1b}'),
        "trailing ESC dropped: {rendered:?}"
    );
    assert!(rendered.contains("text"));
}

#[test]
fn esc_plus_single_byte_escape_is_dropped() {
    // A two-byte ESC + single-char escape (e.g. ESC c = RIS, a full terminal reset) must be
    // dropped whole — both the ESC and its command byte. (AC-27)
    let rendered = flatten(&to_text("a\x1bcb")); // ESC c (RIS) between 'a' and 'b'
    assert!(!rendered.contains('\u{1b}'), "ESC dropped: {rendered:?}");
    assert!(rendered.contains('a') && rendered.contains('b'));
    assert!(
        !rendered.contains('c'),
        "the RIS command byte is consumed with its ESC: {rendered:?}"
    );
}
