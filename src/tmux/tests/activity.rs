//! Tests for the Busy/Idle pane heuristics: the predicate-level pins on the
//! spinner and prompt vocabularies, and the whole-pane verdicts for every claude
//! and codex state we have a real capture of.

use super::*;
use crate::tmux::activity::{
    PANE_TAIL_LINES, PaneActivity, classify_pane, has_composer, idle_fingerprint, is_bare_prompt,
    is_busy_spinner_line, is_codex_composer, is_spinner_glyph, progress_fingerprint,
};

#[test]
fn has_composer_sees_both_engines_prompts_and_no_dialog() {
    // claude's bare prompt, and codex's never-bare composer.
    assert!(has_composer("\u{2500}\u{2500}\n\u{276f}  \n"));
    assert!(has_composer(
        "\u{2022} done\n\u{203a} Summarize recent commits\n"
    ));
    // A pane showing a numbered dialog has NO composer, which is the whole point:
    // pmtui's `s` refuses there rather than letting its trailing Enter confirm a
    // pre-highlighted option.
    assert!(!has_composer(
        " Do you want to create hello.txt?\n \u{276f} 1. Yes\n   2. No\n"
    ));
    assert!(!has_composer("$ plain shell output\n"));
    assert!(!has_composer(""));
    // MID-RESPONSE still has one — both engines keep the composer on screen while
    // generating, so this is deliberately independent of Busy/Idle.
    let busy = "\u{273b} Quantumizing\u{2026} (3s \u{b7} \u{2193} 5 tokens)\n\u{276f}  \n";
    assert!(has_composer(busy));
    assert_eq!(classify_pane(busy), PaneActivity::Busy);
}

#[test]
fn classify_busy_on_esc_to_interrupt_hint() {
    // The canonical claude "streaming" status line.
    assert_eq!(
        classify_pane("✻ Working… (esc to interrupt)"),
        PaneActivity::Busy
    );
}

#[test]
fn classify_busy_on_ctrl_c_to_interrupt_hint() {
    assert_eq!(
        classify_pane("Running a tool (ctrl+c to interrupt)"),
        PaneActivity::Busy
    );
}

#[test]
fn classify_busy_on_braille_spinner_even_above_a_prompt() {
    // A Braille spinner glyph in the tail wins over a trailing bare prompt: without the
    // spinner rule this would classify Idle, so Busy proves the glyph rule fired.
    assert_eq!(classify_pane("\u{28FB} crunching\n❯ "), PaneActivity::Busy);
}

#[test]
fn classify_idle_when_last_line_is_bare_unicode_prompt() {
    assert_eq!(classify_pane("some earlier output\n❯ "), PaneActivity::Idle);
}

#[test]
fn classify_idle_when_last_line_is_bare_ascii_prompt() {
    assert_eq!(classify_pane("done.\n> "), PaneActivity::Idle);
}

#[test]
fn classify_busy_on_arbitrary_output_without_prompt() {
    assert_eq!(
        classify_pane("just some logs\nno prompt on the last line"),
        PaneActivity::Busy
    );
}

#[test]
fn classify_busy_on_empty_or_whitespace_capture() {
    assert_eq!(classify_pane(""), PaneActivity::Busy);
    assert_eq!(classify_pane("   \n\t\n  "), PaneActivity::Busy);
}

#[test]
fn progress_fingerprint_ignores_busy_counters_and_footer_but_tracks_transcript_changes() {
    let first = concat!(
        "• Ran cargo test\n",
        "• Working (3s • esc to interrupt)\n",
        "› Ask Codex to do anything\n",
        "openai.gpt-5.6-sol · Context 40%\n",
    );
    let counters_changed = concat!(
        "• Ran cargo test\n",
        "• Working (93s • esc to interrupt)\n",
        "› Ask Codex to do anything\n",
        "openai.gpt-5.6-sol · Context 41%\n",
    );
    let transcript_changed = concat!(
        "• Ran cargo test\n",
        "  1535 tests passed\n",
        "• Working (94s • esc to interrupt)\n",
        "› Ask Codex to do anything\n",
        "openai.gpt-5.6-sol · Context 41%\n",
    );

    assert_eq!(classify_pane(first), PaneActivity::Busy);
    assert_eq!(classify_pane(counters_changed), PaneActivity::Busy);
    assert_eq!(classify_pane(transcript_changed), PaneActivity::Busy);
    assert_eq!(
        progress_fingerprint(first),
        progress_fingerprint(counters_changed),
        "elapsed time, token counters, and footer churn are not work progress"
    );
    assert_ne!(
        progress_fingerprint(first),
        progress_fingerprint(transcript_changed),
        "new transcript output is observable progress"
    );
}

