//! `s` typing into a row's own pane: the pane it writes to and nothing else, the panes it
//! refuses — a dialog, a corpse, an attached client, a scrollback, an open stop — and the
//! verdict each refusal leads with.

use super::*;

#[test]
fn s_sends_the_typed_text_to_the_rows_own_pane_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, session, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    send_via_keys(&mut app, "check the tests first");

    assert_eq!(
        pane.sends(),
        vec![(session, "check the tests first".to_string())],
        "the text did not reach exactly the selected row's pane"
    );
    assert!(matches!(app.mode, UiMode::Normal), "the field stayed open");
    assert_eq!(app.status, "sent \u{2192} bot");
}

#[test]
fn task_inspector_message_sends_without_leaving_task_view() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, session, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    app.mode = UiMode::Board;
    app.board_detail_open = true;

    send_via_keys(&mut app, "continue from the task inspector");

    assert_eq!(
        pane.sends(),
        vec![(session, "continue from the task inspector".to_string())]
    );
    assert!(matches!(app.mode, UiMode::Board));
    assert!(app.board_detail_open);
    assert!(!app.return_to_board_after_send);
}

#[test]
fn task_message_editor_cancel_and_failure_return_to_tasks_with_the_draft() {
    for fail in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, _, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
        app.mode = UiMode::Board;
        app.board_detail_open = true;
        handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        handle_paste(&mut app, "task draft");
        handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        handle_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert!(matches!(app.mode, UiMode::Board));
        assert!(app.return_to_board_after_send);

        if fail {
            drain_pending_send(&mut app, |_| anyhow::bail!("editor failed"));
        } else {
            drain_pending_send(&mut app, |_| Ok(None));
        }

        assert!(matches!(app.mode, UiMode::Board));
        assert!(!app.return_to_board_after_send);
        assert_eq!(app.message_drafts["bot"].text(), "task draft");
    }
}

#[test]
fn parking_an_empty_task_message_returns_to_tasks_without_a_draft() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.park_send_draft();
    app.mode = UiMode::Sending {
        target: SendTarget {
            id: "bot".into(),
            root: PathBuf::from("/tmp"),
            session: "pm-bot".into(),
            agent_loop: true,
            driven: false,
            in_chat: false,
        },
        input: msg_buf(""),
    };
    app.return_to_board_after_send = true;

    app.park_send_draft();

    assert!(matches!(app.mode, UiMode::Board));
    assert!(app.message_drafts.is_empty());
    assert_eq!(app.status, "message cancelled");
}

#[test]
fn s_refuses_a_pane_showing_a_dialog_and_sends_nothing() {
    // THE DANGEROUS CASE, and the one no `FakeDriver` test would have caught by
    // accident. `send_keys` always finishes with a separate `Enter`, so with a
    // numbered permission dialog on screen the typed characters go to a select
    // widget's accelerators and the Enter CONFIRMS the pre-highlighted option —
    // granting the write, and destroying the human's message while returning `Ok`.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, _, _) = send_app(dir.path(), CLAUDE_DIALOG_PANE);

    send_via_keys(&mut app, "use option 2, and skip the lint");

    assert!(
        pane.sends().is_empty(),
        "bytes reached a dialog: {:?}",
        pane.sends()
    );
    assert!(app.status.contains("dialog up"), "status: {:?}", app.status);
    // AND THE TEXT SURVIVES. A refusal that eats what was typed is the same
    // "it said nothing and nothing happened" failure as a dead key.
    match &app.mode {
        UiMode::Sending { input, .. } => {
            assert_eq!(input.text(), "use option 2, and skip the lint")
        }
        other => panic!("the field closed and the text was lost: {other:?}"),
    }
}

#[test]
fn s_refuses_a_codex_approval_dialog() {
    // codex marks its selected choice with the same lead as its composer, and offers
    // a bare `y` accelerator — so almost any sentence beginning "yes" would approve.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, _, _) = send_app(dir.path(), CODEX_DIALOG_PANE);
    send_via_keys(&mut app, "yes, but rebase first");
    assert!(pane.sends().is_empty(), "bytes reached a codex dialog");
}

