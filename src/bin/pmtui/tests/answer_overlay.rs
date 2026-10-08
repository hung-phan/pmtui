//! The answer overlay: the question and numbered options above the input, the caret
//! pinned however long the question gets, the movable option cursor, and what Enter on
//! an empty field sends — or refuses to send when there are no options.

use super::*;

fn persist_answer_stop(root: &Path, question: &str, options: &[&str]) {
    let paths = ProjectPaths::for_session(root, "test");
    let mut stop = open_stop("stop-test-0", pmstate::StopKind::ConfirmDone);
    stop.question = Some(question.into());
    stop.options = options.iter().map(|option| (*option).to_string()).collect();
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.run = job::JobRun::Blocked {
        stop_ids: vec![stop.id.clone()],
        since: 1000,
    };
    ledger.open_stops = vec![stop];
    job::save(&paths, &ledger).unwrap();
}

#[test]
fn answer_overlay_shows_the_question_and_options_above_the_input() {
    // The overlay is the moment the human decides, so what they are answering has
    // to be readable without leaving the screen.
    let v = view(
        "bot",
        Posture::NeedsYou,
        vec![stop_asking(
            "s1",
            "confirm_done",
            "Confirm and close, or keep adding?",
            &["close it", "keep going"],
        )],
    );
    let app = app_with(
        vec![v],
        UiMode::Answering {
            input: "1".into(),
            choice: 0,
            scroll: 0,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render overlay");
    let screen = screen_text(&terminal);
    assert!(screen.contains("Answer"), "overlay title: {screen}");
    assert!(
        screen.contains("Confirm and close, or keep adding?"),
        "question missing from the overlay: {screen}"
    );
    // ONE PER ROW now, numbered without the `1)` run-on form the old single-line layout
    // needed. Asserted as whole rows, which is the property that matters: the previous
    // rendering could show `1) close it  2) keep g…` and pass a `contains` check while the
    // last option was unreadable.
    let rows = screen_rows_styled(&terminal);
    for want in ["1 close it", "2 keep going"] {
        assert!(
            rows.iter().any(|(t, _)| t.contains(want)),
            "option {want:?} is not on a row of its own: {screen}"
        );
    }
}

#[test]
fn the_answer_overlays_caret_is_pinned_however_long_the_question_gets() {
    // The invariant that used to be bought by CLAMPING the question to two lines, now
    // bought by pinning instead: whatever the question's length and whatever the terminal
    // size, the input row is the last row of the frame and is on screen.
    //
    // Pinning is what made the user's *"I cannot see the whole set of commands"* fixable
    // at all — the old two-line clamp existed precisely because a taller body evicted the
    // caret. Assert the caret, and the question is free to be as long as it likes.
    let v = view(
        "bot",
        Posture::NeedsYou,
        vec![stop_asking(
            "s1",
            "confirm_done",
            &LIVE_QUESTION.repeat(30),
            &["close it", "keep going"],
        )],
    );
    let typed = "y".repeat(400);
    let app = app_with(
        vec![v],
        UiMode::Answering {
            input: typed.into(),
            choice: 0,
            scroll: 0,
        },
    );
    for (w, h) in [(100u16, 30u16), (64, 10), (40, 9), (24, 8)] {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, &app))
            .unwrap_or_else(|e| panic!("render overlay {w}x{h}: {e}"));
    }
    // At a normal size the caret row is still drawn (the tail of a long answer).
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render overlay");
    let screen = screen_text(&terminal);
    assert!(screen.contains("yyyy_"), "input caret evicted: {screen}");
    // A question too long for the frame must SAY it continues. A scrolled region that
    // looks identical to a truncated one is the original bug wearing a different hat.
    assert!(
        screen.contains("more below") || screen.contains("more above"),
        "a clipped question must advertise the scroll: {screen}"
    );
}

