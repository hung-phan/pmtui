//! The crossterm event adapter: key presses and bracketed pastes must reach the same
//! handlers the focused unit tests exercise, while releases and unrelated events stay inert.

use super::*;
use crate::app::scroll::WHEEL_LINES;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::time::Instant;

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(ratatui::crossterm::event::MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn chat_req(label: &str) -> ChatReq {
    ChatReq {
        session_paths: ProjectPaths::new("/tmp/chat-state"),
        root: PathBuf::from("/tmp/chat-root"),
        argv: vec!["agent".into()],
        label: label.into(),
        socket: "pm-test".into(),
        session: "pm-chat".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    }
}

fn create_chat_req(root: &Path, label: &str) -> CreateChatReq {
    let lease = acquire_free_lease(&root.join("create.lock")).expect("test lease should be free");
    CreateChatReq {
        session_paths: ProjectPaths::new(root),
        root: root.to_path_buf(),
        argv: vec!["agent".into()],
        label: label.into(),
        lease,
        socket: "pm-test".into(),
        session: "pm-create".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    }
}

#[test]
fn key_and_paste_events_reach_the_input_dispatchers() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    let mut handled = false;

    let scrolled = handle_event(
        &mut app,
        Event::Key(ratatui::crossterm::event::KeyEvent::new(
            KeyCode::Char('?'),
            KeyModifiers::NONE,
        )),
        &mut handled,
    );
    assert!(!scrolled, "a key ends any wheel-event drain");
    assert!(handled, "a key suppresses the idle refresh for this frame");
    assert!(
        matches!(app.mode, UiMode::Help { .. }),
        "the key must reach handle_key"
    );

    app.mode = UiMode::EditingGoal {
        id: "bot".into(),
        brief: PathBuf::from("/tmp/brief.md"),
        current: String::new(),
        input: goal_buf(""),
        then_autopilot: false,
    };
    handled = false;
    let scrolled = handle_event(&mut app, Event::Paste("ship\nall".into()), &mut handled);
    assert!(!scrolled, "a paste is one input event, not a wheel burst");
    assert!(
        handled,
        "a paste suppresses the idle refresh for this frame"
    );
    let UiMode::EditingGoal { input, .. } = &app.mode else {
        panic!("paste changed the active mode");
    };
    // THE PASTE KEEPS ITS LINE. The goal is a prose buffer, so `handle_paste` cleans it with the
    // delivery path's sanitizer (which keeps `\n`) instead of flattening it for a one-row field.
    assert_eq!(input.text(), "ship\nall");
}

#[test]
fn key_releases_and_non_input_events_are_ignored() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    let mut handled = false;
    let released = ratatui::crossterm::event::KeyEvent::new_with_kind(
        KeyCode::Char('q'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );

    assert!(!handle_event(&mut app, Event::Key(released), &mut handled));
    assert!(!handled);
    assert!(!app.should_quit, "a key release must not fire a binding");

    assert!(!handle_event(
        &mut app,
        Event::Resize(120, 40),
        &mut handled
    ));
    assert!(!handled, "a resize is not user input");
}

#[test]
fn mouse_events_do_not_reach_the_dashboard_through_an_overlay() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Help { scroll: 0 });
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 40, 20),
        detail: Rect::new(40, 0, 40, 20),
    });
    let event = Event::Mouse(ratatui::crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 50,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    let mut handled = false;

    assert!(!handle_event(&mut app, event, &mut handled));
    assert!(!handled, "an overlay owns input while it is visible");
    assert_eq!(app.detail_scroll, 0);
}

#[test]
fn mouse_wheel_and_click_route_only_to_the_hit_session_pane() {
    let mut app = app_with(
        vec![agent_loop_view("one"), agent_loop_view("two")],
        UiMode::Normal,
    );
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 30, 20),
        detail: Rect::new(30, 0, 40, 20),
    });
    app.detail_max.set(usize::MAX);
    app.row_hits.replace(vec![(4, 1)]);
    let mut handled = false;

    assert!(handle_event(
        &mut app,
        mouse(MouseEventKind::ScrollUp, 35, 5),
        &mut handled,
    ));
    assert!(handled);
    assert_eq!(app.detail_scroll, WHEEL_LINES);

    handled = false;
    assert!(!handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 5, 4),
        &mut handled,
    ));
    assert!(handled);
    assert_eq!(app.selected, 1);
    assert_eq!(app.detail_scroll, 0);
    assert_eq!(app.detail_max.get(), 0);

    handled = false;
    assert!(!handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 75, 4),
        &mut handled,
    ));
    assert!(!handled, "a log-pane click must not select by row");
    assert_eq!(app.selected, 1);

    handled = false;
    assert!(!handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 5, 5),
        &mut handled,
    ));
    assert!(!handled, "a sessions-pane gap must not select a row");
    assert_eq!(app.selected, 1);

    assert!(!handle_event(
        &mut app,
        mouse(MouseEventKind::ScrollUp, 120, 40),
        &mut handled,
    ));
}

