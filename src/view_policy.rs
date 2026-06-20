//! View Policy — a pure decision: which content-pane view mode a file gets.
//!
//! Precedence (design.md): changed → diff (even for markdown, AC-9); else markdown →
//! rendered (AC-8); else → syntax-highlighted content (AC-10). The applicable set
//! (AC-11) is what a mode-cycle key steps through, always including raw content so the
//! user can override the auto-selected default. No I/O.

use std::path::PathBuf;

/// Which rendering the content pane is showing for the selected file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Markdown rendered to formatted text.
    RenderedMarkdown,
    /// Unified diff against the active baseline.
    Diff,
    /// Syntax-highlighted file content.
    SyntaxContent,
    /// Plain, unstyled file content (the override floor).
    RawContent,
}

/// The facts the policy needs about a file — no path I/O is performed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDescriptor {
    pub path: PathBuf,
    pub is_markdown: bool,
    pub is_changed: bool,
}

/// The auto-selected default view mode for a file.
pub fn default_mode(fd: &FileDescriptor) -> ViewMode {
    if fd.is_changed {
        ViewMode::Diff
    } else if fd.is_markdown {
        ViewMode::RenderedMarkdown
    } else {
        ViewMode::SyntaxContent
    }
}

/// The modes a cycle key steps through for a file, default first, always ending with
/// a raw-content override (AC-11).
pub fn applicable_modes(fd: &FileDescriptor) -> Vec<ViewMode> {
    let mut modes = vec![default_mode(fd)];
    let add = |modes: &mut Vec<ViewMode>, m: ViewMode| {
        if !modes.contains(&m) {
            modes.push(m);
        }
    };
    if fd.is_changed {
        add(&mut modes, ViewMode::Diff);
    }
    if fd.is_markdown {
        add(&mut modes, ViewMode::RenderedMarkdown);
    }
    add(&mut modes, ViewMode::SyntaxContent);
    add(&mut modes, ViewMode::RawContent);
    modes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fd(name: &str, is_markdown: bool, is_changed: bool) -> FileDescriptor {
        FileDescriptor { path: PathBuf::from(name), is_markdown, is_changed }
    }

    #[test]
    fn unchanged_markdown_defaults_to_rendered_markdown() {
        assert_eq!(default_mode(&fd("README.md", true, false)), ViewMode::RenderedMarkdown);
    }

    #[test]
    fn changed_file_defaults_to_diff_even_when_markdown() {
        assert_eq!(default_mode(&fd("README.md", true, true)), ViewMode::Diff);
        assert_eq!(default_mode(&fd("main.rs", false, true)), ViewMode::Diff);
    }

    #[test]
    fn unchanged_non_markdown_defaults_to_syntax_content() {
        assert_eq!(default_mode(&fd("main.rs", false, false)), ViewMode::SyntaxContent);
    }

    #[test]
    fn applicable_modes_always_include_raw_content_for_override() {
        for (md, ch) in [(true, false), (true, true), (false, false), (false, true)] {
            let modes = applicable_modes(&fd("x", md, ch));
            assert!(
                modes.contains(&ViewMode::RawContent),
                "RawContent must be cyclable (md={md}, changed={ch})"
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
}
