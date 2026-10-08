use super::*;

fn board_app() -> App {
    let mut needs = view("needs", Posture::NeedsYou, vec![]);
    needs.engine = Some(Engine::Claude);
    needs.work_summary = Some("Choose the release strategy".into());
    needs.project_name = Some("pmtui".into());
    let mut working = view("working", Posture::Running, vec![]);
    working.engine = Some(Engine::Codex);
    working.work_summary = Some("Implement task board inspector".into());
    working.project_name = Some("pmtui".into());
    working.session_live = true;
    working.agent_working = Some(true);
    let mut autopilot = autopilot_loop_view("autopilot");
    autopilot.posture = Posture::Monitoring;
    autopilot.agent_working = Some(false);
    autopilot.work_summary = Some("Keep release checks green".into());
    autopilot.project_name = Some("pmtui".into());
    autopilot.forked_from = Some("working".into());
    autopilot.next_action = "check in 00:04:07".into();
    let mut pending = agent_loop_view("pending");
    pending.posture = Posture::Fresh;
    pending.last_activity = None;
    pending.work_summary = Some("Draft the release checklist".into());
    pending.project_name = Some("pmtui".into());
    let mut paused = agent_loop_view("paused");
    paused.enabled = false;
    app_with(
        vec![needs, working, autopilot, pending, paused],
        UiMode::Normal,
    )
}

#[test]
fn task_board_theme_uses_semantic_lanes_metadata_and_selection_surface() {
    let mut app = board_app();
    app.open_board();
    let mut terminal = Terminal::new(TestBackend::new(150, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();

    for (header, color) in [
        ("NEEDS YOU 1", agent_manager::theme::soft()),
        ("WORKING 1", agent_manager::theme::live()),
        ("AUTOPILOT 1", agent_manager::theme::accent()),
        ("PENDING 1", agent_manager::theme::accent_alt()),
        ("PAUSED 1", agent_manager::theme::rule()),
    ] {
        let styles = styles_under_row(&terminal, header, header).unwrap_or_else(|| {
            panic!("missing lane header {header:?}: {}", screen_text(&terminal))
        });
        assert!(
            styles.iter().all(|(fg, _)| *fg == color),
            "{header} lost {color:?}: {styles:?}"
        );
    }

    let claude = styles_under_row(&terminal, "claude · auto", "claude").expect("Claude metadata");
    assert!(
        claude
            .iter()
            .all(|(fg, _)| *fg == agent_manager::theme::brand())
    );
    let auto = styles_under_row(&terminal, "claude · auto", "auto").expect("autopilot metadata");
    assert!(
        auto.iter()
            .all(|(fg, _)| *fg == agent_manager::theme::accent())
    );

    let (autopilot_row, autopilot_styles) = screen_rows_styled(&terminal)
        .into_iter()
        .find(|(text, _)| text.contains("autopilot"))
        .expect("Autopilot card title");
    let id_byte = autopilot_row.find("autopilot").unwrap();
    let id_start = autopilot_row[..id_byte].chars().count();
    let id_styles = &autopilot_styles[id_start..id_start + "autopilot".len()];
    // A card's id is ONE STYLE across every cell. It used to be swept by a per-frame TachyonFX
    // gradient, so each cell held a different hue — that is the thing that had to go, not colour
    // itself: the id now wears the card's own themed style, and the lane, the `[A]` tag and the
    // `control pmd` line are what say a row is driven.
    let first_style = id_styles.first().copied().expect("an id to measure");
    assert!(
        id_styles.iter().all(|style| *style == first_style),
        "the autopilot id is still painted per cell: {id_styles:?}"
    );
    let corner_byte = autopilot_row[..id_byte]
        .char_indices()
        .filter_map(|(index, ch)| (ch == '┌').then_some(index))
        .next_back()
        .expect("Autopilot card corner");
    let corner = autopilot_row[..corner_byte].chars().count();
    assert_eq!(
        autopilot_styles[corner].0,
        agent_manager::theme::accent(),
        "TachyonFX leaked from identity text onto the card border"
    );

    let buf = terminal.backend().buffer();
    let selected_row = (buf.area.top()..buf.area.bottom())
        .find_map(|y| {
            let cells: Vec<_> = (buf.area.left()..buf.area.right())
                .filter_map(|x| buf.cell((x, y)))
                .collect();
            let text: String = cells.iter().map(|cell| cell.symbol()).collect();
            let byte = text.find("Choose the release")?;
            let start = text[..byte].chars().count();
            Some(cells[start..start + "Choose the release".len()].to_vec())
        })
        .expect("selected task title");
    assert!(
        selected_row
            .iter()
            .all(|cell| cell.bg == agent_manager::theme::surface_bg()),
        "selected task did not use the raised theme surface"
    );
}

#[test]
fn board_columns_partition_every_session_from_runtime_truth() {
    let app = board_app();
    let columns: Vec<_> = app.projects.iter().map(board_column).collect();
    assert_eq!(
        columns,
        vec![
            BoardColumn::NeedsYou,
            BoardColumn::Working,
            BoardColumn::Autopilot,
            BoardColumn::Pending,
            BoardColumn::Paused,
        ]
    );
    for column in BoardColumn::ALL {
        assert_eq!(columns.iter().filter(|seen| **seen == column).count(), 1);
    }
}

#[test]
fn task_working_requires_confirmed_agent_activity() {
    let mut unknown_autopilot = autopilot_loop_view("auto-unknown");
    unknown_autopilot.posture = Posture::Monitoring;
    unknown_autopilot.agent_working = None;
    assert_eq!(board_column(&unknown_autopilot), BoardColumn::Autopilot);

    let mut unknown_standard = agent_loop_view("standard-unknown");
    unknown_standard.session_live = true;
    unknown_standard.agent_working = None;
    assert_eq!(board_column(&unknown_standard), BoardColumn::Pending);

    let mut confirmed = unknown_autopilot;
    confirmed.agent_working = Some(true);
    assert_eq!(board_column(&confirmed), BoardColumn::Working);
}

#[test]
fn task_summary_prefers_task_title_then_goal_then_initial_message() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let paths = ProjectPaths::for_session(&root, "task");
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    std::fs::write(paths.brief(), "Goal from brief\nmore").unwrap();
    let mut entry = ProjectEntry {
        id: "task".into(),
        display_name: None,
        root,
        enabled: true,
        mode: Mode::AgentLoop,
        engine: Some(Engine::Claude),
        worker_model: None,
        initial_prompt: Some("Initial message".into()),
        task_title: Some("Explicit task title".into()),
        forked_from: None,
        spawned_by: None,
        launch: None,
        conversation_id: None,
        cadence_s: None,
    };

    assert_eq!(
        session_work_summary(&entry, &paths).as_deref(),
        Some("Explicit task title")
    );
    entry.task_title = None;
    assert_eq!(
        session_work_summary(&entry, &paths).as_deref(),
        Some("Goal from brief")
    );
    std::fs::remove_file(paths.brief()).unwrap();
    assert_eq!(
        session_work_summary(&entry, &paths).as_deref(),
        Some("Initial message")
    );
}

#[test]
fn board_navigation_moves_within_and_across_nonempty_columns() {
    let mut app = board_app();
    app.projects
        .insert(1, view("needs-2", Posture::Stuck, vec![]));
    app.open_board();
    assert!(matches!(app.mode, UiMode::Board));

    app.board_move_vertical(1);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("needs-2")
    );
    app.board_move_horizontal(1);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("pending")
    );
    app.board_move_horizontal(1);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("autopilot")
    );
    app.board_move_horizontal(1);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("working")
    );
    app.board_move_horizontal(1);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("working")
    );
    app.board_move_horizontal(-1);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("autopilot")
    );
    app.board_move_horizontal(-1);
    app.board_move_horizontal(-1);
    app.board_move_horizontal(-1);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("paused")
    );
}

