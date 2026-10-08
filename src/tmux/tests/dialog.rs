//! Tests for dialog recognition: the captured claude and codex dialogs it must
//! extract, and — the larger half — the prose and near-miss shapes it must
//! decline, because a false dialog parks a session that is working fine.

use super::*;
use crate::tmux::activity::{PaneActivity, classify_pane};
use crate::tmux::dialog::{
    CODEX_TRUST_QUESTION, DIALOG_MAX_QUESTION_GAP, PaneDialogClass, PaneDialogMode, classify_dialog,
};
use crate::tmux::dialog_option_is_concrete;

// ---- classify_dialog ------------------------------------------------------

#[test]
fn real_permission_dialog_yields_the_question_and_all_three_options() {
    // THE captured case. First the motivating bug, asserted so it can't silently
    // stop being the reason this exists: a dialog pane has no bare prompt and no
    // busy marker, so `classify_pane` falls to its conservative default `Busy` and
    // the heartbeat would re-park forever.
    assert_eq!(
        classify_pane(FIXTURE_REAL_DIALOG_PERMISSION),
        PaneActivity::Busy,
        "a dialog reads Busy — which is exactly why classify_dialog runs first"
    );
    let d = classify_dialog(FIXTURE_REAL_DIALOG_PERMISSION).expect("dialog recognised");
    assert_eq!(d.question, "Do you want to create hello.txt?");
    assert_eq!(
        d.options,
        vec![
            "Yes".to_string(),
            "Yes, allow all edits during this session (shift+tab)".to_string(),
            "No".to_string(),
        ],
        "the ❯ marker and the `<n>. ` prefix are stripped, order preserved"
    );
    assert_eq!(d.selected_index, Some(0));
    assert_eq!(d.mode, PaneDialogMode::Single);
    assert!(d.checked_indices.is_empty());
    assert_eq!(
        d.class,
        PaneDialogClass::HumanOnly,
        "a tool permission prompt must never be delegated"
    );
}

#[test]
fn classify_dialog_is_none_for_the_idle_busy_and_warmup_panes() {
    // Every pane the heartbeat already handles must stay untouched by the new
    // classifier — a false dialog would park a healthy session on the human.
    for (name, pane) in [
        ("idle", FIXTURE_REAL_IDLE),
        ("idle-after-turn", FIXTURE_REAL_IDLE_AFTER_TURN),
        ("busy", FIXTURE_REAL_BUSY),
        ("busy-warmup", FIXTURE_REAL_BUSY_WARMUP),
        ("deep-footer-busy", FIXTURE_DEEP_FOOTER_BUSY),
    ] {
        assert_eq!(classify_dialog(pane), None, "{name} is not a dialog");
    }
    assert_eq!(classify_dialog(""), None, "empty capture");
    assert_eq!(
        classify_dialog("   \n\t\n "),
        None,
        "whitespace-only capture"
    );
}

#[test]
fn classify_dialog_is_none_for_prose_ending_in_a_question_mark() {
    // Transcript prose (and its `Esc` hints) must not trigger a dialog: there is
    // no numbered option block under the question.
    let prose = concat!(
        "● Should I also add a test for the empty case?\n",
        "  Let me know and I'll do it.\n",
        "\n",
        "────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────\n",
        "  Esc to cancel · Tab to amend\n",
    );
    assert_eq!(classify_dialog(prose), None);
}

#[test]
fn classify_dialog_requires_two_options_and_the_esc_footer() {
    // The two guards that keep the recognition narrow, each removed in isolation.
    let one_option = concat!(
        " Do you want to create hello.txt?\n",
        " ❯ 1. Yes\n",
        "\n",
        " Esc to cancel · Tab to amend\n",
    );
    assert_eq!(
        classify_dialog(one_option),
        None,
        "one option is below DIALOG_MIN_OPTIONS"
    );
    let no_footer = concat!(
        " Do you want to create hello.txt?\n",
        " ❯ 1. Yes\n",
        "   2. No\n",
    );
    assert_eq!(
        classify_dialog(no_footer),
        None,
        "no Esc-to-cancel footer below the options"
    );
}

// ---- classify_dialog, codex -----------------------------------------------