#[test]
fn composing_wheel_scrolls_text_panes_but_never_moves_the_session_selection() {
    // The composer is bound to the session it opened on. The keyboard cannot move the
    // selection while it is open, so a wheel over SESSIONS must not either: otherwise the
    // preview above the field shows one session while Enter types into another.
    let mut app = app_with(
        vec![agent_loop_view("one"), agent_loop_view("two")],
        UiMode::Sending {
            target: SendTarget {
                id: "one".into(),
                root: PathBuf::from("/nonexistent"),
                session: "pm-one".into(),
                agent_loop: true,
                driven: false,
                in_chat: false,
            },
            input: Composer::from_text("reply for one".into()),
        },
    );
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 30, 20),
        detail: Rect::new(30, 0, 40, 20),
    });
    app.detail_max.set(usize::MAX);
    let mut handled = false;

    for kind in [MouseEventKind::ScrollDown, MouseEventKind::ScrollUp] {
        assert!(!handle_event(&mut app, mouse(kind, 5, 5), &mut handled));
    }
    assert!(!handled, "a sessions-pane wheel is inert while composing");
    assert_eq!(
        app.selected, 0,
        "the selection stays on the composer's target"
    );

    assert!(handle_event(
        &mut app,
        mouse(MouseEventKind::ScrollUp, 35, 5),
        &mut handled,
    ));
    assert!(handled);
    assert_eq!(
        app.detail_scroll, WHEEL_LINES,
        "the transcript still scrolls"
    );
    let UiMode::Sending { target, input } = &app.mode else {
        panic!("the wheel closed the composer");
    };
    assert_eq!(target.id, "one");
    assert_eq!(input.text(), "reply for one");
}

#[test]
fn preview_click_routes_through_the_existing_enter_action() {
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_agent_loop(dir.path(), "bot");
    let session = session_name("bot", &root);
    let pane = FakePane::live(&session, "");
    let mut app = app_with_driver(vec![agent_loop_view("bot")], UiMode::Normal, Box::new(pane));
    app.registry_path = registry;
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 30, 20),
        detail: Rect::new(30, 0, 70, 20),
    });
    app.preview_attach_hit.set(Rect::new(31, 0, 24, 1));
    let mut handled = false;

    assert!(!handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 50, 8),
        &mut handled,
    ));
    assert!(!handled, "the transcript body remains inert");
    assert!(app.pending_attach_loop.is_none());

    assert!(!handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 40, 0),
        &mut handled,
    ));

    assert!(handled);
    assert_eq!(app.pending_attach_loop.as_deref(), Some(session.as_str()));
}

#[test]
fn message_shelf_click_routes_through_the_existing_send_action() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let hit = app.composer_hit.get();
    assert_ne!(
        hit,
        Rect::ZERO,
        "roomy dashboard should publish the shelf hitbox"
    );

    let mut handled = false;
    assert!(!handle_event(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            hit.x.saturating_add(1),
            hit.y.saturating_add(1),
        ),
        &mut handled,
    ));

    assert!(handled);
    assert!(matches!(app.mode, UiMode::Sending { .. }));
}

#[test]
fn rendered_keybar_chips_and_create_fields_are_clickable() {
    let mut dashboard = app_with(Vec::new(), UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(140, 24)).unwrap();
    terminal.draw(|frame| render(frame, &dashboard)).unwrap();
    let new_hit = dashboard
        .top_hits
        .borrow()
        .iter()
        .find(|hit| hit.code == KeyCode::Char('n'))
        .cloned()
        .expect("New top-control hitbox");
    let mut handled = false;
    handle_event(
        &mut dashboard,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_hit.area.x,
            new_hit.area.y,
        ),
        &mut handled,
    );
    assert!(handled);
    assert!(matches!(dashboard.mode, UiMode::Creating(_)));

    terminal.draw(|frame| render(frame, &dashboard)).unwrap();
    let message_hit = dashboard
        .create_hits
        .borrow()
        .iter()
        .find(|(_, field)| *field == CreateForm::GOAL)
        .copied()
        .expect("Message row hitbox");
    let before = match &dashboard.mode {
        UiMode::Creating(form) => form.goal.as_str().to_string(),
        _ => unreachable!(),
    };
    handled = false;
    handle_event(
        &mut dashboard,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            message_hit.0.x,
            message_hit.0.y,
        ),
        &mut handled,
    );
    let UiMode::Creating(form) = &dashboard.mode else {
        panic!("click left create mode");
    };
    assert!(handled);
    assert_eq!(form.field, CreateForm::GOAL);
    assert_eq!(
        form.goal.as_str(),
        before,
        "focus must not mutate the value"
    );
}

