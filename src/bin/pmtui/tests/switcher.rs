//! `/` quick session switcher: filtering, stable-id selection, rendering, and pointer parity.

use super::*;

fn items() -> Vec<SwitchItem> {
    vec![
        SwitchItem {
            id: "alpha-session".into(),
            label: "Alpha release".into(),
            root: "/workplace/demo/alpha".into(),
        },
        SwitchItem {
            id: "beta-session".into(),
            label: "Beta tests".into(),
            root: "/workplace/demo/beta".into(),
        },
    ]
}

fn switcher_app(selected: usize) -> App {
    let mut app = app_with(
        vec![
            agent_loop_view("alpha-session"),
            agent_loop_view("beta-session"),
        ],
        UiMode::Normal,
    );
    app.selected = selected;
    app.mode = UiMode::Switching {
        query: Field::new(),
        cursor: selected,
        items: items(),
    };
    app
}

#[test]
fn filtering_is_case_insensitive_over_session_and_project_path() {
    assert_eq!(switch_matches(&items(), "ALPHA"), vec![0]);
    assert_eq!(switch_matches(&items(), "beta"), vec![1]);
    assert_eq!(switch_matches(&items(), "release"), vec![0]);
    assert_eq!(switch_matches(&items(), ""), vec![0, 1]);
    assert!(switch_matches(&items(), "missing").is_empty());
}

#[test]
fn slash_opens_on_the_current_session_and_empty_list_is_honest() {
    let mut app = app_with(
        vec![
            agent_loop_view("alpha-session"),
            agent_loop_view("beta-session"),
        ],
        UiMode::Normal,
    );
    app.selected = 1;
    handle_key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Switching { cursor: 1, .. }));

    let mut empty = app_with(vec![], UiMode::Normal);
    handle_key(&mut empty, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(matches!(empty.mode, UiMode::Normal));
    assert!(empty.status.contains("no sessions"));
}

#[test]
fn opening_reads_registered_roots_for_directory_search() {
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_agent_loop(dir.path(), "alpha-session");
    let mut app = app_with(vec![agent_loop_view("alpha-session")], UiMode::Normal);
    app.registry_path = registry;
    app.begin_switcher();
    let UiMode::Switching { items, .. } = &app.mode else {
        panic!("switcher did not open");
    };
    assert_eq!(items[0].root, root.display().to_string());
}

#[test]
fn typing_resets_the_cursor_and_enter_selects_by_stable_id_after_reorder() {
    let mut app = switcher_app(1);
    handle_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Switching { cursor: 0, .. }));

    app.projects.swap(0, 1);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("alpha-session")
    );
}

#[test]
fn caret_motion_keeps_the_result_cursor_while_paste_resets_it() {
    let mut app = switcher_app(0);
    app.mode = UiMode::Switching {
        query: Field::new(),
        cursor: 1,
        items: items(),
    };
    handle_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Switching { cursor: 1, .. }));

    handle_paste(&mut app, "black");
    let UiMode::Switching { query, cursor, .. } = &app.mode else {
        panic!("switcher closed");
    };
    assert_eq!(*cursor, 0);
    assert_eq!(query.as_str(), "black");
}

#[test]
fn page_navigation_clamps_to_filtered_results() {
    let mut app = switcher_app(0);
    app.move_switch_cursor(app::scroll::PAGE_LINES as isize);
    assert!(matches!(app.mode, UiMode::Switching { cursor: 1, .. }));
    app.move_switch_cursor(-(app::scroll::PAGE_LINES as isize));
    assert!(matches!(app.mode, UiMode::Switching { cursor: 0, .. }));
}

#[test]
fn cursor_helpers_cover_empty_results_and_non_switcher_calls() {
    let mut app = switcher_app(0);
    app.mode = UiMode::Switching {
        query: Field::from("absent"),
        cursor: 7,
        items: items(),
    };
    app.move_switch_cursor(1);
    assert!(matches!(app.mode, UiMode::Switching { cursor: 0, .. }));

    app.mode = UiMode::Normal;
    assert_eq!(app.switch_match_count(), 0);
    app.move_switch_cursor(1);
    app.choose_switch_cursor();
    assert!(matches!(app.mode, UiMode::Normal));
    app.choose_switch_result(0);
    assert_eq!(app.status, "no session matches the search");
}