#[test]
fn codex_approval_dialog_yields_the_question_and_all_three_options() {
    // Before the fix this returned None on all three counts (the `›` marker, the
    // context lines between question and options, and the wrapped option), so the
    // pane sat Busy until the 30-minute stall backstop fired a misleading Stuck.
    let d = classify_dialog(FIXTURE_CODEX_DIALOG_APPROVAL).expect("dialog recognised");
    assert_eq!(d.question, "Would you like to run the following command?");
    assert_eq!(
        d.options,
        vec![
            "Yes, proceed (y)".to_string(),
            // The WRAPPED option is folded back into one choice. Without that the run
            // stopped at the wrap and option 3 was dropped entirely.
            "Yes, and don't ask again for commands that start with `awk '{n++} END \
             {print n}' notes.txt` (p)"
                .to_string(),
            "No, and tell Codex what to do differently (esc)".to_string(),
        ],
        "codex's `›` marker and `<n>. ` prefix stripped, wrapped option rejoined"
    );
    assert_eq!(d.selected_index, Some(0));
    assert_eq!(d.mode, PaneDialogMode::Single);
    assert_eq!(
        d.class,
        PaneDialogClass::HumanOnly,
        "a command approval prompt must never be delegated"
    );
}

#[test]
fn dialog_identity_includes_authority_bearing_context() {
    let first = classify_dialog(concat!(
        " Would you like to run the following command?\n",
        " Environment: local\n",
        " $ cargo test\n",
        " › 1. Yes, proceed (y)\n",
        "   2. No (esc)\n",
        " Esc to cancel\n",
    ))
    .expect("first command dialog");
    let second = classify_dialog(concat!(
        " Would you like to run the following command?\n",
        " Environment: local\n",
        " $ cargo publish\n",
        " › 1. Yes, proceed (y)\n",
        "   2. No (esc)\n",
        " Esc to cancel\n",
    ))
    .expect("second command dialog");

    assert_eq!(first.question, second.question);
    assert_eq!(first.options, second.options);
    assert_ne!(
        first.identity_fingerprint(),
        second.identity_fingerprint(),
        "different commands must never share a dialog identity"
    );
}

#[test]
fn dialog_identity_includes_context_above_the_question() {
    let first = classify_dialog(concat!(
        " Account: test\n",
        " Which deploy target should I use?\n",
        " ❯ 1. alpha\n",
        "   2. beta\n",
        " Enter to select · Esc to cancel\n",
    ))
    .expect("test-account dialog");
    let second = classify_dialog(concat!(
        " Account: production\n",
        " Which deploy target should I use?\n",
        " ❯ 1. alpha\n",
        "   2. beta\n",
        " Enter to select · Esc to cancel\n",
    ))
    .expect("production-account dialog");

    assert_eq!(first.question, second.question);
    assert_eq!(first.options, second.options);
    assert_ne!(first.identity_fingerprint(), second.identity_fingerprint());
}

#[test]
fn a_redrawn_dialog_does_not_hash_the_previous_footer_as_its_heading() {
    let live = concat!(
        " Which path should I take?\n",
        " ❯ 1. Path A\n",
        "   2. Path B\n",
        " Enter to select · Esc to cancel\n",
    );
    let redrawn = concat!(
        " Which old path?\n",
        " ❯ 1. Old A\n",
        "   2. Old B\n",
        " Enter to select · Esc to cancel\n",
        " Which path should I take?\n",
        " ❯ 1. Path A\n",
        "   2. Path B\n",
        " Enter to select · Esc to cancel\n",
    );

    assert_eq!(
        classify_dialog(live).unwrap().identity_fingerprint(),
        classify_dialog(redrawn).unwrap().identity_fingerprint()
    );
}

#[test]
fn only_concrete_dialog_options_are_machine_selectable() {
    for concrete in ["prettier", "Run unit and integration tests"] {
        assert!(dialog_option_is_concrete(concrete), "{concrete}");
    }
    for meta in [
        "Type something.",
        "Something else",
        "Chat about this",
        "Provide your own answer",
        "Enter another response",
        "Write a response",
        "Custom answer",
        "Other...",
    ] {
        assert!(!dialog_option_is_concrete(meta), "{meta}");
    }
}

