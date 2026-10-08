//! The leftover row a failed fork can keep: it has no conversation, so every start, Message and
//! Autopilot flip refuses it with one message, and the chrome offers only the delete it accepts.

use super::*;

/// Append the row an older build left behind when a fork failed after staging.
fn leave_incomplete_fork(fixture: &mut ForkFixture) {
    Registry::update(&fixture.registry, |registry| {
        let mut leftover = registry.projects[0].clone();
        leftover.id = "bot-fork".into();
        leftover.enabled = false;
        leftover.forked_from = Some("bot".into());
        leftover.conversation_id = None;
        registry.projects.push(leftover);
    })
    .unwrap();
    let child = ProjectPaths::for_session(&fixture.root, "bot-fork");
    seed_agent_loop(
        &child,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        None,
        1000,
    )
    .unwrap();
    // No terminal is up for either row, so a start would really launch one.
    install_driver(fixture, FakePane::default());
    fixture.app.refresh();
    let index = fixture
        .app
        .projects
        .iter()
        .position(|view| view.id == "bot-fork")
        .expect("leftover row on the dashboard");
    fixture.app.select_project_index(index);
}

#[test]
fn an_incomplete_fork_row_refuses_resume_and_restart() {
    let mut fixture = fork_fixture(Engine::Claude);
    leave_incomplete_fork(&mut fixture);

    fixture.app.request_attach();
    assert!(
        fixture.app.status.contains("incomplete fork") && fixture.app.status.contains("delete it"),
        "{}",
        fixture.app.status
    );
    fixture.app.restart_agent("bot-fork");
    assert!(
        fixture.app.status.contains("incomplete fork"),
        "{}",
        fixture.app.status
    );
    // `p` on a paused row names the resume key, which this row refuses.
    handle_key(&mut fixture.app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(
        fixture.app.status.contains("incomplete fork") && !fixture.app.status.contains("resumes"),
        "{}",
        fixture.app.status
    );

    assert!(
        fixture.pane.launches().is_empty(),
        "no blank conversation may start under a fork label"
    );
    let registry = Registry::load(&fixture.registry).unwrap();
    let leftover = registry
        .projects
        .iter()
        .find(|entry| entry.id == "bot-fork")
        .unwrap();
    assert!(!leftover.enabled);
}

#[test]
fn m_refuses_to_put_an_incomplete_fork_row_on_autopilot() {
    let mut fixture = fork_fixture(Engine::Claude);
    leave_incomplete_fork(&mut fixture);
    // If the flip got through, its daemon ensure would find this held lock rather than spawn pmd.
    let _held = hold_current_daemon(&fixture.registry, "pm-test");

    handle_key(&mut fixture.app, KeyCode::Char('m'), KeyModifiers::NONE);
    assert!(
        matches!(fixture.app.mode, UiMode::Normal),
        "no goal prompt may open for a row that cannot start: {:?}",
        fixture.app.mode
    );
    assert!(
        fixture.app.status.contains("incomplete fork") && fixture.app.status.contains("delete it"),
        "{}",
        fixture.app.status
    );

    // The goal editor, the cadence field and the `^E` drain all end in this one flip.
    let refused = fixture.app.turn_autopilot_on("bot-fork");
    assert!(refused.contains("incomplete fork"), "{refused}");

    let child = ProjectPaths::for_session(&fixture.root, "bot-fork");
    let config: Config = state::read_json(&child.config()).unwrap();
    assert_eq!(config.autonomy, Tier::Standard);
    let registry = Registry::load(&fixture.registry).unwrap();
    let leftover = registry
        .projects
        .iter()
        .find(|entry| entry.id == "bot-fork")
        .unwrap();
    assert!(!leftover.enabled, "pmd must never drive the leftover row");
    assert!(fixture.pane.launches().is_empty());
}

#[test]
fn an_incomplete_fork_row_offers_delete_instead_of_the_starts_it_refuses() {
    let mut fixture = fork_fixture(Engine::Claude);
    leave_incomplete_fork(&mut fixture);
    let selected = fixture.app.selected_view().unwrap();
    assert!(
        selected.incomplete_fork,
        "refresh must mark the leftover row"
    );
    assert_eq!(
        fork_refusal(selected).as_deref(),
        Some("bot-fork is an incomplete fork with no conversation - delete it with d")
    );
    assert!(
        !fixture.app.projects[0].incomplete_fork,
        "the source row is an ordinary session"
    );

    let mut terminal = Terminal::new(TestBackend::new(140, 32)).unwrap();
    terminal.draw(|frame| render(frame, &fixture.app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(
        screen_rows(&terminal)
            .iter()
            .any(|(row, _)| row.contains("next") && row.contains("delete it with d")),
        "{screen}"
    );
    assert!(!screen.contains("Enter resumes"), "{screen}");
    assert!(
        !screen.contains("m switches autopilot") && screen.contains("no conversation to show"),
        "the empty preview must not point at a key the row refuses: {screen}"
    );

    let normal = line_text(&keybar_line(&fixture.app, 240));
    assert!(normal.contains("Delete"), "{normal}");
    for refused in ["Resume", "Mode", "Restart", "Fork"] {
        assert!(
            !normal.contains(refused),
            "Normal offers {refused}: {normal}"
        );
    }

    fixture.app.mode = UiMode::Board;
    let lane = line_text(&keybar_line(&fixture.app, 240));
    assert!(lane.contains("Delete"), "{lane}");
    for refused in ["Resume", "Restart", "Fork"] {
        assert!(!lane.contains(refused), "the lane offers {refused}: {lane}");
    }
    fixture.app.board_detail_open = true;
    let detail = line_text(&keybar_line(&fixture.app, 240));
    for refused in ["Resume", "Attach", "Mode"] {
        assert!(
            !detail.contains(refused),
            "detail offers {refused}: {detail}"
        );
    }
}

#[test]
fn an_incomplete_fork_row_refuses_message_and_its_shelf_names_delete() {
    let mut fixture = fork_fixture(Engine::Claude);
    leave_incomplete_fork(&mut fixture);
    // The kept row whose child could not be stopped: a Message would type into that child.
    let child_session = session_name("bot-fork", &fixture.root);
    fixture.pane.0.alive.lock().unwrap().insert(child_session);

    handle_key(&mut fixture.app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(
        matches!(fixture.app.mode, UiMode::Normal),
        "no composer may open for a row with no conversation: {:?}",
        fixture.app.mode
    );
    assert!(
        fixture.app.status.contains("incomplete fork") && fixture.app.status.contains("delete it"),
        "{}",
        fixture.app.status
    );

    let normal = line_text(&keybar_line(&fixture.app, 240));
    assert!(!normal.contains("Send"), "Normal offers Send: {normal}");
    let mut terminal = Terminal::new(TestBackend::new(140, 48)).unwrap();
    terminal.draw(|frame| render(frame, &fixture.app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(
        !screen.contains("message this session") && screen.contains("delete this fork with d"),
        "the shelf must not offer the Message its key refuses: {screen}"
    );

    fixture.app.mode = UiMode::Board;
    fixture.app.board_detail_open = true;
    let detail = line_text(&keybar_line(&fixture.app, 240));
    assert!(
        !detail.contains("Send"),
        "Task detail offers Send: {detail}"
    );
    fixture.app.status.clear();
    handle_key(&mut fixture.app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(
        matches!(fixture.app.mode, UiMode::Board) && !fixture.app.return_to_board_after_send,
        "{:?}",
        fixture.app.mode
    );
    assert!(
        fixture.app.status.contains("incomplete fork"),
        "{}",
        fixture.app.status
    );
    assert!(fixture.pane.sends().is_empty());
}

#[test]
fn a_fork_row_with_a_captured_conversation_resumes_normally() {
    let mut fixture = fork_fixture(Engine::Claude);
    leave_incomplete_fork(&mut fixture);
    let child = ProjectPaths::for_session(&fixture.root, "bot-fork");
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 1000);
    ledger.conversation_id = Some(CHILD_ID.into());
    job::save(&child, &ledger).unwrap();

    fixture.app.request_attach();

    let launch = fixture.pane.launches();
    assert_eq!(launch.len(), 1, "{}", fixture.app.status);
    assert!(launch[0].2.iter().any(|arg| arg == CHILD_ID));
}
