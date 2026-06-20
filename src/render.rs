//! Content Renderer — produce the content-pane text for a file, with safety guards.
//!
//! The primary trust boundary: all file bytes are untrusted. This module bounds size
//! (AC-13), refuses to emit raw bytes for binary files (AC-12), and (in later tasks)
//! neutralizes control/escape sequences (AC-27) and delegates styling to external CLIs
//! with a plain-text fallback (AC-24/25). Reads only, never writes (AC-N1).

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// The size cap: files at or above this many bytes are previewed, not shown whole (AC-13).
const MAX_BYTES: u64 = 1024 * 1024; // 1 MB
/// The size cap by line count (AC-13).
const MAX_LINES: usize = 5000;

/// The guarded result of reading a file's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    /// A binary file: a placeholder is shown, never the raw bytes (AC-12).
    Binary,
    /// A file at/above the size cap: a bounded preview plus a visible notice (AC-13).
    Truncated { text: String, notice: String },
    /// A normal text file shown in full.
    Full { text: String },
}

/// Classify a file for display: binary vs. truncated-preview vs. full text. Reads at most
/// `MAX_BYTES` from disk, so a huge or hostile file can never be slurped whole (AC-N1).
pub fn classify(path: &Path) -> Prepared {
    let byte_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let Ok(mut file) = File::open(path) else {
        return Prepared::Full { text: String::new() };
    };
    // Bounded read: at most MAX_BYTES, so a giant/hostile file is never slurped whole.
    let mut buf = Vec::new();
    if file.by_ref().take(MAX_BYTES).read_to_end(&mut buf).is_err() {
        return Prepared::Full { text: String::new() };
    }

    // Binary: a NUL byte anywhere in the (bounded) content. No raw bytes are emitted.
    if buf.contains(&0) {
        return Prepared::Binary;
    }

    let over_bytes = byte_len >= MAX_BYTES;
    // If the file fit under the cap, invalid UTF-8 means binary. If it was capped, the
    // read may have split a multi-byte char, so decode lossily rather than misclassify.
    let text = if over_bytes {
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        match String::from_utf8(buf) {
            Ok(t) => t,
            Err(_) => return Prepared::Binary,
        }
    };

    let line_count = text.lines().count();
    let over_lines = line_count >= MAX_LINES;
    if over_bytes || over_lines {
        let preview: String = text
            .lines()
            .take(MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        let cap = if over_bytes { "1 MB size" } else { "5000-line" };
        let notice = format!(
            "⚠ Truncated preview — showing {} lines ({} of {} bytes); file exceeds the {} cap.",
            preview.lines().count(),
            preview.len(),
            byte_len,
            cap
        );
        return Prepared::Truncated { text: preview, notice };
    }
    Prepared::Full { text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn tmp(name: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "hfv-render-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn nul_bytes_classify_as_binary_without_emitting_raw_bytes() {
        let p = tmp("bin", &[0x00, 0x01, 0x02, b'h', b'i']);
        assert_eq!(classify(&p), Prepared::Binary); // AC-12
        fs::remove_file(&p).ok();
    }

    #[test]
    fn small_text_file_is_returned_in_full() {
        let p = tmp("small.txt", b"hello\nworld\n");
        match classify(&p) {
            Prepared::Full { text } => assert!(text.contains("hello")),
            other => panic!("expected Full, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn file_over_one_megabyte_is_truncated_with_a_notice() {
        let big = vec![b'a'; (MAX_BYTES as usize) + 100];
        let p = tmp("big.txt", &big);
        match classify(&p) {
            Prepared::Truncated { text, notice } => {
                assert!(!notice.is_empty(), "AC-13: a visible truncation notice");
                assert!(text.len() as u64 <= MAX_BYTES, "AC-13: preview is bounded");
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn file_over_five_thousand_lines_is_truncated() {
        let many = "x\n".repeat(6000);
        let p = tmp("many.txt", many.as_bytes());
        match classify(&p) {
            Prepared::Truncated { text, notice } => {
                assert!(text.lines().count() <= MAX_LINES, "AC-13: preview line-bounded");
                assert!(notice.contains("line"), "notice describes the line cap");
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn classify_does_not_modify_the_file() {
        let p = tmp("ro.txt", b"unchanged\n");
        let before = fs::read(&p).unwrap();
        let _ = classify(&p);
        assert_eq!(fs::read(&p).unwrap(), before); // AC-N1
        fs::remove_file(&p).ok();
    }
}
