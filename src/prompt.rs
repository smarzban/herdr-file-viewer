//! Prompt-input primitive — a single-line Unicode-safe text buffer.
//!
//! [`PromptInput`] remains the canonical query editor shared by finder/search callers. A fresh or
//! initial-text buffer starts with its cursor at the end, so their existing `push`/`backspace`
//! sequences retain append/delete-last behavior; cursor-aware callers can additionally edit in the
//! middle and move with Left/Right/Home/End semantics.

/// A single-line, cursor-aware text buffer used for keyboard-driven text entry.
///
/// Cursor positions are UTF-8 byte offsets kept on character boundaries. Construct with
/// [`PromptInput::new`], [`PromptInput::with_text`], or [`Default::default`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PromptInput {
    query: String,
    cursor: usize,
}

impl PromptInput {
    /// Returns an empty `PromptInput` with its cursor at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a `PromptInput` initialized with text and its cursor at the end.
    pub fn with_text(text: impl Into<String>) -> Self {
        let query = text.into();
        let cursor = query.len();
        Self { query, cursor }
    }

    /// Inserts `c` at the cursor and advances past it.
    ///
    /// For existing callers that never move the cursor, this appends exactly as before.
    pub fn push(&mut self, c: char) {
        self.insert(c);
    }

    /// Inserts `c` at the cursor and advances past it.
    pub fn insert(&mut self, c: char) {
        self.query.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Removes the character immediately before the cursor.
    ///
    /// No-op when the cursor is at the beginning. Existing end-cursor callers therefore retain
    /// their former delete-last behavior.
    pub fn backspace(&mut self) {
        let Some(previous) = self.query[..self.cursor].char_indices().next_back() else {
            return;
        };
        self.query.drain(previous.0..self.cursor);
        self.cursor = previous.0;
    }

    /// Removes the character at the cursor without moving the cursor.
    ///
    /// No-op when the cursor is at the end.
    pub fn delete(&mut self) {
        let Some(c) = self.query[self.cursor..].chars().next() else {
            return;
        };
        self.query.drain(self.cursor..self.cursor + c.len_utf8());
    }

    /// Moves the cursor left by one Unicode scalar value.
    pub fn move_left(&mut self) {
        if let Some((previous, _)) = self.query[..self.cursor].char_indices().next_back() {
            self.cursor = previous;
        }
    }

    /// Moves the cursor right by one Unicode scalar value.
    pub fn move_right(&mut self) {
        if let Some(c) = self.query[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    /// Moves the cursor to the beginning (Home).
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Moves the cursor to the end (End).
    pub fn move_end(&mut self) {
        self.cursor = self.query.len();
    }

    /// Sets a byte cursor, clamped to the string and then to the preceding UTF-8 boundary.
    pub fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.query.len());
        while !self.query.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }

    /// Empties the query buffer and resets the cursor to zero.
    pub fn clear(&mut self) {
        self.query.clear();
        self.cursor = 0;
    }

    /// Returns a shared reference to the current query string.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns the cursor as a UTF-8 byte offset on a character boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }
}

impl From<String> for PromptInput {
    fn from(text: String) -> Self {
        Self::with_text(text)
    }
}

impl From<&str> for PromptInput {
    fn from(text: &str) -> Self {
        Self::with_text(text)
    }
}

#[cfg(test)]
mod tests {
    use super::PromptInput;

    #[test]
    fn new_and_default_are_empty_at_zero() {
        for p in [PromptInput::new(), PromptInput::default()] {
            assert_eq!(p.query(), "");
            assert_eq!(p.cursor(), 0);
        }
    }

    #[test]
    fn initial_text_and_from_start_at_end() {
        let p = PromptInput::with_text("café");
        assert_eq!(p.query(), "café");
        assert_eq!(p.cursor(), "café".len());
        assert_eq!(PromptInput::from("hello"), PromptInput::with_text("hello"));
        assert_eq!(PromptInput::from(String::from("owned")).cursor(), 5);
    }