/// SWAPPING VIEWS COSTS THE LOG NOTHING. `record_status` appends the status line to the log after
/// every key, and `1`/`2` are the two keys pressed most, so the log filled with `Task Board` /
/// `opened <id>` — entries for having looked around (user: *"when we swap between 1, and 2. i see the
/// status get log, can we remove that?"*).
///
/// The real outcome of the last ACTION must survive the trip, so this drives a failure into the
/// status first and then swaps views both ways: the log keeps that one line and gains no other.
#[test]
fn swapping_between_the_two_views_adds_nothing_to_the_status_log() {
    let mut app = app_with(
        vec![agent_loop_view("alpha"), agent_loop_view("beta")],
        UiMode::Normal,
    );
    app.status = "could not pause alpha".into();
    app.record_status();
    let after_failure = log_lines(&app);

    for _ in 0..3 {
        handle_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        assert!(matches!(app.mode, UiMode::Board), "2 did not open Tasks");
        handle_key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
        assert!(
            matches!(app.mode, UiMode::Normal),
            "1 did not return to Sessions"
        );
    }
    // Enter a card's detail and leave it again: that is navigation too.
    handle_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);

    assert_eq!(
        log_lines(&app),
        after_failure,
        "view navigation wrote to the log"
    );
    assert_eq!(
        app.status, "could not pause alpha",
        "the last action's outcome did not survive the view switches"
    );
}

#[test]
fn board_empty_and_sparse_navigation_are_bounded() {
    let mut empty = app_with(Vec::new(), UiMode::Normal);
    let before = empty.status.clone();
    empty.open_board();
    assert!(matches!(empty.mode, UiMode::Board));
    // Opening a view says nothing of its own: the lanes already read `no tasks` and the top bar
    // offers `+ Task`.
    assert_eq!(
        empty.status, before,
        "opening an empty board wrote a status"
    );
    empty.record_status();
    empty.board_move_vertical(1);
    empty.board_move_horizontal(1);
    empty.open_board_session("missing");
    assert!(!empty.board_wheel_column(BoardColumn::Working, true));
    empty.close_board_detail();
    empty.close_board();
    assert!(matches!(empty.mode, UiMode::Normal));
    empty.open_board();
    handle_key(&mut empty, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(empty.mode, UiMode::Creating(_)));
    assert!(empty.return_to_board_after_create);

    let needs = confirm_done_view();
    let mut pending = agent_loop_view("pending");
    pending.posture = Posture::Fresh;
    pending.last_activity = None;
    let mut sparse = app_with(vec![needs, pending], UiMode::Board);
    sparse.board_move_horizontal(1);
    assert_eq!(
        sparse.selected_view().map(|view| view.id.as_str()),
        Some("pending"),
        "horizontal navigation should skip empty intermediate task columns"
    );
}