#[test]
fn duplicate_labels_are_not_dashboard_answerable() {
    let radio = classify_dialog(concat!(
        " Which profile should I use?\n",
        " ❯ 1. default\n",
        "   2. default\n",
        " Enter to select · Esc to cancel\n",
    ))
    .expect("radio dialog");
    assert_eq!(radio.class, PaneDialogClass::DelegableChoice);
    assert!(!radio.dashboard_answerable());

    let checkbox = classify_dialog(concat!(
        " Which layers should I run?\n",
        " ❯ 1. [ ] integration\n",
        "   2. [ ] integration\n",
        "      Submit\n",
        " Enter to select · Esc to cancel\n",
    ))
    .expect("checkbox dialog");
    assert_eq!(checkbox.class, PaneDialogClass::DelegableChoice);
    assert!(!checkbox.dashboard_answerable());
}

#[test]
fn claude_goal_choice_is_delegable_and_reports_the_highlighted_index() {
    let pane = concat!(
        " ☐ Test strategy\n",
        "\n",
        "Which harmless test strategy should I use?\n",
        "\n",
        "❯ 1. Unit only\n",
        "     Run just the unit test suite (cargo test).\n",
        "  2. Unit plus tmux\n",
        "     Run unit tests plus the real-tmux integration/acceptance suite.\n",
        "  3. Type something\n",
        "     Provide your own strategy instead of the two above.\n",
        "  4. Type something.\n",
        "────────────────────────────────────────────────────────────\n",
        "  5. Chat about this\n",
        "\n",
        " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
    );
    let d = classify_dialog(pane).expect("choice dialog recognised");
    assert_eq!(
        d.options,
        vec![
            "Unit only Run just the unit test suite (cargo test).".to_string(),
            "Unit plus tmux Run unit tests plus the real-tmux integration/acceptance suite."
                .to_string(),
            "Type something Provide your own strategy instead of the two above.".to_string(),
            "Type something.".to_string(),
        ]
    );
    assert_eq!(d.selected_index, Some(0));
    assert_eq!(d.class, PaneDialogClass::DelegableChoice);
    assert_eq!(d.mode, PaneDialogMode::Single);
    assert!(d.checked_indices.is_empty());
}

#[test]
fn claude_multi_select_extracts_checkbox_state_and_keeps_submit_out_of_options() {
    let pane = concat!(
        "←  ☒ Test layers  ✔ Submit  →\n",
        "\n",
        "Which test layers should I run?\n",
        "\n",
        "❯ 1. [✔] unit\n",
        "  Fast in-process unit tests (cargo test), no external processes.\n",
        "  2. [ ] integration\n",
        "  The integration suite (cargo test --test integration).\n",
        "  3. [✔] real tmux\n",
        "  The ignored acceptance suite that drives a real tmux server.\n",
        "  4. [ ] Type something\n",
        "     Submit\n",
        "────────────────────────────────────────────────────────────\n",
        "  5. Chat about this\n",
        "\n",
        "Enter to select · ↑/↓ to navigate · Esc to cancel\n",
    );
    let d = classify_dialog(pane).expect("multi-select dialog recognised");
    assert_eq!(d.mode, PaneDialogMode::Multiple);
    assert_eq!(d.selected_index, Some(0));
    assert_eq!(d.checked_indices, vec![0, 2]);
    assert_eq!(
        d.options,
        vec![
            "unit Fast in-process unit tests (cargo test), no external processes.".to_string(),
            "integration The integration suite (cargo test --test integration).".to_string(),
            "real tmux The ignored acceptance suite that drives a real tmux server.".to_string(),
            "Type something".to_string(),
        ],
        "checkbox markers and the Submit control are structural, not option text"
    );
    assert_eq!(d.class, PaneDialogClass::DelegableChoice);
}

#[test]
fn codex_goal_choice_is_delegable_and_reports_a_nonfirst_highlight() {
    let pane = concat!(
        "  Which test strategy best matches the requested change?\n",
        "  1. Add only a unit test\n",
        "› 2. Add unit and real-tmux coverage\n",
        "  3. Type something\n",
        "\n",
        "  Press enter to select or esc to cancel\n",
    );
    let d = classify_dialog(pane).expect("choice dialog recognised");
    assert_eq!(d.selected_index, Some(1));
    assert_eq!(d.class, PaneDialogClass::DelegableChoice);
}

