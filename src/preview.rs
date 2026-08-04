//! Preview values shared by the active and pinned preview surfaces.

use crate::infile::SearchState;
use crate::view_policy::ViewMode;
use ratatui::text::Text;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The branch state captured with a preview origin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BranchState {
    /// The branch checked out when the preview was rendered.
    Named(String),
    /// The preview came from a detached HEAD (or another branchless root).
    Detached,
}

/// The identity used to determine whether two previews represent the same file.
///
/// A root identity captures both its root/worktree and branch state. A preview is the same file
/// only when that root identity and its root-relative path match. The absolute path is retained
/// for copying, but does not participate in pin replacement/removal decisions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewIdentity {
    pub root: PathBuf,
    pub branch: BranchState,
    pub root_relative_path: PathBuf,
}

/// The captured location and branch metadata for a display-ready preview.
///
/// This value is purely descriptive. It does not keep the root alive or access the filesystem
/// after capture, allowing a pinned preview to survive a worktree switch or removal.
///
/// ```compile_fail
/// use herdr_file_viewer::preview::{
///     BranchState, PreviewDocument, PreviewOrigin, PreviewPresentation,
/// };
///
/// fn mutate(
///     document: &mut PreviewDocument,
///     origin: &mut PreviewOrigin,
///     presentation: &mut PreviewPresentation,
/// ) {
///     document.notices.push("changed".into());
///     origin.branch = BranchState::Detached;
///     presentation.wrap = true;
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewOrigin {
    /// The root/worktree that produced the preview.
    root: PathBuf,
    /// The branch state captured from that root.
    branch: BranchState,
    /// The fully-qualified path captured for an absolute-path copy request.
    absolute_path: PathBuf,
    /// The path below the root, captured for display and relative-path copying.
    root_relative_path: PathBuf,
}

impl PreviewOrigin {
    /// Capture the location and branch metadata for a preview.
    pub fn new(
        root: PathBuf,
        branch: BranchState,
        absolute_path: PathBuf,
        root_relative_path: PathBuf,
    ) -> Self {
        Self {
            root,
            branch,
            absolute_path,
            root_relative_path,
        }
    }

    /// Return the root/worktree that produced the preview.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return the branch state captured from that root.
    pub fn branch(&self) -> &BranchState {
        &self.branch
    }

    /// Return the fully-qualified path retained for absolute-path copy requests.
    pub fn absolute_path(&self) -> &Path {
        &self.absolute_path
    }

    /// Return the root-relative path retained for display and relative-path copy requests.
    pub fn root_relative_path(&self) -> &Path {
        &self.root_relative_path
    }

    /// Return the stable file identity used by the pin lifecycle.
    pub fn identity(&self) -> PreviewIdentity {
        PreviewIdentity {
            root: self.root.clone(),
            branch: self.branch.clone(),
            root_relative_path: self.root_relative_path.clone(),
        }
    }
}

/// Presentation facts that determine how already-rendered preview content is projected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewPresentation {
    /// The view that produced the captured display content.
    view_mode: ViewMode,
    /// Whether the captured preview is displayed with line wrapping.
    wrap: bool,
    /// Whether transformed content needs a left inset inside its border.
    pad_left: bool,
}

impl PreviewPresentation {
    /// Capture the projection facts applied to preview content.
    pub fn new(view_mode: ViewMode, wrap: bool, pad_left: bool) -> Self {
        Self {
            view_mode,
            wrap,
            pad_left,
        }
    }

    /// Return the view that produced the captured display content.
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// Return whether the captured preview is displayed with line wrapping.
    pub fn wrap(&self) -> bool {
        self.wrap
    }

    /// Return whether transformed content needs a left inset inside its border.
    pub fn pad_left(&self) -> bool {
        self.pad_left
    }
}

/// An immutable, display-ready file preview.
///
/// This value contains only applied file-preview data. Loading placeholders, selection state,
/// action notices, and render lifecycle flags remain session-controller concerns so a cloned
/// document is safe to freeze as a pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewDocument {
    /// Bounded, sanitized display content, including its renderer-provided styling. Shared, not
    /// owned: projections and pins clone the handle, never the styled lines.
    content: Arc<Text<'static>>,
    /// Notices specific to this rendered content, such as truncation or a renderer fallback.
    notices: Vec<String>,
    /// Source lines retained for source-mapped views; absent for transformed views.
    source: Option<Vec<String>>,
    /// The display mode and projection details captured with the content.
    presentation: PreviewPresentation,
    /// The original file location and branch metadata.
    origin: PreviewOrigin,
}

impl PreviewDocument {
    /// Capture a complete, display-ready preview.
    pub fn new(
        content: Text<'static>,
        notices: Vec<String>,
        source: Option<Vec<String>>,
        presentation: PreviewPresentation,
        origin: PreviewOrigin,
    ) -> Self {
        Self {
            content: Arc::new(content),
            notices,
            source,
            presentation,
            origin,
        }
    }