#[test]
fn board_responsively_pages_columns_and_keeps_fork_lineage_visible() {
    for (width, expected, absent) in [
        (63, "NEEDS YOU", "PENDING"),
        (80, "PAUSED", "PENDING"),
        (100, "NEEDS YOU", "PENDING"),
        (120, "PENDING", "WORKING"),
        (180, "AUTOPILOT", "never absent"),
    ] {
        let mut app = board_app();
        app.open_board();
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let screen = screen_text(&terminal);
        assert!(screen.contains(expected), "{width}: {screen}");
        if width < 180 {
            assert!(!screen.contains(absent), "{width}: {screen}");
        }
    }

    let mut app = board_app();
    app.selected = 2;
    app.open_board();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("Keep release checks"), "{screen}");
    assert!(screen.contains("fork:working"), "{screen}");
}

#[test]
fn task_view_uses_full_width_and_reuses_the_detail_inspector() {
    let mut app = board_app();
    app.open_board();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();
    let board = screen_text(&terminal);
    assert!(!board.contains("SESSIONS"), "{board}");
    assert!(board.contains("NEEDS YOU"), "{board}");
    assert_eq!(app.panes.get().sessions, Rect::ZERO);

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let detail = screen_text(&terminal);
    assert!(app.board_detail_open);
    assert!(app.panes.get().detail.width > 0);
    assert!(detail.contains("preview"), "{detail}");
    assert!(detail.contains("Message"), "{detail}");
    assert!(!detail.contains("NEEDS YOU 2"), "{detail}");

    let mut narrow = board_app();
    narrow.open_board();
    narrow.board_detail_open = true;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| render(frame, &narrow)).unwrap();
    assert!(narrow.board_column_hits.borrow().is_empty());
    assert_eq!(narrow.panes.get().detail.width, 80);

    let mut paused = board_app();
    paused.selected = 4;
    paused.open_board();
    paused.board_detail_open = true;
    // Detail's keybar also carries the latest status, so it needs laptop width for chip labels.
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &paused)).unwrap();
    let detail = screen_text(&terminal);
    assert!(detail.contains("Resume"), "{detail}");
    assert!(!detail.contains("Pause"), "{detail}");
    assert!(
        paused
            .key_hits
            .borrow()
            .iter()
            .any(|hit| hit.code == KeyCode::Enter)
    );
}

#[test]
fn task_create_is_intent_first_and_keeps_board_behind_the_form() {
    let mut app = board_app();
    app.open_board();
    app.begin_task_create();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let rows: Vec<String> = screen_rows(&terminal)
        .into_iter()
        .map(|(row, _)| row)
        .collect();
    let message = rows
        .iter()
        .position(|row| row.contains("Message"))
        .expect("Message row");
    let engine = rows
        .iter()
        .position(|row| row.contains("Engine"))
        .expect("Engine row");
    assert!(
        message < engine,
        "Task intent must render before runtime fields"
    );
    assert!(screen_text(&terminal).contains("New task"));
}

#[test]
fn task_board_and_new_are_discoverable_on_the_laptop_workbench() {
    let mut app = board_app();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let screen = screen_text(&terminal);
    assert!(screen.contains("Tasks"), "{screen}");
    assert!(screen.contains("New"), "{screen}");
    // The view tabs lead the header; the action row keeps only the actions.
    assert!(
        screen.contains(" pmtui    1  Sessions   2  Tasks "),
        "{screen}"
    );
    assert!(screen.contains("/  Switch ·  n  New"), "{screen}");
    assert!(
        screen.contains("pmd DOWN"),
        "daemon state was hidden: {screen}"
    );
    for code in [
        KeyCode::Char('/'),
        KeyCode::Char('1'),
        KeyCode::Char('2'),
        KeyCode::Char('n'),
    ] {
        assert!(
            app.top_hits.borrow().iter().any(|hit| hit.code == code),
            "{code:?} is not a clickable top control: {screen}"
        );
    }
    let tasks = app
        .top_hits
        .borrow()
        .iter()
        .find(|hit| hit.code == KeyCode::Char('2'))
        .cloned()
        .expect("Tasks top control");
    let mut handled = false;
    handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: tasks.area.x,
            row: tasks.area.y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(handled);
    assert!(matches!(app.mode, UiMode::Board));

    terminal.draw(|frame| render(frame, &app)).unwrap();
    let switch = app
        .top_hits
        .borrow()
        .iter()
        .find(|hit| hit.code == KeyCode::Char('/'))
        .cloned()
        .expect("Switch top control");
    handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: switch.area.x,
            row: switch.area.y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(matches!(app.mode, UiMode::Switching { .. }));
    assert!(app.return_to_board_after_switch);
}

