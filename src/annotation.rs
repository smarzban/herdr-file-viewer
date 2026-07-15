//! Typed, session-only file annotations and their canonical clipboard format.
//!
//! This module performs no I/O. [`AnnotationStore`] owns normalized notes in memory, and
//! [`format_annotations`] produces the exact portable text copied by the controller.

use crate::text_layout::sanitize_control;
use std::cmp::Ordering;
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// A store-assigned annotation identity. IDs increase monotonically and are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnnotationId(u64);

impl AnnotationId {
    /// The numeric identity assigned by the store.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// An inclusive, normalized, 1-based line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineRange {
    start: usize,
    end: usize,
}

impl LineRange {
    /// Build an inclusive line range, ordering its bounds ascending.
    ///
    /// Line zero is invalid and is rejected rather than clamped.
    pub fn new(first: usize, second: usize) -> Result<Self, LineRangeError> {
        if first == 0 || second == 0 {
            return Err(LineRangeError::Zero);
        }
        Ok(Self {
            start: first.min(second),
            end: first.max(second),
        })
    }

    /// First line in the inclusive range.
    pub fn start(self) -> usize {
        self.start
    }

    /// Last line in the inclusive range.
    pub fn end(self) -> usize {
        self.end
    }
}

/// Failure to construct a valid [`LineRange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineRangeError {
    /// At least one line was zero; annotation lines are 1-based.
    Zero,
}

impl fmt::Display for LineRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => f.write_str("annotation lines are 1-based"),
        }
    }
}

impl std::error::Error for LineRangeError {}

/// The immutable root-relative file and optional inclusive lines an annotation describes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnnotationTarget {
    path: PathBuf,
    lines: Option<LineRange>,
}

impl AnnotationTarget {
    /// Build a target from a root-relative file path and optional line range.
    ///
    /// Empty, absolute, parent-traversing, and otherwise non-normal paths are rejected so an
    /// annotation can never identify content outside the viewer root.
    pub fn new(
        path: impl Into<PathBuf>,
        lines: Option<LineRange>,
    ) -> Result<Self, AnnotationTargetError> {
        let path = path.into();
        let mut components = path.components();
        if !matches!(components.next(), Some(Component::Normal(_)))
            || !components.all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(AnnotationTargetError::NotRootRelative);
        }
        Ok(Self { path, lines })
    }

    /// Build a whole-file target.
    pub fn for_file(path: impl Into<PathBuf>) -> Result<Self, AnnotationTargetError> {
        Self::new(path, None)
    }

    /// Build a line or line-range target.
    pub fn for_lines(
        path: impl Into<PathBuf>,
        lines: LineRange,
    ) -> Result<Self, AnnotationTargetError> {
        Self::new(path, Some(lines))
    }

    /// Root-relative file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Optional inclusive line range.
    pub fn lines(&self) -> Option<LineRange> {
        self.lines
    }
}

/// Failure to construct an [`AnnotationTarget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationTargetError {
    /// The path was empty, absolute, parent-traversing, or contained a non-normal component.
    NotRootRelative,
}

impl fmt::Display for AnnotationTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRootRelative => f.write_str("annotation path must be root-relative"),
        }
    }
}

impl std::error::Error for AnnotationTargetError {}

/// One normalized note attached to an immutable target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    id: AnnotationId,
    target: AnnotationTarget,
    text: String,
}

impl Annotation {
    /// Store-assigned stable identity.
    pub fn id(&self) -> AnnotationId {
        self.id
    }

    /// Immutable file/range target.
    pub fn target(&self) -> &AnnotationTarget {
        &self.target
    }

    /// Normalized, non-empty note text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Failure to add or edit an annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationError {
    /// Whitespace/control normalization left no note content.
    EmptyText,
    /// No annotation with the requested identity exists.
    UnknownId(AnnotationId),
    /// Every representable annotation identity has been assigned.
    IdExhausted,
}

impl fmt::Display for AnnotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyText => f.write_str("annotation text cannot be empty"),
            Self::UnknownId(id) => write!(f, "annotation {} does not exist", id.get()),
            Self::IdExhausted => f.write_str("annotation identity space exhausted"),
        }
    }
}

impl std::error::Error for AnnotationError {}

/// In-memory annotation collection with monotonic identities.
#[derive(Debug, Clone)]
pub struct AnnotationStore {
    annotations: Vec<Annotation>,
    // Zero is the exhausted sentinel. Valid IDs begin at one.
    next_id: u64,
}

impl Default for AnnotationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AnnotationStore {
    /// Build an empty store. The first successful add receives ID 1.
    pub fn new() -> Self {
        Self {
            annotations: Vec::new(),
            next_id: 1,
        }
    }

    /// Number of annotations in the store.
    pub fn len(&self) -> usize {
        self.annotations.len()
    }