#[test]
fn spinner_form_warmup_ellipsis_is_busy() {
    // Form 1 — turn warm-up: in-progress ellipsis, no counter yet. REGRESSION
    // (fix round 1): keying busy on the counter alone read this as Idle and
    // would have nudged an agent that was mid-response.
    assert_eq!(
        classify_pane(&pane_with_spinner("✻ Mustering…")),
        PaneActivity::Busy
    );
}

#[test]
fn spinner_form_streaming_with_counter_is_busy() {
    // Form 2 — streaming: ellipsis plus a live token counter. The bare prompt
    // is on screen here too (see SPINNER_PANE_TAIL), so only the spinner keeps
    // this off Idle — which is why the busy scan runs before the prompt scan.
    assert_eq!(
        classify_pane(&pane_with_spinner("✻ Quantumizing… (3s · ↓ 5 tokens)")),
        PaneActivity::Busy
    );
}

#[test]
fn spinner_form_finished_cogitated_summary_is_idle() {
    // Form 3 — finished summary: spinner-led, but no ellipsis and no counter,
    // so it must NOT veto Idle (it lingers on screen for the whole idle period).
    assert_eq!(
        classify_pane(&pane_with_spinner("✻ Cogitated for 20s")),
        PaneActivity::Idle
    );
}

#[test]
fn spinner_form_finished_brewed_summary_is_idle() {
    // Form 4 — another past-tense finished summary, newly observed live. Same
    // shape as form 3, so the verb must not matter: only ellipsis/counter do.
    assert_eq!(
        classify_pane(&pane_with_spinner("✻ Brewed for 4s")),
        PaneActivity::Idle
    );
}

#[test]
fn real_claude_idle_pane_with_footer_is_idle() {
    // The prompt is FIVE lines above the bottom (separator + the 4-row
    // statusline footer), so a "prompt must be last" rule can never fire here.
    // Position-independent idle detection must.
    assert_eq!(classify_pane(FIXTURE_REAL_IDLE), PaneActivity::Idle);
}

#[test]
fn real_claude_idle_after_completed_turn_is_idle() {
    // The hard case: a FINISHED turn leaves a spinner-led summary line
    // (`✻ Cogitated for 20s`) in the tail. It carries no live-progress marker,
    // so it must NOT veto Idle.
    assert!(FIXTURE_REAL_IDLE_AFTER_TURN.contains("✻ Cogitated for 20s"));
    assert_eq!(
        classify_pane(FIXTURE_REAL_IDLE_AFTER_TURN),
        PaneActivity::Idle
    );
}

#[test]
fn real_claude_busy_pane_with_live_spinner_is_busy() {
    // Captured mid-generation. Note the bare prompt is drawn here TOO (asserted
    // below), so only the live-progress spinner `↓ 5 tokens` prevents a false
    // Idle — which is why the busy scan must run before the prompt scan.
    assert!(FIXTURE_REAL_BUSY.contains("↓ 5 tokens"));
    assert!(FIXTURE_REAL_BUSY.contains("❯\u{a0}"));
    assert_eq!(classify_pane(FIXTURE_REAL_BUSY), PaneActivity::Busy);
}

#[test]
fn real_claude_busy_during_spinner_warmup_is_busy() {
    // REGRESSION (fix round 1): captured ~2s into a turn, before the first token
    // arrived. The spinner line is bare `✻ Mustering…` — no `↓`/`↑`/`tokens` yet —
    // so keying busy on the counter alone read this pane as Idle and would have
    // nudged an agent that was mid-response. The in-progress ellipsis is what
    // makes it Busy.
    assert!(FIXTURE_REAL_BUSY_WARMUP.contains("✻ Mustering…"));
    assert!(!FIXTURE_REAL_BUSY_WARMUP.contains("tokens"));
    assert!(FIXTURE_REAL_BUSY_WARMUP.contains("❯\u{a0}"));
    assert_eq!(classify_pane(FIXTURE_REAL_BUSY_WARMUP), PaneActivity::Busy);
}