#[test]
fn board_cards_wrap_long_next_steps_instead_of_truncating_them() {
    let mut pending = agent_loop_view("pending");
    pending.posture = Posture::Fresh;
    pending.last_activity = None;
    pending.work_summary = Some("Prepare a detailed verification report".into());
    pending.next_action =
        "review the generated artifact after verification completes successfully".into();
    let app = app_with(vec![pending], UiMode::Board);
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let screen = screen_text(&terminal);
    assert!(screen.contains("review the generated artifact"), "{screen}");
    assert!(screen.contains("verification"), "{screen}");
    assert!(screen.contains("completes successfully"), "{screen}");
    assert!(!screen.contains('…'), "{screen}");
    assert!(
        screen.contains("┌ ▸ ○ pending") && screen.contains('└'),
        "the task itself must have a visible frame: {screen}"
    );
}

#[test]
fn board_decision_cards_name_confirmation_and_waiting_age() {
    let mut view = confirm_done_view();
    view.oldest_stop_since = Some(SystemClock.now() - 3 * 3_600);
    let (cue, detail, filled) = board_card_cue(&view, SystemClock.now());

    assert_eq!(cue, "READY TO CLOSE");
    assert_eq!(detail.as_deref(), Some("waiting 3h"));
    assert!(filled);
    assert_eq!(board_column(&view), BoardColumn::NeedsYou);

    let app = app_with(vec![view], UiMode::Board);
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("Goal 'tell me animal joke'"), "{screen}");
    assert!(!screen.contains("dispatch task-4"), "{screen}");
}

#[test]
fn clicking_a_board_card_selects_it_and_updates_bottom_actions() {
    let mut app = board_app();
    app.selected = 4;
    app.open_board();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let hit = app
        .board_hits
        .borrow()
        .iter()
        .find(|(_, id)| id == "paused")
        .cloned()
        .expect("paused card hitbox");
    let event = Event::Mouse(ratatui::crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.0.x,
        row: hit.0.y,
        modifiers: KeyModifiers::NONE,
    });
    app.projects.swap(0, 4);
    let mut handled = false;

    handle_event(&mut app, event, &mut handled);

    assert!(handled);
    assert!(matches!(app.mode, UiMode::Board));
    assert!(!app.board_detail_open);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("paused")
    );

    terminal.draw(|frame| render(frame, &app)).unwrap();
    for code in [KeyCode::Enter, KeyCode::Char('d')] {
        assert!(
            app.key_hits.borrow().iter().any(|hit| hit.code == code),
            "selected card bottom menu is missing {code:?}"
        );
    }
}

#[test]
fn selected_card_restart_and_delete_run_without_opening_detail() {
    let dir = tempfile::tempdir().unwrap();
    let (registry, _root) = reg_with_agent_loop(dir.path(), "bot");
    let make_app = || {
        let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Board);
        app.registry_path = registry.clone();
        app
    };
    let click_action = |app: &mut App, code: KeyCode| {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        assert!(!app.board_detail_open);
        let hit = app
            .key_hits
            .borrow()
            .iter()
            .find(|hit| hit.code == code)
            .cloned()
            .unwrap_or_else(|| panic!("missing bottom action {code:?}"));
        let mut handled = false;
        handle_event(
            app,
            Event::Mouse(ratatui::crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.area.x,
                row: hit.area.y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut handled,
        );
        assert!(handled);
    };

    let mut restart = make_app();
    click_action(&mut restart, KeyCode::Char('r'));
    assert!(matches!(
        restart.mode,
        UiMode::Confirming {
            what: Confirmable::Restart,
            ..
        }
    ));

    let mut delete = make_app();
    click_action(&mut delete, KeyCode::Char('d'));
    assert!(matches!(
        delete.mode,
        UiMode::Confirming {
            what: Confirmable::Remove,
            ..
        }
    ));
}

#[test]
fn board_action_chips_are_clickable_and_route_through_board_keys() {
    let mut app = board_app();
    app.open_board();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let hits = app.key_hits.borrow().clone();
    for code in [
        KeyCode::Enter,
        KeyCode::Char('r'),
        KeyCode::Char('d'),
        KeyCode::Char('f'),
    ] {
        assert!(hits.iter().any(|hit| hit.code == code), "missing {code:?}");
    }
    assert!(line_text(&keybar_line(&app, 100)).contains("Enter  Open"));
    handle_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("pending")
    );

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert!(app.board_detail_open);
    assert!(
        app.key_hits
            .borrow()
            .iter()
            .any(|hit| hit.code == KeyCode::Char('s'))
    );
}