    /// Return the bounded, sanitized display content.
    pub fn content(&self) -> &Text<'static> {
        &self.content
    }

    /// Return a cheap shared handle to the display content, for per-frame projections.
    pub fn shared_content(&self) -> Arc<Text<'static>> {
        Arc::clone(&self.content)
    }

    /// A copy of this document under a different projection presentation, sharing the same
    /// content. The document stays immutable; a presentation change never re-copies the lines.
    pub fn with_presentation(&self, presentation: PreviewPresentation) -> Self {
        Self {
            content: Arc::clone(&self.content),
            notices: self.notices.clone(),
            source: self.source.clone(),
            presentation,
            origin: self.origin.clone(),
        }
    }

    /// Return notices specific to this rendered content.
    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    /// Return source lines for source-mapped views, when retained.
    pub fn source(&self) -> Option<&[String]> {
        self.source.as_deref()
    }

    /// Return the display mode and projection details captured with the content.
    pub fn presentation(&self) -> &PreviewPresentation {
        &self.presentation
    }

    /// Return the original file location and branch metadata.
    pub fn origin(&self) -> &PreviewOrigin {
        &self.origin
    }
}

/// A displayed-text selection local to one preview.
///
/// The active preview uses this value for its ambient mouse-drag selection and for the line-select
/// modal. A pinned preview never accepts selection input, but keeping the value with the other
/// interaction data makes a copied interaction state self-contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewSelection {
    anchor: usize,
    marker: usize,
    anchor_col: usize,
    marker_col: usize,
    char_mode: bool,
}

impl PreviewSelection {
    /// Start a collapsed whole-line selection on `line`.
    pub fn new(line: usize) -> Self {
        Self {
            anchor: line,
            marker: line,
            anchor_col: 0,
            marker_col: 0,
            char_mode: false,
        }
    }

    /// Begin a character-granular selection, clamped to the displayed line extent.
    pub fn begin_char(&mut self, line: usize, col: usize, last: usize) {
        let line = Self::clamp(line, last);
        self.anchor = line;
        self.marker = line;
        self.anchor_col = col;
        self.marker_col = col;
        self.char_mode = true;
    }

    /// Extend a character-granular selection, retaining its original anchor.
    pub fn drag_char(&mut self, line: usize, col: usize, last: usize) {
        self.marker = Self::clamp(line, last);
        self.marker_col = col;
        self.char_mode = true;
    }

    /// Return whether this is a character-granular selection.
    pub fn is_char_mode(&self) -> bool {
        self.char_mode
    }

    /// Return the ordered character span as `(line, column)` carets.
    pub fn char_span(&self) -> ((usize, usize), (usize, usize)) {
        let anchor = (self.anchor, self.anchor_col);
        let marker = (self.marker, self.marker_col);
        if anchor <= marker {
            (anchor, marker)
        } else {
            (marker, anchor)
        }
    }

    /// Move and collapse the marker onto a whole-line selection.
    pub fn move_to(&mut self, line: usize, last: usize) {
        self.marker = Self::clamp(line, last);
        self.anchor = self.marker;
        self.char_mode = false;
    }

    /// Move the marker while retaining the whole-line selection anchor.
    pub fn extend_to(&mut self, line: usize, last: usize) {
        self.marker = Self::clamp(line, last);
        self.char_mode = false;
    }

    /// Return the marker line, using the preview's one-based line convention.
    pub fn marker(&self) -> usize {
        self.marker
    }

    /// Return the selected whole-line range in ascending order.
    pub fn selection(&self) -> (usize, usize) {
        if self.anchor <= self.marker {
            (self.anchor, self.marker)
        } else {
            (self.marker, self.anchor)
        }
    }

    fn clamp(line: usize, last: usize) -> usize {
        line.max(1).min(last.max(1))
    }
}

/// Mutable, session-only interactions for one preview document.
///
/// Active and pinned previews receive separate instances so their viewport and search state never
/// share mutations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewInteractionState {
    /// Vertical viewport offset in displayed rows.
    pub vertical_scroll: u16,
    /// Horizontal viewport offset in display columns.
    pub horizontal_scroll: u16,
    /// The last measured drawable width for this preview.
    pub viewport_width: u16,
    /// The last measured drawable height for this preview.
    pub viewport_height: u16,
    /// The preview-local in-file search state, when a search has begun.
    pub search: Option<SearchState>,
    /// A transient displayed-text selection. Only the active preview projects or mutates it.
    pub selection: Option<PreviewSelection>,
}

