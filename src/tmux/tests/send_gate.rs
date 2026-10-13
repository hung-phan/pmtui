//! Tests for what a pane must show beyond the shared prompt predicate before anything is typed
//! into it: a person's draft in Claude's input box (styled captures), the feedback survey, and the
//! styled idle verdict pmd nudges on. Every `FIXTURE_CLAUDE_STYLED_*` is a live v2.1.296 capture.

use super::*;
use crate::tmux::activity::has_composer;
use crate::tmux::activity::{PaneActivity, classify_pane};
use crate::tmux::send_gate::{
    classify_pane_styled, claude_draft_in_box, feedback_survey, unstyled,
};

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
fn a_survey_by_the_box_blocks_the_send() {
    let above = concat!(
        "● Done.\n",
        "\n",
        "How is Claude doing this session? (optional)\n",
        "1: Bad    2: Fine   3: Good   0: Dismiss\n",
        "────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────\n",
    );
    assert!(feedback_survey(above));
    assert_eq!(
        classify_pane(above),
        PaneActivity::Busy,
        "pmd must not type a nudge whose digits would answer it"
    );

    let below = concat!(
        "────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────\n",
        "  How is Claude doing this session? (optional)\n",
    );
    assert!(feedback_survey(below));
}

#[test]
fn the_survey_question_in_the_transcript_does_not_block() {
    // The live layout: the transcript abuts the box's top rule with no blank row between.
    let quoted_by_the_agent = unstyled(FIXTURE_CLAUDE_STYLED_AFTER_TURN).replace(
        "⏺ ok",
        "⏺ Claude asked \"How is Claude doing this session?\" twice.",
    );
    assert!(
        quoted_by_the_agent.contains("twice."),
        "the fixture changed"
    );
    assert!(!feedback_survey(&quoted_by_the_agent));

    let wrapped_answer = concat!(
        "⏺ The survey reads:\n",
        "  how is claude doing this session? It is optional.\n",
        "────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────\n",
    );
    assert!(
        feedback_survey(wrapped_answer),
        "a row that BEGINS with the question is the survey's shape, even when it is a wrapped \
         answer: the refusal keeps the Message, so this direction costs a press, never a send"
    );
    assert!(!feedback_survey(&unstyled(
        FIXTURE_CLAUDE_STYLED_AFTER_TURN
    )));
}

#[test]
fn rows_far_above_the_box_are_transcript() {
    let pane = concat!(
        "How is Claude doing this session? (optional)\n",
        "line one\n",
        "line two\n",
        "line three\n",
        "line four\n",
        "────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────\n",
    );
    assert!(
        !feedback_survey(pane),
        "only the rows directly above the rule are notices"
    );
}

#[test]
fn plain_text_counts_only_an_empty_box_as_idle() {
    for (name, styled) in [
        ("hint", FIXTURE_CLAUDE_STYLED_HINT),
        ("draft", FIXTURE_CLAUDE_STYLED_DRAFT),
        ("multi-line draft", FIXTURE_CLAUDE_STYLED_MULTILINE_DRAFT),
    ] {
        assert_eq!(
            classify_pane(&unstyled(styled)),
            PaneActivity::Busy,
            "{name}: without styles the hint and a draft are the same text"
        );
    }
    assert_eq!(
        classify_pane(&unstyled(FIXTURE_CLAUDE_STYLED_AFTER_TURN)),
        PaneActivity::Idle
    );
}

#[test]
fn the_styled_verdict_is_idle_on_a_hint_and_busy_on_a_draft() {
    assert_eq!(
        classify_pane_styled(FIXTURE_CLAUDE_STYLED_HINT),
        PaneActivity::Idle
    );
    assert_eq!(
        classify_pane_styled(FIXTURE_CLAUDE_STYLED_AFTER_TURN),
        PaneActivity::Idle
    );
    for (name, styled) in [
        ("draft", FIXTURE_CLAUDE_STYLED_DRAFT),
        ("multi-line draft", FIXTURE_CLAUDE_STYLED_MULTILINE_DRAFT),
    ] {
        assert_eq!(
            classify_pane_styled(styled),
            PaneActivity::Busy,
            "{name}: pmd must not glue a nudge onto a person's unsent text"
        );
    }
    let drafted = concat!(
        "────────────────────────────────\n",
        "❯ wait, do not deploy yet\n",
        "────────────────────────────────\n",
    );
    assert_eq!(classify_pane(drafted), PaneActivity::Busy);
    assert_eq!(classify_pane_styled(drafted), PaneActivity::Busy);
}

