//! How a fork is asked for: `f` requests it and the next frame runs it, so the dashboard can say
//! what is happening and drop the input typed while it blocked. A keybar click takes the same path.

use super::*;

/// Run the fork `f` requested, as the dashboard's next frame does once it has drawn it.
fn run_requested_fork(app: &mut App) {
    assert!(
        app.pending_fork.is_some(),
        "f must request a fork: {}",
        app.status
    );
    assert!(drain_events(app, |_| Ok(false), || unreachable!("nothing is queued")).unwrap());
}

#[test]
fn f_draws_a_forking_status_before_the_blocking_fork_and_drops_input_typed_during_it() {
    let mut fixture = fork_fixture(Engine::Claude);
    handle_key(&mut fixture.app, KeyCode::Char('f'), KeyModifiers::NONE);

    // Nothing blocks inside the key handler, so the next frame can say what is happening.
    assert!(fixture.pane.launches().is_empty());
    assert!(
        fixture.app.status.contains("forking bot"),
        "{}",
        fixture.app.status
    );
    let mut terminal = Terminal::new(TestBackend::new(160, 32)).unwrap();
    terminal.draw(|frame| render(frame, &fixture.app)).unwrap();
    assert!(screen_text(&terminal).contains("forking bot"));

    // Keys pressed while the dashboard waited were aimed at the source. Delivered after the fork
    // selected its child, Enter would attach to it and `d`, `y` would delete it.
    let queued: std::cell::RefCell<std::collections::VecDeque<Event>> = std::cell::RefCell::new(
        [KeyCode::Enter, KeyCode::Char('d'), KeyCode::Char('y')]
            .into_iter()
            .map(|code| {
                Event::Key(ratatui::crossterm::event::KeyEvent::new(
                    code,
                    KeyModifiers::NONE,
                ))
            })
            .collect(),
    );
    let handled = drain_events(
        &mut fixture.app,
        |_| Ok(!queued.borrow().is_empty()),
        || Ok(queued.borrow_mut().pop_front().expect("event queued")),
    )
    .unwrap();

    assert!(handled);
    assert!(queued.borrow().is_empty(), "the stale input is consumed");
    assert!(fixture.app.pending_fork.is_none());
    assert!(
        matches!(fixture.app.mode, UiMode::Normal),
        "{:?}",
        fixture.app.mode
    );
    assert!(fixture.app.pending_attach_loop.is_none() && fixture.app.pending_chat.is_none());
    assert_eq!(fixture.pane.launches().len(), 1, "only the fork launched");
    assert_forked(&fixture, Engine::Claude);
}

#[test]
fn f_refuses_a_working_source_without_waiting_a_frame() {
    let mut fixture = fork_fixture(Engine::Claude);
    fixture.app.projects[0].session_live = true;
    fixture.app.projects[0].agent_working = Some(true);

    handle_key(&mut fixture.app, KeyCode::Char('f'), KeyModifiers::NONE);

    assert!(fixture.app.pending_fork.is_none());
    assert!(
        fixture.app.status.contains("still working"),
        "{}",
        fixture.app.status
    );
    let mut nothing_pending = false;
    assert!(
        !drain_events(
            &mut fixture.app,
            |_| {
                nothing_pending = true;
                Ok(false)
            },
            || unreachable!("nothing is queued"),
        )
        .unwrap()
    );
    assert!(
        nothing_pending,
        "with no fork pending the frame polls as usual"
    );
    assert!(fixture.pane.launches().is_empty());
}

#[test]
fn board_fork_selects_the_child_without_opening_detail() {
    let mut fixture = fork_fixture(Engine::Claude);
    fixture.app.mode = UiMode::Board;

    handle_key(&mut fixture.app, KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(matches!(fixture.app.mode, UiMode::Board));
    run_requested_fork(&mut fixture.app);

    assert!(matches!(fixture.app.mode, UiMode::Board));
    assert!(!fixture.app.board_detail_open);
    assert_eq!(
        fixture.app.selected_view().map(|view| view.id.as_str()),
        Some("bot-fork")
    );
    fixture.app.mode = UiMode::Normal;
    assert_forked(&fixture, Engine::Claude);
}

#[test]
fn fork_keybar_click_dispatches_the_same_action_as_f() {
    let mut fixture = fork_fixture(Engine::Claude);
    let mut terminal = Terminal::new(TestBackend::new(300, 32)).unwrap();
    terminal.draw(|frame| render(frame, &fixture.app)).unwrap();
    let hit = fixture
        .app
        .key_hits
        .borrow()
        .iter()
        .find(|hit| hit.code == KeyCode::Char('f'))
        .cloned()
        .expect("Fork keybar chip");
    let event = Event::Mouse(ratatui::crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.area.x,
        row: hit.area.y,
        modifiers: KeyModifiers::NONE,
    });
    let mut handled = false;

    handle_event(&mut fixture.app, event, &mut handled);

    assert!(handled);
    run_requested_fork(&mut fixture.app);
    assert_forked(&fixture, Engine::Claude);
}