#[test]
fn the_answer_overlay_lists_every_option_on_its_own_row_with_a_movable_cursor() {
    // THE REPORT: *"when it needs me, it is not interactable or for me to be able to see
    // the whole set of commands"*. Both halves, on the shape the user actually saw — a
    // hard `confirm_done` whose options are full sentences. They used to be concatenated
    // onto ONE truncated row, so the third was simply unreadable.
    let mut app = app_with(
        vec![confirm_done_view()],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    let draw = |app: &App| {
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        t.draw(|f| render(f, app)).expect("render");
        (screen_text(&t), t)
    };

    let (screen, _t) = draw(&app);
    // Every option, numbered, and each on a row of its own — asserted via the whole tail
    // of the longest one, which is exactly what the old single-row layout ate.
    for want in [
        "1 Close the session - the joke was delivered",
        "2 Keep going: more animal jokes (different animals / styles)",
        "3 Keep going: a specific animal I name",
    ] {
        assert!(screen.contains(want), "option missing: {want:?}\n{screen}");
    }
    // The whole question, not a clipped head.
    assert!(
        screen.contains("Close the session?"),
        "the question's tail is missing: {screen}"
    );
    // The cursor is ON option 1, and the hint teaches the interaction.
    assert!(screen.contains("\u{25b8} 1 Close"), "no cursor: {screen}");
    assert!(
        screen.contains("\u{2191}\u{2193} pick"),
        "no hint: {screen}"
    );

    // DOWN moves it, and it CLAMPS at the last option rather than wrapping — on this
    // screen a wrap would silently return you to "Close the session" while you believed
    // you were moving away from it.
    for _ in 0..5 {
        handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    let UiMode::Answering { choice, .. } = &app.mode else {
        panic!("{:?}", app.mode)
    };
    assert_eq!(*choice, 2, "Down must clamp at the last option");
    let (screen, _t) = draw(&app);
    assert!(
        screen.contains("\u{25b8} 3 Keep going: a specific animal"),
        "the cursor did not move: {screen}"
    );
    // And UP clamps at the first.
    for _ in 0..5 {
        handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    }
    let UiMode::Answering { choice, .. } = &app.mode else {
        panic!("{:?}", app.mode)
    };
    assert_eq!(*choice, 0, "Up must clamp at the first option");

    // AUTO-SCROLL, on a terminal too short to show every option at once (80x14 clips the
    // third — measured). Moving the cursor onto an option below the fold has to bring it
    // INTO view: a highlight nobody can see is worse than no highlight, because Enter is
    // about to send whatever it is sitting on.
    for _ in 0..2 {
        handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    let mut t = Terminal::new(TestBackend::new(80, 14)).unwrap();
    t.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&t);
    assert!(
        screen.contains("\u{25b8} 3 Keep going: a specific animal"),
        "the cursor moved below the fold and was not scrolled into view:\n{screen}"
    );
    // And the caret is STILL the last row — auto-scrolling must not buy visibility by
    // evicting the input.
    assert!(
        screen.contains("> _"),
        "caret evicted by auto-scroll:\n{screen}"
    );
}

#[test]
fn answer_overlay_wraps_every_long_option_without_cutting_its_tail() {
    let long_option = format!(
        "Keep the session running and complete every remaining validation step, preserve the current conversation, summarize each result for review, and only then return for confirmation {}",
        "WRAP_TAIL_VISIBLE"
    );
    let app = app_with(
        vec![view(
            "bot",
            Posture::NeedsYou,
            vec![stop_asking(
                "s1",
                "ambiguity",
                "Choose the complete next action.",
                &[long_option.as_str(), "Stop now"],
            )],
        )],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    terminal.draw(|frame| render(frame, &app)).expect("render");
    let screen = screen_text(&terminal);

    assert!(screen.contains("1 Keep the session running"), "{screen}");
    assert!(
        screen.contains("WRAP_TAIL_VISIBLE"),
        "the long option tail was cut instead of wrapped: {screen}"
    );
    assert!(screen.contains("2 Stop now"), "{screen}");
    assert!(
        screen.contains("wheel/Pg scroll"),
        "scroll hint missing: {screen}"
    );
}

#[test]
fn answer_overlay_scroll_reaches_both_ends_of_a_long_question_without_options() {
    let question = format!(
        "QUESTION_HEAD {} QUESTION_TAIL_VISIBLE",
        "middle ".repeat(120)
    );
    let mut app = app_with(
        vec![view(
            "bot",
            Posture::NeedsYou,
            vec![stop_asking("s1", "ambiguity", &question, &[])],
        )],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    terminal
        .draw(|frame| render(frame, &app))
        .expect("render top");
    assert!(screen_text(&terminal).contains("QUESTION_HEAD"));

    for _ in 0..100 {
        handle_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        terminal
            .draw(|frame| render(frame, &app))
            .expect("page down");
    }
    let bottom = screen_text(&terminal);
    assert!(bottom.contains("QUESTION_TAIL_VISIBLE"), "{bottom}");
    assert!(bottom.contains("> _"), "input was not pinned: {bottom}");

    for _ in 0..100 {
        handle_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
        terminal.draw(|frame| render(frame, &app)).expect("page up");
    }
    assert!(screen_text(&terminal).contains("QUESTION_HEAD"));
}

#[test]
fn a_wrapped_option_taller_than_the_viewport_keeps_its_choice_explicit_while_scrolling() {
    let option = format!("OPTION_HEAD {} OPTION_TAIL_VISIBLE", "detail ".repeat(120));
    let mut app = app_with(
        vec![view(
            "bot",
            Posture::NeedsYou,
            vec![stop_asking(
                "s1",
                "ambiguity",
                "Choose.",
                &[option.as_str()],
            )],
        )],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    // Option navigation requests cursor-follow even when the one choice is already selected.
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
    terminal
        .draw(|frame| render(frame, &app))
        .expect("render followed choice");
    let followed = screen_text(&terminal);
    assert!(followed.contains("▸ 1 OPTION_HEAD"), "{followed}");

    for _ in 0..100 {
        handle_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        terminal
            .draw(|frame| render(frame, &app))
            .expect("page option");
    }
    let tail = screen_text(&terminal);
    assert!(tail.contains("OPTION_TAIL_VISIBLE"), "{tail}");
    assert!(tail.contains("selected option 1"), "{tail}");
    assert!(tail.contains("> _"), "input was not pinned: {tail}");
}

#[test]
fn answer_wrapping_has_no_fixed_row_cap() {
    let text = "x".repeat(5_000);
    let rows = wrap_all(&text, 1);
    assert_eq!(rows.len(), text.len());
    assert_eq!(rows.concat(), text);
}

#[test]
fn enter_on_an_empty_field_sends_the_highlighted_option_verbatim() {
    // The "interactable" half, end to end and on disk. What lands in `answers.json` must
    // be the option TEXT the cursor was on — not an index the agent would have to map, and
    // emphatically not the old literal `"ok"`.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "test");
    persist_answer_stop(
        &root,
        "Goal 'tell me animal joke' looks satisfied",
        &[
            "Close the session - the joke was delivered",
            "Keep going: more animal jokes (different animals / styles)",
            "Keep going: a specific animal I name",
        ],
    );
    let mut app = app_with(
        vec![confirm_done_view()],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    app.registry_path = reg_path;
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // option 2
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal), "{:?}", app.mode);
    let answers =
        state::read_json::<Vec<Answer>>(&ProjectPaths::for_session(&root, "test").answers())
            .expect("answers.json written");
    assert_eq!(answers.len(), 1);
    assert_eq!(
        answers[0].answer, "Keep going: more animal jokes (different animals / styles)",
        "the highlighted option must be sent verbatim"
    );
    assert_eq!(answers[0].stop_id, "stop-test-0");
}

#[test]
fn task_inspector_answer_returns_to_the_task_view() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "test");
    persist_answer_stop(
        &root,
        "Ready for sign-off?",
        &["Close the session", "Keep working"],
    );
    let mut app = app_with(vec![confirm_done_view()], UiMode::Board);
    app.registry_path = reg_path;
    app.board_detail_open = true;

    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Answering { .. }));
    assert!(app.return_to_board_after_answer);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert!(matches!(app.mode, UiMode::Board));
    assert!(app.board_detail_open);
    assert!(!app.return_to_board_after_answer);
    let answers =
        state::read_json::<Vec<Answer>>(&ProjectPaths::for_session(&root, "test").answers())
            .expect("answers.json written");
    assert_eq!(answers[0].answer, "Close the session");
}