#[test]
fn responsive_and_expanded_controls_publish_only_current_hit_regions() {
    let dashboard = app_with(Vec::new(), UiMode::Normal);
    let mut wide = Terminal::new(TestBackend::new(140, 24)).unwrap();
    wide.draw(|frame| render(frame, &dashboard)).unwrap();
    assert_eq!(
        dashboard
            .top_hits
            .borrow()
            .iter()
            .map(|hit| hit.code)
            .collect::<Vec<_>>(),
        // The view tabs lead the header, then the two action badges — ONE list, because the click
        // path reads exactly this vec.
        vec![
            KeyCode::Char('1'),
            KeyCode::Char('2'),
            KeyCode::Char('0'),
            KeyCode::Char('/'),
            KeyCode::Char('n')
        ]
    );
    assert_eq!(
        dashboard
            .key_hits
            .borrow()
            .iter()
            .map(|hit| hit.code)
            .collect::<Vec<_>>(),
        vec![KeyCode::Char('?'), KeyCode::Char('q')]
    );
    let mut narrow = Terminal::new(TestBackend::new(24, 8)).unwrap();
    narrow.draw(|frame| render(frame, &dashboard)).unwrap();
    assert!(dashboard.top_hits.borrow().is_empty());
    assert_eq!(
        dashboard
            .key_hits
            .borrow()
            .iter()
            .map(|hit| hit.code)
            .collect::<Vec<_>>(),
        vec![KeyCode::Char('?'), KeyCode::Char('q')]
    );

    let mut form = CreateForm::new();
    form.tier = Tier::Autopilot;
    form.field = CreateForm::WORKER_MODEL;
    form.model_choices = vec![
        ModelInfo {
            label: "One".into(),
            value: "one".into(),
        },
        ModelInfo {
            label: "Two".into(),
            value: "two".into(),
        },
    ];
    let app = app_with(Vec::new(), UiMode::Creating(form));
    let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let hits = app.create_hits.borrow();
    assert_eq!(hits.len(), CreateForm::FIELDS);
    let mut rows: Vec<u16> = hits.iter().map(|(area, _)| area.y).collect();
    rows.sort_unstable();
    rows.dedup();
    assert_eq!(
        rows.len(),
        CreateForm::FIELDS,
        "expanded model rows must not overlap fields"
    );
}

#[test]
fn compact_modal_keybar_is_visible_where_its_click_targets_are_published() {
    let app = app_with(Vec::new(), UiMode::Creating(CreateForm::new()));
    let mut terminal = Terminal::new(TestBackend::new(24, 8)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();

    let expected = line_text(&keybar_line(&app, 24));
    let actual = screen_rows(&terminal)
        .last()
        .map(|(row, _)| row.trim_end().to_string())
        .unwrap_or_default();
    assert_eq!(actual, expected.trim_end());
    assert!(!app.key_hits.borrow().is_empty());
    assert!(
        app.key_hits
            .borrow()
            .iter()
            .all(|hit| hit.area.y == 7 && hit.area.height == 1),
        "every published target must be on the visible final keybar"
    );
}

#[test]
fn stale_hidden_and_boundary_pointer_regions_are_inert() {
    assert!(!rect_contains(Rect::ZERO, 0, 0));
    let bounds = Rect::new(4, 5, 3, 2);
    assert!(rect_contains(bounds, 4, 5));
    assert!(rect_contains(bounds, 6, 6));
    assert!(!rect_contains(bounds, 7, 6));
    assert!(!rect_contains(bounds, 6, 7));

    let mut creating = app_with(Vec::new(), UiMode::Creating(CreateForm::new()));
    creating.create_hits.replace(vec![
        (Rect::ZERO, CreateForm::GOAL),
        (Rect::new(5, 5, 20, 1), CreateForm::CADENCE),
    ]);
    let mut handled = false;
    handle_event(
        &mut creating,
        mouse(MouseEventKind::Down(MouseButton::Left), 6, 5),
        &mut handled,
    );
    assert!(!handled, "a hidden Standard cadence row cannot be focused");
    let UiMode::Creating(form) = &creating.mode else {
        unreachable!()
    };
    assert_eq!(form.field, CreateForm::GOAL);

    let mut normal = app_with(vec![agent_loop_view("one")], UiMode::Normal);
    normal.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 20, 10),
        detail: Rect::new(20, 0, 40, 10),
    });
    normal.row_hits.replace(vec![(4, 0)]);
    normal.preview_attach_hit.set(Rect::new(21, 0, 10, 1));
    handled = false;
    handle_event(
        &mut normal,
        mouse(MouseEventKind::Down(MouseButton::Left), 5, 3),
        &mut handled,
    );
    assert!(
        !handled,
        "a sessions-pane blank row has no selection target"
    );
    handle_event(
        &mut normal,
        mouse(MouseEventKind::Down(MouseButton::Left), 31, 0),
        &mut handled,
    );
    assert!(!handled, "the preview-title right edge is exclusive");
}

