//! Session-only annotation editor and overview transitions.

use super::*;
use crate::annotation::{
    AnnotationError, AnnotationId, AnnotationStore, AnnotationTarget, LineRange,
};
use crate::prompt::PromptInput;

/// Cursor state for the annotation overview.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnnotationListState {
    cursor: usize,
}

impl AnnotationListState {
    fn new(cursor: usize, len: usize) -> Self {
        Self {
            cursor: clamp_cursor(cursor, len),
        }
    }

    /// Selected row in the canonical annotation ordering.
    pub fn cursor(&self) -> usize {
        self.cursor
    }
}

#[derive(Debug, Clone)]
enum AnnotationEditorMode {
    Add {
        restore_line_select: Option<LineSelectState>,
    },
    Edit {
        id: AnnotationId,
        overview_cursor: usize,
    },
}

/// Typed state for the add/edit annotation modal.
#[derive(Debug, Clone)]
pub struct AnnotationEditorState {
    target: AnnotationTarget,
    input: PromptInput,
    error: Option<String>,
    mode: AnnotationEditorMode,
}

impl AnnotationEditorState {
    fn add(target: AnnotationTarget, restore_line_select: Option<LineSelectState>) -> Self {
        Self {
            target,
            input: PromptInput::new(),
            error: None,
            mode: AnnotationEditorMode::Add {
                restore_line_select,
            },
        }
    }

    fn edit(
        id: AnnotationId,
        target: AnnotationTarget,
        text: &str,
        overview_cursor: usize,
    ) -> Self {
        Self {
            target,
            input: PromptInput::with_text(text),
            error: None,
            mode: AnnotationEditorMode::Edit {
                id,
                overview_cursor,
            },
        }
    }

    /// Immutable file/range target being annotated.
    pub fn target(&self) -> &AnnotationTarget {
        &self.target
    }

    /// Current single-line editor contents.
    pub fn text(&self) -> &str {
        self.input.query()
    }

    /// UTF-8 byte cursor in [`Self::text`].
    pub fn cursor(&self) -> usize {
        self.input.cursor()
    }

    /// Inline validation error, if the most recent save was rejected.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether this modal edits an existing annotation rather than adding one.
    pub fn is_edit(&self) -> bool {
        matches!(self.mode, AnnotationEditorMode::Edit { .. })
    }
}

fn clamp_cursor(cursor: usize, len: usize) -> usize {
    if len == 0 { 0 } else { cursor.min(len - 1) }
}

fn target_view(target: &AnnotationTarget) -> AnnotationTargetView {
    AnnotationTargetView {
        path: target.path().to_path_buf(),
        lines: target.lines(),
    }
}

fn merge_line_ranges(mut ranges: Vec<LineRange>) -> Vec<LineRange> {
    ranges.sort_unstable_by_key(|range| (range.start(), range.end()));
    let mut merged: Vec<LineRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        let Some(current) = merged.last_mut() else {
            merged.push(range);
            continue;
        };
        if range.start() <= current.end().saturating_add(1) {
            *current = LineRange::new(current.start(), current.end().max(range.end()))
                .expect("merged annotation ranges remain 1-based");
        } else {
            merged.push(range);
        }
    }
    merged
}

impl Controller {
    /// Owned persistent-indicator projection for the pure Presenter.
    pub(super) fn annotation_indicators_view(&self) -> AnnotationIndicatorsView {
        let displayed_relative = self
            .content_path
            .as_deref()
            .and_then(|path| path.strip_prefix(&self.root).ok());
        let source_mapped = self.content_source.is_some();
        let mut view = AnnotationIndicatorsView::default();
        let mut displayed_ranges = Vec::new();

        for annotation in self.annotations.ordered() {
            let target = annotation.target();
            view.annotated_files.insert(self.root.join(target.path()));
            if displayed_relative != Some(target.path()) {
                continue;
            }
            view.displayed_file_annotated = true;
            if source_mapped && let Some(lines) = target.lines() {
                displayed_ranges.push(lines);
            }
        }
        view.displayed_line_ranges = merge_line_ranges(displayed_ranges);
        view
    }

