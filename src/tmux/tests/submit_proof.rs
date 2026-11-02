//! The question asked AFTER an Enter: did the composer actually take what we typed?
//!
//! `send_keys` presses Enter and then reads the composer back, for BOTH engines, because tmux
//! accepting the keystrokes is not the same fact as the agent receiving a message. A single Enter
//! on Claude can confirm a bracketed paste and leave the text in the box, which `pmtui message`
//! reported as delivered with nothing sent.
//!
//! Two predicates, two different questions, and the difference is load-bearing:
//!
//! - [`claude_draft_visible`] asks "is MY text still showing?" — the submit proof, which compares
//!   against what was sent, so a concurrent keystroke from somebody else cannot answer it.
//! - [`codex_draft_in_box`] asks "is ANY unsent text showing?" — the pre-send gate's question,
//!   which has no text to compare against because it runs before there is one.

use super::*;
use crate::tmux::send_gate::{claude_draft_visible, codex_draft_in_box};

/// Codex's composer holding a human's typed draft, with the `›` lead undimmed.
const FIXTURE_CODEX_TYPED_DRAFT: &str = concat!(
    "\x1b[38;5;244m────────────────────────────────\n",
    "\x1b[39m› half typed\n",
    "\x1b[38;5;244m────────────────────────────────\n",
);

/// Codex's composer holding a pasted-content label.
const FIXTURE_CODEX_PASTED_DRAFT: &str = concat!(
    "\x1b[38;5;244m────────────────────────────────\n",
    "\x1b[39m› [Pasted Content 801 chars]\n",
    "\x1b[38;5;244m────────────────────────────────\n",
);

// ---------------------------------------------------------------- Claude, literal send

/// THE BUG THIS EXISTS FOR. A literal send whose text is still sitting in the box is NOT
/// delivered, and before this predicate Claude's branch pressed one Enter and reported success.
#[test]
fn our_own_literal_text_still_in_the_box_is_not_delivered() {
    assert!(claude_draft_visible(
        FIXTURE_CLAUDE_STYLED_DRAFT,
        "half typed",
        false
    ));
}

/// A wrapped draft: the box continues a long line onto an indented row, so the comparison has to
/// join the rows and ignore whitespace rather than match one row.
#[test]
fn a_wrapped_literal_draft_is_still_recognised_as_ours() {
    assert!(claude_draft_visible(
        FIXTURE_CLAUDE_STYLED_MULTILINE_DRAFT,
        "first line second line",
        false
    ));
}

/// The empty box drawn with Claude's dim hint reads as "not ours" by INEQUALITY — the hint is not
/// the text we sent — so this branch never needs to read a style. An engine build that greyed its
/// hint differently therefore cannot turn a delivered send into a failure.
#[test]
fn the_dim_hint_is_never_mistaken_for_our_text() {
    assert!(!claude_draft_visible(
        FIXTURE_CLAUDE_STYLED_HINT,
        "half typed",
        false
    ));
}

/// The box after a completed turn: our message is echoed OUTSIDE the box and the box itself is a
/// bare `❯`. The echo must not read as a draft, or every successful send would fail its proof.
#[test]
fn a_submitted_message_echoed_above_the_box_is_delivered() {
    assert!(!claude_draft_visible(
        FIXTURE_CLAUDE_STYLED_AFTER_TURN,
        "Reply with just: ok",
        false
    ));
}

/// FAIL CLOSED ON A DIALOG. A surface that opened after our paste must never be answered by our
/// next Enter, so the proof reports "not ours" and the send stops retrying.
#[test]
fn a_dialog_stops_the_retry_rather_than_being_answered() {
    assert!(!claude_draft_visible(
        FIXTURE_CODEX_DIALOG_APPROVAL,
        "half typed",
        false
    ));
}

/// No Claude box at all (a bare shell, or a Codex pane): nothing to prove, and the decision is
/// left to the caller's own engine branch.
#[test]
fn no_claude_box_reports_nothing_of_ours() {
    assert!(!claude_draft_visible("captured\n", "half typed", false));
}

/// An empty `text` can never be "still showing", so a send of nothing is never a stuck draft.
#[test]
fn empty_text_is_never_stuck() {
    assert!(!claude_draft_visible(
        FIXTURE_CLAUDE_STYLED_DRAFT,
        "",
        false
    ));
}