#[cfg(test)]
mod tests {
    use super::{
        BranchState, PreviewDocument, PreviewInteractionState, PreviewOrigin, PreviewPresentation,
    };
    use crate::infile::SearchState;
    use crate::search::Match;
    use crate::view_policy::ViewMode;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span, Text};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn origin(root: &str) -> PreviewOrigin {
        PreviewOrigin {
            root: PathBuf::from(root),
            branch: BranchState::Named("feature/pin".into()),
            absolute_path: PathBuf::from(root).join("src/lib.rs"),
            root_relative_path: PathBuf::from("src/lib.rs"),
        }
    }

    fn document() -> PreviewDocument {
        PreviewDocument {
            content: Arc::new(Text::from(Line::from(Span::styled(
                "pub fn run() {}",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )))),
            notices: vec!["renderer fallback: plain text".into()],
            source: Some(vec!["pub fn run() {}".into()]),
            presentation: PreviewPresentation {
                view_mode: ViewMode::SyntaxContent,
                wrap: false,
                pad_left: false,
            },
            origin: origin("/worktrees/feature"),
        }
    }

    #[test]
    fn preview_document_equality_covers_every_captured_value() {
        let document = document();
        assert_eq!(document.clone(), document);

        assert_ne!(
            document,
            PreviewDocument {
                content: Arc::new(Text::from(Line::from(Span::styled(
                    "pub fn run() {}",
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                )))),
                ..document.clone()
            },
            "styled display content is part of a frozen document"
        );
        assert_ne!(
            document,
            PreviewDocument {
                notices: vec!["truncated preview".into()],
                ..document.clone()
            },
            "content-specific notices are part of a frozen document"
        );
        assert_ne!(
            document,
            PreviewDocument {
                source: None,
                ..document.clone()
            },
            "retained source metadata is part of a frozen document"
        );
        assert_ne!(
            document,
            PreviewDocument {
                presentation: PreviewPresentation {
                    view_mode: ViewMode::RenderedMarkdown,
                    wrap: true,
                    pad_left: true,
                },
                ..document.clone()
            },
            "presentation metadata is part of a frozen document"
        );
        assert_ne!(
            document,
            PreviewDocument {
                origin: PreviewOrigin {
                    absolute_path: PathBuf::from("/worktrees/feature/src/main.rs"),
                    ..origin("/worktrees/feature")
                },
                ..document.clone()
            },
            "the absolute path is part of a frozen document"
        );
        assert_ne!(
            document,
            PreviewDocument {
                origin: PreviewOrigin {
                    root_relative_path: PathBuf::from("src/main.rs"),
                    ..origin("/worktrees/feature")
                },
                ..document.clone()
            },
            "the root-relative path is part of a frozen document"
        );
        assert_ne!(
            document,
            PreviewDocument {
                origin: PreviewOrigin {
                    root: PathBuf::from("/worktrees/other"),
                    ..origin("/worktrees/feature")
                },
                ..document.clone()
            },
            "the root identity is part of a frozen document"
        );
        assert_ne!(
            document,
            PreviewDocument {
                origin: PreviewOrigin {
                    branch: BranchState::Detached,
                    ..origin("/worktrees/feature")
                },
                ..document.clone()
            },
            "the branch identity is part of a frozen document"
        );
    }

    #[test]
    fn preview_identity_is_the_root_identity_and_root_relative_path_pair() {
        let original = origin("/worktrees/feature");
        let same_file = origin("/worktrees/feature");
        let another_file = PreviewOrigin {
            root_relative_path: PathBuf::from("src/main.rs"),
            absolute_path: PathBuf::from("/worktrees/feature/src/main.rs"),
            ..origin("/worktrees/feature")
        };

        assert_eq!(original.identity(), same_file.identity());
        let same_file_with_a_different_absolute_path = PreviewOrigin {
            absolute_path: PathBuf::from("/copied-preview/src/lib.rs"),
            ..origin("/worktrees/feature")
        };
        assert_eq!(
            original.identity(),
            same_file_with_a_different_absolute_path.identity(),
            "absolute paths are retained for copying, not preview identity"
        );
        assert_ne!(original.identity(), another_file.identity());

        let detached = PreviewOrigin {
            branch: BranchState::Detached,
            ..origin("/worktrees/feature")
        };
        assert_ne!(original.identity(), detached.identity());
    }

    #[test]
    fn the_same_relative_path_in_two_roots_has_a_different_preview_identity() {
        assert_ne!(
            origin("/worktrees/feature-a").identity(),
            origin("/worktrees/feature-b").identity()
        );
    }

    #[test]
    fn cloned_preview_interaction_state_is_equal_then_mutates_independently() {
        let interaction = PreviewInteractionState {
            vertical_scroll: 12,
            horizontal_scroll: 3,
            viewport_width: 80,
            viewport_height: 22,
            search: Some(SearchState {
                query: "needle".into(),
                matches: vec![Match {
                    line: 4,
                    start: 2,
                    end: 8,
                }],
                current: 0,
            }),
            selection: None,
        };
        let mut copied = interaction.clone();

        assert_eq!(copied, interaction);

        copied.vertical_scroll = 19;
        copied.search.as_mut().unwrap().query = "changed".into();

        assert_eq!(interaction.vertical_scroll, 12);
        assert_eq!(interaction.search.as_ref().unwrap().query, "needle");
        assert_ne!(copied, interaction);
    }
}
