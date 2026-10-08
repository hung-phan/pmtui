//! The Message composer's seam: which library call each key means.
//!
//! `ratatui-textarea` owns the editing — word motion, the kills, wrapping, undo — and it is tested
//! upstream, so these tests do not re-test it. What is OURS is the routing: `Enter` sends rather than
//! opening a line, `Esc` parks the draft, three chords open a line because terminals disagree about
//! which ones they can report, `^X^E` reaches `$EDITOR`, and everything else must reach the library
//! unmolested.
//!
//! A handful of library bindings ARE asserted — `Alt+b`, `Ctrl+W`, `Alt+d`, `Ctrl+K` — not to re-test
//! the crate but to prove the key ever arrives: an interception upstream of this would silently eat
//! them and the field would feel dead in exactly the way the user reported before this existed.

use super::*;

/// `Enter` SENDS, so a newline needs a chord — and all three are bound, because `Ctrl+J` always
/// arrives, `Alt+Enter` nearly always does, and `Shift+Enter` only where the terminal implements the
/// kitty protocol (measured under tmux: it collapses to a bare `Enter`).
#[test]
fn enter_sends_and_every_newline_chord_opens_a_line() {
    let mut c = Composer::from_text("one".into());
    assert_eq!(c.key(KeyCode::Enter, KeyModifiers::NONE), Edit::Send);
    assert_eq!(c.text(), "one", "Enter must not also type something");
    assert_eq!(c.key(KeyCode::Esc, KeyModifiers::NONE), Edit::Cancel);

    for mods in [
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::SHIFT,
    ] {
        let mut c = Composer::from_text("one".into());
        assert_eq!(c.key(KeyCode::Enter, mods), Edit::Consumed, "{mods:?}");
        assert_eq!(c.text(), "one\n", "{mods:?} did not open a line");
    }
    // …and `Ctrl+J`, the one a terminal can always deliver: crossterm decodes `0x0A` as
    // `Char('j')+CONTROL`, distinct from `Enter`'s `0x0D`.
    let mut c = Composer::from_text("one".into());
    assert_eq!(
        c.key(KeyCode::Char('j'), KeyModifiers::CONTROL),
        Edit::Consumed
    );
    assert_eq!(c.text(), "one\n");
}

/// `^X^E` is bash's own editor chord, which frees bare `^E` to be readline's end-of-line — the thing a
/// terminal-grade editor has to mean by it.
#[test]
fn the_editor_is_ctrl_x_ctrl_e_and_bare_ctrl_e_ends_the_line() {
    let mut c = Composer::from_text("hello".into());
    assert_eq!(
        c.key(KeyCode::Char('x'), KeyModifiers::CONTROL),
        Edit::Consumed,
        "the prefix itself edits nothing"
    );
    assert_eq!(c.text(), "hello");
    assert_eq!(
        c.key(KeyCode::Char('e'), KeyModifiers::CONTROL),
        Edit::Editor
    );

    // Bare `^E` is end-of-line…
    let mut c = Composer::from_text("hello".into());
    c.set_cursor(0, 0);
    assert_eq!(
        c.key(KeyCode::Char('e'), KeyModifiers::CONTROL),
        Edit::Consumed
    );
    assert_eq!(c.cursor(), (0, 5));
    // …and `^A` is its start.
    assert_eq!(
        c.key(KeyCode::Char('a'), KeyModifiers::CONTROL),
        Edit::Consumed
    );
    assert_eq!(c.cursor(), (0, 0));
}

