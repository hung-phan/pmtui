use super::*;

fn named_app(name: Option<&str>) -> (tempfile::TempDir, PathBuf, PathBuf, App) {
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_agent_loop(dir.path(), "bot");
    if let Some(name) = name {
        Registry::update(&registry, |registry| {
            registry.projects[0].display_name = Some(name.into());
        })
        .unwrap();
    }
    let app = loop_app(&registry);
    (dir, registry, root, app)
}

fn rename_input(app: &mut App, value: &str) {
    let UiMode::Renaming { input, .. } = &mut app.mode else {
        panic!("rename field did not open");
    };
    *input = Field::from(value);
}

#[test]
fn display_name_normalization_accepts_clear_and_rejects_invalid_values() {
    assert_eq!(
        agent_manager::registry::normalize_display_name("  Release work  ").unwrap(),
        Some("Release work".into())
    );
    assert_eq!(
        agent_manager::registry::normalize_display_name("   ").unwrap(),
        None
    );
    assert!(agent_manager::registry::normalize_display_name("bad\nname").is_err());
    assert!(agent_manager::registry::normalize_display_name(&"x".repeat(65)).is_err());
}

#[test]
fn uppercase_r_renames_without_changing_runtime_identity() {
    let (_dir, registry, root, mut app) = named_app(None);
    let session = session_name("bot", &root);
    let state_dir = ProjectPaths::for_session(&root, "bot").state_dir();

    handle_key(&mut app, KeyCode::Char('R'), KeyModifiers::SHIFT);
    rename_input(&mut app, "Release coordinator");
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    let entry = Registry::load(&registry).unwrap().projects.remove(0);
    assert_eq!(entry.id, "bot");
    assert_eq!(entry.display_name.as_deref(), Some("Release coordinator"));
    assert_eq!(session_name(&entry.id, &entry.root), session);
    assert_eq!(
        ProjectPaths::for_session(&entry.root, &entry.id).state_dir(),
        state_dir
    );
    assert_eq!(
        app.selected_view().map(ProjectView::label),
        Some("Release coordinator")
    );
    assert!(matches!(app.mode, UiMode::Normal));

    let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert!(screen_text(&terminal).contains("Release coordinator"));
    app.mode = UiMode::Board;
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert!(screen_text(&terminal).contains("Release coordinator"));
}

#[test]
fn board_rename_is_clickable_returns_to_board_and_blank_clears() {
    let (_dir, registry, _root, mut app) = named_app(Some("Old name"));
    app.mode = UiMode::Board;
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let rename = app
        .key_hits
        .borrow()
        .iter()
        .find(|hit| hit.code == KeyCode::Char('R'))
        .cloned()
        .expect("Rename card action");
    let mut handled = false;

    handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rename.area.x,
            row: rename.area.y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(handled);
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("Rename · bot"), "{screen}");
    assert!(screen.contains("current: Old name"), "{screen}");
    rename_input(&mut app, "");
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert!(matches!(app.mode, UiMode::Board));
    assert!(
        Registry::load(&registry).unwrap().projects[0]
            .display_name
            .is_none()
    );
    assert_eq!(app.selected_view().map(ProjectView::label), Some("bot"));
}

#[test]
fn rename_cancel_and_write_failures_keep_existing_metadata() {
    let (_dir, registry, _root, mut app) = named_app(Some("Keep me"));
    handle_key(&mut app, KeyCode::Char('R'), KeyModifiers::SHIFT);
    rename_input(&mut app, "Discard me");
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(
        Registry::load(&registry).unwrap().projects[0]
            .display_name
            .as_deref(),
        Some("Keep me")
    );

    handle_key(&mut app, KeyCode::Char('R'), KeyModifiers::SHIFT);
    rename_input(&mut app, "Cannot save");
    std::fs::write(&registry, "not-json").unwrap();
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Renaming { .. }));
    assert!(app.status.contains("could not rename"), "{}", app.status);
}

#[test]
fn create_form_persists_the_optional_name() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut app = creating_agent_loop_app(&registry, &project, Engine::Claude, "", 300);
    let UiMode::Creating(form) = &mut app.mode else {
        panic!("create form missing");
    };
    form.name = Field::from("Release coordinator");
    let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert!(screen_text(&terminal).contains("Name"));

    app.submit_create();

    assert_eq!(
        Registry::load(&registry).unwrap().projects[0]
            .display_name
            .as_deref(),
        Some("Release coordinator")
    );
}

#[test]
fn rename_reports_a_removed_target_without_recreating_it() {
    let (_dir, registry, _root, mut app) = named_app(None);
    handle_key(&mut app, KeyCode::Char('R'), KeyModifiers::SHIFT);
    rename_input(&mut app, "Lost session");
    Registry::update(&registry, |registry| registry.projects.clear()).unwrap();

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert!(matches!(app.mode, UiMode::Normal));
    assert!(app.status.contains("gone from the session list"));
    assert!(Registry::load(&registry).unwrap().projects.is_empty());
}

#[test]
fn empty_board_and_invalid_persisted_names_fall_back_safely() {
    let mut empty = app_with(Vec::new(), UiMode::Board);
    handle_key(&mut empty, KeyCode::Char('R'), KeyModifiers::SHIFT);
    assert!(matches!(empty.mode, UiMode::Board));
    assert!(!empty.return_to_board_after_action);

    for invalid in ["   ".to_string(), "bad\nname".to_string(), "x".repeat(65)] {
        let (_dir, registry, _root, mut app) = named_app(None);
        Registry::update(&registry, |registry| {
            registry.projects[0].display_name = Some(invalid);
        })
        .unwrap();
        app.refresh();
        assert_eq!(app.selected_view().map(ProjectView::label), Some("bot"));
    }
}

#[test]
fn rename_entrypoints_cover_invalid_input_paste_and_inactive_calls() {
    let (_dir, _registry, _root, mut app) = named_app(None);
    let status = app.status.clone();
    app.submit_rename();
    assert_eq!(app.status, status);

    handle_key(&mut app, KeyCode::Char('R'), KeyModifiers::SHIFT);
    handle_paste(&mut app, "Pasted name");
    let UiMode::Renaming { input, .. } = &app.mode else {
        panic!("rename field closed after paste");
    };
    assert_eq!(input.as_str(), "Pasted name");

    rename_input(&mut app, "bad\nname");
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Renaming { .. }));
    assert!(app.status.contains("control characters"));
}
