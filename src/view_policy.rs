//! View Policy — a pure decision: which content-pane view mode a file gets.
//!
//! Precedence (design.md): deleted → diff; other changed files → the configured preference
//! (diff by default, or the normal file-type view); else markdown → rendered (AC-8); else →
//! syntax-highlighted content (AC-10). The applicable set (AC-11) is what a mode-cycle key steps
//! through; for a changed file it also offers a full-context diff (the whole file with line numbers
//! and the diff shown inline). No I/O.

use std::path::PathBuf;

/// Which rendering the content pane is showing for the selected file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Markdown rendered to formatted text.
    RenderedMarkdown,
    /// Unified diff against the active baseline — only the changed hunks.
    Diff,
    /// Full-context diff against the active baseline: the whole file with a line-number
    /// gutter, syntax highlighting on unchanged lines, and the diff shown inline.
    FullDiff,
    /// Syntax-highlighted file content.
    SyntaxContent,
}

/// Which policy changed files use for their automatic initial view.
///
/// `Content` means the normal file-type policy for paths that still exist (render Markdown,
/// syntax-highlight everything else), not a forced [`ViewMode::SyntaxContent`] view. Deleted paths
/// remain diff-first because they have no content to render. Manual cycling still includes both
/// diff modes regardless of this preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChangedFileView {
    /// Prefer the compact diff, preserving the viewer's original behavior.
    #[default]
    Diff,
    /// Bypass the Git-specific preference and use the normal file-type view.
    Content,
}

impl ChangedFileView {
    /// The lowercase config/help label.
    pub fn label(self) -> &'static str {
        match self {
            ChangedFileView::Diff => "diff",
            ChangedFileView::Content => "content",
        }
    }
}

/// The facts the policy needs about a file — no path I/O is performed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDescriptor {
    pub path: PathBuf,
    pub is_markdown: bool,
    pub is_changed: bool,
    /// The cached Git status says this path was deleted and therefore has no content to render.
    pub is_deleted: bool,
}

/// The normal non-Git view mode for a file, based only on its type.
fn content_mode(fd: &FileDescriptor) -> ViewMode {
    if fd.is_markdown {
        ViewMode::RenderedMarkdown
    } else {
        ViewMode::SyntaxContent
    }
}

/// The auto-selected default view mode for a file.
pub fn default_mode(fd: &FileDescriptor, changed_file_view: ChangedFileView) -> ViewMode {
    if fd.is_changed && (fd.is_deleted || changed_file_view == ChangedFileView::Diff) {
        ViewMode::Diff
    } else {
        content_mode(fd)
    }
}