#[test]
fn an_unrecognised_dialog_footer_fails_closed_to_human_only() {
    let pane = concat!(
        " Which path should I take?\n",
        " ❯ 1. Path A\n",
        "   2. Path B\n",
        "\n",
        " Esc to cancel\n",
    );
    let d = classify_dialog(pane).expect("numbered dialog still recognised");
    assert_eq!(d.class, PaneDialogClass::HumanOnly);
}

#[test]
fn a_quoted_choice_followed_by_the_real_composer_is_human_only() {
    let pane = concat!(
        "● For reference, the question UI looks like this:\n",
        " Which path should I take?\n",
        " ❯ 1. Path A\n",
        "   2. Path B\n",
        " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
        "────────────────────────────────────────────────────────────\n",
        "❯\u{a0}\n",
        "────────────────────────────────────────────────────────────\n",
        "  Opus | Context: 10%\n",
    );
    let d = classify_dialog(pane).expect("quoted shape remains visible as a dialog");
    assert_eq!(
        d.class,
        PaneDialogClass::HumanOnly,
        "delegation requires the choice footer to be the live pane tail"
    );
}

#[test]
fn permission_wording_stays_human_only_even_with_choice_style_chrome() {
    let pane = concat!(
        " Do you want to create hello.txt?\n",
        " ❯ 1. Yes\n",
        "   2. Yes, allow all edits during this session\n",
        "   3. No\n",
        "\n",
        " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
    );
    let d = classify_dialog(pane).expect("permission dialog recognised");
    assert_eq!(d.class, PaneDialogClass::HumanOnly);

    let codex = concat!(
        "  Would you like to run the following command?\n",
        "› 1. Yes, proceed\n",
        "  2. No\n",
        "\n",
        "  Press enter to select or esc to cancel\n",
    );
    let d = classify_dialog(codex).expect("command approval recognised");
    assert_eq!(d.class, PaneDialogClass::HumanOnly);

    let privilege = concat!(
        " Authorize elevated privileges for the test role?\n",
        " ❯ 1. Yes\n",
        "   2. No\n",
        " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
    );
    let d = classify_dialog(privilege).expect("privilege prompt recognised");
    assert_eq!(
        crate::advise::hard_floor_hit_in(&d.question, &d.options),
        Some("authorize")
    );
}

/// THE BUG THIS CLOSES: the trust dialog blocks the first launch in every directory
/// codex has not seen, and it used to be unrecognised — so the pane sat `Busy`, the
/// nudge gate never fired, and the human heard nothing until the ~30-minute stall
/// backstop called a perfectly healthy session "wedged". Reported three times as
/// "i don't see autopilot does anything".
#[test]
fn codex_trust_dialog_yields_the_question_and_both_options() {
    let d = classify_dialog(FIXTURE_CODEX_DIALOG_TRUST).expect("trust dialog recognised");
    // The QUESTION, not the wrapped paragraph it leads — the human reads this as one
    // line in pmtui, and its original capitalisation survives the lowercased anchor.
    assert_eq!(d.question, "Do you trust the contents of this directory?");
    assert_eq!(
        d.options,
        vec!["Yes, continue".to_string(), "No, quit".to_string()],
        "codex's `›` marker and the `<n>. ` prefix stripped, order preserved"
    );
    assert_eq!(d.selected_index, Some(0));
    assert_eq!(d.mode, PaneDialogMode::Single);
    assert_eq!(d.class, PaneDialogClass::HumanOnly);
    // Still `Busy`, and that is CORRECT, not a leftover: a dialog draws no bare prompt
    // and no busy marker, so `classify_pane` falls to its conservative default — which
    // is exactly why `drive` consults `classify_dialog` FIRST. Reading it Idle would
    // type free text at a numbered choice.
    assert_eq!(
        classify_pane(FIXTURE_CODEX_DIALOG_TRUST),
        PaneActivity::Busy
    );
}