#[test]
fn mouse_wheel_scrolls_the_full_screen_audit_and_answer_body() {
    let mut audit = app_with(
        vec![],
        UiMode::Decisions {
            tab: AuditTab::Decisions,
            scroll: 0,
            other_scroll: 0,
            since: None,
            id: "audit".into(),
        },
    );
    audit.scroll_max.set(20);
    let mut handled = false;
    assert!(handle_event(
        &mut audit,
        mouse(MouseEventKind::ScrollUp, 5, 5),
        &mut handled,
    ));
    assert!(handled);
    assert!(matches!(
        audit.mode,
        UiMode::Decisions {
            scroll: WHEEL_LINES,
            ..
        }
    ));

    handled = false;
    assert!(handle_event(
        &mut audit,
        mouse(MouseEventKind::ScrollDown, 5, 5),
        &mut handled,
    ));
    assert!(matches!(audit.mode, UiMode::Decisions { scroll: 0, .. }));

    let mut answer = app_with(
        vec![],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    answer.scroll_max.set(20);
    handled = false;
    assert!(handle_event(
        &mut answer,
        mouse(MouseEventKind::ScrollDown, 5, 5),
        &mut handled,
    ));
    assert!(matches!(
        answer.mode,
        UiMode::Answering {
            scroll: WHEEL_LINES,
            ..
        }
    ));

    handled = false;
    assert!(handle_event(
        &mut answer,
        mouse(MouseEventKind::ScrollUp, 5, 5),
        &mut handled,
    ));
    assert!(matches!(answer.mode, UiMode::Answering { scroll: 0, .. }));
}

#[test]
fn event_drain_uses_the_animation_timeout_and_coalesces_only_wheels() {
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 30, 20),
        detail: Rect::new(30, 0, 40, 20),
    });
    app.detail_max.set(usize::MAX);
    let events = RefCell::new(VecDeque::from([
        mouse(MouseEventKind::ScrollUp, 35, 5),
        mouse(MouseEventKind::ScrollUp, 35, 5),
        Event::Key(ratatui::crossterm::event::KeyEvent::new(
            KeyCode::Char('?'),
            KeyModifiers::NONE,
        )),
    ]));
    let polls = RefCell::new(Vec::new());

    let handled = drain_events(
        &mut app,
        |timeout| {
            polls.borrow_mut().push(timeout);
            Ok(!events.borrow().is_empty())
        },
        || Ok(events.borrow_mut().pop_front().expect("event queued")),
    )
    .unwrap();

    assert!(handled);
    assert_eq!(app.detail_scroll, WHEEL_LINES * 2);
    assert!(matches!(app.mode, UiMode::Help { .. }));
    assert!(events.borrow().is_empty());
    // ONE cadence, whatever the fleet is doing. A row on Autopilot used to make the loop poll eight
    // times a second to animate a sweep over its id; that effect is gone, so the fast tier went with
    // it and an autopilot fleet no longer costs seven extra wakeups a second.
    assert_eq!(polls.borrow()[0], Duration::from_millis(IDLE_POLL_MS));
    assert!(
        polls.borrow()[1..]
            .iter()
            .all(|timeout| *timeout == Duration::ZERO)
    );
}

#[test]
fn session_wheel_forces_a_redraw_before_a_queued_preview_click() {
    let mut app = app_with(
        vec![agent_loop_view("alpha"), agent_loop_view("beta")],
        UiMode::Normal,
    );
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 30, 20),
        detail: Rect::new(30, 0, 40, 20),
    });
    app.preview_attach_hit.set(Rect::new(30, 0, 40, 1));
    let events = RefCell::new(VecDeque::from([
        mouse(MouseEventKind::ScrollDown, 5, 5),
        mouse(MouseEventKind::Down(MouseButton::Left), 35, 0),
    ]));

    assert!(
        drain_events(
            &mut app,
            |_| Ok(!events.borrow().is_empty()),
            || Ok(events.borrow_mut().pop_front().expect("event queued")),
        )
        .unwrap()
    );
    assert_eq!(app.selected, 1, "the wheel selected the next row");
    assert_eq!(
        events.borrow().len(),
        1,
        "the click waits for a fresh frame"
    );
    assert!(app.pending_attach_loop.is_none());
    assert!(app.pending_chat.is_none());
}

#[test]
fn event_drain_is_idle_when_empty_and_caps_an_unbounded_wheel_burst() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    let observed = Cell::new(Duration::ZERO);
    assert!(
        !drain_events(
            &mut app,
            |timeout| {
                observed.set(timeout);
                Ok(false)
            },
            || unreachable!("an empty poll must not read"),
        )
        .unwrap()
    );
    assert_eq!(observed.get(), Duration::from_millis(IDLE_POLL_MS));

    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 30, 20),
        detail: Rect::new(30, 0, 40, 20),
    });
    app.detail_max.set(usize::MAX);
    let events = RefCell::new(VecDeque::from_iter(
        (0..=COALESCE_MAX).map(|_| mouse(MouseEventKind::ScrollUp, 35, 5)),
    ));
    assert!(
        drain_events(
            &mut app,
            |_| Ok(!events.borrow().is_empty()),
            || Ok(events.borrow_mut().pop_front().expect("event queued")),
        )
        .unwrap()
    );
    assert_eq!(app.detail_scroll, COALESCE_MAX * WHEEL_LINES);
    assert_eq!(
        events.borrow().len(),
        1,
        "one event remains for the next frame"
    );
}

