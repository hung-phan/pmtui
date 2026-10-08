//! Making human-typed text safe to hand to `Driver::send_keys`, newlines and all.
//! Its own file because what counts as dangerous here is a property of the
//! TERMINAL — `send-keys -l` writes bytes verbatim — not of the pane heuristics
//! that read a capture or of the driver that does the typing.

/// Make human-typed text safe to hand to [`Driver::send_keys`](super::Driver::send_keys), KEEPING newlines.
///
/// [`crate::advise::sanitize_control_bytes`] is the wrong tool for a message: it turns
/// `\n` into a space and collapses whitespace runs, which is right for a one-line
/// harness-quoted string and wrong for a pasted snippet or an `$EDITOR` buffer.
///
/// What has to go regardless: every other C0 byte, DEL and C1. `send-keys -l` writes
/// bytes VERBATIM (measured against a real tmux server), so `\x1b[Z` arrives as
/// shift+tab and cycles claude's permission mode, a lone `\r` submits a second message,
/// and a TAB may trigger completion. CRLF collapses to LF first so a Windows-authored
/// file does not submit twice per line.
pub fn sanitize_send_text(s: &str) -> String {
    let unified = s.replace("\r\n", "\n");
    let cleaned: String = unified
        .chars()
        .map(|c| {
            if c == '\n' {
                return '\n';
            }
            let u = c as u32;
            if u < 0x20 || u == 0x7f || (0x80..=0x9f).contains(&u) {
                // A dropped control byte becomes a space, not nothing, so words that were
                // separated by a TAB do not run together.
                ' '
            } else {
                c
            }
        })
        .collect();
    // Trailing whitespace per line, and on the whole, so a stray blank tail cannot become
    // an extra submitted line.
    cleaned
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}