/// A PREFIX WAITS EXACTLY ONE KEY. A mistyped chord costs a keystroke, not the key after it.
#[test]
fn a_dangling_ctrl_x_is_dropped_by_the_next_key() {
    let mut c = Composer::from_text(String::new());
    c.key(KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!(
        c.key(KeyCode::Char('z'), KeyModifiers::NONE),
        Edit::Consumed
    );
    assert_eq!(c.text(), "z", "the key after the prefix must still type");
    // The prefix is spent: `^E` means end-of-line again, not the editor.
    assert_eq!(
        c.key(KeyCode::Char('e'), KeyModifiers::CONTROL),
        Edit::Consumed
    );
}

/// A REPEATED PREFIX RE-ARMS. `^X^X^E` is still the editor and `^X^X^R` still the chord: a human who
/// presses `^X` twice is reaching for a chord. The first ordering of those two branches spent the second
/// `^X` as an unknown chord and the key after it on the library — a rescind that did nothing.
#[test]
fn a_repeated_prefix_still_reaches_the_chord() {
    let mut c = Composer::from_text("x".into());
    c.key(KeyCode::Char('x'), KeyModifiers::CONTROL);
    c.key(KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!(
        c.key(KeyCode::Char('e'), KeyModifiers::CONTROL),
        Edit::Editor
    );

    let mut c = Composer::from_text("x".into());
    c.key(KeyCode::Char('x'), KeyModifiers::CONTROL);
    c.key(KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!(
        c.key(KeyCode::Char('r'), KeyModifiers::CONTROL),
        Edit::Chord('r'),
        "the directive field's rescind"
    );
    assert_eq!(c.text(), "x", "no chord may edit the buffer");
}

/// A `^X` chord the surface does not know is REPORTED, not acted on: `Chord` carries the letter so the
/// field decides (the directive's `^X^R`), and the composer stays free of any one surface's vocabulary.
#[test]
fn an_unknown_chord_is_handed_back_with_its_letter() {
    let mut c = Composer::from_text("x".into());
    c.key(KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!(
        c.key(KeyCode::Char('Q'), KeyModifiers::CONTROL),
        Edit::Chord('q'),
        "lowercased, so a caller matches one letter"
    );
    assert_eq!(c.text(), "x");
}

/// THE TERMINAL'S CHORDS ARRIVE. The library implements them; this proves nothing upstream eats them.
#[test]
fn the_readline_chords_reach_the_library() {
    let start = "the quick brown fox";
    // Alt+b, twice: back two words.
    let mut c = Composer::from_text(start.into());
    c.key(KeyCode::Char('b'), KeyModifiers::ALT);
    c.key(KeyCode::Char('b'), KeyModifiers::ALT);
    assert_eq!(c.cursor(), (0, "the quick ".chars().count()));
    // Alt+f: forward one — to the START of the next word, which is this library's `WordForward`.
    c.key(KeyCode::Char('f'), KeyModifiers::ALT);
    assert_eq!(c.cursor(), (0, "the quick brown ".chars().count()));

    // Ctrl+W: kill the word behind.
    let mut c = Composer::from_text(start.into());
    c.key(KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert_eq!(c.text(), "the quick brown ");

    // Alt+d: kill the word ahead.
    let mut c = Composer::from_text(start.into());
    c.set_cursor(0, 4);
    c.key(KeyCode::Char('d'), KeyModifiers::ALT);
    assert_eq!(c.text(), "the  brown fox");

    // Ctrl+K: to the end of the line.
    let mut c = Composer::from_text(start.into());
    c.set_cursor(0, 4);
    c.key(KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(c.text(), "the ");

    // Ctrl+U: the library's undo — it puts the killed text back.
    c.key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(c.text(), start);

    // Backspace and Delete still do what they say, and a plain letter types.
    let mut c = Composer::from_text("ab".into());
    c.key(KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(c.text(), "a");
    c.key(KeyCode::Char('c'), KeyModifiers::NONE);
    assert_eq!(c.text(), "ac");
}

/// Up and Down move WITHIN the message, which is the point of a multi-line composer.
#[test]
fn the_arrows_move_between_the_lines() {
    let mut c = Composer::from_text("first\nsecond".into());
    assert_eq!(c.cursor(), (1, 6));
    assert_eq!(c.key(KeyCode::Up, KeyModifiers::NONE), Edit::Consumed);
    assert_eq!(c.cursor().0, 0, "Up must reach the first line");
    assert_eq!(c.key(KeyCode::Down, KeyModifiers::NONE), Edit::Consumed);
    assert_eq!(c.cursor().0, 1);
}

/// The text the send path gets is the buffer, newlines and all — which is what the tmux paste-buffer
/// route in `Driver::send_keys` exists to deliver as ONE input.
#[test]
fn the_text_is_the_whole_buffer_and_emptiness_ignores_whitespace() {
    let mut c = Composer::from_text("one".into());
    c.key(KeyCode::Char('j'), KeyModifiers::CONTROL);
    c.key(KeyCode::Char('2'), KeyModifiers::NONE);
    assert_eq!(c.text(), "one\n2");

    assert!(Composer::default().is_empty());
    assert!(Composer::from_text("  \n\t\n ".into()).is_empty());
    assert!(!Composer::from_text("\nx".into()).is_empty());
}

/// A PASTE KEEPS ITS LINES. The old one-line field flattened a pasted snippet into one row; this is the
/// direct payoff of the composer being a real buffer.
#[test]
fn a_paste_keeps_its_newlines() {
    let mut c = Composer::from_text("head: ".into());
    c.insert_str("one\ntwo\nthree");
    assert_eq!(c.text(), "head: one\ntwo\nthree");
}

/// The caret survives the trip through `$EDITOR`, which is why `SendReq` carries `(row, column)`.
#[test]
fn the_cursor_round_trips_for_the_editor_handoff() {
    let mut c = Composer::from_text("first\nsecond line".into());
    c.set_cursor(1, 3);
    assert_eq!(c.cursor(), (1, 3));
    // Out of range is the library's to clamp, not ours to guard.
    c.set_cursor(99, 99);
    assert_eq!(c.cursor().0, 1, "a row past the end clamps to the last");
}