#[test]
fn event_drain_stops_when_a_wheel_burst_has_no_second_event() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 30, 20),
        detail: Rect::new(30, 0, 40, 20),
    });
    app.detail_max.set(usize::MAX);
    let polls = Cell::new(0usize);

    let handled = drain_events(
        &mut app,
        |_| {
            let call = polls.get();
            polls.set(call + 1);
            Ok(call == 0)
        },
        || Ok(mouse(MouseEventKind::ScrollUp, 35, 5)),
    )
    .unwrap();

    assert!(handled);
    assert_eq!(polls.get(), 2, "initial poll plus empty burst poll");
    assert_eq!(app.detail_scroll, WHEEL_LINES);
}

#[test]
fn event_drain_propagates_a_burst_poll_failure_after_scrolling() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 30, 20),
        detail: Rect::new(30, 0, 40, 20),
    });
    app.detail_max.set(usize::MAX);
    let polls = Cell::new(0usize);

    let error = drain_events(
        &mut app,
        |_| {
            let call = polls.get();
            polls.set(call + 1);
            if call == 0 {
                Ok(true)
            } else {
                Err(io::Error::other("burst poll failed"))
            }
        },
        || Ok(mouse(MouseEventKind::ScrollUp, 35, 5)),
    )
    .unwrap_err();

    assert!(error.to_string().contains("burst poll failed"));
    assert_eq!(app.detail_scroll, WHEEL_LINES);
}

#[test]
fn event_drain_propagates_poll_and_read_failures() {
    let mut app = app_with(vec![], UiMode::Normal);
    let poll_error = drain_events(
        &mut app,
        |_| Err(io::Error::other("poll failed")),
        || unreachable!(),
    )
    .unwrap_err();
    assert!(poll_error.to_string().contains("poll failed"));

    let read_error = drain_events(
        &mut app,
        |_| Ok(true),
        || Err(io::Error::other("read failed")),
    )
    .unwrap_err();
    assert!(read_error.to_string().contains("read failed"));
}

#[test]
fn chat_requests_report_detach_end_and_error_and_forget_preview_fit() {
    let mut app = app_with(vec![], UiMode::Normal);
    app.pending_chat = Some(chat_req("bot"));
    app.agent_pane_fit.replace(Some(("bot".into(), 100, 30)));
    drain_pending_chat(&mut app, |_| Ok(true));
    assert_eq!(app.status, chat_return_status("bot", true));
    assert!(app.pending_chat.is_none());
    assert!(app.agent_pane_fit.borrow().is_none());

    app.pending_chat = Some(chat_req("bot"));
    drain_pending_chat(&mut app, |_| Ok(false));
    assert_eq!(app.status, chat_return_status("bot", false));

    app.pending_chat = Some(chat_req("bot"));
    drain_pending_chat(&mut app, |_| anyhow::bail!("launch failed"));
    assert_eq!(app.status, "chat ended: launch failed");
}

#[test]
fn create_chat_requests_report_liveness_and_errors_and_are_consumed() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(vec![], UiMode::Normal);
    app.pending_create_chat = Some(create_chat_req(dir.path(), "bot"));
    app.agent_pane_fit.replace(Some(("bot".into(), 100, 30)));
    drain_pending_create_chat(&mut app, |_| Ok(true));
    assert_eq!(app.status, format!("created bot; {}", chat_park_note(true)));
    assert!(app.pending_create_chat.is_none());
    assert!(app.agent_pane_fit.borrow().is_none());

    app.pending_create_chat = Some(create_chat_req(dir.path(), "bot"));
    drain_pending_create_chat(&mut app, |_| anyhow::bail!("create failed"));
    assert_eq!(app.status, "create chat ended: create failed");
}

#[test]
fn attach_requests_resolve_the_selected_registry_entry_and_report_missing_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = loop_app(&reg_path);
    app.pending_attach_loop = Some("pm-loop".into());
    app.agent_pane_fit.replace(Some(("bot".into(), 100, 30)));
    let called = Cell::new(false);

    drain_pending_attach(&mut app, |paths, socket, session| {
        called.set(true);
        assert_eq!(
            paths.brief(),
            ProjectPaths::for_session(&root, "bot").brief()
        );
        assert_eq!(socket, "pm-test");
        assert_eq!(session, "pm-loop");
        Ok(())
    });
    assert!(called.get());
    assert_eq!(app.status, "detached from bot (still running)");
    assert!(app.agent_pane_fit.borrow().is_none());

    app.pending_attach_loop = Some("pm-loop".into());
    drain_pending_attach(&mut app, |_, _, _| anyhow::bail!("attach broke"));
    assert_eq!(app.status, "attach failed: attach broke");

    app.registry_path = dir.path().join("missing.json");
    app.pending_attach_loop = Some("pm-loop".into());
    drain_pending_attach(&mut app, |_, _, _| {
        panic!("a missing registry row must not attach")
    });
    assert_eq!(app.status, "attach failed: bot is gone from the registry");

    let mut unselected = app_with(vec![], UiMode::Normal);
    unselected.pending_attach_loop = Some("pm-fallback".into());
    drain_pending_attach(&mut unselected, |_, _, _| {
        panic!("an unselected row must not attach")
    });
    assert_eq!(
        unselected.status,
        "attach failed: pm-fallback is gone from the registry"
    );

    let corrupt = dir.path().join("corrupt.json");
    std::fs::write(&corrupt, "{broken").unwrap();
    unselected.registry_path = corrupt;
    unselected.pending_attach_loop = Some("pm-corrupt".into());
    drain_pending_attach(&mut unselected, |_, _, _| {
        panic!("a corrupt registry must not attach")
    });
    assert_eq!(
        unselected.status,
        "attach failed: pm-corrupt is gone from the registry"
    );
}