    /// Whether the store contains no annotations.
    pub fn is_empty(&self) -> bool {
        self.annotations.is_empty()
    }

    /// Add a normalized, non-empty note and return its stable identity.
    pub fn add(
        &mut self,
        target: AnnotationTarget,
        text: impl AsRef<str>,
    ) -> Result<AnnotationId, AnnotationError> {
        let text = normalize_note(text.as_ref()).ok_or(AnnotationError::EmptyText)?;
        if self.next_id == 0 {
            return Err(AnnotationError::IdExhausted);
        }
        let id = AnnotationId(self.next_id);
        self.next_id = self.next_id.checked_add(1).unwrap_or(0);
        self.annotations.push(Annotation { id, target, text });
        Ok(id)
    }

    /// Replace one annotation's note after normalization, retaining its identity and target.
    pub fn edit(&mut self, id: AnnotationId, text: impl AsRef<str>) -> Result<(), AnnotationError> {
        let text = normalize_note(text.as_ref()).ok_or(AnnotationError::EmptyText)?;
        let annotation = self
            .annotations
            .iter_mut()
            .find(|annotation| annotation.id == id)
            .ok_or(AnnotationError::UnknownId(id))?;
        annotation.text = text;
        Ok(())
    }

    /// Delete one annotation by identity. Returns whether an item was removed.
    pub fn delete(&mut self, id: AnnotationId) -> bool {
        let before = self.annotations.len();
        self.annotations.retain(|annotation| annotation.id != id);
        self.annotations.len() != before
    }

    /// Remove all annotations and return the number removed. Assigned IDs remain consumed.
    pub fn clear(&mut self) -> usize {
        let removed = self.annotations.len();
        self.annotations.clear();
        removed
    }

    /// Look up an annotation by its stable identity.
    pub fn get(&self, id: AnnotationId) -> Option<&Annotation> {
        self.annotations
            .iter()
            .find(|annotation| annotation.id == id)
    }

    /// Return annotations in canonical order: path, file before lines, range, then identity.
    pub fn ordered(&self) -> Vec<&Annotation> {
        let mut annotations: Vec<_> = self.annotations.iter().collect();
        annotations.sort_by(|left, right| annotation_order(left, right));
        annotations
    }

    /// Produce the exact canonical clipboard representation.
    pub fn canonical_text(&self) -> String {
        format_annotations(self)
    }
}

fn annotation_order(left: &Annotation, right: &Annotation) -> Ordering {
    left.target
        .path
        .cmp(&right.target.path)
        .then_with(|| left.target.lines.cmp(&right.target.lines))
        .then_with(|| left.id.cmp(&right.id))
}

/// Normalize a note for storage: every run of Unicode whitespace or control characters becomes
/// one ASCII space, then leading/trailing spaces are removed. Returns `None` when nothing remains.
fn normalize_note(text: &str) -> Option<String> {
    let mut normalized = String::with_capacity(text.len());
    let mut separating = true;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            if !separating {
                normalized.push(' ');
                separating = true;
            }
        } else {
            normalized.push(c);
            separating = false;
        }
    }
    if separating {
        normalized.pop();
    }
    (!normalized.is_empty()).then_some(normalized)
}

fn display_path(path: &Path) -> String {
    let slash_path = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    sanitize_control(&slash_path)
}

