//! e2e (pty): session annotations work through the live event loop and live OSC 52 clipboard.
//!
//! The flow uses the same stable synchronization rules as the existing pty suite: initial content,
//! modal titles, targets, and freshly opened overlay rows are positive paint anchors; a short gap
//! surrounds bare Esc so crossterm cannot combine it with the following character as Alt+char.
//! The copy assertion captures the actual OSC 52 sequence emitted by `Osc52Clipboard`, decodes its
//! RFC 4648 payload locally (no dependency), and compares the decoded bytes with the complete
//! canonical annotation wrapper.
//!
//! Unix-only: see `tests/cli_smoke.rs` for why the existing `expectrl` pty suite uses this gate.
#![cfg(unix)]

mod common;

use common::{TempDir, viewer_command};
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Regex, Session};
use std::time::Duration;

const ESC_SETTLE: Duration = Duration::from_millis(150);
const EXPECTED_EXPORT: &[u8] = b"<file-annotations>\n- annotate.txt:File note\n- annotate.txt:1-3:Range &lt;note&gt; &amp; exact\n</file-annotations>";

fn decode_rfc4648(input: &[u8]) -> Result<Vec<u8>, String> {
    if !input.len().is_multiple_of(4) {
        return Err(format!(
            "base64 length {} is not divisible by four",
            input.len()
        ));
    }

    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let mut decoded = Vec::with_capacity(input.len() / 4 * 3);
    let chunk_count = input.len() / 4;
    for (index, chunk) in input.chunks_exact(4).enumerate() {
        let is_last = index + 1 == chunk_count;
        let padding = match (chunk[2], chunk[3]) {
            (b'=', b'=') => 2,
            (_, b'=') => 1,
            (b'=', _) => return Err("base64 has padding before a data byte".to_string()),
            _ => 0,
        };
        if padding > 0 && !is_last {
            return Err("base64 padding appears before the final quartet".to_string());
        }

        let a = value(chunk[0]).ok_or_else(|| format!("invalid base64 byte: {:#x}", chunk[0]))?;
        let b = value(chunk[1]).ok_or_else(|| format!("invalid base64 byte: {:#x}", chunk[1]))?;
        let c = if padding == 2 {
            0
        } else {
            value(chunk[2]).ok_or_else(|| format!("invalid base64 byte: {:#x}", chunk[2]))?
        };
        let d = if padding > 0 {
            0
        } else {
            value(chunk[3]).ok_or_else(|| format!("invalid base64 byte: {:#x}", chunk[3]))?
        };
        let bits = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | d as u32;
        decoded.push((bits >> 16) as u8);
        if padding < 2 {
            decoded.push((bits >> 8) as u8);
        }
        if padding == 0 {
            decoded.push(bits as u8);
        }
    }
    Ok(decoded)
}

macro_rules! send_esc {
    ($session:expr) => {{
        std::thread::sleep(ESC_SETTLE);
        $session.send("\u{1b}").expect("send a lone Esc");
        std::thread::sleep(ESC_SETTLE);
    }};
}

#[test]
fn rfc4648_decoder_handles_standard_padding_vectors() {
    for (encoded, plain) in [
        (b"".as_slice(), b"".as_slice()),
        (b"Zg==".as_slice(), b"f".as_slice()),
        (b"Zm8=".as_slice(), b"fo".as_slice()),
        (b"Zm9v".as_slice(), b"foo".as_slice()),
        (b"Zm9vYmFy".as_slice(), b"foobar".as_slice()),
    ] {
        assert_eq!(decode_rfc4648(encoded).unwrap(), plain);
    }
}