#[test]
fn current_codex_folder_access_dialog_is_human_only() {
    let pane = r#"
  Folder access
  /tmp/project

  Trust this folder? Codex can read, edit, and run files here, subject to your permission settings.

› 1. Trust and continue
  2. Quit




  enter continue · esc quit
"#;
    let dialog = classify_dialog(pane).expect("current trust dialog");
    assert_eq!(dialog.question, "Trust this folder?");
    assert_eq!(
        dialog.options.first().map(String::as_str),
        Some("Trust and continue")
    );
    assert!(
        dialog
            .options
            .get(1)
            .is_some_and(|option| option.starts_with("Quit")),
        "{:?}",
        dialog.options
    );
    assert_eq!(dialog.class, PaneDialogClass::HumanOnly);
}

/// THE REAL DELIVERABLE. The trust dialog is recognised by a phrase anchor, NOT by
/// relaxing the general rules — and this asserts both clauses that reject it are
/// still rejecting it. If someone later "simplifies" the exception away by accepting
/// a mid-line `?` or a bare press-enter footer generally, this test is what fails
/// before the negative tests below start letting prose through.
#[test]
fn the_general_dialog_rules_are_not_widened_for_the_trust_dialog() {
    assert!(
        !FIXTURE_CODEX_DIALOG_TRUST
            .lines()
            .any(|l| l.trim().ends_with('?')),
        "general clause 1 still cannot see it: the `?` is mid-paragraph"
    );
    assert!(
        !FIXTURE_CODEX_DIALOG_TRUST
            .to_ascii_lowercase()
            .contains("esc to cancel"),
        "general clause 3 still cannot see it: the footer is `Press enter to continue`"
    );
    // Same shape, ANCHOR REMOVED: a wrapped mid-line `?`, two numbered options and a
    // `Press enter to continue` footer. If the general rules had been widened for the
    // trust dialog this would be a dialog too — and it must not be, because that shape
    // is ordinary prose plus an ordinary list.
    let same_shape_without_the_anchor = concat!(
        "  Which of these should I do first? Both touch the same module, so the\n",
        "  order matters for the second patch.\n",
        "\n",
        "  1. Extract the helper\n",
        "  2. Fix the off-by-one\n",
        "\n",
        "  Press enter to continue\n",
    );
    assert_eq!(
        classify_dialog(same_shape_without_the_anchor),
        None,
        "the trust dialog's SHAPE must not be enough — only its wording is"
    );
}

/// The anchored exception must not fire on PROSE. Every case here is realistic agent
/// output that carries the two things the general rules refused to accept — a mid-line
/// `?` and a "press enter" hint — because those two, accepted structurally, match a
/// large amount of ordinary text. A false dialog escalates a session that is working
/// fine and hands the human a question nobody asked.
#[test]
fn ordinary_prose_with_a_mid_line_question_and_press_enter_is_never_a_dialog() {
    for (name, pane) in [
        // An agent narrating a plan, with a numbered list right under the question.
        (
            "plan with a numbered list",
            concat!(
                "● Should I refactor this first? The tests are green either way, so\n",
                "  here is the order I would pick:\n",
                "\n",
                "  1. Extract the option collector\n",
                "  2. Add the anchored branch\n",
                "  3. Re-run the suite\n",
                "\n",
                "  Press enter to continue, or tell me to stop\n",
            ),
        ),
        // An agent QUOTING the trust question's subject matter without the wording.
        (
            "prose about directory trust",
            concat!(
                "● Do you want me to trust this directory? Working in an untrusted tree\n",
                "  means codex will re-ask on every launch.\n",
                "\n",
                "  1. Yes\n",
                "  2. No\n",
                "\n",
                "  Press enter to continue\n",
            ),
        ),
        // A README/changelog rendered in the pane: mid-line `?`, numbered sections.
        (
            "rendered document",
            concat!(
                "  Why does this exist? The harness cannot answer a permission prompt\n",
                "  on the human's behalf.\n",
                "\n",
                "  1. Background\n",
                "  2. Design\n",
                "\n",
                "  press enter to page down\n",
            ),
        ),
        // An interactive installer's output — the shape closest to a real dialog.
        (
            "installer output",
            concat!(
                "  Continue with the install? Nothing is written until you choose.\n",
                "\n",
                "  1. Install\n",
                "  2. Abort\n",
                "\n",
                "  Press ENTER to continue\n",
            ),
        ),
        // A pane where "press enter" is the only chrome and the `?` is deep in prose.
        (
            "chatty answer",
            concat!(
                "• I could not tell whether you meant the loop session or the chat\n",
                "  session — which did you mean? I will assume the loop session and\n",
                "  press enter for you if you say nothing.\n",
                "\n",
                "› Summarize recent commits\n",
                "\n",
                "  <codex statusline row>\n",
            ),
        ),
    ] {
        assert_eq!(
            classify_dialog(pane),
            None,
            "prose must not be a dialog: {name}"
        );
    }
}