#[test]
fn s_refuses_a_pane_with_no_prompt_on_screen() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, _, _) = send_app(dir.path(), "$ some shell output\n");
    send_via_keys(&mut app, "hello");
    assert!(pane.sends().is_empty());
    assert!(app.status.contains("no prompt"), "{:?}", app.status);
}

#[test]
fn s_refuses_a_corpse_an_attached_pane_and_a_scrollback() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session = session_name("bot", &root);
    // A `remain-on-exit` corpse still shows the prompt and reads Idle, which is why
    // the dead-pane probe exists at all.
    for (label, pane, want) in [
        (
            "corpse",
            FakePane::sendable(&session, IDLE_CLAUDE_PANE).with(|i| i.pane_dead = true),
            "agent exited",
        ),
        (
            "attached",
            FakePane::sendable(&session, IDLE_CLAUDE_PANE).with(|i| i.attached = true),
            "attached there",
        ),
        (
            "copy-mode",
            FakePane::sendable(&session, IDLE_CLAUDE_PANE).with(|i| i.in_mode = true),
            "scrolled back",
        ),
        (
            "capture failed",
            FakePane::capture_fails(&session),
            "can't read the pane",
        ),
    ] {
        let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
        app.registry_path = reg_path.clone();
        app.refresh();
        send_via_keys(&mut app, "hello");
        assert!(pane.sends().is_empty(), "{label}: it sent anyway");
        assert!(
            app.status.contains(want),
            "{label}: expected {want:?}, got {:?}",
            app.status
        );
    }
}

#[test]
fn composed_message_refuses_if_a_stop_opens_before_submission() {
    // `s` routes to Answer when the stop already exists. This covers the remaining
    // race: a stop can open after the Message field was composed.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Sending { .. }));
    app.projects[0].stops = vec![stop("s1", "confirm_done", RiskClass::Hard)];

    for c in "ordinary message".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(pane.sends().is_empty(), "it sent into an open stop");
    assert!(app.status.contains("stop open"), "{:?}", app.status);
}

#[test]
fn stop_guard_is_keyed_to_the_composer_target_not_the_current_selection() {
    let dir = tempfile::tempdir().unwrap();
    let target = SendTarget {
        id: "bot".into(),
        root: dir.path().to_path_buf(),
        session: "pm-bot".into(),
        agent_loop: true,
        driven: true,
        in_chat: false,
    };
    let mut bot = agent_loop_view("bot");
    bot.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    let mut other = agent_loop_view("other");
    other.stops.clear();
    let mut app = app_with(vec![other, bot], UiMode::Normal);
    app.selected = 0;

    assert_eq!(
        app.plan_send(&target, "answer elsewhere"),
        SendPlan::Refuse("stop open — cancel, then press s to answer it".into())
    );
}

#[test]
fn s_reports_a_send_failure_verbatim_and_keeps_the_text() {
    // Every keybar status used to be drawn green, which paints a failure as a
    // success; and losing the message on a failed send is unforgivable.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session = session_name("bot", &root);
    let pane = FakePane::sendable(&session, IDLE_CLAUDE_PANE).with(|i| i.fail_send = true);
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
    app.registry_path = reg_path;
    app.refresh();
    send_via_keys(&mut app, "hello");
    assert!(app.status.starts_with("send failed:"), "{:?}", app.status);
    assert!(app.status.contains("no such session"), "{:?}", app.status);
    assert!(matches!(app.mode, UiMode::Sending { .. }), "text was lost");
}

