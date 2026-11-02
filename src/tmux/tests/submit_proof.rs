//! The PRE-SEND question, for both engines: is there already unsent text in the composer?
//!
//! [`codex_draft_in_box`] is the half this file covers, and it is a different question from
//! `real::codex_draft_visible`'s submit proof. The proof asks "is MY text still showing?" and can
//! compare against what it sent. This one asks "is ANY unsent text showing?" and runs before there
//! is a text to compare — which is why its answer on an unstyled capture is the subtle case below.
//!
//! It exists so a refused send does not DOUBLE. `send_keys` bails when codex's composer still
//! holds the message after its Enter presses, `pmtui message` maps that to the retry family, and a
//! retry blind to the stranded text would paste onto it.

use super::*;
use crate::tmux::send_gate::codex_draft_in_box;

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
