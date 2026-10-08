//! A single-line editable text buffer with a movable caret — the one model behind every
//! text field the dashboard has (the answer overlay, the inline goal/directive/cadence
//! fields, the send field, and the create form's Directory/Goal rows).
//!
//! Before this, each of those stored a bare `String` and only ever `push`/`pop`ed the END,
//! so `Left`/`Right`/`Home`/`End` did nothing and a typo three characters back meant deleting
//! everything after it. User: *"when i enter goal, direct, or send, i try to use arrow key to
//! navigate and change the input text but it doesn't seem to work"*. A [`Field`] carries the
//! text AND the caret together, so the caret can never desync from the string it indexes.

/// A one-line text buffer and the caret position within it.
///
/// The caret is a CHARACTER index in `0..=char_len` (not a byte offset), so the arithmetic the
/// key handlers and the renderer do is in the same units the display counts in (one char = one
/// column, matching [`crate::text_cols`]). Byte offsets are computed only at the moment the
/// backing `String` is mutated, so a multi-byte character can never split.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Field {
    text: String,
    /// Character index of the caret, always `<= self.char_len()`.
    caret: usize,
}

impl Field {
    /// An empty field with the caret at the start.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// The caret's character index — what the renderer needs to place the cursor.
    pub(crate) fn caret(&self) -> usize {
        self.caret
    }

    /// Character count (the caret's upper bound). Deliberately not named `len`, so
    /// `str::len` (BYTES, reached through [`std::ops::Deref`]) stays available and distinct.
    pub(crate) fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset of character `idx`, or `text.len()` when `idx` is at/after the end — the
    /// one place a char index becomes a byte index, so `String::insert`/`remove` (which take
    /// bytes) can never land mid-character.
    fn byte_at(&self, idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(idx)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    /// Insert `c` at the caret and step past it.
    pub(crate) fn insert(&mut self, c: char) {
        let b = self.byte_at(self.caret);
        self.text.insert(b, c);
        self.caret += 1;
    }

    /// Insert a whole string at the caret (a paste), stepping the caret to its end.
    pub(crate) fn insert_str(&mut self, s: &str) {
        let b = self.byte_at(self.caret);
        self.text.insert_str(b, s);
        self.caret += s.chars().count();
    }

    /// Delete the character BEFORE the caret (Backspace); a no-op at the start.
    pub(crate) fn backspace(&mut self) {
        if self.caret > 0 {
            let b = self.byte_at(self.caret - 1);
            self.text.remove(b);
            self.caret -= 1;
        }
    }

    /// Delete the character AT the caret (Delete); a no-op at the end. The caret does not move.
    pub(crate) fn delete(&mut self) {
        if self.caret < self.char_len() {
            let b = self.byte_at(self.caret);
            self.text.remove(b);
        }
    }

    pub(crate) fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub(crate) fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.char_len());
    }

    pub(crate) fn home(&mut self) {
        self.caret = 0;
    }

    pub(crate) fn end(&mut self) {
        self.caret = self.char_len();
    }

    /// The windowed slice a one-row widget `cols` columns wide should draw: the text to the
    /// LEFT of the caret, the character UNDER it (empty when the caret is at the end, which the
    /// renderer draws as a trailing `_`), and the text to its RIGHT — scrolled so the caret is
    /// always visible.
    ///
    /// The caret always occupies one column (a reversed character, or the trailing `_`), so at
    /// most `cols` columns are ever returned across the three parts. While the text fits it is
    /// shown from the start; once it outgrows the row the window follows the caret at the right
    /// edge — the same tail-scroll the old `caret_tail` did, now around a caret that can sit
    /// anywhere in the line.
    pub(crate) fn caret_view(&self, cols: usize) -> Caret {
        let cols = cols.max(1);
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        let caret = self.caret().min(n);
        // At most `cols - 1` characters share the row with the caret's own column; scroll so the
        // caret sits at the last visible column once the text outgrows the row (0 while it fits).
        let text_budget = cols.saturating_sub(1);
        let start = caret.saturating_sub(text_budget);
        let left: String = chars[start..caret].iter().collect();
        let used = (caret - start) + 1; // characters left of the caret + the caret column itself
        let (at, right) = if caret < n {
            let take = cols.saturating_sub(used); // columns left over for trailing context
            let right_end = (caret + 1 + take).min(n);
            (
                chars[caret].to_string(),
                chars[caret + 1..right_end].iter().collect(),
            )
        } else {
            (String::new(), String::new())
        };
        Caret { left, at, right }
    }
}

/// The three pieces of a [`Field`] a one-row widget draws around its caret. `at` is empty when
/// the caret is at the very end (drawn as a trailing `_`); otherwise it is the single character
/// under the caret, which the renderer highlights.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Caret {
    pub(crate) left: String,
    pub(crate) at: String,
    pub(crate) right: String,
}

