//! View Policy — a pure decision: which content-pane view mode a file gets.
//!
//! Precedence (design.md): changed → diff (even for markdown, AC-9); else markdown →
//! rendered (AC-8); else → syntax-highlighted content (AC-10). The applicable set
//! (AC-11) is what a mode-cycle key steps through; for a changed file it also offers a
//! full-context diff (the whole file with line numbers and the diff shown inline). No I/O.

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
    /// An image or a video, placed inline via the herdr graphics socket. One mode covers both:
    /// the mode is chosen per file (media kind) and playback state lives in the controller.
    Media,
}

/// The facts the policy needs about a file — no path I/O is performed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDescriptor {
    pub path: PathBuf,
    pub is_markdown: bool,
    pub is_changed: bool,
    /// What kind of media the path names, if any (`None` for plain text/diff files).
    pub media: Option<crate::media::MediaKind>,
}

/// The auto-selected default view mode for a file.
pub fn default_mode(fd: &FileDescriptor) -> ViewMode {
    if fd.is_changed {
        ViewMode::Diff
    } else if fd.media.is_some() {
        ViewMode::Media
    } else if fd.is_markdown {
        ViewMode::RenderedMarkdown
    } else {
        ViewMode::SyntaxContent
    }
}

/// The modes a cycle key steps through for a file, default first (AC-11). A changed file
/// also offers a full-context diff (whole file + line numbers + inline diff) right after
/// the compact diff; markdown adds its rendered view; media adds a media view; every file
/// ends with syntax content.
pub fn applicable_modes(fd: &FileDescriptor) -> Vec<ViewMode> {
    let mut modes = vec![default_mode(fd)];
    let add = |modes: &mut Vec<ViewMode>, m: ViewMode| {
        if !modes.contains(&m) {
            modes.push(m);
        }
    };
    if fd.is_changed {
        add(&mut modes, ViewMode::Diff);
        add(&mut modes, ViewMode::FullDiff);
    }
    if fd.media.is_some() {
        add(&mut modes, ViewMode::Media);
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
            media: None,
        }
    }

    fn media_fd(name: &str, is_changed: bool) -> FileDescriptor {
        FileDescriptor {
            path: PathBuf::from(name),
            is_markdown: false,
            is_changed,
            media: Some(crate::media::MediaKind::Png),
        }
    }

    #[test]
    fn unchanged_markdown_defaults_to_rendered_markdown() {
        assert_eq!(
            default_mode(&fd("README.md", true, false)),
            ViewMode::RenderedMarkdown
        );
    }

    #[test]
    fn changed_file_defaults_to_diff_even_when_markdown() {
        assert_eq!(default_mode(&fd("README.md", true, true)), ViewMode::Diff);
        assert_eq!(default_mode(&fd("main.rs", false, true)), ViewMode::Diff);
    }

    #[test]
    fn unchanged_non_markdown_defaults_to_syntax_content() {
        assert_eq!(
            default_mode(&fd("main.rs", false, false)),
            ViewMode::SyntaxContent
        );
    }

    #[test]
    fn changed_file_cycle_offers_a_full_context_diff_right_after_the_compact_diff() {
        // AC-11: a changed file can cycle from the compact diff to a full-context diff
        // (whole file + line numbers + inline diff) before the content views.
        let modes = applicable_modes(&fd("main.rs", false, true));
        assert_eq!(
            modes,
            vec![ViewMode::Diff, ViewMode::FullDiff, ViewMode::SyntaxContent]
        );
        // For a changed markdown file the rendered view sits after the two diff views.
        let md = applicable_modes(&fd("README.md", true, true));
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
    fn unchanged_file_has_no_diff_views_in_its_cycle() {
        // A full-context (or compact) diff only makes sense for a changed file — there is no
        // diff for an unchanged one, so neither diff mode is offered.
        for md in [true, false] {
            let modes = applicable_modes(&fd("x", md, false));
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
        assert_eq!(applicable_modes(&f).first(), Some(&default_mode(&f)));
    }

    #[test]
    fn applicable_modes_have_no_duplicates() {
        let f = fd("README.md", true, true);
        let modes = applicable_modes(&f);
        let mut seen = modes.clone();
        seen.dedup();
        assert_eq!(modes, seen, "applicable modes must not repeat");
    }

    #[test]
    fn media_defaults_to_media_unless_changed() {
        // A media file defaults to Media, but a changed media file is Diff (git first).
        assert_eq!(default_mode(&media_fd("image.png", false)), ViewMode::Media);
        assert_eq!(default_mode(&media_fd("image.png", true)), ViewMode::Diff);
    }

    #[test]
    fn media_cycle_offers_media_then_plain_content() {
        // `Tab` still reaches the plain placeholder text beneath the image (AC-11).
        let modes = applicable_modes(&media_fd("image.png", false));
        assert_eq!(modes, vec![ViewMode::Media, ViewMode::SyntaxContent]);
        // A changed media file puts the diffs first, media next, plain content last.
        let changed = applicable_modes(&media_fd("image.png", true));
        assert_eq!(
            changed,
            vec![
                ViewMode::Diff,
                ViewMode::FullDiff,
                ViewMode::Media,
                ViewMode::SyntaxContent
            ]
        );
    }
}
