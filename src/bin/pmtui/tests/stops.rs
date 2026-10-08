//! The stops block above the transcript: the rows it reserves, the ladder it degrades
//! down when it runs short, how long it says a decision has been waiting, and the
//! product string a synthesized stop renders instead of a bare kind.

use super::*;

#[test]
fn preview_reserves_stops_above_the_log() {
    // Stops are the thing a human must ACT on, so their rows are reserved before
    // the log tail — they stay on screen even in a short pane.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "blocked", "seq": 9, "status": "need a decision" }"#),
    );
    app.projects[0].stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render stops");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains(" stops "),
        "Stops section missing: {screen}"
    );
    // The id is still on screen but DEMOTED to a dim tail: it is how a human ties this row
    // to `answers.json` and the pmd log, and it is worth nothing as a headline. It used to
    // lead the row in BOLD, 26 opaque characters wide, pushing the risk, the kind and the
    // question to the right.
    assert!(screen.contains("s1"), "stop id missing entirely: {screen}");
    assert!(
        !screen.contains("[s1]"),
        "the id is back in the headline position: {screen}"
    );
    assert!(
        screen.contains(" preview "),
        "Log section missing: {screen}"
    );
    // The rule NAMES THE KEY that shows the whole stop. This block is a truncating summary
    // by design (its rows are reserved out of the log's), so without this a human looking
    // at a clipped question has no signal that anything will show them the rest — which is
    // exactly half of what the user reported.
    assert!(
        screen.contains("press s to answer"),
        "the Stops rule must point at `a` in plain language: {screen}"
    );
}

#[test]
fn preview_says_how_long_a_decision_has_been_waiting() {
    // `OpenStop.first_posted` has been on disk since day one and was rendered
    // nowhere, so nothing anywhere said a decision had been waiting three hours —
    // and `!!` looked identical for one stop and for five.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "blocked", "seq": 9, "status": "need a decision" }"#),
    );
    app.projects[0].posture = Posture::NeedsYou;
    app.projects[0].stops = vec![
        stop("s1", "ambiguity", RiskClass::Medium),
        stop("s2", "ambiguity", RiskClass::Medium),
    ];
    app.projects[0].oldest_stop_since = Some(SystemClock.now() - 3 * 3_600);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("⧗ waiting 3h · 2 decisions"),
        "no waiting age: {screen}"
    );
    // A short pane STILL shows the transcript: the age rides the head line that
    // already exists rather than taking a row of its own. Adding a row here starved
    // the log to zero rows at 100x14 — measured, and the reason for this shape.
    let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render tiny");
    let tiny = screen_text(&terminal);
    assert!(
        tiny.contains(" stops ") && tiny.contains(" preview "),
        "{tiny}"
    );
}

#[test]
fn preview_says_one_decision_in_the_singular() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(dir.path(), None);
    app.projects[0].posture = Posture::NeedsYou;
    app.projects[0].stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    app.projects[0].oldest_stop_since = Some(SystemClock.now() - 90);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    assert!(screen_text(&terminal).contains("· 1 decision"));
}

#[test]
fn preview_stops_rule_and_posture_badge_are_filled_when_a_stop_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(dir.path(), None);
    app.projects[0].posture = Posture::NeedsYou;
    app.projects[0].stops = vec![stop("s1", "publish", RiskClass::Hard)];
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    // The rule over the block to act on, and the posture badge above it, carry the
    // row's severity — so row and preview read as one system.
    let rule = styles_under(&terminal, " stops ").expect("no Stops rule");
    assert!(
        rule.iter()
            .all(|(fg, m)| *fg == agent_manager::theme::hard() && m.contains(Modifier::REVERSED)),
        "the Stops rule is still the same accent as Log: {rule:?}"
    );
    // Target the PREVIEW HEAD row (which carries the `autopilot` meta field), not the SESSIONS row
    // that also says "needs you". (Was keyed on "step #", which is gone now that it's always 0.)
    let badge = styles_under_row(&terminal, "autopilot", "needs you").expect("no badge");
    assert!(
        badge
            .iter()
            .all(|(fg, _)| *fg == agent_manager::theme::hard())
    );

    // With nothing open the rule stays the quiet accent.
    app.projects[0].stops.clear();
    app.projects[0].posture = Posture::Monitoring;
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render calm");
    let rule = styles_under(&terminal, " preview ").expect("no Log rule");
    assert!(rule.iter().all(|(_, m)| !m.contains(Modifier::REVERSED)));
}