#[test]
fn board_bottom_menu_tracks_the_selected_card_state() {
    let mut paused = agent_loop_view("paused");
    paused.enabled = false;
    let needs = confirm_done_view();
    let pending = agent_loop_view("pending");
    let mut autopilot = autopilot_loop_view("autopilot");
    autopilot.posture = Posture::Monitoring;
    autopilot.agent_working = Some(false);
    let mut working = view("working", Posture::Running, vec![]);
    working.session_live = true;
    working.agent_working = Some(true);

    for (view, expected, expected_keys) in [
        (
            paused,
            vec!["Open", "Resume", "Restart", "Delete", "Fork", "Rename"],
            vec![
                KeyCode::Enter,
                KeyCode::Char('p'),
                KeyCode::Char('r'),
                KeyCode::Char('d'),
                KeyCode::Char('f'),
                KeyCode::Char('R'),
            ],
        ),
        (
            needs,
            vec!["Open", "Answer", "Restart", "Delete", "Fork", "Rename"],
            vec![
                KeyCode::Enter,
                KeyCode::Char('s'),
                KeyCode::Char('r'),
                KeyCode::Char('d'),
                KeyCode::Char('f'),
                KeyCode::Char('R'),
            ],
        ),
        (
            pending,
            vec!["Open", "Restart", "Delete", "Fork", "Rename"],
            vec![
                KeyCode::Enter,
                KeyCode::Char('r'),
                KeyCode::Char('d'),
                KeyCode::Char('f'),
                KeyCode::Char('R'),
            ],
        ),
        (
            autopilot,
            vec![
                "Open", "Pause", "Restart", "Delete", "Fork", "Rename", "Audit",
            ],
            vec![
                KeyCode::Enter,
                KeyCode::Char('p'),
                KeyCode::Char('r'),
                KeyCode::Char('d'),
                KeyCode::Char('f'),
                KeyCode::Char('R'),
                KeyCode::Char('v'),
            ],
        ),
        (
            // A working agent cannot be forked, so the lane does not offer it.
            working,
            vec!["Open", "Pause", "Restart", "Delete", "Rename"],
            vec![
                KeyCode::Enter,
                KeyCode::Char('p'),
                KeyCode::Char('r'),
                KeyCode::Char('d'),
                KeyCode::Char('R'),
            ],
        ),
    ] {
        let app = app_with(vec![view], UiMode::Board);
        let text = line_text(&keybar_line(&app, 100));
        for label in expected {
            assert!(text.contains(label), "{label} missing from {text}");
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert_eq!(
            app.key_hits
                .borrow()
                .iter()
                .map(|hit| hit.code)
                .collect::<Vec<_>>(),
            expected_keys,
            "selected card published the wrong actions: {text}"
        );
    }
}

#[test]
fn a_staged_spawn_card_offers_only_the_actions_its_keys_accept() {
    // A paused card offers Resume, Restart and Fork (above); a staged one sits in the same lane,
    // but every start refuses it until its spawn request launches it.
    let dir = tempfile::tempdir().unwrap();
    let (registry, _root) = reg_with_agent_loop(dir.path(), "kid");
    let mut app = app_with(vec![staged_spawn_view("kid")], UiMode::Board);
    app.registry_path = registry;
    let published = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        app.key_hits.borrow().clone()
    };

    let lane = line_text(&keybar_line(&app, 100));
    for kept in ["Open", "Delete", "Rename"] {
        assert!(lane.contains(kept), "{kept} missing: {lane}");
    }
    for refused in ["Resume", "Restart", "Fork", "Pause", "Send", "Answer"] {
        assert!(!lane.contains(refused), "the lane offers {refused}: {lane}");
    }
    assert_eq!(
        published(&app)
            .iter()
            .map(|hit| hit.code)
            .collect::<Vec<_>>(),
        vec![KeyCode::Enter, KeyCode::Char('d'), KeyCode::Char('R')]
    );

    // The lane's `f` chip and key read one rule, so the key refuses before it queues a fork.
    assert_eq!(
        fork_refusal(app.selected_view().unwrap()).as_deref(),
        Some("kid is still being created by a spawn request")
    );
    handle_key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(app.pending_fork.is_none(), "{}", app.status);
    assert_eq!(app.status, "kid is still being created by a spawn request");
    assert!(matches!(app.mode, UiMode::Board));

    // Detail: Enter would resume it and `m` would start Autopilot, so Rename and Back remain.
    app.board_detail_open = true;
    let detail = line_text(&keybar_line(&app, 100));
    for refused in ["Resume", "Attach", "Mode", "Send", "Answer"] {
        assert!(
            !detail.contains(refused),
            "detail offers {refused}: {detail}"
        );
    }
    assert_eq!(
        published(&app)
            .iter()
            .map(|hit| hit.code)
            .collect::<Vec<_>>(),
        vec![KeyCode::Char('R'), KeyCode::Esc]
    );

    // Delete still acts on the card, and its click runs the same handler as `d`.
    app.board_detail_open = false;
    let delete = published(&app)
        .into_iter()
        .find(|hit| hit.code == KeyCode::Char('d'))
        .expect("the lane publishes Delete");
    let mut handled = false;
    handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: delete.area.x,
            row: delete.area.y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(handled);
    assert!(
        matches!(
            app.mode,
            UiMode::Confirming {
                what: Confirmable::Remove,
                ..
            }
        ),
        "{:?}",
        app.mode
    );
}

#[test]
fn board_message_starts_from_detail_where_the_transcript_is_visible() {
    let dir = tempfile::tempdir().unwrap();
    let (registry, _root) = reg_with_agent_loop(dir.path(), "pending");
    let pending = agent_loop_view("pending");
    let mut app = app_with(vec![pending], UiMode::Board);
    app.registry_path = registry;

    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Sending { .. }));
    assert!(app.return_to_board_after_send);
}