/// The second guard on the anchored branch, removed in isolation: the anchor ALONE is
/// not enough. An agent that merely talks about the trust dialog (this project's own
/// sessions do, constantly) must not park a healthy session on a fake stop.
#[test]
fn the_trust_anchor_without_a_numbered_option_block_is_not_a_dialog() {
    // The anchor discussed in prose, no options at all.
    let discussed = concat!(
        "● codex asks `Do you trust the contents of this directory?` on the first\n",
        "  launch in any new tree, and neither approval flag skips it.\n",
        "\n",
        "› Summarize recent commits\n",
        "\n",
        "  <codex statusline row>\n",
    );
    assert_eq!(classify_dialog(discussed), None, "anchor in prose only");
    // The anchor with ONE option is still below `DIALOG_MIN_OPTIONS`.
    let one_option = concat!(
        "  Do you trust the contents of this directory? Working with untrusted contents\n",
        "› 1. Yes, continue\n",
    );
    assert_eq!(
        classify_dialog(one_option),
        None,
        "one option is below DIALOG_MIN_OPTIONS"
    );
    // ...and options pushed beyond `DIALOG_MAX_QUESTION_GAP` are a different block, so
    // the anchor cannot reach down the pane to an unrelated numbered list.
    let mut too_far =
        String::from("  Do you trust the contents of this directory? Working with untrusted\n");
    for i in 0..=DIALOG_MAX_QUESTION_GAP {
        too_far.push_str(&format!("  wrapped paragraph line {i}\n"));
    }
    too_far.push_str("› 1. Yes, continue\n  2. No, quit\n");
    assert_eq!(
        classify_dialog(&too_far),
        None,
        "an option block beyond the gap is not this question's block"
    );
}

/// THE LIMITATION, pinned rather than hidden: a pane that QUOTES a whole dialog —
/// anchor line, numbered choices, footer — is detected as that dialog. It cannot be
/// otherwise for a classifier that reads pixels; the same is true of the general rule
/// and always has been (a pane showing [`FIXTURE_REAL_DIALOG_PERMISSION`] is a claude
/// dialog as far as [`classify_dialog`] can tell), and no "is this a quote?" heuristic
/// would be anything but guessing.
///
/// Three things bound the harm, and they are why this is accepted rather than fixed:
///   - the quote must sit inside the last [`DIALOG_TAIL_LINES`] non-empty lines, i.e.
///     at the very bottom of the pane, with the option block within
///     [`DIALOG_MAX_QUESTION_GAP`] of the question;
///   - the outcome is a VISIBLE `Capability` stop the human can dismiss, never a
///     keystroke typed into a working agent and never an auto-answer;
///   - it is the safe direction of the pre-existing trade: the alternative failure —
///     missing a real dialog — is the silent 30-minute `Stuck` this whole feature
///     exists to remove.
///
/// This project's own sessions edit this file, so the case is REAL here, not
/// hypothetical, and a future reader deserves to find it asserted.
#[test]
fn a_pane_quoting_a_whole_dialog_is_detected_as_one_and_that_is_accepted() {
    let quoted = concat!(
        "● For reference, the dialog codex paints looks like this:\n",
        "\n",
        "    Do you trust the contents of this directory? Working with untrusted\n",
        "    contents comes with higher risk of prompt injection.\n",
        "\n",
        "  › 1. Yes, continue\n",
        "    2. No, quit\n",
        "\n",
        "    Press enter to continue\n",
    );
    let d = classify_dialog(quoted).expect("a verbatim quote is indistinguishable");
    assert_eq!(d.question, "Do you trust the contents of this directory?");
    // The bound that keeps it survivable: nothing here answers anything. `classify_*`
    // is pure and sends no keys, so the worst case is a stop the human dismisses.
    assert_eq!(d.options.len(), 2);
}