#[test]
fn a_low_risk_stop_is_dimmed_and_carries_its_own_glyph() {
    // Low risk was `Style::default()` — no styling at all, i.e. indistinguishable
    // from the agent's own question text below it.
    let lines = stop_preview_lines(&stop("s1", "ambiguity", RiskClass::Low), 60, 8);
    let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(header.contains(attention::LOW_RISK), "no glyph: {header}");
    let glyph = lines[0]
        .spans
        .iter()
        .find(|s| s.content.contains(attention::LOW_RISK))
        .expect("no glyph span");
    assert!(glyph.style.add_modifier.contains(Modifier::DIM));
    // Hard is filled, and its glyph is the stuck bucket's shape.
    let hard = stop_preview_lines(&stop("s2", "publish", RiskClass::Hard), 60, 8);
    let g = hard[0]
        .spans
        .iter()
        .find(|s| s.content.contains('✕'))
        .expect("no hard glyph");
    assert!(g.style.add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn the_stops_block_spends_the_rows_it_has_and_degrades_down_a_ladder() {
    // The user's actual stop, at the three shapes that matter. The complaint was about how
    // this block LOOKS on the main view, and "looks good" here is a measurable thing: is
    // the question wrapped rather than cut off, is every option readable, is the structure
    // indented, and does the transcript survive.
    let s = stop_asking(
        "stop-test-0-1786869300",
        "confirm_done",
        "Goal 'tell me animal joke' looks satisfied: joke told in chat, written to \
         /workplace/phahng/test/jokes.md, and sent via Slack self-DM. Close the session, \
         or keep going?",
        &[
            "Close the session - the joke was delivered",
            "Keep going: more animal jokes (different animals / styles)",
            "Keep going: a specific animal or style I name",
        ],
    );

    // ROOMY (the 11 rows a 26-row pane grants): the whole question, wrapped, and every
    // option on its own row. Nothing elided at all.
    let rows = stop_preview_lines(&s, 96, 11);
    let text: Vec<String> = rows.iter().map(|l| l.to_string()).collect();
    assert!(
        text.iter().any(|t| t.contains("or keep going?")),
        "the question's TAIL must survive at this size: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.contains('\u{2026}')),
        "nothing should be elided with 11 rows: {text:?}"
    );
    for (i, want) in [
        "1 Close the session",
        "2 Keep going: more",
        "3 Keep going: a specific",
    ]
    .iter()
    .enumerate()
    {
        assert!(
            text.iter().any(|t| t.contains(want)),
            "option {} is not on a row of its own: {text:?}",
            i + 1
        );
    }
    // INDENTED under the header. This is the part that was invisible until the
    // `Wrap { trim: true }` came off the paragraph — `trim` strips leading whitespace, so
    // the structure never reached the screen however the rows were built.
    assert!(
        text[1..]
            .iter()
            .all(|t| t.trim().is_empty() || t.starts_with("  ")),
        "the body must be indented under the header: {text:?}"
    );
    // SPACING between the prose and the choices, which is what the user asked for after
    // seeing the first version of this layout.
    let first_opt = text.iter().position(|t| t.contains("1 Close")).unwrap();
    assert!(
        text[first_opt - 1].trim().is_empty(),
        "there must be a blank row between the question and the options: {text:?}"
    );
    // The header leads with SEVERITY, and the id is a tail rather than a bold headline.
    // The severity CHIP leads, and its padding is inside the fill — that space is what
    // keeps the reversed background off the next word.
    assert!(
        text[0].starts_with(" \u{2715} hard  "),
        "the severity chip must lead, padded: {:?}",
        text[0]
    );
    assert!(
        text[0].ends_with("stop-test-0-1786869300"),
        "the id belongs at the end: {:?}",
        text[0]
    );

    // TIGHT (4 rows): options compact onto ONE row rather than showing a partial list —
    // three options showing two reads as two options. (At FIVE they still each get a row:
    // options outrank extra question rows, which is the documented ladder.)
    let text: Vec<String> = stop_preview_lines(&s, 96, 4)
        .iter()
        .map(|l| l.to_string())
        .collect();
    assert!(text.len() <= 4, "over budget: {text:?}");
    assert!(
        text.iter()
            .any(|t| t.contains("1) Close") && t.contains("2) Keep")),
        "the options must compact onto one row, not vanish: {text:?}"
    );

    // TIGHTEST (2 rows): the one spare row goes to the QUESTION. `a` shows the options.
    let text: Vec<String> = stop_preview_lines(&s, 96, 2)
        .iter()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(text.len(), 2);
    assert!(
        text[1].contains("Goal 'tell me animal joke'"),
        "the last row must be the question: {text:?}"
    );
}

#[test]
fn preview_shows_the_stops_question_and_numbered_options() {
    // The gap this closes: the preview used to render the kind alone, so a human
    // was told a decision was needed but not what was being asked.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "blocked", "seq": 9, "status": "need a decision" }"#),
    );
    app.projects[0].stops = vec![stop_asking(
        "s1",
        "confirm_done",
        "Confirm and close, or keep adding?",
        &["close it", "keep going"],
    )];
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render stops");
    let screen = screen_text(&terminal);
    assert!(screen.contains("s1"), "stop id missing entirely: {screen}");
    assert!(
        screen.contains("confirm_done"),
        "kind still shown: {screen}"
    );
    assert!(
        screen.contains("Confirm and close, or keep adding?"),
        "the agent's question is missing: {screen}"
    );
    // ONE PER ROW when they fit, which at 100x30 they do. The `1) a  2) b` run-on form is
    // now only the COMPACT fallback for a pane with a single row to spare — and the whole
    // reason for the change is that the run-on form routinely made the last option
    // unreadable on a real stop.
    let rows = screen_rows_styled(&terminal);
    for want in ["1 close it", "2 keep going"] {
        assert!(
            rows.iter().any(|(t, _)| t.contains(want)),
            "option {want:?} is not on a row of its own: {screen}"
        );
    }
    // INDENTED under the header, which is the structure that makes this block readable at a
    // glance. It was invisible until the `Wrap { trim: true }` on this paragraph came off —
    // `trim` strips leading whitespace, so every row landed flush against the border and no
    // amount of restructuring upstream could have shown through.
    assert!(
        rows.iter().any(|(t, _)| t.contains("  1 close it")),
        "the option rows lost their indent: {screen}"
    );
}

#[test]
fn preview_question_never_pushes_the_transcript_off_screen() {
    // A paragraph-length question must not grow the pinned Stops block: the block's
    // rows are RESERVED out of the log's, so an unbounded question would silently
    // evict the transcript. Long question, short pane, several stops.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "blocked", "seq": 9, "status": "need a decision" }"#),
    );
    let huge = LIVE_QUESTION.repeat(20);
    // MORE than `PREVIEW_MAX_STOPS`, so something is always elided and the `+N more` marker
    // is always exercised. Four used to overflow the row budget on its own; it no longer
    // does, because every stop is now guaranteed at least its header row (a second decision
    // must never be invisible) — so the count has to come from the stop cap instead.
    app.projects[0].stops = (0..8)
        .map(|i| {
            stop_asking(
                &format!("s{i}"),
                "confirm_done",
                &huge,
                &["a really quite long first choice", "and a second one"],
            )
        })
        .collect();
    for (w, h) in [(100u16, 14u16), (100, 20), (60, 20), (100, 30)] {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, &app))
            .unwrap_or_else(|e| panic!("render {w}x{h}: {e}"));
        let screen = screen_text(&terminal);
        assert!(screen.contains(" stops "), "{w}x{h}: Stops missing");
        assert!(screen.contains(" preview "), "{w}x{h}: Log section evicted");
        assert!(
            screen.contains("All done here"),
            "{w}x{h}: transcript pushed off-screen: {screen}"
        );
        assert!(
            screen.contains(" more"),
            "{w}x{h}: elided stops not counted with a `+N more` marker: {screen}"
        );
    }
}