/// A Board action's failure is still readable, and now it is also KEPT. It used to need a ` STATUS `
/// pane below the lanes; the pane is gone, so the keybar shows the line and the log file holds it
/// along with every line before it (which the twenty-entry pane could not).
#[test]
fn a_board_action_failure_shows_on_the_keybar_and_lands_in_the_log() {
    let mut app = board_app();
    seed_log(
        &app,
        &["status 0: an earlier Board action", "status 1: another"],
    );
    app.open_board();
    app.status = "could not fork selected task: conversation is still working".into();
    app.record_status();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let screen = screen_text(&terminal);
    assert!(screen.contains("could not fork selected"), "{screen}");
    // No pane, so no pane title and no rows taken from the lanes.
    assert!(
        !screen.contains("STATUS"),
        "the status pane is back: {screen}"
    );
    let log = log_lines(&app);
    assert_eq!(
        log.last().map(String::as_str),
        Some("could not fork selected task: conversation is still working"),
        "{log:?}"
    );
    assert_eq!(log.len(), 3, "earlier lines were dropped: {log:?}");
}

/// Every row of the Task view's body belongs to the lanes, or to a card's detail — the status pane
/// used to take the bottom rows whenever there was history to show.
#[test]
fn the_task_view_body_is_never_shortened_by_a_status_pane() {
    let mut app = board_app();
    seed_log(&app, &["retained action result"]);
    app.status = "retained action result".into();
    app.open_board();
    app.board_detail_open = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    // 28 rows less the top bar and the keybar.
    assert_eq!(app.panes.get().detail.height, 26);
    assert!(screen_text(&terminal).contains("preview"));
}

#[test]
fn stale_board_return_flags_fall_through_to_the_active_mode() {
    let mut app = board_app();
    app.mode = UiMode::Normal;
    app.return_to_board_after_create = true;
    app.return_to_board_after_send = true;
    app.return_to_board_after_answer = true;
    app.return_to_board_after_switch = true;
    app.return_to_board_after_action = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    assert!(screen_text(&terminal).contains("SESSIONS"));
}

#[test]
fn task_origin_switcher_and_rename_render_over_the_board() {
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();

    let mut switcher = board_app();
    switcher.open_board();
    handle_key(&mut switcher, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(switcher.return_to_board_after_switch);
    terminal.draw(|frame| render(frame, &switcher)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("Switch session"), "{screen}");
    assert!(screen.contains("NEEDS YOU 1"), "{screen}");

    let mut rename = board_app();
    rename.open_board();
    handle_key(&mut rename, KeyCode::Char('R'), KeyModifiers::SHIFT);
    assert!(rename.return_to_board_after_action);
    terminal.draw(|frame| render(frame, &rename)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("Rename \u{b7} needs"), "{screen}");
    assert!(screen.contains("NEEDS YOU 1"), "{screen}");
}

#[test]
fn mouse_wheel_moves_tasks_in_the_column_under_the_pointer() {
    let mut app = board_app();
    let second = view("needs-2", Posture::Stuck, vec![]);
    app.projects.insert(1, second);
    app.open_board();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let needs = app
        .board_column_hits
        .borrow()
        .iter()
        .find(|(_, column)| *column == BoardColumn::NeedsYou)
        .map(|(area, _)| *area)
        .expect("Needs You column");
    let mut handled = false;

    handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: needs.x + 1,
            row: needs.y + 2,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );

    assert!(handled);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("needs-2")
    );
}

#[test]
fn first_wheel_on_another_column_focuses_without_skipping_its_first_task() {
    let mut app = board_app();
    app.projects
        .insert(2, view("working-2", Posture::Running, vec![]));
    app.open_board();
    let mut terminal = Terminal::new(TestBackend::new(180, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let working = app
        .board_column_hits
        .borrow()
        .iter()
        .find(|(_, column)| *column == BoardColumn::Working)
        .map(|(area, _)| *area)
        .expect("Working column");
    let mut handled = false;

    handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: working.x + 1,
            row: working.y + 2,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );

    assert!(handled);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("working")
    );
}

/// Enter draws one card's detail over the whole work area — and the movement keys went on moving
/// the selection underneath it, so the detail silently became another card's. User: *"when in the
/// board view and i tap enter, it enter the big screen, however the navigation key is still active.
/// i don't want that to happen"*. Every movement key is inert while a detail is open; closing it
/// hands them all back in the same test, so a board that had simply stopped navigating fails here.
#[test]
fn movement_keys_are_inert_while_a_card_detail_is_open() {
    let mut app = board_app();
    // A second Needs You card gives j/k somewhere to go, and the lanes either side of it give
    // h/l somewhere to go — so every key under test would move if it were still live.
    app.projects
        .insert(1, view("needs-2", Posture::Stuck, vec![]));
    app.open_board();
    let selected = |app: &App| app.selected_view().map(|view| view.id.to_string());
    assert_eq!(selected(&app).as_deref(), Some("needs"));

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.board_detail_open);
    for code in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Char('h'),
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::Char('l'),
    ] {
        handle_key(&mut app, code, KeyModifiers::NONE);
        assert_eq!(
            selected(&app).as_deref(),
            Some("needs"),
            "{code:?} moved the card under the open detail"
        );
        assert!(app.board_detail_open, "{code:?} closed the detail");
        assert!(
            matches!(app.mode, UiMode::Board),
            "{code:?} left the Task view"
        );
    }

    // Esc closes the detail, and then the very same keys move again.
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.board_detail_open);
    for (code, want) in [
        (KeyCode::Char('j'), "needs-2"),
        (KeyCode::Char('k'), "needs"),
        (KeyCode::Down, "needs-2"),
        (KeyCode::Up, "needs"),
        (KeyCode::Char('l'), "pending"),
        (KeyCode::Char('h'), "needs"),
        (KeyCode::Right, "pending"),
        (KeyCode::Left, "needs"),
    ] {
        handle_key(&mut app, code, KeyModifiers::NONE);
        assert_eq!(selected(&app).as_deref(), Some(want), "{code:?}");
    }
}

