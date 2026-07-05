//! Line-reference formatting for the copy-line-reference feature — turns a selected line or
//! line range on a file into the `path:line` / `path:start-end` string the Copy adapter (T-7)
//! puts on the clipboard.

/// Format `rel_path` plus a 1-based line selection as `"<rel>:<n>"` for a single line
/// (`start == end`) or `"<rel>:<lo>-<hi>"` for a range, normalizing `start`/`end` to ascending
/// order first so a selection dragged either direction reads the same. Pure formatting only —
/// no sanitization of `rel_path` (the Copy adapter, T-7, handles that before this is called).
// #[allow(dead_code)] removed in T-7 when the copy adapter calls this.
#[allow(dead_code)]
pub(crate) fn format_line_reference(rel_path: &str, start: usize, end: usize) -> String {
    let (lo, hi) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if lo == hi {
        format!("{rel_path}:{lo}")
    } else {
        format!("{rel_path}:{lo}-{hi}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_has_no_range_suffix() {
        assert_eq!(
            format_line_reference("src/editor.rs", 50, 50),
            "src/editor.rs:50"
        );
    }

    #[test]
    fn range_normalizes_ascending() {
        assert_eq!(
            format_line_reference("src/editor.rs", 50, 58),
            "src/editor.rs:50-58"
        );
        assert_eq!(
            format_line_reference("src/editor.rs", 58, 50),
            "src/editor.rs:50-58"
        );
    }
}