#[test]
fn deep_footer_keeps_the_spinner_inside_the_window_and_is_busy() {
    // The `PANE_TAIL_LINES` pin. Both facts are asserted, not assumed, so the
    // test explains itself when it breaks.
    let non_empty: Vec<&str> = FIXTURE_DEEP_FOOTER_BUSY
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    // (a) The pane is longer than the window, so the window really truncates.
    assert!(
        non_empty.len() > PANE_TAIL_LINES,
        "fixture must outgrow the window, got {} lines",
        non_empty.len()
    );
    // (b) ...and the deep footer still leaves the spinner inside it.
    let up_from_bottom = non_empty.len()
        - non_empty
            .iter()
            .rposition(|l| l.contains("Quantumizing"))
            .expect("fixture must keep a streaming spinner line");
    assert!(
        up_from_bottom <= PANE_TAIL_LINES,
        "spinner is {up_from_bottom} lines up but the window spans only {PANE_TAIL_LINES}"
    );
    assert_eq!(classify_pane(FIXTURE_DEEP_FOOTER_BUSY), PaneActivity::Busy);
}

#[test]
fn bare_prompt_not_on_last_line_is_idle() {
    // Minimal synthetic of the same shape: bare prompt then a 4-line footer.
    let pane = concat!(
        "❯\u{a0}\n",
        "────────────────────────────────\n",
        "  <statusline row 1>\n",
        "  <statusline row 2>\n",
        "  ⏵⏵ auto mode on (shift+tab to cycle)\n",
    );
    assert_eq!(classify_pane(pane), PaneActivity::Idle);
}

#[test]
fn every_observed_spinner_form_is_classified_correctly() {
    // Verbatim spinner lines from all nine real captures: busy-1 (warm-up) and
    // busy-2..6, sampled at 2s intervals through one turn. Pinning them at the
    // predicate level keeps the whole observed vocabulary covered without
    // embedding six near-identical panes.
    for line in [
        "✻ Mustering…",
        "✻ Quantumizing… (3s · ↓ 5 tokens)",
        "✻ Quantumizing… (5s · ↓ 98 tokens)",
        "✻ Quantumizing… (7s · ↓ 98 tokens · thinking with xhigh effort)",
        "✻ Quantumizing… (9s · ↓ 211 tokens · thought for 3s)",
        "✻ Quantumizing… (11s · ↓ 412 tokens)",
        // The plain-ASCII spinner lead older/plainer builds draw instead of ✻.
        "* Pondering the request (↓ 12 tokens)",
    ] {
        assert!(is_busy_spinner_line(line), "must read as busy: {line}");
    }
    // The finished-turn summaries: spinner-led, but neither ellipsis nor counter.
    assert!(!is_busy_spinner_line("✻ Cogitated for 20s"));
    assert!(!is_busy_spinner_line("✻ Brewed for 4s"));
    // Ellipsis-bearing lines that are NOT spinner-led must stay non-busy, or an
    // idle pane would be pinned at Busy forever by ordinary footer/transcript text.
    assert!(!is_busy_spinner_line("     … +1 line (ctrl+o to expand)"));
    assert!(!is_busy_spinner_line(
        "● Reading 1 file… (ctrl+o to expand)"
    ));
    assert!(!is_busy_spinner_line(
        "  ⚠️ a wrapped statusline row that trails off in an ellipsis…"
    ));
}

/// THE ANIMATED SPINNER LEAD. Every one of these six leads was captured live from a
/// STREAMING claude pane, byte-identical apart from the first character — the spinner
/// animates through them. The original predicate listed only `*`, `✻` and `·`, so
/// THREE of the six fell through to the bare-prompt test and classified the pane
/// **Idle** while the agent was mid-response, which makes the harness type a nudge
/// into a working agent. Pinning all six here is what stops that regressing; the
/// non-spinner leads below are the other half of the guarantee, because a lead widened
/// too far would pin an ordinary transcript row at Busy forever.
#[test]
fn every_captured_spinner_lead_reads_busy_and_other_row_leads_do_not() {
    for lead in ['*', '·', '✢', '✶', '✻', '✽'] {
        let line = format!("{lead} Scampering…");
        assert!(
            is_busy_spinner_line(&line),
            "captured streaming lead {lead:?} (U+{:04X}) must read BUSY — reading it \
             Idle nudges an agent that is mid-response",
            lead as u32
        );
    }
    // The leads that begin OTHER claude rows must NOT become spinner leads, even with
    // an ellipsis present: `●` tool header, `⎿` tool result, box drawing, an arrow.
    for lead in ['●', '⎿', '─', '│', '→'] {
        let line = format!("{lead} something in progress…");
        assert!(
            !is_busy_spinner_line(&line),
            "row lead {lead:?} (U+{:04X}) must NOT count as a spinner, or ordinary \
             transcript text pins the pane at Busy forever",
            lead as u32
        );
    }
}