#[test]
fn create_form_brief_edits_cover_saved_empty_and_failed_results() {
    let mut app = app_with(vec![], UiMode::Creating(CreateForm::new()));
    app.pending_brief_edit = Some(BriefEdit {
        goal: "seed".into(),
        target: BriefEditTarget::CreateForm,
    });
    drain_pending_brief_edit(&mut app, |_| Ok(Some("one\ntwo".into())));
    let UiMode::Creating(form) = &app.mode else {
        panic!("create form should remain open");
    };
    assert_eq!(form.goal.as_str(), "one\ntwo");
    assert_eq!(app.status, "brief updated (2 lines)");

    app.pending_brief_edit = Some(BriefEdit {
        goal: "seed".into(),
        target: BriefEditTarget::CreateForm,
    });
    drain_pending_brief_edit(&mut app, |_| Ok(Some("one".into())));
    assert_eq!(app.status, "brief updated");

    app.pending_brief_edit = Some(BriefEdit {
        goal: "seed".into(),
        target: BriefEditTarget::CreateForm,
    });
    drain_pending_brief_edit(&mut app, |_| Ok(None));
    assert_eq!(app.status, "brief unchanged (empty save keeps it)");

    app.pending_brief_edit = Some(BriefEdit {
        goal: "seed".into(),
        target: BriefEditTarget::CreateForm,
    });
    drain_pending_brief_edit(&mut app, |_| anyhow::bail!("editor broke"));
    assert_eq!(app.status, "brief editor failed: editor broke");
}

#[test]
fn session_brief_edits_preserve_empty_saves_and_gate_autopilot_on_write_success() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut app = loop_app(&reg_path);
    let _daemon = hold_current_daemon(&reg_path, &app.socket);

    app.pending_brief_edit = Some(BriefEdit {
        goal: "goal".into(),
        target: BriefEditTarget::Session {
            id: "bot".into(),
            brief: paths.brief(),
            then_autopilot: true,
        },
    });
    drain_pending_brief_edit(&mut app, |_| Ok(Some("new goal".into())));
    assert_eq!(std::fs::read_to_string(paths.brief()).unwrap(), "new goal");
    assert_eq!(tier_on_disk(&root, "bot"), Tier::Autopilot);
    assert!(app.status.contains("goal via $EDITOR"));

    app.pending_brief_edit = Some(BriefEdit {
        goal: "new goal".into(),
        target: BriefEditTarget::Session {
            id: "bot".into(),
            brief: paths.brief(),
            then_autopilot: false,
        },
    });
    drain_pending_brief_edit(&mut app, |_| Ok(None));
    assert_eq!(app.status, "bot goal unchanged (empty/all-comments)");

    let failed_dir = tempfile::tempdir().unwrap();
    let (failed_reg, failed_root) = reg_with_agent_loop(failed_dir.path(), "blocked");
    let mut failed_app = loop_app(&failed_reg);
    let blocked_parent = failed_dir.path().join("not-a-directory");
    std::fs::write(&blocked_parent, "file").unwrap();
    failed_app.pending_brief_edit = Some(BriefEdit {
        goal: String::new(),
        target: BriefEditTarget::Session {
            id: "blocked".into(),
            brief: blocked_parent.join("brief.md"),
            then_autopilot: true,
        },
    });
    drain_pending_brief_edit(&mut failed_app, |_| Ok(Some("cannot write".into())));
    assert!(
        failed_app.status.contains("autopilot NOT turned on"),
        "{}",
        failed_app.status
    );
    assert_eq!(tier_on_disk(&failed_root, "blocked"), Tier::Standard);

    app.pending_brief_edit = Some(BriefEdit {
        goal: "goal".into(),
        target: BriefEditTarget::Session {
            id: "bot".into(),
            brief: paths.brief(),
            then_autopilot: false,
        },
    });
    drain_pending_brief_edit(&mut app, |_| anyhow::bail!("editor broke"));
    assert_eq!(app.status, "bot goal editor failed: editor broke");
}

#[test]
fn directive_edits_report_saved_empty_and_failed_results() {
    let dir = tempfile::tempdir().unwrap();
    let directive = dir.path().join("directive.md");
    let mut app = app_with(vec![], UiMode::Normal);

    app.pending_directive_edit = Some(DirectiveEditReq {
        id: "bot".into(),
        directive: directive.clone(),
        current: "old".into(),
    });
    drain_pending_directive_edit(&mut app, |_| Ok(Some("new direction".into())));
    assert_eq!(
        std::fs::read_to_string(&directive).unwrap(),
        "new direction"
    );
    assert!(app.status.contains("bot directive"));

    app.pending_directive_edit = Some(DirectiveEditReq {
        id: "bot".into(),
        directive: directive.clone(),
        current: "new direction".into(),
    });
    drain_pending_directive_edit(&mut app, |_| Ok(None));
    assert_eq!(
        app.status,
        "bot directive unchanged (empty save keeps it — press ^X to rescind)"
    );

    app.pending_directive_edit = Some(DirectiveEditReq {
        id: "bot".into(),
        directive,
        current: "new direction".into(),
    });
    drain_pending_directive_edit(&mut app, |_| anyhow::bail!("editor broke"));
    assert_eq!(app.status, "bot directive editor failed: editor broke");
}