#[test]
fn live_file_range_overview_copy_clear_and_modal_isolation() {
    let dir = TempDir::new();
    let root = dir.path();
    std::fs::write(
        root.join("annotate.txt"),
        "TOPANNOTATIONMARK\nline two\nline three\nline four\n",
    )
    .unwrap();

    let mut cmd = viewer_command(root);
    // Keep this test hermetic even if the developer running it has remapped global `a`/`A`.
    cmd.env("HERDR_PLUGIN_CONFIG_DIR", root.join("missing-config"));
    // If modal routing regresses and `e` leaks globally, the hand-off exits immediately rather
    // than opening a developer's editor; the subsequent modal assertions still fail.
    cmd.env("EDITOR", "true");
    let mut session = Session::spawn(cmd).expect("spawn the viewer in a pty");
    session.set_expect_timeout(Some(Duration::from_secs(15)));

    // Initial content is the render-completion barrier. It guarantees `L` can synchronously map
    // source lines later, regardless of whether bat is installed or plain fallback is in use.
    session
        .expect("TOPANNOTATIONMARK")
        .expect("the selected file finished its initial render");

    // Editor modal isolation + Esc cancellation: every character below is also a global/modal key
    // elsewhere. Here it must remain text. In particular, a leaked `q` exits, leaked `e` hands off,
    // and leaked `y` emits an earlier OSC 52 sequence that the exact capture below would detect.
    session.send("a").expect("open the annotation editor");
    session
        .expect("Add annotation")
        .expect("the add editor owns subsequent keys");
    session
        .send("qedDy")
        .expect("send globally meaningful characters as editor text");
    send_esc!(session);
    session.send("A").expect("open the overview after cancel");
    session
        .expect("Annotations (0)")
        .expect("Esc canceled the editor without saving or leaking modal keys");
    session
        .expect("No annotations")
        .expect("the canceled editor left the store empty");
    send_esc!(session);

    // Global `a` -> editor -> Enter saves a file-level annotation, and global `A` shows it.
    session.send("a").expect("add a file annotation");
    session
        .expect("Add annotation")
        .expect("file annotation editor opens");
    session.send("File note").expect("type the file note");
    session.send("\r").expect("save the file annotation");
    session
        .expect("annotations: 1")
        .expect("the saved annotation count appears in the content border");
    session.send("A").expect("show the annotation overview");
    session
        .expect("Annotations (1)")
        .expect("the overview reports the saved file annotation");
    session
        .expect("annotate.txt — File note")
        .expect("the overview renders the saved file target and note");
    send_esc!(session);

    // Focus content, enter `L`, extend from line 1 through line 3 with uppercase `J`, then use the
    // line-select-local `a`. The editor target is the live proof that the range was snapshotted.
    session.send("\t").expect("focus the content pane");
    session.send("L").expect("enter line-select mode");
    session.send("J").expect("extend the range to line 2");
    session.send("J").expect("extend the range to line 3");
    session
        .send("a")
        .expect("open the annotation editor for the selected range");
    session
        .expect("Add annotation")
        .expect("range annotation editor opens");
    session
        .expect("Target:")
        .expect("the range editor renders its target label");
    session
        .expect("annotate.txt:1-3")
        .expect("L -> a captured the normalized live line range");
    session
        .send("Range <note> & exact")
        .expect("type a range note that exercises list escaping");
    session.send("\r").expect("save the range annotation");

    // `y` must invoke the real live clipboard adapter. Open a fresh overlay as the save-completion
    // anchor, then capture the complete OSC 52 sequence, extract its RFC 4648 group, decode it, and
    // compare every resulting byte to the canonical wrapper/list string. An earlier leaked editor
    // `y` would be captured first and fail here.
    session.send("A").expect("show both saved annotations");
    session
        .expect("Annotations (2)")
        .expect("overview contains both annotations before copy");
    session.send("y").expect("copy all annotations");
    let capture = session
        .expect(Regex(r"\x1b\]52;c;([A-Za-z0-9+/=]+)\x07"))
        .expect("capture the live OSC 52 clipboard sequence");
    let encoded = capture.get(1).expect("OSC 52 regex captured its payload");
    let decoded = decode_rfc4648(encoded).expect("OSC 52 payload is valid RFC 4648 base64");
    assert_eq!(
        decoded.as_slice(),
        EXPECTED_EXPORT,
        "decoded live OSC 52 bytes equal the complete canonical annotation export"
    );

    // Copy closes but does not mutate the overview. Reopen, then send exactly one uppercase `D`.
    // The immediate empty-state paint proves one key cleared all; the overview remains open.
    session
        .send("A")
        .expect("reopen the overview after copy closed it");
    session
        .expect("Annotations (2)")
        .expect("copy preserved both annotations");
    session
        .send("D")
        .expect("send one uppercase D to clear all");
    session
        .expect("No annotations")
        .expect("one uppercase D immediately clears all and keeps the overview visible");

    send_esc!(session);
    session.send("q").expect("quit after closing the overview");
    session
        .expect(Eof)
        .expect("the viewer exits cleanly after all annotation flows");
    match session.get_process().wait().expect("reap the viewer") {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0, "annotation flow exits cleanly"),
        other => panic!("expected a clean exit, got {other:?}"),
    }
}