    /// Owned, typed annotation-overview projection for the pure Presenter.
    /// The store's annotations as owned rows in canonical order. Shared by the overview and the
    /// quit confirm so the two can never name the same annotation differently.
    fn annotation_rows(&self) -> Vec<AnnotationRowView> {
        self.annotations
            .ordered()
            .into_iter()
            .map(|annotation| AnnotationRowView {
                target: target_view(annotation.target()),
                note: annotation.text().to_string(),
            })
            .collect()
    }

    pub(super) fn annotation_overview_view(&self) -> Option<AnnotationOverviewView> {
        let list = self.modal.annotations()?;
        Some(AnnotationOverviewView {
            rows: self.annotation_rows(),
            cursor: list.cursor(),
        })
    }

    /// Owned discard-confirm projection: the annotations the pending action would discard, plus the
    /// verb and proceed key naming that action. `None` while the confirm is closed.
    pub(super) fn discard_confirm_view(&self) -> Option<DiscardConfirmView> {
        let Modal::DiscardConfirm(action) = &self.modal else {
            return None;
        };
        Some(DiscardConfirmView {
            rows: self.annotation_rows(),
            verb: action.verb(),
            proceed_key: match action {
                DiscardAction::Quit => "q",
                DiscardAction::SwitchRoot(_) => "⏎",
            },
        })
    }

    /// Owned, typed add/edit projection for the pure Presenter.
    pub(super) fn annotation_editor_view(&self) -> Option<AnnotationEditorView> {
        let editor = self.modal.annotation_editor()?;
        Some(AnnotationEditorView {
            kind: if editor.is_edit() {
                AnnotationEditorKind::Edit
            } else {
                AnnotationEditorKind::Add
            },
            target: target_view(editor.target()),
            text: editor.text().to_string(),
            cursor: editor.cursor(),
            error: editor.error().map(str::to_string),
        })
    }

    /// All session annotations for the current root.
    pub fn annotations(&self) -> &AnnotationStore {
        &self.annotations
    }

    /// The annotation overview state when open.
    pub fn annotation_list(&self) -> Option<&AnnotationListState> {
        self.modal.annotations()
    }

    /// The annotation editor state when open.
    pub fn annotation_editor(&self) -> Option<&AnnotationEditorState> {
        self.modal.annotation_editor()
    }

    /// Whether the annotation overview or add/edit editor owns input.
    pub fn annotation_modal_open(&self) -> bool {
        self.annotation_list().is_some() || self.annotation_editor().is_some()
    }

    /// Whether any annotation modal owns *raw* keys, so the run loop must route them before global
    /// decoding. The single predicate the event loop gates on: [`route_annotation_key`] handles
    /// exactly this set, so a new raw-key modal that lands in one and not the other is a routing
    /// hole (the quit confirm was, and its keys silently fell through to global actions).
    ///
    /// [`route_annotation_key`]: crate::app
    pub fn annotation_raw_keys_owned(&self) -> bool {
        self.annotation_modal_open() || self.discard_confirm_open()
    }

    /// Open an empty add editor for the selected file.
    pub(super) fn add_annotation(&mut self) -> Effects {
        let Some(node) = self.tree.selected() else {
            self.action_notice = Some("Select a file to annotate".to_string());
            return Effects::redraw();
        };
        if node.kind != NodeKind::File {
            self.action_notice = Some("Directories cannot be annotated; select a file".to_string());
            return Effects::redraw();
        }
        let Some(rel) = self.rel(&node.path) else {
            self.action_notice = Some("Selected file is outside the current root".to_string());
            return Effects::redraw();
        };
        let Ok(target) = AnnotationTarget::for_file(rel) else {
            self.action_notice = Some("Selected file cannot be annotated".to_string());
            return Effects::redraw();
        };
        self.modal = Modal::AnnotationEditor(AnnotationEditorState::add(target, None));
        Effects::redraw()
    }