    #[test]
    fn existing_push_and_backspace_callers_remain_end_cursor_appenders() {
        let mut p = PromptInput::new();
        for c in "hello".chars() {
            p.push(c);
        }
        assert_eq!(p.query(), "hello");
        assert_eq!(p.cursor(), 5);
        p.backspace();
        assert_eq!(p.query(), "hell");
        assert_eq!(p.cursor(), 4);
    }

    #[test]
    fn push_and_insert_edit_at_cursor_and_advance_over_unicode() {
        let mut p = PromptInput::with_text("a界");
        p.move_left();
        assert_eq!(p.cursor(), 1);
        p.push('é');
        assert_eq!(p.query(), "aé界");
        assert_eq!(p.cursor(), 3);
        p.insert('🙂');
        assert_eq!(p.query(), "aé🙂界");
        assert_eq!(p.cursor(), 7);
    }

    #[test]
    fn backspace_removes_unicode_scalar_before_cursor_and_clamps_at_home() {
        let mut p = PromptInput::with_text("aé🙂z");
        p.move_left();
        p.backspace();
        assert_eq!(p.query(), "aéz");
        assert_eq!(p.cursor(), "aé".len());
        p.backspace();
        assert_eq!(p.query(), "az");
        assert_eq!(p.cursor(), 1);
        p.move_home();
        p.backspace();
        assert_eq!(p.query(), "az");
        assert_eq!(p.cursor(), 0);
    }

    #[test]
    fn delete_removes_unicode_scalar_at_cursor_and_is_noop_at_end() {
        let mut p = PromptInput::with_text("aé🙂z");
        p.move_home();
        p.move_right();
        p.delete();
        assert_eq!(p.query(), "a🙂z");
        assert_eq!(p.cursor(), 1);
        p.delete();
        assert_eq!(p.query(), "az");
        assert_eq!(p.cursor(), 1);
        p.move_end();
        p.delete();
        assert_eq!(p.query(), "az");
        assert_eq!(p.cursor(), 2);
    }

    #[test]
    fn left_right_home_end_follow_unicode_boundaries_and_clamp() {
        let mut p = PromptInput::with_text("é🙂x");
        assert_eq!(p.cursor(), 7);
        p.move_right();
        assert_eq!(p.cursor(), 7);
        p.move_left();
        assert_eq!(p.cursor(), 6);
        p.move_left();
        assert_eq!(p.cursor(), 2);
        p.move_left();
        assert_eq!(p.cursor(), 0);
        p.move_left();
        assert_eq!(p.cursor(), 0);
        p.move_end();
        assert_eq!(p.cursor(), 7);
        p.move_home();
        assert_eq!(p.cursor(), 0);
    }

    #[test]
    fn set_cursor_clamps_length_and_inside_multibyte_to_preceding_boundary() {
        let mut p = PromptInput::with_text("aé🙂z");
        p.set_cursor(usize::MAX);
        assert_eq!(p.cursor(), p.query().len());
        p.set_cursor(2); // interior byte of é (which begins at byte 1)
        assert_eq!(p.cursor(), 1);
        p.set_cursor(5); // interior byte of 🙂 (which begins at byte 3)
        assert_eq!(p.cursor(), 3);
        p.set_cursor(0);
        assert_eq!(p.cursor(), 0);
    }

    #[test]
    fn clear_empties_and_resets_cursor_from_any_position() {
        let mut p = PromptInput::with_text("abc");
        p.move_left();
        p.clear();
        assert_eq!(p.query(), "");
        assert_eq!(p.cursor(), 0);
        p.clear();
        p.delete();
        p.backspace();
        assert_eq!(p, PromptInput::new());
    }

    #[test]
    fn mixed_unicode_edit_sequence_never_splits_utf8() {
        let mut p = PromptInput::with_text("Aé界🙂Z");
        p.move_home();
        for _ in 0..3 {
            p.move_right();
        }
        assert!(p.query().is_char_boundary(p.cursor()));
        p.backspace();
        p.insert('ß');
        p.delete();
        assert_eq!(p.query(), "AéßZ");
        assert!(p.query().is_char_boundary(p.cursor()));
    }
}