#[test]
fn send_editor_requests_report_empty_error_and_submit_saved_text() {
    let target = SendTarget {
        id: "bot".into(),
        root: PathBuf::from("/tmp/bot"),
        session: "pm-bot".into(),
        agent_loop: true,
        driven: false,
        in_chat: false,
    };
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);

    app.pending_send = Some(SendReq {
        target: target.clone(),
        seed: String::new(),
        cursor: (0, 0),
    });
    drain_pending_send(&mut app, |_| Ok(None));
    assert_eq!(app.status, "nothing to send (empty buffer)");

    app.pending_send = Some(SendReq {
        target: target.clone(),
        seed: String::new(),
        cursor: (0, 0),
    });
    drain_pending_send(&mut app, |_| anyhow::bail!("editor broke"));
    assert_eq!(app.status, "send editor failed: editor broke");

    app.pending_send = Some(SendReq {
        target: target.clone(),
        seed: "keep this draft".into(),
        cursor: (0, 4),
    });
    drain_pending_send(&mut app, |_| anyhow::bail!("editor broke again"));
    assert_eq!(
        app.status,
        "send editor failed: editor broke again; draft kept"
    );
    assert_eq!(app.message_drafts["bot"].text(), "keep this draft");

    app.pending_send = Some(SendReq {
        target,
        seed: "seed".into(),
        cursor: (0, 4),
    });
    drain_pending_send(&mut app, |_| Ok(Some("send this".into())));
    assert!(matches!(app.mode, UiMode::Sending { .. }));
    assert_eq!(app.status, "no agent running — press Enter to open one");
}

#[test]
fn frame_completion_logs_input_and_throttles_idle_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    // The one dashboard on its session list, so its frames also broker spawn requests.
    app.registry_path = dir.path().join("registry.json");
    app.dashboard_owner_nonce = Some("owner".into());
    app.status = "typed".into();
    let mut last_refresh = Instant::now();

    finish_frame(&mut app, true, &mut last_refresh);
    assert_eq!(log_lines(&app), vec!["typed"]);
    let unchanged = last_refresh;

    finish_frame(&mut app, false, &mut last_refresh);
    assert_eq!(last_refresh, unchanged);

    last_refresh = Instant::now() - Duration::from_millis(REFRESH_MS as u64 + 1);
    let overdue = last_refresh;
    finish_frame(&mut app, false, &mut last_refresh);
    assert!(last_refresh > overdue);
}

#[test]
fn terminal_modes_and_cleanup_restore_on_success_and_error() {
    let mut output = Vec::new();
    set_terminal_input_modes(&mut output, true);
    let enabled = String::from_utf8_lossy(&output);
    assert!(enabled.contains("?2004h"));
    assert!(enabled.contains("?1000h"));

    output.clear();
    let restored = Cell::new(false);
    let err = finish_terminal(
        &mut output,
        || restored.set(true),
        Err(anyhow::anyhow!("loop failed")),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "loop failed");
    assert!(restored.get());
    let disabled = String::from_utf8_lossy(&output);
    assert!(disabled.contains("?2004l"));
    assert!(disabled.contains("?1000l"));

    let ok = finish_terminal(&mut output, || {}, Ok(()));
    assert!(ok.is_ok());
}

#[test]
fn empty_request_drains_are_inert_and_do_not_call_terminal_actions() {
    let mut app = app_with(vec![], UiMode::Normal);
    let status = app.status.clone();

    drain_pending_chat(&mut app, |_| unreachable!("no chat request"));
    drain_pending_create_chat(&mut app, |_| unreachable!("no create request"));
    drain_pending_attach(&mut app, |_, _, _| unreachable!("no attach request"));
    drain_pending_brief_edit(&mut app, |_| unreachable!("no brief request"));
    drain_pending_directive_edit(&mut app, |_| unreachable!("no directive request"));
    drain_pending_send(&mut app, |_| unreachable!("no send request"));

    assert_eq!(app.status, status);
}

#[test]
fn run_loop_finishes_input_frames_stops_on_quit_and_propagates_frame_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(vec![], UiMode::Normal);
    app.registry_path = dir.path().join("registry.json");
    app.dashboard_owner_nonce = Some("owner".into());
    app.status = "first frame".into();
    let calls = Cell::new(0);

    run_loop(&mut app, |app| {
        let call = calls.get() + 1;
        calls.set(call);
        if call == 2 {
            app.should_quit = true;
        }
        Ok(true)
    })
    .unwrap();

    assert_eq!(calls.get(), 2);
    assert_eq!(log_lines(&app), vec!["first frame"]);

    let mut failed = app_with(vec![], UiMode::Normal);
    let error = run_loop(&mut failed, |_| anyhow::bail!("frame failed")).unwrap_err();
    assert_eq!(error.to_string(), "frame failed");
}