#[test]
fn typing_overrides_the_highlight_and_the_cursor_stops_claiming_otherwise() {
    // Free text has to stay reachable — "none of these, do X instead" is a real answer.
    // And once you have typed, the highlight must GO DARK: leaving it lit would point at
    // an outcome Enter is no longer going to produce, which on this screen means showing
    // the human one resolution while sending another.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "test");
    persist_answer_stop(
        &root,
        "Goal 'tell me animal joke' looks satisfied",
        &[
            "Close the session - the joke was delivered",
            "Keep going: more animal jokes (different animals / styles)",
            "Keep going: a specific animal I name",
        ],
    );
    let mut app = app_with(
        vec![confirm_done_view()],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    app.registry_path = reg_path;
    for c in "tell a plant joke".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&t);
    // Scoped to the OPTION rows: `\u{25b8}` is also the selected-session gutter in the
    // list behind the overlay, so a bare `contains` would always be true.
    assert!(
        !screen.contains("\u{25b8} 1 ")
            && !screen.contains("\u{25b8} 2 ")
            && !screen.contains("\u{25b8} 3 "),
        "the cursor must go dark once text is typed: {screen}"
    );
    assert!(
        screen.contains("1 Close the session"),
        "the options must still be READABLE, just not selected: {screen}"
    );
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let answers =
        state::read_json::<Vec<Answer>>(&ProjectPaths::for_session(&root, "test").answers())
            .expect("answers.json written");
    assert_eq!(
        answers[0].answer, "tell a plant joke",
        "typed text must win over the highlight"
    );
}