/// THE BUG: every codex state used to classify `Busy`, so the twice-Idle nudge gate
/// could never fire, no codex session was ever nudged, and each one ended in a
/// misleading 30-minute `Stuck`. This pins the verdict for every state at once, which
/// is the deliverable — a widened character class alone would not have been.
#[test]
fn every_codex_pane_state_classifies_correctly() {
    for (name, pane, want) in [
        ("idle at composer", FIXTURE_CODEX_IDLE, PaneActivity::Idle),
        (
            "idle after a finished turn",
            FIXTURE_CODEX_IDLE_AFTER_TURN,
            PaneActivity::Idle,
        ),
        (
            "free-text question",
            FIXTURE_CODEX_QUESTION,
            PaneActivity::Idle,
        ),
        ("mid-work", FIXTURE_CODEX_BUSY, PaneActivity::Busy),
        // Both dialogs must stay Busy: a dialog is not a prompt awaiting prose, and
        // reading one as Idle would type free text at a numbered choice.
        (
            "approval dialog",
            FIXTURE_CODEX_DIALOG_APPROVAL,
            PaneActivity::Busy,
        ),
        (
            "startup trust dialog",
            FIXTURE_CODEX_DIALOG_TRUST,
            PaneActivity::Busy,
        ),
    ] {
        assert_eq!(classify_pane(pane), want, "codex {name}");
    }
}

#[test]
fn codex_composer_is_never_bare_so_only_the_new_predicate_can_see_it() {
    // The fact the whole fix turns on, asserted so it cannot silently stop being the
    // reason this predicate exists: codex's composer always carries a placeholder, so
    // `is_bare_prompt` returns false for it no matter which placeholder is drawn.
    for line in ["› Summarize recent commits", "› Write tests for @filename"] {
        assert!(!is_bare_prompt(line), "codex composer is not bare: {line}");
        assert!(is_codex_composer(line), "but it IS a composer: {line}");
    }
    // A codex DIALOG CHOICE wears the same `›` marker and must NOT read as a composer.
    for line in [
        "› 1. Yes, proceed (y)",
        "› 1. Yes, continue",
        "  2. No, quit",
    ] {
        assert!(
            !is_codex_composer(line),
            "a numbered choice is not a composer: {line}"
        );
    }
    // And the claude side is untouched: `❯` is a bare prompt, never a codex composer.
    assert!(is_bare_prompt("❯\u{a0}"));
    assert!(!is_codex_composer("❯\u{a0}"));
}

#[test]
fn codex_busy_marker_beats_the_composer_and_the_echoed_user_message() {
    // The composer is on screen mid-turn, and the submitted message is echoed with its
    // own `›` lead — so TWO `›`-led lines are present while codex is working. Only the
    // busy scan running first keeps this off Idle.
    assert!(FIXTURE_CODEX_BUSY.contains("esc to interrupt"));
    assert_eq!(
        FIXTURE_CODEX_BUSY
            .lines()
            .filter(|l| l.starts_with('›'))
            .count(),
        2,
        "fixture must carry BOTH the echoed user message and the composer"
    );
    assert_eq!(classify_pane(FIXTURE_CODEX_BUSY), PaneActivity::Busy);
}

#[test]
fn codex_transcript_leads_never_become_spinner_glyphs() {
    // codex leads EVERY transcript row with `•` U+2022 — `• Explored`, `• Ran …`,
    // `• Working (…)`, and its prose answers. Adding it to `is_spinner_glyph` would
    // therefore pin any codex pane whose last answer happened to contain an ellipsis
    // at Busy forever, so it is deliberately NOT a spinner lead: codex's busy state is
    // recognised by `esc to interrupt` instead.
    assert!(!is_spinner_glyph('•'));
    assert!(!is_busy_spinner_line("• Reading notes.txt…"));
    assert!(!is_busy_spinner_line("• Explored"));
    // The `›` composer lead must not become one either.
    assert!(!is_spinner_glyph('›'));
}