    /// Replace line-select with an add editor while retaining an exact cancel snapshot.
    pub(super) fn add_annotation_for_line_selection(&mut self) -> Effects {
        let Some(snapshot) = self.modal.line_select().copied() else {
            return Effects::noop();
        };
        let Some(node) = self
            .tree
            .selected()
            .filter(|node| node.kind == NodeKind::File)
        else {
            return Effects::noop();
        };
        let Some(rel) = self.rel(&node.path) else {
            return Effects::noop();
        };
        let (start, end) = snapshot.selection();
        let Ok(lines) = LineRange::new(start, end) else {
            return Effects::noop();
        };
        let Ok(target) = AnnotationTarget::for_lines(rel, lines) else {
            return Effects::noop();
        };
        self.drag = None;
        self.modal = Modal::AnnotationEditor(AnnotationEditorState::add(target, Some(snapshot)));
        Effects::redraw()
    }

    /// Open the canonical annotation overview, including its typed empty state.
    pub(super) fn show_annotations(&mut self) -> Effects {
        self.modal = Modal::Annotations(AnnotationListState::default());
        Effects::redraw()
    }

    /// Route fixed overview controls. Global key remapping does not apply inside this modal.
    pub fn handle_annotations_key(&mut self, key: KeyEvent) -> Effects {
        if key.modifiers.difference(KeyModifiers::SHIFT) != KeyModifiers::NONE {
            return Effects::noop();
        }
        if self.modal.annotations().is_none() {
            return Effects::noop();
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = Modal::None;
                Effects::redraw()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let len = self.annotations.len();
                if let Some(list) = self.modal.annotations_mut() {
                    list.cursor = clamp_cursor(list.cursor.saturating_add(1), len);
                }
                Effects::redraw()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(list) = self.modal.annotations_mut() {
                    list.cursor = list.cursor.saturating_sub(1);
                }
                Effects::redraw()
            }
            KeyCode::Enter | KeyCode::Char('e') => self.edit_selected_annotation(),
            KeyCode::Char('d') => self.delete_selected_annotation(),
            // Some terminals encode Shift+D solely in the uppercase character and omit the
            // separate SHIFT bit. The modifier guard above already rejects Ctrl/Alt chords, so
            // either uppercase spelling safely owns clear-all.
            KeyCode::Char('D') => {
                self.annotations.clear();
                if let Some(list) = self.modal.annotations_mut() {
                    list.cursor = 0;
                }
                Effects::redraw()
            }
            KeyCode::Char('y') => self.copy_annotations(),
            _ => Effects::noop(),
        }
    }

    fn edit_selected_annotation(&mut self) -> Effects {
        let Some(cursor) = self.modal.annotations().map(AnnotationListState::cursor) else {
            return Effects::noop();
        };
        let Some(annotation) = self.annotations.ordered().get(cursor).copied() else {
            return Effects::noop();
        };
        self.modal = Modal::AnnotationEditor(AnnotationEditorState::edit(
            annotation.id(),
            annotation.target().clone(),
            annotation.text(),
            cursor,
        ));
        Effects::redraw()
    }

    fn delete_selected_annotation(&mut self) -> Effects {
        let Some(cursor) = self.modal.annotations().map(AnnotationListState::cursor) else {
            return Effects::noop();
        };
        if let Some(id) = self
            .annotations
            .ordered()
            .get(cursor)
            .map(|annotation| annotation.id())
        {
            self.annotations.delete(id);
        }
        let len = self.annotations.len();
        if let Some(list) = self.modal.annotations_mut() {
            list.cursor = clamp_cursor(list.cursor, len);
        }
        Effects::redraw()
    }

    /// Copy the canonical export to the clipboard and report the outcome as an action notice.
    /// Returns whether the write succeeded. Leaves the modal alone so callers can decide what a
    /// failure means: the overview closes either way, the quit confirm stays open.
    pub(super) fn copy_annotations_to_clipboard(&mut self) -> bool {
        if self.annotations.is_empty() {
            return false;
        }
        let count = self.annotations.len();
        let text = self.annotations.canonical_text();
        match self.clipboard.copy(&text) {
            Ok(()) => {
                self.action_notice = Some(format!(
                    "Copied {count} annotation{}",
                    if count == 1 { "" } else { "s" }
                ));
                true
            }
            Err(error) => {
                self.action_notice = Some(format!("Could not copy annotations: {error}"));
                false
            }
        }
    }

    fn copy_annotations(&mut self) -> Effects {
        if self.annotations.is_empty() {
            return Effects::noop();
        }
        self.copy_annotations_to_clipboard();
        self.modal = Modal::None;
        Effects::redraw()
    }

    /// Route the annotation editor's text-entry and cursor controls.
    pub fn handle_annotation_editor_key(&mut self, key: KeyEvent) -> Effects {
        if key.modifiers.difference(KeyModifiers::SHIFT) != KeyModifiers::NONE {
            return Effects::noop();
        }
        if self.modal.annotation_editor().is_none() {
            return Effects::noop();
        }

        match key.code {
            KeyCode::Esc => self.cancel_annotation_editor(),
            KeyCode::Enter => self.save_annotation_editor(),
            KeyCode::Backspace => self.edit_annotation_input(|input| input.backspace()),
            KeyCode::Delete => self.edit_annotation_input(|input| input.delete()),
            KeyCode::Left => self.edit_annotation_input(|input| input.move_left()),
            KeyCode::Right => self.edit_annotation_input(|input| input.move_right()),
            KeyCode::Home => self.edit_annotation_input(|input| input.move_home()),
            KeyCode::End => self.edit_annotation_input(|input| input.move_end()),
            KeyCode::Char(c) => self.edit_annotation_input(|input| input.insert(c)),
            _ => Effects::noop(),
        }
    }

    fn edit_annotation_input(&mut self, edit: impl FnOnce(&mut PromptInput)) -> Effects {
        if let Some(editor) = self.modal.annotation_editor_mut() {
            edit(&mut editor.input);
            editor.error = None;
            Effects::redraw()
        } else {
            Effects::noop()
        }
    }

    fn cancel_annotation_editor(&mut self) -> Effects {
        let Some(editor) = self.modal.annotation_editor().cloned() else {
            return Effects::noop();
        };
        self.modal = match editor.mode {
            AnnotationEditorMode::Add {
                restore_line_select,
            } => restore_line_select.map_or(Modal::None, Modal::LineSelect),
            AnnotationEditorMode::Edit {
                overview_cursor, ..
            } => Modal::Annotations(AnnotationListState::new(
                overview_cursor,
                self.annotations.len(),
            )),
        };
        Effects::redraw()
    }

    fn save_annotation_editor(&mut self) -> Effects {
        let Some(editor) = self.modal.annotation_editor().cloned() else {
            return Effects::noop();
        };
        let result = match editor.mode {
            AnnotationEditorMode::Add { .. } => self
                .annotations
                .add(editor.target.clone(), editor.input.query())
                .map(|_| None),
            AnnotationEditorMode::Edit {
                id,
                overview_cursor,
            } => self
                .annotations
                .edit(id, editor.input.query())
                .map(|()| Some(overview_cursor)),
        };

        match result {
            Ok(None) => self.modal = Modal::None,
            Ok(Some(cursor)) => {
                self.modal =
                    Modal::Annotations(AnnotationListState::new(cursor, self.annotations.len()));
            }
            Err(AnnotationError::EmptyText) => {
                if let Some(editor) = self.modal.annotation_editor_mut() {
                    editor.error = Some("Annotation text cannot be empty".to_string());
                }
                return Effects::redraw();
            }
            Err(error) => {
                if let Some(editor) = self.modal.annotation_editor_mut() {
                    editor.error = Some(error.to_string());
                }
                return Effects::redraw();
            }
        }
        Effects::redraw()
    }
}

#[cfg(test)]
mod tests {
    use super::merge_line_ranges;
    use crate::annotation::LineRange;

    fn range(start: usize, end: usize) -> LineRange {
        LineRange::new(start, end).unwrap()
    }

    #[test]
    fn annotation_indicator_ranges_sort_and_merge_overlap_and_adjacency() {
        assert_eq!(
            merge_line_ranges(vec![range(8, 10), range(2, 4), range(4, 7), range(20, 20)]),
            vec![range(2, 10), range(20, 20)]
        );
    }

    #[test]
    fn annotation_indicator_range_merge_saturates_at_usize_max() {
        assert_eq!(
            merge_line_ranges(vec![
                range(usize::MAX, usize::MAX),
                range(usize::MAX - 1, usize::MAX - 1),
            ]),
            vec![range(usize::MAX - 1, usize::MAX)]
        );
    }
}