/// The same rule for the pointer: a wheel over a LANE moves that lane's selection, so it must be
/// as inert as j/k while a detail is open. The lane rects on screen belong to the frame before
/// Enter, and a queued wheel event arriving after it must not spend them.
#[test]
fn a_stale_lane_wheel_cannot_move_the_card_under_an_open_detail() {
    let mut app = board_app();
    app.projects
        .insert(1, view("needs-2", Posture::Stuck, vec![]));
    app.open_board();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let needs = app
        .board_column_hits
        .borrow()
        .iter()
        .find(|(_, column)| *column == BoardColumn::NeedsYou)
        .map(|(area, _)| *area)
        .expect("Needs You column");
    let wheel = |column: u16, row: u16| {
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let mut handled = false;
    handle_event(&mut app, wheel(needs.x + 1, needs.y + 2), &mut handled);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("needs"),
        "a stale lane wheel moved the card under the open detail"
    );

    // A wheel over the DETAIL pane still scrolls the detail.
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let detail = app.panes.get().detail;
    assert!(detail.width > 0);
    let mut handled = false;
    handle_event(&mut app, wheel(detail.x + 1, detail.y + 1), &mut handled);
    assert!(handled, "the open detail no longer scrolls under the wheel");
}

#[test]
fn task_origin_confirm_edit_and_audit_actions_return_to_tasks() {
    let mut answer = app_with(
        vec![confirm_done_view()],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    answer.return_to_board_after_answer = true;
    handle_key(&mut answer, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(answer.mode, UiMode::Board));
    assert!(!answer.return_to_board_after_answer);

    let mut confirm = board_app();
    confirm.mode = UiMode::Confirming {
        session: "pm-needs".into(),
        id: "needs".into(),
        what: Confirmable::Restart,
    };
    confirm.return_to_board_after_action = true;
    handle_key(&mut confirm, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(confirm.mode, UiMode::Board));

    let mut goal = board_app();
    goal.mode = UiMode::EditingGoal {
        id: "needs".into(),
        brief: PathBuf::from("/tmp/brief"),
        current: "goal".into(),
        input: goal_buf("goal"),
        then_autopilot: true,
    };
    goal.return_to_board_after_action = true;
    handle_key(&mut goal, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(goal.mode, UiMode::Board));

    let mut audit = board_app();
    audit.mode = UiMode::Decisions {
        tab: AuditTab::Turns,
        scroll: 0,
        other_scroll: 0,
        since: None,
        id: "needs".into(),
    };
    audit.return_to_board_after_action = true;
    handle_key(&mut audit, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(audit.mode, UiMode::Board));
}

#[test]
fn confirmed_task_removal_returns_to_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "needs");
    let mut app = app_with(
        vec![agent_loop_view("needs")],
        UiMode::Confirming {
            session: session_name("needs", &root),
            id: "needs".into(),
            what: Confirmable::Remove,
        },
    );
    app.registry_path = reg_path.clone();
    app.return_to_board_after_action = true;

    handle_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);

    assert!(matches!(app.mode, UiMode::Board));
    assert!(!app.return_to_board_after_action);
    assert!(Registry::load(&reg_path).unwrap().projects.is_empty());
}

#[test]
fn task_actions_without_a_selected_session_clear_their_return_state() {
    let mut app = app_with(Vec::new(), UiMode::Board);
    app.board_detail_open = true;

    for key in [KeyCode::Char('m'), KeyCode::Char('r'), KeyCode::Char('d')] {
        handle_key(&mut app, key, KeyModifiers::NONE);
        assert!(matches!(app.mode, UiMode::Board));
        assert!(!app.return_to_board_after_action);
    }
}

#[test]
fn task_audit_refusal_for_a_standard_session_stays_in_tasks() {
    let mut app = app_with(vec![agent_loop_view("standard")], UiMode::Board);
    app.board_detail_open = true;

    handle_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);

    assert!(matches!(app.mode, UiMode::Board));
    assert!(!app.return_to_board_after_action);
    assert!(app.status.contains("Standard"), "{}", app.status);
}

