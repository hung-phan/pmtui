//! Test for nudge-text sanitizing: newlines survive, and every byte the terminal
//! would interpret rather than type does not.

use crate::tmux::sanitize_send_text;

#[test]
fn sanitize_send_text_keeps_newlines_and_removes_everything_dangerous() {
    // `send-keys -l` writes bytes VERBATIM: ESC arrives as shift+tab (cycling
    // claude's permission mode) and a lone CR submits a second message. Newlines,
    // though, are legitimate — `send_keys` routes them through paste-buffer.
    let out = sanitize_send_text("first\r\nsecond\u{1b}[31m\tthird\u{7f}\n");
    assert_eq!(out, "first\nsecond [31m third");
    assert!(!out.contains('\u{1b}') && !out.contains('\r') && !out.contains('\u{7f}'));
    // Multi-line survives intact, and a trailing blank line cannot become an extra
    // submitted turn.
    assert_eq!(sanitize_send_text("a\nb\n\n"), "a\nb");
    assert_eq!(sanitize_send_text("   \n\t\n"), "");
}