/// The measured warm-up gap, pinned rather than hidden. See
/// [`FIXTURE_CODEX_WARMUP_GAP`] for why an Idle verdict here is safe.
#[test]
fn codex_warmup_gap_reads_idle_and_is_covered_by_the_confirmation_gate() {
    assert!(
        !FIXTURE_CODEX_WARMUP_GAP.contains("esc to interrupt"),
        "the gap frame is defined by having NO busy marker"
    );
    assert_eq!(
        classify_pane(FIXTURE_CODEX_WARMUP_GAP),
        PaneActivity::Idle,
        "measured: codex paints no busy marker for ~100-200ms after a submit. The \
         two-observation gate (2 Idles, 5s apart) is what makes this harmless; if \
         that gate is ever removed, this is the fixture that says why it cannot be."
    );
}

// --- idle_fingerprint: the content-stability signal behind the confirmation gate ------

#[test]
fn idle_fingerprint_changes_when_the_transcript_above_the_prompt_grows() {
    // Bug A in one predicate. A STREAMING claude classifies Idle — bare `❯`, no busy marker
    // on screen while answer tokens render (`●` U+25CF is deliberately not a spinner glyph)
    // — but its response block GROWS frame to frame. The fingerprint must move so the
    // heartbeat's confirmation gate RE-ARMS instead of typing a nudge into a working agent.
    let v1 = "● The History of Timekeeping\n  1. Sundials and water clocks\n────\n❯ \n────\nOpus 4.8 (1M context) | Context: 3%\n⏵⏵ auto mode on\n";
    let v2 = "● The History of Timekeeping\n  1. Sundials and water clocks\n  2. The mechanical escapement\n────\n❯ \n────\nOpus 4.8 (1M context) | Context: 4%\n⏵⏵ auto mode on\n";
    // Premise: BOTH frames classify Idle — this is exactly the false-Idle streaming window,
    // not a Busy pane the gate would refuse anyway.
    assert_eq!(classify_pane(v1), PaneActivity::Idle);
    assert_eq!(classify_pane(v2), PaneActivity::Idle);
    assert_ne!(
        idle_fingerprint(v1),
        idle_fingerprint(v2),
        "a growing response block above the prompt must move the fingerprint"
    );
}

#[test]
fn idle_fingerprint_ignores_the_volatile_footer_below_the_prompt() {
    // THE LOAD-BEARING EXCLUSION. The real UI draws a statusline BELOW the prompt whose
    // counters (the `Context %`, the `Session` clock) tick every second even while the agent
    // WAITS. If the fingerprint hashed them, two captures a recheck apart would never match
    // and a genuinely idle session would NEVER earn its two byte-stable observations — so it
    // would never be nudged. A stable transcript with only footer churn must fingerprint
    // identically. Only the `Session` clock differs here, and it sits below the `❯`.
    let a = "● Done — wrote the file\n────\n❯ \n────\nOpus 4.8 (1M context) | Context: 8% | Session: 11h 05m\n⏵⏵ auto mode on\n";
    let b = "● Done — wrote the file\n────\n❯ \n────\nOpus 4.8 (1M context) | Context: 8% | Session: 11h 04m\n⏵⏵ auto mode on\n";
    assert_eq!(classify_pane(a), PaneActivity::Idle);
    assert_eq!(
        idle_fingerprint(a),
        idle_fingerprint(b),
        "footer churn below the prompt must NOT move the fingerprint, or an idle session \
         never confirms and is never nudged"
    );
}

#[test]
fn idle_fingerprint_falls_back_to_the_whole_capture_with_no_prompt_line() {
    // The gate only calls this on an Idle-classified capture (which has a prompt), but be
    // defensive: with no prompt line the fingerprint hashes the whole capture and still
    // moves when the content does, so a missing boundary can never freeze it into a
    // spurious "stable" match.
    let a = "some transcript\nno prompt here\n";
    let b = "some transcript\nno prompt here\nand more\n";
    assert_ne!(idle_fingerprint(a), idle_fingerprint(b));
}