#[test]
fn every_task_view_action_routes_without_losing_the_task_shell() {
    let mut app = board_app();
    for key in [
        KeyCode::Char(' '),
        KeyCode::Enter,
        KeyCode::Char('s'),
        KeyCode::Char('m'),
        KeyCode::Char('p'),
        KeyCode::Char('r'),
        KeyCode::Char('d'),
        KeyCode::Char('v'),
    ] {
        app.mode = UiMode::Board;
        app.board_detail_open = true;
        handle_key(&mut app, key, KeyModifiers::NONE);
    }
    app.mode = UiMode::Board;
    app.board_detail_open = true;
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));
    assert!(!app.board_detail_open);
    handle_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn task_origin_overlays_render_over_the_task_view() {
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();

    let mut sending = board_app();
    sending.mode = UiMode::Sending {
        target: SendTarget {
            id: "needs".into(),
            root: PathBuf::from("/tmp"),
            session: "pm-needs".into(),
            agent_loop: true,
            driven: true,
            in_chat: false,
        },
        input: Composer::from_text("message".into()),
    };
    sending.board_detail_open = true;
    sending.return_to_board_after_send = true;
    terminal.draw(|frame| render(frame, &sending)).unwrap();
    assert!(screen_text(&terminal).contains("Message"));

    let mut answering = app_with(
        vec![confirm_done_view()],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    answering.board_detail_open = true;
    answering.return_to_board_after_answer = true;
    terminal.draw(|frame| render(frame, &answering)).unwrap();
    assert!(screen_text(&terminal).contains("Answer"));

    let mut goal = board_app();
    goal.mode = UiMode::EditingGoal {
        id: "needs".into(),
        brief: PathBuf::from("/tmp/brief"),
        current: "goal".into(),
        input: goal_buf("goal"),
        then_autopilot: true,
    };
    goal.return_to_board_after_action = true;
    terminal.draw(|frame| render(frame, &goal)).unwrap();
    assert!(screen_text(&terminal).contains("Autopilot"));

    let mut cadence = board_app();
    cadence.mode = UiMode::EditingCadence {
        id: "needs".into(),
        root: PathBuf::from("/tmp"),
        current: 300,
        input: Field::from("300"),
        then_autopilot: true,
    };
    cadence.return_to_board_after_action = true;
    terminal.draw(|frame| render(frame, &cadence)).unwrap();
    assert!(screen_text(&terminal).contains("Autopilot"));

    let mut confirm = board_app();
    confirm.mode = UiMode::Confirming {
        session: "pm-needs".into(),
        id: "needs".into(),
        what: Confirmable::Restart,
    };
    confirm.return_to_board_after_action = true;
    terminal.draw(|frame| render(frame, &confirm)).unwrap();
    assert!(screen_text(&terminal).contains("Restart"));

    let mut create_dir = board_app();
    let mut form = CreateForm::new();
    form.task_mode = true;
    create_dir.mode = UiMode::ConfirmCreateDir {
        form,
        dir: "/tmp/new-task".into(),
    };
    create_dir.return_to_board_after_create = true;
    terminal.draw(|frame| render(frame, &create_dir)).unwrap();
    assert!(screen_text(&terminal).contains("Create directory"));
}

#[test]
fn task_pointer_routes_detail_preview_and_composer_regions() {
    let mut app = board_app();
    app.mode = UiMode::Board;
    app.board_detail_open = true;
    app.panes.set(PaneRects {
        detail: Rect::new(20, 1, 60, 20),
        ..PaneRects::default()
    });
    app.preview_attach_hit.set(Rect::new(21, 2, 10, 1));
    app.composer_hit.set(Rect::new(21, 15, 40, 4));
    let mouse = |kind, column, row| {
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    let mut handled = false;

    assert!(handle_event(
        &mut app,
        mouse(MouseEventKind::ScrollUp, 30, 10),
        &mut handled,
    ));
    app.mode = UiMode::Board;
    app.board_detail_open = true;
    handled = false;
    handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 22, 2),
        &mut handled,
    );
    assert!(handled);

    app.mode = UiMode::Board;
    app.board_detail_open = true;
    handled = false;
    handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 22, 16),
        &mut handled,
    );
    assert!(handled);
}

#[test]
fn board_keys_open_close_and_reuse_the_existing_create_surface() {
    let mut app = board_app();
    handle_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));
    handle_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
    let UiMode::Creating(form) = &app.mode else {
        panic!("Task Board did not open create");
    };
    assert!(form.task_mode);
    assert!(app.return_to_board_after_create);

    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));

    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn every_board_key_routes_without_escaping_into_normal_bindings() {
    let mut app = board_app();
    app.mode = UiMode::Board;
    for key in [
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::Right,
        KeyCode::Left,
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::Char('l'),
        KeyCode::Char('h'),
    ] {
        handle_key(&mut app, key, KeyModifiers::NONE);
        assert!(matches!(app.mode, UiMode::Board));
    }

    handle_key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Switching { .. }));
    assert!(app.return_to_board_after_switch);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));
    assert!(!app.return_to_board_after_switch);
    app.mode = UiMode::Board;
    handle_key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));
    app.mode = UiMode::Board;
    handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));
    app.mode = UiMode::Board;
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));
    assert!(app.board_detail_open);
}

#[test]
fn board_render_handles_tiny_empty_and_zero_sized_columns() {
    let app = app_with(
        vec![view("needs", Posture::NeedsYou, vec![])],
        UiMode::Board,
    );
    let mut tiny = Terminal::new(TestBackend::new(MIN_W - 1, MIN_H - 1)).unwrap();
    tiny.draw(|frame| render(frame, &app)).unwrap();
    assert!(screen_text(&tiny).contains("Terminal too small"));

    let mut terminal = Terminal::new(TestBackend::new(150, 20)).unwrap();
    terminal
        .draw(|frame| {
            render_board_column(frame, &app, BoardColumn::Working, Rect::new(0, 0, 30, 8));
            render_board_column(frame, &app, BoardColumn::NeedsYou, Rect::ZERO);
        })
        .unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("WORKING"), "{screen}");
    assert!(screen.contains("no tasks"), "{screen}");
}
