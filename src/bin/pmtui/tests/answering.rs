//! Answering a stop from the dashboard: the session state `refresh` reads off disk, the
//! tier gate on the overlay, and the per-session `answers.json` a submit writes.

use super::*;

fn blocked_answer_app() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session_paths = ProjectPaths::for_session(&root, "bot");
    set_tier(&session_paths, Tier::Autopilot);
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    ledger.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    ledger.open_stops = vec![open_stop("stop-1", pmstate::StopKind::Ambiguity)];
    job::save(&session_paths, &ledger).unwrap();
    (dir, loop_app(&reg_path))
}

#[test]
fn refresh_reads_agent_loop_session_state() {
    // refresh() must read a Blocked agent-loop session from its per-session
    // ledger, NOT the (absent) root stops.json — so its stops are non-empty, its
    // posture reflects the ledger, and its tier comes from the per-session config.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session_paths = ProjectPaths::for_session(&root, "bot");
    // Re-seed at a distinctive tier, then park Blocked on one hard stop.
    seed_agent_loop(
        &session_paths,
        Tier::Autopilot,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        1000,
    )
    .unwrap();
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    l.open_stops = vec![open_stop("stop-1", pmstate::StopKind::Publish)];
    job::save(&session_paths, &l).unwrap();

    let app = loop_app(&reg_path);
    let v = app
        .projects
        .iter()
        .find(|v| v.id == "bot")
        .expect("bot view");
    assert!(
        !v.stops.is_empty(),
        "stops read from the ledger, not the (absent) root stops.json"
    );
    assert_eq!(v.stops[0].id, "stop-1");
    assert_eq!(
        v.posture,
        Posture::NeedsYou,
        "a Blocked ledger surfaces NeedsYou"
    );
    assert_eq!(
        v.tier,
        Some(Tier::Autopilot),
        "tier comes from the per-session config"
    );
}

/// THE ARM ORDER IN `begin_answer`, which is the whole point of the reorder.
///
/// `view::read_agent_loop` reports `NeedsYou` on an autopilot-OFF row whose worker
/// wrote a blocked marker, while deliberately leaving `stops` EMPTY (pmd skips the
/// whole tick for such a row, so nothing is answerable — only the glyph and the header
/// tally become honest). That combination — needs-you AND no stops — was previously
/// unreachable, and with the emptiness arm first it answered `a` with "no open stop to
/// answer" on a row visibly displaying "needs you". This pins the honest message.
#[test]
fn a_on_an_autopilot_off_row_that_needs_you_explains_itself_not_no_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session_paths = ProjectPaths::for_session(&root, "bot");
    // Autopilot OFF is what makes an answer undeliverable…
    set_tier(&session_paths, Tier::Standard);
    // …and the ledger is left Idle with NO open stops, exactly the shape `view.rs`
    // produces for a blocked marker on an OFF row.
    let l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    job::save(&session_paths, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.begin_answer();

    assert!(
        matches!(app.mode, UiMode::Normal),
        "an undeliverable answer must not open the overlay"
    );
    assert!(
        app.status.contains("autopilot off"),
        "the refusal must name the REASON (autopilot is off), not claim there is \
         nothing to answer — the row itself says `needs you`: {}",
        app.status
    );
    assert!(
        !app.status.contains("no question"),
        "the emptiness arm must not win: {}",
        app.status
    );
}

#[test]
fn begin_answer_with_nothing_selected_names_the_live_answer_key() {
    let mut app = app_with(vec![], UiMode::Normal);
    app.begin_answer();
    assert_eq!(
        app.status,
        "s answers a session's question (nothing is selected)"
    );
}