#[test]
fn s_never_interleaves_with_an_input_already_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let paths = ProjectPaths::for_session(&root, "bot");
    let _held = lease::try_acquire(&paths.input_lock())
        .unwrap()
        .expect("input lock is free");
    let session = session_name("bot", &root);
    let pane = FakePane::sendable(&session, IDLE_CLAUDE_PANE);
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
    app.registry_path = reg_path;
    app.refresh();

    send_via_keys(&mut app, "hello");
    assert!(app.status.contains("input") || app.status.contains("delivered"));
    assert!(pane.sends().is_empty(), "bytes crossed a held input lock");
    assert!(matches!(app.mode, UiMode::Sending { .. }), "text was lost");
}

#[test]
fn s_refuses_an_empty_send_before_touching_tmux() {
    // `send-keys -l -- ""` plus `Enter` submits an EMPTY turn to a live agent.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    let probes_before = pane.captures().len();
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(pane.sends().is_empty());
    assert_eq!(
        pane.captures().len(),
        probes_before,
        "it probed the pane for a message that does not exist"
    );
    assert!(app.status.contains("nothing typed"), "{:?}", app.status);
}

#[test]
fn escape_parks_a_per_session_draft_and_reopen_restores_text_and_caret() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    handle_paste(&mut app, "review this change");
    handle_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    let draft = app.message_drafts.get("bot").expect("draft parked").clone();
    assert_eq!(draft.text(), "review this change");
    // The caret is `(row, column)` now: the composer is multi-line.
    assert_eq!(draft.cursor(), (0, "review this chang".chars().count()));

    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    let UiMode::Sending { input, .. } = &app.mode else {
        panic!("composer did not reopen");
    };
    assert_eq!(
        (input.text(), input.cursor()),
        (draft.text(), draft.cursor())
    );
}

#[test]
fn drafts_are_isolated_by_session() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let other_root = dir.path().join("other-root");
    std::fs::create_dir_all(&other_root).unwrap();
    let mut registry = Registry::load(&reg_path).unwrap();
    let mut other = registry.projects[0].clone();
    other.id = "other".into();
    other.root = other_root;
    registry.projects.push(other);
    registry.save(&reg_path).unwrap();

    let mut app = app_with(
        vec![agent_loop_view("bot"), agent_loop_view("other")],
        UiMode::Normal,
    );
    app.registry_path = reg_path;
    app.selected = 0;
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    handle_paste(&mut app, "first draft");
    handle_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    app.selected = 1;
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    handle_paste(&mut app, "second draft");
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    assert_eq!(
        app.message_drafts.get("bot").map(Composer::text),
        Some("first draft".to_string())
    );
    assert_eq!(
        app.message_drafts.get("other").map(Composer::text),
        Some("second draft".to_string())
    );
}

#[test]
fn editor_cancel_restores_the_session_draft() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    handle_paste(&mut app, "keep this draft");
    handle_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    let expected_caret = "keep this draf".chars().count();
    // `^X^E`, bash's `edit-and-execute-command`: bare `^E` is end-of-line in the composer now.
    handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    handle_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    drain_pending_send(&mut app, |_| Ok(None));

    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(
        app.message_drafts.get("bot").map(Composer::text),
        Some("keep this draft".to_string())
    );
    assert_eq!(
        app.message_drafts.get("bot").map(Composer::cursor),
        Some((0, expected_caret))
    );
}

#[test]
fn successful_send_clears_a_restored_draft() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    handle_paste(&mut app, "send after reopen");
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(pane.sends().len(), 1);
    assert!(!app.message_drafts.contains_key("bot"));
}

#[test]
fn active_composer_is_titled_with_the_label_its_row_and_shelf_show() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    Registry::update(&app.registry_path, |registry| {
        registry.projects[0].display_name = Some("Release work".into());
    })
    .unwrap();
    app.refresh();
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let dormant = screen_text(&terminal);
    assert!(dormant.contains("Message \u{b7} Release work"), "{dormant}");

    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let active = screen_text(&terminal);
    assert!(active.contains("Message \u{b7} Release work"), "{active}");
    assert!(!active.contains("Message \u{b7} bot"), "{active}");
    let UiMode::Sending { target, .. } = &app.mode else {
        panic!("s did not open the composer");
    };
    assert_eq!(target.id, "bot", "delivery still routes by stable id");
}