/// The modes a cycle key steps through for a file, default first (AC-11). A changed file
/// also offers a full-context diff (whole file + line numbers + inline diff) right after
/// the compact diff; markdown adds its rendered view; every file ends with syntax content.
pub fn applicable_modes(fd: &FileDescriptor, changed_file_view: ChangedFileView) -> Vec<ViewMode> {
    let mut modes = vec![default_mode(fd, changed_file_view)];
    let add = |modes: &mut Vec<ViewMode>, m: ViewMode| {
        if !modes.contains(&m) {
            modes.push(m);
        }
    };
    if fd.is_changed {
        add(&mut modes, ViewMode::Diff);
        add(&mut modes, ViewMode::FullDiff);
    }
    if fd.is_markdown {
        add(&mut modes, ViewMode::RenderedMarkdown);
    }
    add(&mut modes, ViewMode::SyntaxContent);
    modes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fd(name: &str, is_markdown: bool, is_changed: bool) -> FileDescriptor {
        FileDescriptor {
            path: PathBuf::from(name),
            is_markdown,
            is_changed,
            is_deleted: false,
        }
    }

    fn deleted_fd(name: &str, is_markdown: bool) -> FileDescriptor {
        FileDescriptor {
            path: PathBuf::from(name),
            is_markdown,
            is_changed: true,
            is_deleted: true,
        }
    }

    #[test]
    fn unchanged_markdown_defaults_to_rendered_markdown() {
        for preference in [ChangedFileView::Diff, ChangedFileView::Content] {
            assert_eq!(
                default_mode(&fd("README.md", true, false), preference),
                ViewMode::RenderedMarkdown
            );
        }
    }

    #[test]
    fn changed_file_defaults_to_diff_even_when_markdown() {
        assert_eq!(
            default_mode(&fd("README.md", true, true), ChangedFileView::Diff),
            ViewMode::Diff
        );
        assert_eq!(
            default_mode(&fd("main.rs", false, true), ChangedFileView::Diff),
            ViewMode::Diff
        );
    }

    #[test]
    fn changed_file_content_preference_uses_the_normal_file_type_policy() {
        assert_eq!(
            default_mode(&fd("README.md", true, true), ChangedFileView::Content),
            ViewMode::RenderedMarkdown
        );
        assert_eq!(
            default_mode(&fd("main.rs", false, true), ChangedFileView::Content),
            ViewMode::SyntaxContent
        );
    }

    #[test]
    fn deleted_file_stays_diff_first_under_content_preference() {
        for is_markdown in [false, true] {
            assert_eq!(
                default_mode(
                    &deleted_fd(if is_markdown { "gone.md" } else { "gone.rs" }, is_markdown),
                    ChangedFileView::Content
                ),
                ViewMode::Diff,
                "a deleted path has no on-disk content to render"
            );
        }
    }

    #[test]
    fn unchanged_non_markdown_defaults_to_syntax_content() {
        for preference in [ChangedFileView::Diff, ChangedFileView::Content] {
            assert_eq!(
                default_mode(&fd("main.rs", false, false), preference),
                ViewMode::SyntaxContent
            );
        }
    }

    #[test]
    fn changed_file_cycle_offers_a_full_context_diff_right_after_the_compact_diff() {
        // AC-11: a changed file can cycle from the compact diff to a full-context diff
        // (whole file + line numbers + inline diff) before the content views.
        let modes = applicable_modes(&fd("main.rs", false, true), ChangedFileView::Diff);
        assert_eq!(
            modes,
            vec![ViewMode::Diff, ViewMode::FullDiff, ViewMode::SyntaxContent]
        );
        // For a changed markdown file the rendered view sits after the two diff views.
        let md = applicable_modes(&fd("README.md", true, true), ChangedFileView::Diff);
        assert_eq!(
            md,
            vec![
                ViewMode::Diff,
                ViewMode::FullDiff,
                ViewMode::RenderedMarkdown,
                ViewMode::SyntaxContent
            ]
        );
    }

    #[test]
    fn content_preference_cycle_starts_normally_and_keeps_diff_available() {
        assert_eq!(
            applicable_modes(&fd("main.rs", false, true), ChangedFileView::Content),
            vec![ViewMode::SyntaxContent, ViewMode::Diff, ViewMode::FullDiff]
        );
        assert_eq!(
            applicable_modes(&fd("README.md", true, true), ChangedFileView::Content),
            vec![
                ViewMode::RenderedMarkdown,
                ViewMode::Diff,
                ViewMode::FullDiff,
                ViewMode::SyntaxContent
            ]
        );
    }

    #[test]
    fn unchanged_file_has_no_diff_views_in_its_cycle() {
        // A full-context (or compact) diff only makes sense for a changed file — there is no
        // diff for an unchanged one, so neither diff mode is offered.
        for md in [true, false] {
            let modes = applicable_modes(&fd("x", md, false), ChangedFileView::Content);
            assert!(
                !modes.contains(&ViewMode::Diff),
                "no compact diff when unchanged (md={md})"
            );
            assert!(
                !modes.contains(&ViewMode::FullDiff),
                "no full diff when unchanged (md={md})"
            );
        }
    }

    #[test]
    fn applicable_modes_start_with_the_default_so_cycling_overrides_it() {
        let f = fd("README.md", true, false);
        for preference in [ChangedFileView::Diff, ChangedFileView::Content] {
            assert_eq!(
                applicable_modes(&f, preference).first(),
                Some(&default_mode(&f, preference))
            );
        }
    }

    #[test]
    fn applicable_modes_have_no_duplicates() {
        let f = fd("README.md", true, true);
        for preference in [ChangedFileView::Diff, ChangedFileView::Content] {
            let modes = applicable_modes(&f, preference);
            let mut seen = modes.clone();
            seen.dedup();
            assert_eq!(modes, seen, "applicable modes must not repeat");
        }
    }
}