impl From<&str> for Field {
    /// Seed a field with text and put the caret at the END — the natural place when a field
    /// opens on an existing value the human is about to extend.
    fn from(s: &str) -> Self {
        let caret = s.chars().count();
        Field {
            text: s.to_string(),
            caret,
        }
    }
}

impl From<String> for Field {
    fn from(s: String) -> Self {
        let caret = s.chars().count();
        Field { text: s, caret }
    }
}

/// `str` methods (`trim`, `is_empty`, `contains`, `chars`, …) and `&Field`→`&str` coercion, so
/// the many read sites that only inspect the text keep reading exactly as they did on a `String`.
impl std::ops::Deref for Field {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_move_edit_in_the_middle() {
        // The whole point: type, move the caret back into the text, and edit THERE — the thing
        // a push/pop-only `String` could never do.
        let mut f = Field::from("helo");
        assert_eq!(f.caret(), 4, "From<&str> seeds the caret at the end");
        f.left(); // between 'l' and 'o'
        f.insert('l'); // "hello"
        assert_eq!(f.as_str(), "hello");
        assert_eq!(f.caret(), 4);
    }

    // `restored_caret_is_character_indexed_and_clamped` lived here. It guarded
    // `from_text_and_caret`, whose only caller was the Message composer's `$EDITOR` round-trip — and
    // that surface is a `Composer` now (a real multi-line buffer), so the constructor went with it.
    // `crate::tests::composer::the_cursor_round_trips_for_the_editor_handoff` is its successor.

    #[test]
    fn backspace_and_delete_respect_the_caret() {
        let mut f = Field::from("abc");
        f.home();
        f.delete(); // removes 'a' at the caret
        assert_eq!(f.as_str(), "bc");
        assert_eq!(f.caret(), 0);
        f.end();
        f.backspace(); // removes 'c' before the caret
        assert_eq!(f.as_str(), "b");
        assert_eq!(f.caret(), 1);
    }

    #[test]
    fn edges_are_no_ops_not_panics() {
        let mut f = Field::new();
        f.left();
        f.backspace();
        f.delete();
        assert_eq!(f.as_str(), "");
        assert_eq!(f.caret(), 0);
        f.insert('x');
        f.right(); // already at end
        assert_eq!(f.caret(), 1);
    }

    #[test]
    fn caret_stays_on_char_boundaries_with_multibyte_text() {
        // A byte-offset caret would split 'é' (2 bytes) or the 4-byte emoji and panic on
        // `String::insert`. Char-indexed arithmetic cannot.
        let mut f = Field::from("é🎉");
        assert_eq!(f.char_len(), 2);
        f.left(); // between 'é' and '🎉'
        f.insert('x');
        assert_eq!(f.as_str(), "éx🎉");
        f.home();
        f.delete();
        assert_eq!(f.as_str(), "x🎉");
    }

    #[test]
    fn insert_str_pastes_at_the_caret() {
        let mut f = Field::from("ac");
        f.left(); // before 'c'
        f.insert_str("bbb");
        assert_eq!(f.as_str(), "abbbc");
        assert_eq!(f.caret(), 4);
    }

    #[test]
    fn caret_view_shows_the_whole_short_line_with_the_caret_in_place() {
        let mut f = Field::from("hello");
        f.home();
        f.right();
        f.right(); // caret over 'l' (index 2)
        let v = f.caret_view(20);
        assert_eq!(v.left, "he");
        assert_eq!(v.at, "l");
        assert_eq!(v.right, "lo");
    }

    #[test]
    fn caret_view_empty_and_end_render_a_trailing_caret() {
        let end = Field::from("hi").caret_view(20);
        assert_eq!(
            (end.left.as_str(), end.at.as_str(), end.right.as_str()),
            ("hi", "", "")
        );
        let empty = Field::new().caret_view(20);
        assert_eq!(
            (empty.left.as_str(), empty.at.as_str(), empty.right.as_str()),
            ("", "", "")
        );
    }

    #[test]
    fn caret_view_scrolls_to_keep_a_long_line_caret_visible_within_budget() {
        // Caret at the end of a line far longer than the row: the window tail-scrolls, and the
        // total columns (text + the trailing caret cell) never exceed the budget.
        let f = Field::from("x".repeat(200).as_str());
        let cols = 10;
        let v = f.caret_view(cols);
        assert!(v.at.is_empty(), "caret is at the end → trailing '_'");
        assert_eq!(v.right, "");
        assert_eq!(
            v.left.chars().count() + 1, // + the trailing caret column
            cols,
            "a caret at the end shows cols-1 chars plus its own column"
        );

        // Caret in the MIDDLE of a long line still fits: left + caret + right <= cols.
        let mut mid = f.clone();
        mid.home();
        for _ in 0..100 {
            mid.right();
        }
        let vm = mid.caret_view(cols);
        let total = vm.left.chars().count() + vm.at.chars().count() + vm.right.chars().count();
        assert!(
            total <= cols,
            "windowed view stays within the column budget: {total} > {cols}"
        );
        assert_eq!(vm.at, "x", "the caret sits over a character mid-line");
    }
}