#[test]
fn composer_is_inline_and_keeps_the_dashboard_visible() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).expect("render");
    let text = screen_text(&terminal);
    assert!(text.contains("SESSIONS"));
    assert!(text.contains("Message"));
    assert!(app.panes.get().detail.height > 0);
}

#[test]
fn inline_composer_stays_inside_narrow_and_minimum_terminals() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    for (width, height) in [(24, 8), (49, 12), (80, 16)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, &app))
            .unwrap_or_else(|error| panic!("render failed at {width}x{height}: {error}"));
        assert_eq!(terminal.backend().buffer().area.width, width);
        assert_eq!(terminal.backend().buffer().area.height, height);
        assert!(
            screen_text(&terminal).contains("Message"),
            "active composer hidden at {width}x{height}"
        );
    }
}

#[test]
fn send_entrypoints_are_noops_outside_the_send_field() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    let before = app.status.clone();

    app.submit_send();
    app.escalate_send();

    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(app.status, before);
    assert!(app.pending_send.is_none());
}

#[test]
fn send_refuses_during_the_chat_attach_intent_window() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    let paths = ProjectPaths::for_session(&root, "bot");
    let now = SystemClock.now();
    chat_lock::mark(&paths, std::process::id(), "pm-bot", "pm-test", now).unwrap();
    let target = SendTarget {
        id: "bot".into(),
        root,
        session: "pm-bot".into(),
        agent_loop: true,
        driven: true,
        in_chat: false,
    };
    let mut app = app_with(vec![], UiMode::Normal);

    let plan = app.plan_send(&target, "hello");

    assert_eq!(
        plan,
        SendPlan::Refuse("a chat is opening on this session — try again".into())
    );
}

#[test]
fn final_send_recheck_rejects_control_only_dead_attached_and_changed_panes() {
    let make_target = |root: &Path| SendTarget {
        id: "bot".into(),
        root: root.to_path_buf(),
        session: "pm-bot".into(),
        agent_loop: true,
        driven: true,
        in_chat: false,
    };

    let dir = tempfile::tempdir().unwrap();
    let mut empty = app_with_driver(vec![], UiMode::Normal, Box::new(FakePane::default()));
    let error = empty
        .deliver(&make_target(dir.path()), "\u{1b}\r")
        .unwrap_err();
    assert!(error.to_string().contains("nothing left"));

    let dir = tempfile::tempdir().unwrap();
    let mut dead = app_with_driver(vec![], UiMode::Normal, Box::new(FakePane::default()));
    let error = dead.deliver(&make_target(dir.path()), "hello").unwrap_err();
    assert!(error.to_string().contains("session ended"));

    let dir = tempfile::tempdir().unwrap();
    let attached_pane =
        FakePane::sendable("pm-bot", IDLE_CLAUDE_PANE).with(|inner| inner.attached = true);
    let mut attached = app_with_driver(vec![], UiMode::Normal, Box::new(attached_pane.clone()));
    let error = attached
        .deliver(&make_target(dir.path()), "hello")
        .unwrap_err();
    assert!(error.to_string().contains("client attached"));
    assert!(attached_pane.sends().is_empty());

    let dir = tempfile::tempdir().unwrap();
    let changed_pane = FakePane::sendable("pm-bot", "$ no composer\n");
    let mut changed = app_with_driver(vec![], UiMode::Normal, Box::new(changed_pane.clone()));
    let error = changed
        .deliver(&make_target(dir.path()), "hello")
        .unwrap_err();
    assert!(error.to_string().contains("pane changed"));
    assert!(changed_pane.sends().is_empty());
}

