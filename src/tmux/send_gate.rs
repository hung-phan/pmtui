//! What a pane must show, beyond the shared prompt predicate, before anything is typed into it.
//!
//! [`has_composer`](super::has_composer) answers "is there a prompt?". Typing also needs
//! "will the text land in it ALONE?", and two things can make that false while the prompt is
//! drawn: Claude's feedback survey above the box ([`feedback_survey`]), whose digit keys a typed
//! Message would answer, and a person's unsent draft already in the box
//! ([`claude_draft_in_box`]), which the send would join to ours and submit. pmtui's `s` and pmd's
//! nudge both refuse on either, through [`classify_pane_styled`] and pmtui's `pane_refusal`.
//!
//! The draft check reads a STYLED capture (`capture-pane -e`): Claude's hint and a typed draft
//! are the same characters in the same place, and only the hint is drawn dim. Every other check
//! reads [`unstyled`] text.

use ratatui::style::{Color, Modifier, Style};

use super::activity::{
    PaneActivity, classify_with, claude_box, is_box_top_rule, is_prompt_row, is_spinner_glyph,
};

/// How a feedback-survey row begins, lowercased. Taken from a production report of another tmux
/// supervisor typing into exactly this pane (Claudfather/Claudlobby#1598), not from a capture of
/// our own: replace it with captured text when one is taken. If the wording differs, the survey
/// is not seen and the send goes ahead, as it did before this check existed.
const SURVEY_LEAD: &str = "how is claude doing this session";

/// How many non-blank rows directly above the box's top rule can hold the survey.
const NOTICE_ROWS: usize = 4;

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

/// Whether Claude's feedback survey is drawn by its input box, in an UNSTYLED capture.
///
/// Only the box's surroundings are read: at most [`NOTICE_ROWS`] non-blank rows directly above
/// its top rule, and everything below it. The run above stops at the first row a transcript entry
/// leads (`⏺`, `●`, `⎿`, `❯`, `›`, `>` or a spinner glyph), because the transcript can abut the
/// rule with no blank row between them (see `FIXTURE_CLAUDE_STYLED_AFTER_TURN`). A row must
/// BEGIN with the survey's question, so an answer that quotes it mid-sentence cannot block.
pub fn feedback_survey(capture: &str) -> bool {
    let lines: Vec<&str> = capture.lines().collect();
    let Some(at) = (0..lines.len())
        .rev()
        .find(|&at| claude_box(&lines, at).is_some())
    else {
        return false;
    };
    let Some(top) = (0..at).rev().find(|&i| is_box_top_rule(lines[i])) else {
        return false;
    };
    let above = lines[..top]
        .iter()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(NOTICE_ROWS)
        .take_while(|line| !is_transcript_entry(line));
    above.chain(&lines[at..]).any(|line| {
        line.trim_start()
            .to_ascii_lowercase()
            .starts_with(SURVEY_LEAD)
    })
}

/// A row that opens a transcript entry: the agent's answer, a tool result, an echoed message or a
/// spinner line.
fn is_transcript_entry(line: &str) -> bool {
    line.trim_start().chars().next().is_some_and(|lead| {
        matches!(lead, '⏺' | '●' | '⎿' | '❯' | '›' | '>') || is_spinner_glyph(lead)
    })
}

/// [`classify_pane`](super::classify_pane) for a STYLED capture: the same verdict, except that a
/// prompt whose box holds only Claude's dim hint is Idle, and a box holding a person's draft is
/// Busy.
///
/// Plain text cannot tell the hint from a draft, so the plain classifier counts only an EMPTY box
/// as Idle. This one can, so a fresh session showing its hint — or an engine whose box always holds
/// one — becomes nudgeable without making a drafted box nudgeable too. A capture with no styles
/// (a test driver's) reads every hint as a draft, which is the safe direction.
///
/// It also fails closed on a dialog. This is the verdict every typing path reads last, and it
/// accepts a box whatever it holds, so a dialog drawn inside a box must never reach the draft
/// check as a prompt.
pub fn classify_pane_styled(styled: &str) -> PaneActivity {
    let plain = unstyled(styled);
    if super::dialog::classify_dialog(&plain).is_some() {
        return PaneActivity::Busy;
    }
    match classify_with(&plain, is_prompt_row) {
        PaneActivity::Idle if claude_draft_in_box(styled) => PaneActivity::Busy,
        activity => activity,
    }
}

/// [`classify_pane_styled`] on a fresh styled capture of `session`, for a caller that reads the
/// pane's PLAIN capture for everything else. The two are separate captures on purpose: the plain
/// readers (dialog recognition, fingerprints) compare their text with later plain captures, and
/// text rebuilt from a styled capture need not match it byte for byte. A capture that fails is
/// Busy.
pub fn styled_activity(driver: &dyn super::Driver, session: &str, lines: usize) -> PaneActivity {
    driver
        .capture_tail_styled(session, lines)
        .map_or(PaneActivity::Busy, |styled| classify_pane_styled(&styled))
}

/// Whether Claude's input box holds unsent text, read from a STYLED capture.
///
/// Measured on Claude Code v2.1.296: the hint of an empty box is drawn with SGR 2 (dim) —
/// `❯ ESC[2mTry "how do I log an error?"` — while typed text carries no style, and a
/// multi-line draft continues on indented rows inside the box. So any character in the box
/// that is not whitespace and not dim, after the `❯` lead, is a person's draft.
///
/// A grey foreground counts as dim too ([`is_hint_style`]).
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
            .any(|(_, style)| !is_hint_style(style))
    })
}

/// Whether a cell is drawn the way a hint is: dim (SGR 2), or in a grey foreground.
///
/// v2.1.296 draws its hint with SGR 2, but a build that greys it with a colour instead
/// (`ESC[38;5;244m`, as the box rules are drawn) must not read as a permanent draft. Typed text
/// is drawn in the default colour; a highlighted slash command keeps a saturated one, so only a
/// grey counts. White is not a hint colour.
fn is_hint_style(style: Style) -> bool {
    if style.add_modifier.contains(Modifier::DIM) {
        return true;
    }
    match style.fg {
        Some(Color::DarkGray | Color::Gray) => true,
        Some(Color::Indexed(index)) => index == 8 || (232..=250).contains(&index),
        Some(Color::Rgb(r, g, b)) => {
            let (lo, hi) = (r.min(g).min(b), r.max(g).max(b));
            hi - lo <= 16 && hi <= 200
        }
        _ => false,
    }
}