#[test]
fn stop_preview_lines_never_exceeds_its_budget_and_degrades_without_a_question() {
    // The budget is now the CALLER's, and it is hard: this block's height is reserved out
    // of the transcript's, so a stop that overspent by one row would take that row from the
    // log. Swept from 0, because a zero budget is reachable on a tiny pane and must return
    // nothing rather than a header nobody asked for.
    let long = stop_asking("s1", "confirm_done", &LIVE_QUESTION.repeat(50), &["a", "b"]);
    for budget in 0..12 {
        let rows = stop_preview_lines(&long, 40, budget);
        assert!(
            rows.len() <= budget,
            "budget {budget}: emitted {} rows",
            rows.len()
        );
        for r in &rows {
            assert!(r.width() <= 42, "row wider than the pane: {r:?}");
        }
    }
    // A single spare row goes to the QUESTION, not the options: "what is being asked" beats
    // "what the choices are" when only one of them fits, and `a` shows both regardless.
    let rows = stop_preview_lines(&long, 40, 2);
    assert_eq!(rows.len(), 2);
    assert!(
        !rows[1].to_string().trim_start().starts_with('1'),
        "the one spare row went to the options: {:?}",
        rows[1]
    );
    // …and with no question it is exactly the single header row it always was.
    let bare = Stop {
        question: String::new(),
        ..stop("s1", "publish", RiskClass::Hard)
    };
    let rows = stop_preview_lines(&bare, 40, 6);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].to_string().contains("publish"),
        "kind fallback: {:?}",
        rows[0]
    );
}

