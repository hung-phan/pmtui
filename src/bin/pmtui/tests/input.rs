//! Moved out of `input.rs`: this binary keeps its tests in this directory,
//! one file per behaviour, and a production file does not carry its own test module.

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