// ---------------------------------------------------------------- Claude, pasted send

/// A PASTED send has nothing to compare: Claude collapses a large paste into a label, so the proof
/// falls back to "is there any draft at all". Sound only because the send was refused unless the
/// box was empty and `input.lock` has been held ever since.
#[test]
fn a_pasted_send_falls_back_to_any_draft_at_all() {
    assert!(claude_draft_visible(
        FIXTURE_CLAUDE_STYLED_DRAFT,
        "something else entirely",
        true
    ));
    assert!(!claude_draft_visible(
        FIXTURE_CLAUDE_STYLED_HINT,
        "something else entirely",
        true
    ));
}

// ---------------------------------------------------------------- Codex pre-send gate

/// THE DOUBLING FIX. A Codex composer holding unsent text must refuse the next send, because
/// `send_keys` bails on an unsubmittable draft and `pmtui message` maps that to the retry family.
#[test]
fn codex_typed_text_is_an_unsent_draft() {
    assert!(codex_draft_in_box(FIXTURE_CODEX_TYPED_DRAFT));
}

#[test]
fn codex_pasted_content_is_an_unsent_draft() {
    assert!(codex_draft_in_box(FIXTURE_CODEX_PASTED_DRAFT));
}

/// Codex's rotating placeholder is drawn dim and is chrome, not a draft. Reading it as one would
/// refuse every send to an idle Codex session, which is why this reads a STYLED capture.
#[test]
fn codex_placeholder_is_not_a_draft() {
    let styled = concat!(
        "\x1b[38;5;244m────────────────────────────────\n",
        "\x1b[39m› \x1b[2mSummarize recent commits\x1b[39m\n",
        "\x1b[38;5;244m────────────────────────────────\n",
    );
    assert!(!codex_draft_in_box(styled));
}

/// AN UNSTYLED CAPTURE IS NOT A DRAFT, and "fail closed" was the wrong instinct here — the
/// real-tmux dispatch acceptance proved it.
///
/// The reasoning that failed: refusing a send costs a retry, appending to somebody's unsent text
/// costs their message, so refuse. But a refusal on PANE CONTENT is not retryable. The pane does
/// not change, so `pmtui message` answers 3 — "ask again under the SAME arguments" — forever. The
/// acceptance drives a stub agent printing a plain `› Ready` and every `s` to it was refused; the
/// unit suite was green, because a fixture cannot show a retry that never succeeds.
///
/// So with no style there is nothing to tell chrome from typing, and the honest answer is "no
/// draft". Production always passes `capture_tail_styled`, where codex's placeholder is dim and
/// typed text is not, so the doubling case this predicate exists for still reads true
/// ([`codex_typed_text_is_an_unsent_draft`]). `real::codex_draft_visible` needs none of this: it
/// compares against the text it sent, so it stays sound unstyled.
#[test]
fn an_unstyled_codex_composer_is_not_a_draft() {
    assert!(!codex_draft_in_box(FIXTURE_CODEX_IDLE));
    assert!(!codex_draft_in_box(FIXTURE_CODEX_IDLE_AFTER_TURN));
    // The stub-agent frame from the acceptance, verbatim in shape: a `›` row with no SGR at all.
    assert!(!codex_draft_in_box("› Ready\n"));
    // A `[Pasted Content ` label is still a draft without a style, because the WORDS are the
    // evidence there and nothing else draws them.
    assert!(codex_draft_in_box("› [Pasted Content 801 chars]\n"));
}

/// A dialog's `› 1. Yes, proceed` row is a CHOICE, not a composer awaiting text. The dialog arm of
/// `pane_refusal` runs first and gives the clearer reason; this must not claim a draft as well.
#[test]
fn a_codex_dialog_option_row_is_not_a_draft() {
    assert!(!codex_draft_in_box(FIXTURE_CODEX_DIALOG_APPROVAL));
}

/// No Codex composer on screen: no draft to report, and the prompt check owns that decision.
#[test]
fn no_codex_composer_reports_no_draft() {
    assert!(!codex_draft_in_box(FIXTURE_CLAUDE_STYLED_HINT));
    assert!(!codex_draft_in_box("captured\n"));
}