#[test]
fn switcher_ignores_modified_text_keys() {
    let mut app = switcher_app(0);

    handle_key(
        &mut app,
        KeyCode::Char('x'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );

    let UiMode::Switching {
        query,
        cursor,
        items,
    } = &app.mode
    else {
        panic!("modified key closed the switcher");
    };
    assert!(query.is_empty());
    assert_eq!(*cursor, 0);
    assert_eq!(items.len(), 2);
}

#[test]
fn escape_and_no_match_leave_the_previous_selection_unchanged() {
    let mut app = switcher_app(1);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.selected, 1);

    app.mode = UiMode::Switching {
        query: Field::from("absent"),
        cursor: 0,
        items: items(),
    };
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Switching { .. }));
    assert_eq!(app.selected, 1);
    assert!(app.status.contains("no session matches"));
}

#[test]
fn board_switch_selection_returns_to_the_board() {
    let mut app = switcher_app(0);
    app.return_to_board_after_switch = true;

    app.choose_switch_result(1);

    assert!(matches!(app.mode, UiMode::Board));
    assert!(!app.return_to_board_after_switch);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("beta-session")
    );
}

#[test]
fn a_removed_result_fails_without_selecting_a_replacement() {
    let mut app = switcher_app(0);
    app.projects = vec![agent_loop_view("beta-session")];
    app.selected = 0;
    app.choose_switch_result(0);
    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("beta-session")
    );
    assert!(app.status.contains("alpha-session is gone"));
}

#[test]
fn rendered_result_click_commits_through_the_same_selection_method() {
    let mut app = switcher_app(1);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).expect("render");
    assert!(
        screen_text(&terminal).contains("Alpha release · alpha-session"),
        "switcher must disambiguate display name with stable id"
    );
    let hit = app.switch_hits.borrow()[0].0;
    let mut handled = false;
    handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(handled);
    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("alpha-session")
    );
}

#[test]
fn switcher_renders_an_explicit_empty_result() {
    let mut app = switcher_app(0);
    app.mode = UiMode::Switching {
        query: Field::from("absent"),
        cursor: 0,
        items: items(),
    };
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).expect("render");
    assert!(screen_text(&terminal).contains("No sessions match"));
    assert!(app.switch_hits.borrow().is_empty());
}

#[test]
fn long_project_paths_keep_the_identifying_tail_visible() {
    let mut app = switcher_app(0);
    app.mode = UiMode::Switching {
        query: Field::new(),
        cursor: 0,
        items: vec![SwitchItem {
            id: "bot".into(),
            label: "Build bot".into(),
            root: format!("/{}important-project", "目录/".repeat(30)),
        }],
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
    terminal.draw(|frame| render(frame, &app)).expect("render");
    assert!(screen_text(&terminal).contains("important-project"));
}

#[test]
fn result_hit_regions_are_frame_local() {
    let mut app = switcher_app(0);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).expect("render");
    assert!(!app.switch_hits.borrow().is_empty());

    app.mode = UiMode::Normal;
    terminal.draw(|frame| render(frame, &app)).expect("render");
    assert!(app.switch_hits.borrow().is_empty());
}

#[test]
fn every_dashboard_hit_region_clears_before_a_too_small_frame_returns() {
    let app = app_with(vec![agent_loop_view("alpha-session")], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).expect("render");
    assert!(!app.row_hits.borrow().is_empty());
    assert_ne!(app.panes.get(), PaneRects::default());

    let mut tiny = Terminal::new(TestBackend::new(20, 4)).unwrap();
    tiny.draw(|frame| render(frame, &app)).expect("render");
    assert!(app.row_hits.borrow().is_empty());
    assert!(app.create_hits.borrow().is_empty());
    assert!(app.key_hits.borrow().is_empty());
    assert!(app.switch_hits.borrow().is_empty());
    assert_eq!(app.preview_attach_hit.get(), Rect::ZERO);
    assert_eq!(app.panes.get(), PaneRects::default());
}
