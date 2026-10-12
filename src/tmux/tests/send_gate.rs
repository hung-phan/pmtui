//! Tests for what pmtui's `s` key reads beyond the shared prompt predicate: a person's draft in
//! Claude's input box (styled captures) and the states that draw the box but must not be typed
//! into. Every Claude fixture is a live v2.1.296 capture.

use super::*;
use crate::tmux::activity::has_composer;
use crate::tmux::send_gate::{SendBlocker, claude_draft_in_box, send_blocker, unstyled};

#[test]
fn unstyled_strips_every_escape_and_keeps_the_text() {
    let plain = unstyled(FIXTURE_CLAUDE_STYLED_HINT);
    assert!(!plain.contains('\x1b'), "no escape survives: {plain:?}");
    assert!(plain.contains("❯ Try \"how do I log an error?\""));
}

#[test]
fn every_live_box_is_a_prompt_whatever_it_holds() {
    for (name, styled) in [
        ("hint", FIXTURE_CLAUDE_STYLED_HINT),
        ("after a turn", FIXTURE_CLAUDE_STYLED_AFTER_TURN),
        ("draft", FIXTURE_CLAUDE_STYLED_DRAFT),
        ("multi-line draft", FIXTURE_CLAUDE_STYLED_MULTILINE_DRAFT),
    ] {
        assert!(
            has_composer(&unstyled(styled)),
            "{name}: the input box is a prompt"
        );
    }
}

#[test]
fn the_dim_hint_and_an_empty_box_are_not_drafts() {
    assert!(
        !claude_draft_in_box(FIXTURE_CLAUDE_STYLED_HINT),
        "the hint is drawn dim, so it is not a person's text"
    );
    assert!(
        !claude_draft_in_box(FIXTURE_CLAUDE_STYLED_AFTER_TURN),
        "an empty box is not a draft, and the echoed message above it is outside the box"
    );
}

#[test]
fn plain_text_in_the_box_is_a_draft_on_any_of_its_rows() {
    assert!(claude_draft_in_box(FIXTURE_CLAUDE_STYLED_DRAFT));
    assert!(claude_draft_in_box(FIXTURE_CLAUDE_STYLED_MULTILINE_DRAFT));
    let second_row_only = concat!(
        "────────────────────────────────\n",
        "❯ \x1b[2mTry \"fix lint errors\"\x1b[0m\n",
        "  still typing\n",
        "────────────────────────────────\n",
    );
    assert!(
        claude_draft_in_box(second_row_only),
        "a draft row below a dim first row is still a draft"
    );
}

#[test]
fn an_unstyled_hint_reads_as_a_draft_which_refuses() {
    // Without styles the hint is indistinguishable from typed text. Reading it as a draft
    // refuses the send: annoying, never harmful.
    assert!(claude_draft_in_box(&unstyled(FIXTURE_CLAUDE_STYLED_HINT)));
}

#[test]
fn no_claude_box_means_no_draft_verdict() {
    assert!(!claude_draft_in_box("❯ Tell me a joke.\n● Sure.\n"));
    assert!(!claude_draft_in_box(FIXTURE_CODEX_IDLE));
}

#[test]
fn an_echoed_message_outside_the_box_is_not_a_prompt() {
    assert!(!has_composer(
        "❯ Tell me a joke.\n\n● Why do programmers…\n"
    ));
    assert!(
        !has_composer("❯ Try \"write a test for <filepath>\"\n"),
        "outside its box, even the hint's own wording is not a prompt"
    );
}

#[test]
fn a_survey_or_usage_limit_by_the_box_blocks_the_send() {
    let survey = concat!(
        "● Done.\n",
        "\n",
        "How is Claude doing this session? (optional)\n",
        "1: Bad    2: Fine   3: Good   0: Dismiss\n",
        "────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────\n",
    );
    assert_eq!(send_blocker(survey), Some(SendBlocker::Survey));

    let limit = concat!(
        "● Done.\n",
        "\n",
        "⚠ Usage limit reached · continuing automatically at 5pm · esc to cancel\n",
        "────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────\n",
    );
    assert_eq!(send_blocker(limit), Some(SendBlocker::UsageLimit));

    let below = concat!(
        "────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────\n",
        "  How is Claude doing this session? (optional)\n",
    );
    assert_eq!(send_blocker(below), Some(SendBlocker::Survey));
}

#[test]
fn a_blocker_phrase_in_the_transcript_does_not_block() {
    let mentioned = concat!(
        "● Your API returned \"usage limit reached\" twice today.\n",
        "\n",
        "────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────\n",
    );
    assert_eq!(
        send_blocker(mentioned),
        None,
        "the agent's answer is transcript, separated from the box by a blank row"
    );
    assert_eq!(
        send_blocker(&unstyled(FIXTURE_CLAUDE_STYLED_AFTER_TURN)),
        None
    );
}

/// A box whose top rule carries a title, as a terminal agent draws it to label the session or
/// turn the box belongs to. Styled, with a dim hint.
const TITLED_BOX: &str = concat!(
    "● Done.\n",
    "\n",
    "──────────────────────────────── Fix the parser ─\n",
    "❯ \x1b[2mFind and fix a bug in @filename\x1b[0m\n",
    "────────────────────────────────────────────────\n",
    "  model · Context 94% left\n",
);

#[test]
fn a_titled_top_rule_still_opens_the_box() {
    let plain = unstyled(TITLED_BOX);
    assert!(has_composer(&plain), "the titled box is a prompt");
    assert_eq!(
        crate::tmux::classify_pane(&plain),
        crate::tmux::PaneActivity::Idle
    );
    assert!(
        !claude_draft_in_box(TITLED_BOX),
        "the dim hint is not a draft"
    );
    let drafted = TITLED_BOX.replace(
        "\x1b[2mFind and fix a bug in @filename\x1b[0m",
        "half typed",
    );
    assert!(claude_draft_in_box(&drafted), "plain text is a draft");
}

#[test]
fn a_blocker_above_a_titled_box_still_blocks() {
    let pane = concat!(
        "⚠ Usage limit reached · resets 5pm\n",
        "──────────────────────────────── Fix the parser ─\n",
        "❯\u{a0}\n",
        "────────────────────────────────────────────────\n",
    );
    assert_eq!(send_blocker(pane), Some(SendBlocker::UsageLimit));
}

#[test]
fn a_titled_rule_needs_a_long_lead_and_a_closing_rule() {
    for top in [
        "──── Fix the parser ─",                             // lead too short
        "──────────────────────────────── Fix the parser",   // no closing `─`
        "Fix the parser ──────────────────────────────────", // title first
    ] {
        let pane = format!("{top}\n❯\u{a0}Try \"x\"\n────────────────────────────────\n");
        assert!(!has_composer(&pane), "not a box top: {top:?}");
    }
}