#[test]
fn startup_covers_help_singleton_lock_fault_and_dashboard_result_paths() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let args = |registry: PathBuf, help| Args {
        registry,
        socket: "pm-events".into(),
        help,
    };

    let calls = Cell::new(0);
    start_app(args(registry.clone(), true), &mut |_| {
        calls.set(calls.get() + 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(calls.get(), 0);

    start_app(args(registry.clone(), false), &mut |app| {
        calls.set(calls.get() + 1);
        assert_eq!(app.registry_path, registry);
        assert_eq!(app.socket, "pm-events");
        Ok(())
    })
    .unwrap();
    assert_eq!(calls.get(), 1);

    let lock_path = lease::pmtui_lock_path(&registry, "pm-events");
    let lock = acquire_free_lease(&lock_path).expect("startup lock should be free");
    start_app(args(registry.clone(), false), &mut |_| {
        panic!("a second dashboard must not launch")
    })
    .unwrap();
    drop(lock);
    wait_until_free(&lock_path);

    let ownership = crate::claim(&registry, "pm-events")
        .unwrap()
        .expect("metadata-bearing dashboard lock should be free");
    start_app(args(registry.clone(), false), &mut |_| {
        panic!("a non-interactive contender must not replace the running dashboard")
    })
    .unwrap();
    drop(ownership);
    wait_until_free(&lock_path);

    let blocker = dir.path().join("not-a-directory");
    std::fs::write(&blocker, "file").unwrap();
    start_app(args(blocker.join("registry.json"), false), &mut |_| {
        calls.set(calls.get() + 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(calls.get(), 2);

    wait_until_free(&lock_path);
    let error = start_app(args(registry, false), &mut |_| {
        anyhow::bail!("dashboard failed")
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "dashboard failed");
}

#[test]
fn argument_defaults_and_failures_are_covered_with_the_event_adapter() {
    let defaults = parse_args_from(&[]).unwrap();
    assert_eq!(defaults.socket, "pmd");
    assert!(!defaults.help);

    let values = [
        "--registry",
        "/tmp/events.json",
        "--socket",
        "isolated",
        "-h",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let parsed = parse_args_from(&values).unwrap();
    assert_eq!(parsed.registry, PathBuf::from("/tmp/events.json"));
    assert_eq!(parsed.socket, "isolated");
    assert!(parsed.help);

    for values in [
        vec!["--registry".to_string()],
        vec!["--socket".to_string()],
        vec!["--unknown".to_string()],
    ] {
        assert!(parse_args_from(&values).is_err());
    }

    assert_eq!(
        registry_path_for_home(None),
        PathBuf::from("./.config/pmd/registry.json")
    );
    assert_eq!(
        registry_path_for_home(Some(PathBuf::from("/home/test"))),
        PathBuf::from("/home/test/.config/pmd/registry.json")
    );
}

#[test]
fn left_clicks_while_composing_never_retarget_attach_or_reopen_the_field() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    app.begin_send();
    assert!(matches!(app.mode, UiMode::Sending { .. }), "{}", app.status);
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 0, 30, 20),
        detail: Rect::new(30, 0, 70, 20),
    });
    app.row_hits.replace(vec![(4, 0)]);
    app.preview_attach_hit.set(Rect::new(31, 0, 24, 1));
    app.composer_hit.set(Rect::new(31, 15, 60, 4));
    let selected = app.selected;

    for (column, row) in [(5, 4), (40, 0), (40, 16)] {
        let mut handled = false;
        assert!(!handle_event(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), column, row),
            &mut handled,
        ));
        assert!(
            !handled,
            "click at {column},{row} must not act while composing"
        );
        assert!(matches!(app.mode, UiMode::Sending { .. }));
        assert_eq!(app.selected, selected);
        assert!(app.pending_attach_loop.is_none());
    }
}

#[test]
fn a_stale_task_card_hit_opens_nothing_once_the_board_is_closed() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.board_hits
        .replace(vec![(Rect::new(0, 0, 20, 5), "bot".to_string())]);
    let mut handled = false;

    handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 2),
        &mut handled,
    );

    assert!(matches!(app.mode, UiMode::Normal));
    assert!(!app.board_detail_open);
}

#[test]
fn input_discarded_after_a_fork_propagates_poll_and_read_errors() {
    let broken = || std::io::Error::other("tty gone");

    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.pending_fork = Some(false);
    let error = drain_events(&mut app, |_| Err(broken()), || unreachable!()).unwrap_err();
    assert!(error.to_string().contains("tty gone"), "{error:#}");

    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.pending_fork = Some(false);
    let error = drain_events(&mut app, |_| Ok(true), || Err(broken())).unwrap_err();
    assert!(error.to_string().contains("tty gone"), "{error:#}");
}