#[test]
fn enter_on_an_empty_field_with_no_options_refuses_instead_of_approving() {
    // THE FOOTGUN THIS REMOVES. `submit_answer` used to send the literal `"ok"` for an
    // empty field. On a `hard confirm_done` — the exact stop the user was looking at —
    // pressing `a` then a reflex Enter therefore approved a session's completion with a
    // keystroke that reads like "open the field". A decision that size has to be something
    // the human actually said.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "test");
    persist_answer_stop(&root, "Done?", &[]);
    let mut app = app_with(
        // No options at all, so there is nothing to highlight and nothing to infer.
        vec![view(
            "test",
            Posture::NeedsYou,
            vec![stop_asking("stop-test-0", "confirm_done", "Done?", &[])],
        )],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    app.registry_path = reg_path;
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        !ProjectPaths::for_session(&root, "test").answers().exists(),
        "an empty Enter must not write an answer"
    );
    assert!(
        app.status.contains("type an answer"),
        "and it must say what to do instead: {}",
        app.status
    );
    // Reopened on the SAME stop, not dropped to the dashboard: the human meant to answer,
    // and making them press `a` again to be told the same thing is a dead end.
    assert!(
        matches!(app.mode, UiMode::Answering { .. }),
        "the field must stay open: {:?}",
        app.mode
    );
}

#[test]
fn pgup_pgdn_read_a_long_question_and_stop_at_both_ends() {
    // Reading is a SEPARATE pair of keys from choosing. Conflating them on one pair is how
    // a reader ends up having silently changed their decision while scrolling.
    let mut app = app_with(
        vec![view(
            "bot",
            Posture::NeedsYou,
            vec![stop_asking(
                "s1",
                "confirm_done",
                &LIVE_QUESTION.repeat(20),
                &["a", "b"],
            )],
        )],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    // The ceiling is published by the RENDER (only it knows the granted frame), so draw
    // once before paging — exactly the contract the wake view uses.
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    t.draw(|f| render(f, &app)).expect("render");

    handle_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
    let UiMode::Answering { scroll, .. } = &app.mode else {
        panic!()
    };
    assert_eq!(*scroll, 0, "PgUp at the top must not underflow");

    for _ in 0..200 {
        handle_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        t.draw(|f| render(f, &app)).expect("render");
    }
    let UiMode::Answering { scroll, choice, .. } = &app.mode else {
        panic!()
    };
    assert!(
        *scroll <= app.scroll_max.get(),
        "PgDn ran past the end: {scroll} > {}",
        app.scroll_max.get()
    );
    assert_eq!(*choice, 0, "scrolling must never change the decision");
}