#[test]
fn begin_answer_opens_overlay_for_agent_loop_with_open_stop() {
    // `a` on a Blocked agent-loop session opens the answer overlay (its stops now
    // flow from the ledger), NOT the "no question to answer" refusal.
    //
    // AUTOPILOT, not the fixture's default Standard: pmd only delivers an answer for
    // a row it drives, so answering is Autopilot behaviour now (`begin_answer`'s
    // deliverability gate). On Standard this would take the refusal branch — see
    // `begin_answer_refuses_when_autopilot_is_off`.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session_paths = ProjectPaths::for_session(&root, "bot");
    set_tier(&session_paths, Tier::Autopilot);
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    l.open_stops = vec![open_stop("stop-1", pmstate::StopKind::Ambiguity)];
    job::save(&session_paths, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.begin_answer();
    assert!(
        matches!(app.mode, UiMode::Answering { .. }),
        "an open ledger stop opens the overlay (status was {:?})",
        app.status
    );
}

#[test]
fn s_uses_the_same_answer_surface_in_session_and_task_detail() {
    let (_dir, mut session) = blocked_answer_app();
    assert_eq!(
        normal_chips(&session)
            .into_iter()
            .filter(|chip| matches!(chip.key, "s" | "a"))
            .map(|chip| chip.key)
            .collect::<Vec<_>>(),
        vec!["s"]
    );

    handle_key(&mut session, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(matches!(session.mode, UiMode::Answering { .. }));
    assert!(!session.return_to_board_after_answer);

    let (_dir, mut task) = blocked_answer_app();
    task.mode = UiMode::Board;
    task.board_detail_open = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| render(frame, &task)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("Message \u{b7} bot"), "{screen}");
    assert!(
        screen.contains("press s or click to answer this decision"),
        "{screen}"
    );
    assert_eq!(
        task.key_hits
            .borrow()
            .iter()
            .filter(|hit| matches!(hit.code, KeyCode::Char('s' | 'a')))
            .map(|hit| hit.code)
            .collect::<Vec<_>>(),
        vec![KeyCode::Char('s')]
    );

    let shelf = task.composer_hit.get();
    let mut handled = false;
    handle_event(
        &mut task,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: shelf.x + 1,
            row: shelf.y + 1,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(handled);
    assert!(matches!(task.mode, UiMode::Answering { .. }));
    assert!(task.return_to_board_after_answer);
    assert!(!task.return_to_board_after_send);
}

#[test]
fn an_undriven_rows_open_stop_opens_the_composer_rather_than_naming_m() {
    // Autopilot turned off while a stop is open. The ledger still carries the stop, and the preview
    // still hides its block (nothing is deliverable there, and the agent's question is on screen in
    // its own terminal) — but `s` must not DEAD-END. It used to refuse and tell the human to turn
    // autopilot back on first (user: *"when my session is on autopilot or off-autopilot and display
    // needs you, it prompt me to enable autopilot for s to work. this is not correct"*), which asked
    // them to change the session's autonomy so an answer could go through a file only a driven row
    // reads. This row is an ordinary Message row: `s` types into the session, which is where that
    // agent is waiting.
    let (dir, mut app) = blocked_answer_app();
    set_tier(
        &ProjectPaths::for_session(dir.path().join("bot"), "bot"),
        Tier::Standard,
    );
    app.refresh();
    assert!(
        !app.projects[0].stops.is_empty(),
        "the ledger stop survives"
    );
    assert_eq!(message_route(&app.projects[0]), MessageRoute::Message);
    // The chip is PUBLISHED, and keeps its plain `Send` label rather than relabelling to `Answer` —
    // the honest name for a key that opens the composer.
    let s_chip = normal_chips(&app)
        .into_iter()
        .find(|chip| chip.key == "s")
        .map(|chip| chip.label);
    assert_eq!(
        s_chip,
        Some("Send"),
        "{:?}",
        normal_chips(&app)
            .iter()
            .map(|chip| (chip.key, chip.label))
            .collect::<Vec<_>>()
    );

    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("press s or click to message this session"),
        "{screen}"
    );
    assert!(
        !screen.contains("autopilot off \u{b7} press m"),
        "no surface tells a human to change autonomy before `s` works: {screen}"
    );
    assert!(!screen.contains("press s or click to answer"), "{screen}");

    // A shelf click goes the same way the key does, and lands in the composer.
    let shelf = app.composer_hit.get();
    let mut handled = false;
    handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: shelf.x + 1,
            row: shelf.y + 1,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(handled);
    assert!(
        matches!(app.mode, UiMode::Sending { .. }),
        "got {:?} (status {:?})",
        app.mode,
        app.status
    );
}

#[test]
fn begin_answer_refuses_when_autopilot_is_off() {
    // `a` must not accept an answer nothing will deliver. With autopilot OFF pmd does
    // not drive this row (`daemon::pmd_drives_row`), so `JobScheduler::on_blocked` —
    // the ONLY reader of `answers.json` — never runs. Writing the answer anyway would
    // report "answered stop-1" and leave the agent waiting, which is precisely the
    // "it says it worked and nothing happens" failure this milestone removes.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // seeds Standard
    let session_paths = ProjectPaths::for_session(&root, "bot");
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    l.open_stops = vec![open_stop("stop-1", pmstate::StopKind::Ambiguity)];
    job::save(&session_paths, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.begin_answer();
    assert!(
        matches!(app.mode, UiMode::Normal),
        "the overlay must NOT open on an undeliverable row, got {:?}",
        app.mode
    );
    assert!(
        app.status.contains("autopilot off") && app.status.contains("press m"),
        "the refusal must name the reason AND the way that works — `m`, because turning the dial \
         back on is what makes an answer deliverable and what clears the stop (only a nudge clears \
         `open_stops`, so the Enter this used to name settles the question in conversation and \
         leaves the row still reading \"needs you\"): {}",
        app.status
    );
    // Nothing was written: no answer file, so nothing to strand.
    assert!(
        !session_paths.answers().exists(),
        "a refused answer must not reach answers.json"
    );

    // Flip autopilot ON and the SAME key works — the gate is a live read of the
    // tier, not a property of the row.
    set_tier(&session_paths, Tier::Autopilot);
    app.refresh();
    app.begin_answer();
    assert!(
        matches!(app.mode, UiMode::Answering { .. }),
        "turning autopilot on makes `a` answerable again (status was {:?})",
        app.status
    );
}

#[test]
fn answer_reaches_the_agent_is_tier_gated_only_for_agent_loop_rows() {
    // The pure predicate behind the `a` gate. An agent-loop row is answerable only
    // under Autopilot; a native `Mode::Auto` row is driven at EVERY tier and stays
    // answerable (gating it would break the phase machine's default tier); an
    // interactive row is never pmd's and never carries stops.
    assert!(answer_reaches_the_agent(
        Mode::AgentLoop,
        Some(Tier::Autopilot)
    ));
    assert!(!answer_reaches_the_agent(
        Mode::AgentLoop,
        Some(Tier::Standard)
    ));
    // An unreadable tier CLOSES `a`, following the daemon: driving is opt-in, so nothing
    // is reading `answers.json` for that row and an accepted answer would be a promise
    // nobody keeps. This assertion is the reverse of what it was, and it flipped for free
    // — `answer_reaches_the_agent` delegates instead of re-deriving, which is the whole
    // reason that delegation is there.
    assert!(!answer_reaches_the_agent(Mode::AgentLoop, None));
}

#[test]
fn submit_answer_writes_to_per_session_answers_for_agent_loop() {
    // Submitting an answer for a Blocked agent-loop session must append to the
    // PER-SESSION answers.json (where `JobScheduler::on_blocked` reads), carrying
    // the ledger's open-stop id — and must NOT touch the shared root answers.json.
    //
    // AUTOPILOT: this test pokes `UiMode::Answering` directly, so it would still pass
    // on a Standard fixture — but only because it bypasses `begin_answer`'s
    // deliverability gate, i.e. it would be asserting a state a human can no longer
    // reach. Seed the tier that actually gets here.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session_paths = ProjectPaths::for_session(&root, "bot");
    set_tier(&session_paths, Tier::Autopilot);
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    l.open_stops = vec![open_stop("stop-1", pmstate::StopKind::Ambiguity)];
    job::save(&session_paths, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.mode = UiMode::Answering {
        input: "use channel b".into(),
        choice: 0,
        scroll: 0,
    };
    app.submit_answer();

    let answers: Vec<Answer> = state::read_json(&session_paths.answers()).unwrap();
    assert_eq!(answers.len(), 1, "one answer appended to the session inbox");
    assert_eq!(
        answers[0].stop_id, "stop-1",
        "the ledger's open stop id is preserved end-to-end"
    );
    assert_eq!(answers[0].answered_by, "user");
    assert!(answers[0].answered_at > 0, "stamped with now");
    assert!(
        !ProjectPaths::new(&root).answers().exists(),
        "the shared root answers.json must not be written"
    );
}

#[test]
fn answer_overlay_never_retargets_input_to_a_replacement_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let paths = ProjectPaths::for_session(&root, "bot");
    set_tier(&paths, Tier::Autopilot);
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    ledger.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-a".into()],
        since: 1000,
    };
    ledger.open_stops = vec![open_stop("stop-a", pmstate::StopKind::Ambiguity)];
    job::save(&paths, &ledger).unwrap();
    let mut app = loop_app(&reg_path);
    app.begin_answer();
    assert_eq!(app.answering_stop_id.as_deref(), Some("stop-a"));
    app.submit_answer();
    assert!(matches!(app.mode, UiMode::Answering { .. }));
    assert_eq!(
        app.answering_stop_id.as_deref(),
        Some("stop-a"),
        "an empty submission that reopens the field keeps its stop binding"
    );

    ledger.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-b".into()],
        since: 1001,
    };
    ledger.open_stops = vec![open_stop("stop-b", pmstate::StopKind::Ambiguity)];
    job::save(&paths, &ledger).unwrap();
    app.refresh();
    app.submit_answer();

    assert!(
        app.status.contains("already answered"),
        "stale overlay must refuse instead of answering stop-b: {}",
        app.status
    );
    assert!(!paths.answers().exists());
}
