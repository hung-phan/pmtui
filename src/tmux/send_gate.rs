//! What pmtui's `s` key reads from a pane BEYOND the shared prompt predicate before it types.
//!
//! [`has_composer`](super::has_composer) answers "is there a prompt?". The send gate also needs
//! "will typed text land in it ALONE?", and two things can make that false while the prompt is
//! drawn: a state that takes keys or holds the turn while Claude still draws its input box
//! ([`send_blocker`]), and a person's unsent draft already in the box
//! ([`claude_draft_in_box`]). Both refuse, because a refused send keeps the Message and a wrong
//! send either answers something on the person's behalf or submits their half-written text.
//!
//! The draft check is the one place a classifier reads a STYLED capture (`capture-pane -e`):
//! Claude's hint and a typed draft are the same characters in the same place, and only the hint
//! is drawn dim. Every other check reads [`unstyled`] text, as `Driver::capture_tail_styled`
//! requires.

use ratatui::style::Modifier;

use super::activity::{claude_box, is_box_rule};

/// A pane state that draws Claude's input box but must not receive a typed Message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendBlocker {
    /// The inline "How is Claude doing this session?" survey. Its options are digit keys, so a
    /// Message containing a digit answers it on the person's behalf.
    Survey,
    /// A usage-limit banner. The Message would wait behind the limit, possibly for hours,
    /// while the dashboard reported it as sent.
    UsageLimit,
}

/// Lowercase phrases that identify each [`SendBlocker`]. Taken from a production report of
/// another tmux supervisor typing into exactly these panes (Claudfather/Claudlobby#1598), not
/// from a capture of our own; replace them with captured text when one is taken.
const BLOCKER_PHRASES: &[(&str, SendBlocker)] = &[
    ("how is claude doing this session?", SendBlocker::Survey),
    ("usage limit reached", SendBlocker::UsageLimit),
];

/// The capture with every escape sequence removed: the plain text every classifier except
/// [`claude_draft_in_box`] reads.
pub fn unstyled(capture: &str) -> String {
    crate::ansi::styled_lines(capture)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A [`SendBlocker`] drawn around Claude's input box in an UNSTYLED capture.
///
/// Only the box's surroundings are read: the run of non-blank rows directly above its top rule
/// (where Claude draws notices) and everything below it. The transcript further up is never
/// read, so an agent that merely MENTIONS a usage limit in its answer cannot block a send.
/// With no input box there is nothing to protect, and the prompt check refuses on its own.
pub fn send_blocker(capture: &str) -> Option<SendBlocker> {
    let lines: Vec<&str> = capture.lines().collect();
    let at = (0..lines.len())
        .rev()
        .find(|&at| claude_box(&lines, at).is_some())?;
    let top = (0..at).rev().find(|&i| is_box_rule(lines[i]))?;
    let notices = lines[..top]
        .iter()
        .rev()
        .take_while(|line| !line.trim().is_empty());
    notices.chain(&lines[at..]).find_map(|line| {
        let lower = line.to_lowercase();
        BLOCKER_PHRASES
            .iter()
            .find(|(phrase, _)| lower.contains(phrase))
            .map(|&(_, blocker)| blocker)
    })
}

/// Whether Claude's input box holds unsent text, read from a STYLED capture.
///
/// Measured on Claude Code v2.1.296: the hint of an empty box is drawn with SGR 2 (dim) —
/// `❯ ESC[2mTry "how do I log an error?"` — while typed text carries no style, and a
/// multi-line draft continues on indented rows inside the box. So any character in the box
/// that is not whitespace and not dim, after the `❯` lead, is a person's draft.
///
/// Reads the LAST box, the live one. With no Claude box at all (a bare prompt, or Codex) it
/// reports no draft and leaves the decision to the prompt check. An unstyled capture shows a
/// hint as plain text and therefore reads as a draft: the safe direction.
pub fn claude_draft_in_box(styled: &str) -> bool {
    let lines = crate::ansi::styled_lines(styled);
    let rows: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let Some((at, close)) = (0..rows.len())
        .rev()
        .find_map(|at| claude_box(&rows, at).map(|close| (at, close)))
    else {
        return false;
    };
    lines[at..close].iter().enumerate().any(|(offset, line)| {
        let cells = line
            .spans
            .iter()
            .flat_map(|span| span.content.chars().map(move |ch| (ch, span.style)));
        // The prompt row's first visible character is the `❯` lead itself.
        let skip = usize::from(offset == 0);
        cells
            .filter(|(ch, _)| !ch.is_whitespace())
            .skip(skip)
            .any(|(_, style)| !style.add_modifier.contains(Modifier::DIM))
    })
}