#[test]
fn s_writes_no_ledger_and_no_answers_json() {
    // THE SINGLE-WRITER GUARD. pmd owns the ledger; `pending_context` is the only
    // carrier for delivered-later text and it lives there. `s` must add no fifth file
    // to pmtui's write set either.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, _, root) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    let paths = ProjectPaths::for_session(&root, "bot");
    let before = std::fs::read(paths.pmstate()).ok();

    send_via_keys(&mut app, "one message");

    assert_eq!(pane.sends().len(), 1, "it did not actually send");
    assert_eq!(
        std::fs::read(paths.pmstate()).ok(),
        before,
        "`s` rewrote the agent-loop ledger"
    );
    assert!(
        !paths.answers().exists(),
        "`s` wrote answers.json — the text would be delivered twice"
    );
    // And the interlock it took is RELEASED on the way out, on every path.
    assert!(
        !paths.chat_lock().exists(),
        "the chat marker leaked; pmd would defer this row"
    );
}

#[test]
fn s_refuses_an_empty_list() {
    let app_none = &mut app_with(vec![], UiMode::Normal);
    handle_key(app_none, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(app_none.status.contains("nothing is selected"));
    assert!(matches!(app_none.mode, UiMode::Normal));
}

#[test]
fn every_send_refusal_leads_with_its_verdict() {
    // `keybar_line` reserves at most a THIRD of the bar for a status and truncates
    // the TAIL, so at 80 columns only the first ~26 characters are guaranteed to be
    // read. The verdict has to be inside them.
    let t = send_target(true);
    let cases: Vec<(SendPlan, &str)> = vec![
        (send_precheck("").unwrap(), "nothing typed"),
        (
            send_precheck(&"x".repeat(SEND_MAX_BYTES + 1)).unwrap(),
            "too long",
        ),
        (
            send_decision(
                &t,
                &SendProbe {
                    pane_dead: true,
                    ..Default::default()
                },
            ),
            "agent exited",
        ),
        (
            send_decision(
                &t,
                &SendProbe {
                    alive: Some(false),
                    ..Default::default()
                },
            ),
            "agent not up yet",
        ),
        (
            send_decision(
                &t,
                &SendProbe {
                    alive: Some(true),
                    attached: true,
                    ..Default::default()
                },
            ),
            "attached there",
        ),
        (
            send_decision(
                &t,
                &SendProbe {
                    alive: Some(true),
                    in_mode: true,
                    ..Default::default()
                },
            ),
            "scrolled back",
        ),
        (
            send_decision(
                &t,
                &SendProbe {
                    alive: Some(true),
                    ..Default::default()
                },
            ),
            "can't read the pane",
        ),
        (
            send_decision(
                &t,
                &SendProbe {
                    alive: Some(true),
                    capture: Some(CLAUDE_DIALOG_PANE.into()),
                    ..Default::default()
                },
            ),
            "dialog up",
        ),
        (
            send_decision(
                &t,
                &SendProbe {
                    alive: Some(true),
                    capture: Some("$ nothing here\n".into()),
                    ..Default::default()
                },
            ),
            "no prompt on screen",
        ),
    ];
    for (plan, verdict) in cases {
        let SendPlan::Refuse(msg) = plan else {
            panic!("{verdict}: expected a refusal, got {plan:?}");
        };
        let head: String = msg.chars().take(26).collect();
        assert!(
            head.contains(verdict),
            "{verdict:?} is not in the first 26 columns of {msg:?}"
        );
        assert!(msg.chars().count() <= 60, "status too long: {msg:?}");
    }
}

#[test]
fn a_standard_row_is_told_enter_opens_one_and_an_autopilot_row_that_pmd_will() {
    // The same fact, said two ways, because the way FORWARD differs: with autopilot
    // off nothing is going to start an agent for you.
    let probe = SendProbe {
        alive: Some(false),
        ..Default::default()
    };
    assert_eq!(
        send_decision(&send_target(false), &probe),
        SendPlan::Refuse("no agent running \u{2014} press Enter to open one".into())
    );
    assert_eq!(
        send_decision(&send_target(true), &probe),
        SendPlan::Refuse("agent not up yet \u{2014} pmd starts it".into())
    );
}

#[test]
fn a_working_pane_is_sent_to_and_the_receipt_says_so() {
    // Refusing here would reproduce "it did nothing": the composer accepts the text
    // and it becomes the agent's next turn. The send only DELAYS pmd's next nudge.
    let busy = "\u{273b} Quantumizing\u{2026} (3s \u{b7} \u{2193} 5 tokens)\n\u{276f}  \n";
    assert_eq!(
        send_decision(
            &send_target(true),
            &SendProbe {
                alive: Some(true),
                capture: Some(busy.into()),
                ..Default::default()
            }
        ),
        SendPlan::Send("sent \u{2192} bot (it was working)".into())
    );
}

// --- M29: the overlay frame -------------------------------------------------

#[test]
fn s_types_into_the_chat_pane_when_that_is_where_the_conversation_lives() {
    // User: *"when i try to send, it displays this 'a chat holds this session — type it there' but
    // the chat is idle"*.
    //
    // `s` aimed at the daemon's `pmloop-` pane, but on a Standard row Enter creates the conversation
    // in a `pmchat-` pane, and that pane KEEPS RUNNING after Ctrl+q — the survive/re-attach model.
    // `chat_lock::is_active` defers on any live chat session with no age cap, so `s` refused forever
    // and told the human to type in a pane nobody was sitting in. The interlock was right that two
    // writers on one conversation is a hazard; it was aiming at the wrong pane.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let chat = session_name("bot", &root);
    // ONLY the chat pane is alive — exactly the state a Ctrl+q detach leaves behind.
    let pane = FakePane::sendable(&chat, IDLE_CLAUDE_PANE);
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
    app.registry_path = reg_path;
    app.refresh();

    send_via_keys(&mut app, "keep going, skip the lint");

    assert_eq!(
        pane.sends(),
        vec![(chat, "keep going, skip the lint".to_string())],
        "the text must reach the pane that HOLDS the conversation, not the dead loop pane: {}",
        app.status
    );
    assert!(matches!(app.mode, UiMode::Normal), "the field stayed open");
    assert!(
        !app.status.contains("type it there"),
        "it must not point at a pane nobody is sitting in: {}",
        app.status
    );

    // AND THE CHAT'S OWN MARKER SURVIVES. `deliver` skips the interlock on this path precisely
    // because releasing it would clear the real chat's `chat.json` — which would unpark the poll
    // underneath a live REPL and put two writers on one conversation, the very thing the interlock
    // exists to prevent.
    let sp = ProjectPaths::for_session(&root, "bot");
    assert!(
        !sp.chat_lock().exists(),
        "s must not leave a chat marker of its own behind"
    );
}

/// THE DORMANT SHELF SAYS A DRAFT IS WAITING, on one line, with its newlines flattened to spaces.
///
/// A multi-line draft cannot be previewed in a one-row shelf, and the shelf's job is only to say that
/// reopening `s` has something in it. Nothing rendered this arm before — every draft test asserted
/// `message_drafts` rather than the frame — which the coverage gate caught once the composer's old
/// hand-rolled wrapping stopped diluting the file.
#[test]
fn the_dormant_shelf_previews_a_parked_draft_on_one_line() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

    // With no draft the shelf offers the key instead.
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("press s or click to message this session"),
        "no draft, so the shelf should offer the key: {screen}"
    );

    app.message_drafts
        .insert("bot".into(), msg_buf("first line\nsecond line"));
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("draft \u{b7} first line second line"),
        "the shelf must preview the parked draft, flattened: {screen}"
    );

    // A whitespace-only draft is not a draft: the shelf goes back to offering the key rather than
    // promising something the composer would open empty.
    app.message_drafts.insert("bot".into(), msg_buf("   \n\t "));
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("press s or click to message this session"),
        "a blank draft must not be advertised: {screen}"
    );
}