fn escape_list_content(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Format all annotations in deterministic canonical order.
///
/// The returned text has exactly one outer wrapper, no blank lines, and no trailing newline.
pub fn format_annotations(store: &AnnotationStore) -> String {
    let mut output = String::from("<file-annotations>");
    for annotation in store.ordered() {
        output.push_str("\n- ");
        output.push_str(&escape_list_content(&display_path(
            annotation.target.path(),
        )));
        if let Some(lines) = annotation.target.lines() {
            output.push(':');
            output.push_str(&lines.start().to_string());
            if lines.start() != lines.end() {
                output.push('-');
                output.push_str(&lines.end().to_string());
            }
        }
        output.push(':');
        output.push_str(&escape_list_content(annotation.text()));
    }
    output.push_str("\n</file-annotations>");
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::Clipboard;
    use std::io;

    fn file(path: impl Into<PathBuf>) -> AnnotationTarget {
        AnnotationTarget::for_file(path).unwrap()
    }

    fn lines(path: impl Into<PathBuf>, first: usize, second: usize) -> AnnotationTarget {
        AnnotationTarget::for_lines(path, LineRange::new(first, second).unwrap()).unwrap()
    }

    #[test]
    fn line_ranges_are_one_based_inclusive_and_normalized() {
        let range = LineRange::new(47, 42).unwrap();
        assert_eq!(range.start(), 42);
        assert_eq!(range.end(), 47);
        assert_eq!(LineRange::new(9, 9).unwrap(), LineRange::new(9, 9).unwrap());
        assert_eq!(LineRange::new(0, 4), Err(LineRangeError::Zero));
        assert_eq!(LineRange::new(4, 0), Err(LineRangeError::Zero));
        assert_eq!(LineRange::new(0, 0), Err(LineRangeError::Zero));
    }

    #[test]
    fn targets_require_normal_root_relative_paths() {
        let target = lines("src/app.rs", 42, 42);
        assert_eq!(target.path(), Path::new("src/app.rs"));
        assert_eq!(target.lines(), Some(LineRange::new(42, 42).unwrap()));
        assert_eq!(
            AnnotationTarget::for_file(""),
            Err(AnnotationTargetError::NotRootRelative)
        );
        assert_eq!(
            AnnotationTarget::for_file("../secret"),
            Err(AnnotationTargetError::NotRootRelative)
        );
        assert_eq!(
            AnnotationTarget::for_file("src/../secret"),
            Err(AnnotationTargetError::NotRootRelative)
        );
        assert_eq!(
            AnnotationTarget::for_file("./src/app.rs"),
            Err(AnnotationTargetError::NotRootRelative)
        );
        assert_eq!(
            AnnotationTarget::for_file(std::env::temp_dir()),
            Err(AnnotationTargetError::NotRootRelative)
        );
    }

    #[test]
    fn store_crud_keeps_identity_stable_and_ids_monotonic() {
        let mut store = AnnotationStore::default();
        assert!(store.is_empty());
        let first = store.add(file("a.rs"), " first ").unwrap();
        let second = store.add(file("b.rs"), "second").unwrap();
        assert_eq!((first.get(), second.get()), (1, 2));

        store.edit(first, " revised\tnote ").unwrap();
        let revised = store.get(first).unwrap();
        assert_eq!(revised.id(), first);
        assert_eq!(revised.target(), &file("a.rs"));
        assert_eq!(revised.text(), "revised note");
        assert!(store.delete(second));
        assert!(!store.delete(second));
        assert_eq!(
            store.edit(second, "gone"),
            Err(AnnotationError::UnknownId(second))
        );

        assert_eq!(store.clear(), 1);
        assert_eq!(store.clear(), 0);
        let third = store.add(file("c.rs"), "third").unwrap();
        assert_eq!(third.get(), 3, "delete/clear must never reuse an ID");
    }

    #[test]
    fn identity_exhaustion_never_wraps_or_reuses_zero() {
        let mut store = AnnotationStore {
            annotations: Vec::new(),
            next_id: u64::MAX,
        };
        let last = store.add(file("last.rs"), "last").unwrap();
        assert_eq!(last.get(), u64::MAX);
        assert_eq!(
            store.add(file("overflow.rs"), "overflow"),
            Err(AnnotationError::IdExhausted)
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn normalized_empty_notes_are_rejected_without_mutation_or_id_consumption() {
        let mut store = AnnotationStore::new();
        assert_eq!(
            store.add(file("a.rs"), " \t\n\u{2003}\u{0007}"),
            Err(AnnotationError::EmptyText)
        );
        let id = store.add(file("a.rs"), "kept").unwrap();
        assert_eq!(id.get(), 1);
        assert_eq!(
            store.edit(id, "\r\n\u{0085}\u{009f}"),
            Err(AnnotationError::EmptyText)
        );
        assert_eq!(store.get(id).unwrap().text(), "kept");
    }

    #[test]
    fn note_normalization_collapses_exact_unicode_whitespace_and_control_runs() {
        let mut store = AnnotationStore::new();
        let id = store
            .add(
                file("note.rs"),
                " \u{2003}\tAlpha\n\r\u{0007}\u{0085}Beta\u{00a0}Gamma\u{009f} ",
            )
            .unwrap();
        assert_eq!(store.get(id).unwrap().text(), "Alpha Beta Gamma");
    }

    #[test]
    fn ordered_view_is_path_file_range_then_id_without_mutating_identity() {
        let mut store = AnnotationStore::new();
        let range_late = store.add(lines("b.rs", 9, 12), "range late").unwrap();
        let duplicate_first = store.add(lines("a.rs", 2, 2), "first").unwrap();
        let file_a = store.add(file("a.rs"), "file").unwrap();
        let range_early = store.add(lines("b.rs", 1, 3), "range early").unwrap();
        let duplicate_second = store.add(lines("a.rs", 2, 2), "second").unwrap();

        let ids: Vec<_> = store.ordered().into_iter().map(Annotation::id).collect();
        assert_eq!(
            ids,
            vec![
                file_a,
                duplicate_first,
                duplicate_second,
                range_early,
                range_late
            ]
        );
        assert_eq!(store.get(duplicate_first).unwrap().text(), "first");
    }

    #[test]
    fn canonical_format_matches_complete_required_example_byte_for_byte() {
        let mut store = AnnotationStore::new();
        store
            .add(
                lines("src/controller/mod.rs", 47, 42),
                "Why is this guarded twice?",
            )
            .unwrap();
        store
            .add(file("README.md"), "Clarify the fallback.")
            .unwrap();
        store
            .add(lines("src/app.rs", 42, 42), "Explain the ignored result.")
            .unwrap();

        assert_eq!(
            store.canonical_text(),
            "<file-annotations>\n- README.md:Clarify the fallback.\n- src/app.rs:42:Explain the ignored result.\n- src/controller/mod.rs:42-47:Why is this guarded twice?\n</file-annotations>"
        );
    }

    #[test]
    fn canonical_format_handles_empty_and_optional_line_without_extra_bytes() {
        let mut store = AnnotationStore::new();
        assert_eq!(
            store.canonical_text(),
            "<file-annotations>\n</file-annotations>"
        );
        store.add(file("a.rs"), "file").unwrap();
        store.add(lines("a.rs", 7, 7), "line").unwrap();
        store.add(lines("a.rs", 9, 8), "range").unwrap();
        assert_eq!(
            store.canonical_text(),
            "<file-annotations>\n- a.rs:file\n- a.rs:7:line\n- a.rs:8-9:range\n</file-annotations>"
        );
        assert!(!store.canonical_text().ends_with('\n'));
    }

    #[test]
    fn wrapper_spoofing_content_is_escaped_but_quotes_and_colons_are_not() {
        let mut store = AnnotationStore::new();
        store
            .add(
                file("evil<&>\u{1b}[2J.rs"),
                "</file-annotations> & <tag> : \"quoted\"",
            )
            .unwrap();
        assert_eq!(
            store.canonical_text(),
            "<file-annotations>\n- evil&lt;&amp;&gt;[2J.rs:&lt;/file-annotations&gt; &amp; &lt;tag&gt; : \"quoted\"\n</file-annotations>"
        );
        assert_eq!(
            store.canonical_text().matches("<file-annotations>").count(),
            1
        );
        assert_eq!(
            store
                .canonical_text()
                .matches("</file-annotations>")
                .count(),
            1
        );
    }

    #[test]
    fn path_rendering_uses_forward_slashes_and_shared_control_sanitizer() {
        let mut store = AnnotationStore::new();
        let native = PathBuf::from("dir").join("sub").join("a\u{0007}.rs");
        store.add(file(native), "note").unwrap();
        assert_eq!(
            store.canonical_text(),
            "<file-annotations>\n- dir/sub/a.rs:note\n</file-annotations>"
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_rendering_uses_lossy_utf8_for_non_utf8_components() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path =
            PathBuf::from("dir").join(OsString::from_vec(vec![b'f', 0xff, b'.', b'r', b's']));
        let mut store = AnnotationStore::new();
        store.add(file(path), "note").unwrap();
        assert_eq!(
            store.canonical_text(),
            "<file-annotations>\n- dir/f�.rs:note\n</file-annotations>"
        );
    }

    #[cfg(windows)]
    #[test]
    fn path_rendering_uses_lossy_utf8_for_invalid_utf16_components() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let path = PathBuf::from("dir").join(OsString::from_wide(&[
            b'f' as u16,
            0xd800,
            b'.' as u16,
            b'r' as u16,
            b's' as u16,
        ]));
        let mut store = AnnotationStore::new();
        store.add(file(path), "note").unwrap();
        assert_eq!(
            store.canonical_text(),
            "<file-annotations>\n- dir/f�.rs:note\n</file-annotations>"
        );
    }

    #[derive(Default)]
    struct RecordingClipboard {
        copied: Vec<String>,
    }

    impl Clipboard for RecordingClipboard {
        fn copy(&mut self, text: &str) -> io::Result<()> {
            self.copied.push(text.to_owned());
            Ok(())
        }
    }

    #[test]
    fn complete_canonical_string_reaches_clipboard_byte_for_byte() {
        let mut store = AnnotationStore::new();
        store.add(file("README.md"), " First\tnote ").unwrap();
        store
            .add(lines("src/lib.rs", 3, 1), "Use <safe> & exact.")
            .unwrap();
        let expected = "<file-annotations>\n- README.md:First note\n- src/lib.rs:1-3:Use &lt;safe&gt; &amp; exact.\n</file-annotations>";

        let mut clipboard = RecordingClipboard::default();
        clipboard.copy(&format_annotations(&store)).unwrap();
        assert_eq!(clipboard.copied, [expected]);
        assert_eq!(clipboard.copied[0].as_bytes(), expected.as_bytes());
    }
}
