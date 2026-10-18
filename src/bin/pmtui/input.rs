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