#[test]
fn a_grey_hint_colour_is_dim_but_a_highlighted_command_is_not() {
    for grey in ["\x1b[38;5;244m", "\x1b[90m", "\x1b[38;2;128;128;128m"] {
        let pane = FIXTURE_CLAUDE_STYLED_HINT.replace("\x1b[2m", grey);
        assert_ne!(pane, FIXTURE_CLAUDE_STYLED_HINT, "the fixture changed");
        assert!(
            !claude_draft_in_box(&pane),
            "{grey:?}: a hint greyed by colour is still a hint"
        );
    }
    let command = concat!(
        "────────────────────────────────\n",
        "❯ \x1b[38;5;75m/compact\x1b[0m\n",
        "────────────────────────────────\n",
    );
    assert!(
        claude_draft_in_box(command),
        "a coloured typed command is a draft"
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
    assert_eq!(classify_pane_styled(TITLED_BOX), PaneActivity::Idle);
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
fn a_survey_above_a_titled_box_still_blocks() {
    let pane = concat!(
        "How is Claude doing this session? (optional)\n",
        "──────────────────────────────── Fix the parser ─\n",
        "❯\u{a0}\n",
        "────────────────────────────────────────────────\n",
    );
    assert!(feedback_survey(pane));
}

#[test]
fn a_titled_rule_needs_a_long_lead_and_a_closing_rule() {
    for top in [
        "──── Fix the parser ─",                             // lead too short
        "──────────────────────────────── Fix the parser",   // no closing `─`
        "Fix the parser ──────────────────────────────────", // title first
        "────────────────Fix the parser ─",                  // no space before the title
        "──────────────── Fix the parser─",                  // no space after it
        "──────────────── a ── b ─",                         // a rule inside the title
    ] {
        let pane = format!("{top}\n❯\u{a0}Try \"x\"\n────────────────────────────────\n");
        assert!(!has_composer(&pane), "not a box top: {top:?}");
    }
}

#[test]
fn a_markdown_separator_over_a_quoted_line_is_not_a_box() {
    let long_title = format!("──────────────── {} ─", "x".repeat(81));
    let pane = format!("{long_title}\n❯ quoted\n────────────────────────────────\n");
    assert!(
        !has_composer(&pane),
        "a title past the bound is not a box top"
    );

    let streaming = concat!(
        "⏺ Here is the layout:\n",
        "──────────────── src/foo.rs ───\n",
        "❯ cargo run\n",
        "────────────────────────────────\n",
    );
    assert_eq!(
        classify_pane(streaming),
        PaneActivity::Busy,
        "plain text never reads a box holding text as idle"
    );
    assert_eq!(
        classify_pane_styled(streaming),
        PaneActivity::Busy,
        "an agent's own text is not dim, so it reads as a draft"
    );
}

#[test]
fn the_styled_verdict_is_busy_on_a_dialog_even_inside_a_box() {
    let dialog = concat!(
        "────────────────────────────────\n",
        "❯ \x1b[2mDo you want to proceed?\x1b[0m\n",
        "  \x1b[2m1. Yes\x1b[0m\n",
        "  \x1b[2m2. No\x1b[0m\n",
        "────────────────────────────────\n",
        "Esc to cancel\n",
    );
    assert!(crate::tmux::classify_dialog(&unstyled(dialog)).is_some());
    assert_eq!(
        classify_pane_styled(dialog),
        PaneActivity::Busy,
        "an Enter typed here would answer the dialog"
    );
    assert_eq!(
        classify_pane_styled(FIXTURE_CLAUDE_STYLED_HINT),
        PaneActivity::Idle
    );
}