/// The two dialogs that already worked must be BYTE-IDENTICAL in verdict after the
/// exception was added. Asserted here in one place, because "I added a branch and
/// nothing else changed" is the claim a reviewer actually needs checked.
#[test]
fn the_existing_dialog_verdicts_are_unchanged_by_the_anchored_exception() {
    let claude = classify_dialog(FIXTURE_REAL_DIALOG_PERMISSION).expect("claude dialog");
    assert_eq!(claude.question, "Do you want to create hello.txt?");
    assert_eq!(
        claude.options,
        vec![
            "Yes".to_string(),
            "Yes, allow all edits during this session (shift+tab)".to_string(),
            "No".to_string(),
        ]
    );
    let codex = classify_dialog(FIXTURE_CODEX_DIALOG_APPROVAL).expect("codex dialog");
    assert_eq!(
        codex.question,
        "Would you like to run the following command?"
    );
    assert_eq!(codex.options.len(), 3);
    assert_eq!(codex.options[0], "Yes, proceed (y)");
    assert_eq!(
        codex.options[2],
        "No, and tell Codex what to do differently (esc)"
    );
    // Neither carries the trust anchor, so neither can be reaching the new branch.
    for (name, pane) in [
        ("claude permission", FIXTURE_REAL_DIALOG_PERMISSION),
        ("codex approval", FIXTURE_CODEX_DIALOG_APPROVAL),
    ] {
        assert!(
            !pane.to_ascii_lowercase().contains(CODEX_TRUST_QUESTION),
            "{name} is recognised by the general rules, not the anchor"
        );
    }
}

#[test]
fn classify_dialog_is_none_for_every_codex_non_dialog_pane() {
    for (name, pane) in [
        ("idle", FIXTURE_CODEX_IDLE),
        ("idle-after-turn", FIXTURE_CODEX_IDLE_AFTER_TURN),
        ("busy", FIXTURE_CODEX_BUSY),
        ("warmup-gap", FIXTURE_CODEX_WARMUP_GAP),
        // The free-text question ENDS in `?` and still must not be a dialog: there is
        // no numbered option block under it.
        ("free-text question", FIXTURE_CODEX_QUESTION),
    ] {
        assert_eq!(classify_dialog(pane), None, "codex {name} is not a dialog");
    }
}

#[test]
fn a_question_too_far_above_the_options_is_not_a_dialog() {
    // The guard that keeps `DIALOG_MAX_QUESTION_GAP` from becoming "anything goes":
    // push the option block one line beyond the gap and recognition must stop, or
    // transcript prose could reach an unrelated numbered list further down the pane.
    let mut pane = String::from("  Would you like to run the following command?\n");
    for i in 0..=DIALOG_MAX_QUESTION_GAP {
        pane.push_str(&format!("  context line {i}\n"));
    }
    pane.push_str("› 1. Yes, proceed (y)\n");
    pane.push_str("  2. No (esc)\n");
    pane.push_str("  Press enter to confirm or esc to cancel\n");
    assert_eq!(
        classify_dialog(&pane),
        None,
        "an option block further than DIALOG_MAX_QUESTION_GAP below the question \
         must not be attached to it"
    );
    // …and exactly AT the gap it is still recognised, so the bound is the only thing
    // being tested here.
    let mut ok = String::from("  Would you like to run the following command?\n");
    for i in 0..DIALOG_MAX_QUESTION_GAP {
        ok.push_str(&format!("  context line {i}\n"));
    }
    ok.push_str("› 1. Yes, proceed (y)\n");
    ok.push_str("  2. No (esc)\n");
    ok.push_str("  Press enter to confirm or esc to cancel\n");
    assert!(
        classify_dialog(&ok).is_some(),
        "at the bound it still fires"
    );
}