#[test]
fn a_synthesized_stop_renders_its_product_string_not_a_bare_kind() {
    // The park a human actually hits: the harness (not the agent) raised the stop,
    // so there is no draft text and BOTH surfaces used to show just the kind —
    // `confirm_done` on its own says nothing about what is being decided or what
    // to do about it. Asserted on both surfaces with the SAME expected text, so
    // the Stops block and the answer overlay cannot drift apart.
    for (kind, want) in SYNTHESIZED_STOP_COPY {
        // (1) The pinned Stops block in the PREVIEW.
        let dir = tempfile::tempdir().unwrap();
        let (mut app, _sp) = app_with_wake_fixture(
            dir.path(),
            Some(r#"{ "state": "blocked", "seq": 9, "status": "need a decision" }"#),
        );
        app.projects[0].stops = vec![stop_textless("s1", kind)];
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).expect("render stops");
        let screen = screen_text(&terminal);
        assert!(
            screen.contains(kind),
            "{kind}: the kind header is still the label: {screen}"
        );
        assert!(
            screen.contains(want),
            "{kind}: the Stops block shows a bare kind, not {want:?}: {screen}"
        );

        // (2) The ANSWER overlay — the moment the human decides.
        let v = view("bot", Posture::NeedsYou, vec![stop_textless("s1", kind)]);
        let app = app_with(
            vec![v],
            UiMode::Answering {
                input: Field::new(),
                choice: 0,
                scroll: 0,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).expect("render overlay");
        let screen = screen_text(&terminal);
        assert!(
            screen.contains(want),
            "{kind}: the overlay shows a bare kind, not {want:?}: {screen}"
        );
    }
}

#[test]
fn stop_product_text_prefers_the_agents_own_words_and_never_invents_them() {
    // The ONE mapping both surfaces go through, asserted directly.
    // The agent's own question always wins over any canned string…
    let asked = stop_asking("s1", "confirm_done", "Ship it?", &["yes"]);
    assert_eq!(stop_product_text(&asked), "Ship it?");
    // …a text-less SYNTHESIZED kind gets its product string…
    for (kind, want) in SYNTHESIZED_STOP_COPY {
        let bare = stop_textless("s1", kind);
        let text = stop_product_text(&bare);
        assert_eq!(
            text,
            state::synthesized_stop_text(kind).expect("a synthesized kind has a product string"),
            "{kind} must resolve through the one helper"
        );
        assert!(
            text.starts_with(want),
            "{kind}: front-loaded copy, got {text}"
        );
    }
    // …and a text-less kind the harness NEVER synthesizes keeps the bare-kind
    // fallback rather than a fabricated sentence: a `publish` stop with no question
    // does not tell us what would be published, so there is nothing true to say.
    for kind in [
        "publish",
        "merge",
        "ambiguity",
        "expert_needed",
        "worker_stuck",
    ] {
        assert!(
            stop_product_text(&stop_textless("s1", kind)).is_empty(),
            "{kind} must not gain invented copy"
        );
        assert_eq!(
            stop_preview_lines(&stop_textless("s1", kind), 40, 8).len(),
            1
        );
    }
}

#[test]
fn a_synthesized_stop_lays_out_on_a_tiny_terminal() {
    // The product string is longer than any kind name, and it renders in a block
    // whose height is RESERVED out of the log's — so sweep the sizes that shrink
    // both, in the dashboard AND in the fixed-size answer overlay.
    for (kind, _) in SYNTHESIZED_STOP_COPY {
        let v = view("bot", Posture::NeedsYou, vec![stop_textless("s1", kind)]);
        for mode in [
            UiMode::Normal,
            UiMode::Answering {
                input: "1".into(),
                choice: 0,
                scroll: 0,
            },
        ] {
            let app = app_with(vec![v.clone()], mode);
            for (w, h) in [
                (1u16, 1u16),
                (20, 5),
                (24, 8),
                (30, 10),
                (60, 20),
                (120, 30),
            ] {
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                terminal
                    .draw(|f| render(f, &app))
                    .unwrap_or_else(|e| panic!("{kind} at {w}x{h}: {e}"));
            }
        }
    }
}

#[test]
fn wrap_clamped_fits_the_budget_and_marks_elision() {
    // Breaks on whitespace…
    assert_eq!(
        wrap_clamped("one two three", 7, 3),
        vec!["one two", "three"]
    );
    // …hard-splits a word longer than the row…
    assert_eq!(wrap_clamped("abcdefgh", 4, 3), vec!["abcd", "efgh"]);
    // …and never exceeds max_lines, ellipsising what is left over.
    let rows = wrap_clamped(LIVE_QUESTION, 20, 2);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.chars().count() <= 20), "{rows:?}");
    assert!(rows[1].ends_with('…'), "elision marked: {rows:?}");
    // Degenerate inputs can't panic.
    assert!(wrap_clamped("x", 0, 3).is_empty());
    assert!(wrap_clamped("x", 10, 0).is_empty());
    assert!(wrap_clamped("", 10, 3).is_empty());
    // Multi-byte text splits on char boundaries.
    assert_eq!(wrap_clamped("ααα βββ", 3, 2), vec!["ααα", "βββ"]);
}
