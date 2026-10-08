//! Keyboard dispatch across text fields and full-screen modes.

use super::*;

/// A one-line field over a live row, for the SHARED `edit_active_field` keys.
///
/// It was the goal field until that became a `ratatui-textarea` (whose motions are the library's, and
/// tested in `tests::composer`). Rename is the surviving surface with the same mechanism: a [`Field`],
/// a caret, and nothing else.
fn one_line_field(text: &str) -> App {
    app_with(
        vec![agent_loop_view("bot")],
        UiMode::Renaming {
            id: "bot".into(),
            current: None,
            input: Field::from(text),
        },
    )
}

#[test]
fn shared_field_keys_edit_at_the_caret_and_stop_at_boundaries() {
    let mut app = one_line_field("abc");

    handle_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::End, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Delete, KeyModifiers::NONE);

    let UiMode::Renaming { input, .. } = &app.mode else {
        panic!("editing keys changed mode");
    };
    assert_eq!(input.as_str(), "Xc");
    assert_eq!(input.caret(), 2);
}

#[test]
fn answer_keys_tolerate_no_options_and_page_the_question() {
    let mut app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    app.scroll_max.set(25);

    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
    let UiMode::Answering { choice, scroll, .. } = &app.mode else {
        unreachable!();
    };
    assert_eq!(*choice, 0);
    assert_eq!(*scroll, ANSWER_PAGE_ROWS.min(25));

    handle_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
    let UiMode::Answering { scroll, .. } = &app.mode else {
        unreachable!();
    };
    assert_eq!(*scroll, 0);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn create_directory_confirmation_routes_confirm_and_cancel_keys() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let missing = dir.path().join("cancelled");
    let mut cancel = creating_loop_app(
        &reg_path,
        &missing,
        Engine::Claude,
        Tier::Standard,
        "goal",
        300,
    );
    cancel.submit_create();
    handle_key(&mut cancel, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(cancel.mode, UiMode::Creating(_)));
    assert!(!missing.exists());

    let created = dir.path().join("created");
    let mut confirm = creating_loop_app(
        &reg_path,
        &created,
        Engine::Codex,
        Tier::Standard,
        "goal",
        300,
    );
    confirm.submit_create();
    handle_key(&mut confirm, KeyCode::Char('Y'), KeyModifiers::NONE);
    assert!(created.exists());
    assert!(matches!(confirm.mode, UiMode::Normal));
}

#[test]
fn decision_lane_navigation_clamps_and_returns_to_normal() {
    let mut app = app_with(
        vec![autopilot_loop_view("bot")],
        UiMode::Decisions {
            tab: AuditTab::Decisions,
            scroll: 0,
            other_scroll: 0,
            since: None,
            id: "bot".into(),
        },
    );
    app.scroll_max.set(25);

    for code in [KeyCode::Up, KeyCode::Char('k'), KeyCode::PageUp] {
        handle_key(&mut app, code, KeyModifiers::NONE);
    }
    let UiMode::Decisions { scroll, .. } = &app.mode else {
        unreachable!();
    };
    assert_eq!(*scroll, 12);

    handle_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    let UiMode::Decisions { scroll, .. } = &app.mode else {
        unreachable!();
    };
    assert_eq!(*scroll, 25);
    handle_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::End, KeyModifiers::NONE);
    let UiMode::Decisions { scroll, .. } = &app.mode else {
        unreachable!();
    };
    assert_eq!(*scroll, 0);

    handle_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(app.status, "left the decision lane");
}

#[test]
fn bracketed_paste_covers_empty_directive_and_send_fields() {
    let mut app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingDirective {
            id: "bot".into(),
            directive: PathBuf::from("/tmp/directive.md"),
            input: directive_buf(""),
        },
    );
    handle_paste(&mut app, "\n\t");
    let UiMode::EditingDirective { input, .. } = &app.mode else {
        unreachable!();
    };
    assert!(input.is_empty(), "sanitized empty paste is ignored");

    handle_paste(&mut app, "never merge");
    let UiMode::EditingDirective { input, .. } = &app.mode else {
        unreachable!();
    };
    assert_eq!(input.text(), "never merge");

    app.mode = UiMode::Sending {
        target: send_target(true),
        input: msg_buf(""),
    };
    handle_paste(&mut app, "check status");
    let UiMode::Sending { input, .. } = &app.mode else {
        unreachable!();
    };
    assert_eq!(input.text(), "check status");
}

#[test]
fn overlay_specific_unhandled_keys_are_safe_noops() {
    let mut answering = app_with(
        vec![agent_loop_view("bot")],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    handle_key(&mut answering, KeyCode::Tab, KeyModifiers::NONE);
    assert!(matches!(answering.mode, UiMode::Answering { .. }));

    let mut cadence = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingCadence {
            id: "bot".into(),
            root: PathBuf::from("/tmp"),
            then_autopilot: false,
            current: 300,
            input: Field::new(),
        },
    );
    handle_key(&mut cadence, KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(cadence.mode, UiMode::EditingCadence { .. }));

    let mut goal = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingGoal {
            id: "bot".into(),
            brief: PathBuf::from("/tmp/brief.md"),
            current: String::new(),
            input: goal_buf("goal"),
            then_autopilot: false,
        },
    );
    handle_key(&mut goal, KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(goal.mode, UiMode::EditingGoal { .. }));

    let mut directive = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingDirective {
            id: "bot".into(),
            directive: PathBuf::from("/tmp/directive.md"),
            input: directive_buf(""),
        },
    );
    handle_key(&mut directive, KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(directive.mode, UiMode::EditingDirective { .. }));

    let mut sending = app_with(
        vec![agent_loop_view("bot")],
        UiMode::Sending {
            target: send_target(true),
            input: msg_buf(""),
        },
    );
    handle_key(&mut sending, KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(sending.mode, UiMode::Sending { .. }));
}

#[test]
fn wake_decision_help_and_picker_modes_own_their_remaining_keys() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    let mut wake = app_with(
        vec![agent_loop_view("bot")],
        UiMode::WakeView {
            id: "bot".into(),
            paths,
            scroll: 7,
        },
    );
    wake.scroll_max.set(25);
    handle_key(&mut wake, KeyCode::PageDown, KeyModifiers::NONE);
    handle_key(&mut wake, KeyCode::End, KeyModifiers::NONE);
    handle_key(&mut wake, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(matches!(wake.mode, UiMode::WakeView { scroll: 0, .. }));

    let mut decisions = app_with(
        vec![autopilot_loop_view("bot")],
        UiMode::Decisions {
            tab: AuditTab::Decisions,
            scroll: 4,
            other_scroll: 0,
            since: None,
            id: "bot".into(),
        },
    );
    handle_key(&mut decisions, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(matches!(
        decisions.mode,
        UiMode::Decisions { scroll: 4, .. }
    ));

    let mut help = app_with(vec![agent_loop_view("bot")], UiMode::Help { scroll: 0 });
    handle_key(&mut help, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(matches!(help.mode, UiMode::Normal));

    let mut picker = app_with(
        vec![agent_loop_view("bot")],
        UiMode::ModelPicker {
            target: PickTarget::Worker,
            id: "bot".into(),
            stage: PickStage::Model,
            engine: Engine::Claude,
            cursor: 0,
            stored_model: None,
        },
    );
    handle_key(&mut picker, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(matches!(picker.mode, UiMode::ModelPicker { .. }));
}

#[test]
fn explicit_restart_confirmation_routes_to_the_restart_action() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session = session_name("bot", &root);
    let pane = FakePane::default();
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Confirming {
            id: "bot".into(),
            session,
            what: Confirmable::Restart,
        },
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path;

    handle_key(&mut app, KeyCode::Char('Y'), KeyModifiers::NONE);

    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(pane.launches().len(), 1);
    assert!(app.status.contains("restarted"), "{}", app.status);
}

#[test]
fn create_key_routing_covers_text_toggles_submit_cancel_and_wrong_mode() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let mut cancelled = creating_loop_app(
        &reg_path,
        &project,
        Engine::Claude,
        Tier::Standard,
        "goal",
        300,
    );
    handle_create_key(&mut cancelled, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(cancelled.mode, UiMode::Normal));

    let mut submitted = creating_loop_app(
        &reg_path,
        &project,
        Engine::Codex,
        Tier::Standard,
        "goal",
        300,
    );
    handle_create_key(&mut submitted, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(submitted.mode, UiMode::Normal));

    handle_create_key(&mut submitted, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(matches!(submitted.mode, UiMode::Normal));

    let mut text = creating_loop_app(
        &dir.path().join("text-registry.json"),
        &project,
        Engine::Claude,
        Tier::Standard,
        "goal",
        300,
    );
    let UiMode::Creating(form) = &mut text.mode else {
        unreachable!();
    };
    form.field = 2;
    handle_create_key(&mut text, KeyCode::BackTab, KeyModifiers::NONE);
    let UiMode::Creating(form) = &mut text.mode else {
        unreachable!();
    };
    form.field = 2;
    handle_create_key(&mut text, KeyCode::Home, KeyModifiers::NONE);
    handle_create_key(&mut text, KeyCode::Delete, KeyModifiers::NONE);
    handle_create_key(&mut text, KeyCode::Left, KeyModifiers::NONE);
    handle_create_key(&mut text, KeyCode::Right, KeyModifiers::NONE);
    handle_create_key(&mut text, KeyCode::End, KeyModifiers::NONE);
    handle_create_key(&mut text, KeyCode::F(1), KeyModifiers::NONE);

    let mut engine = creating_loop_app(
        &dir.path().join("engine-registry.json"),
        &project,
        Engine::Claude,
        Tier::Autopilot,
        "goal",
        300,
    );
    engine.model_catalog.insert(
        Engine::Codex,
        vec![ModelInfo {
            label: "gpt".into(),
            value: "gpt".into(),
        }],
    );
    let UiMode::Creating(form) = &mut engine.mode else {
        unreachable!();
    };
    form.field = 0;
    handle_create_key(&mut engine, KeyCode::Right, KeyModifiers::NONE);
    let UiMode::Creating(form) = &engine.mode else {
        unreachable!();
    };
    assert_eq!(form.engine, Engine::Codex);
    assert_eq!(form.model_choices.len(), 1);

    engine.model_catalog.insert(
        Engine::Codex,
        vec![ModelInfo {
            label: "decider".into(),
            value: "decider".into(),
        }],
    );
    let UiMode::Creating(form) = &mut engine.mode else {
        unreachable!();
    };
    form.field = CreateForm::DECIDER;
    form.decider_engine = Engine::Claude;
    handle_create_key(&mut engine, KeyCode::Right, KeyModifiers::NONE);
    let UiMode::Creating(form) = &engine.mode else {
        unreachable!();
    };
    assert_eq!(form.decider_engine, Engine::Codex);
    assert_eq!(form.decider_model_choices.len(), 1);
}

/// A PASTE KEEPS ITS LINES IN A PROSE BUFFER AND IS FLATTENED IN A ONE-LINE FIELD.
///
/// Both halves matter. `handle_paste` sanitized every field with the one-line cleaner, so the three
/// buffers silently lost the line breaks of anything pasted into them — while `tests::composer` passed,
/// because it calls `insert_str` directly and never goes through here. And a one-line `Field` still has
/// to be flattened: it cannot render a newline or move a caret past one.
#[test]
fn a_paste_keeps_its_lines_only_where_a_buffer_can_hold_them() {
    let block = "first\n\tsecond\nthird";

    for (name, mode) in [
        (
            "message",
            UiMode::Sending {
                target: send_target(true),
                input: msg_buf(""),
            },
        ),
        (
            "goal",
            UiMode::EditingGoal {
                id: "bot".into(),
                brief: PathBuf::from("/tmp/brief.md"),
                current: String::new(),
                input: goal_buf(""),
                then_autopilot: false,
            },
        ),
        (
            "directive",
            UiMode::EditingDirective {
                id: "bot".into(),
                directive: PathBuf::from("/tmp/directive.md"),
                input: directive_buf(""),
            },
        ),
    ] {
        let mut app = app_with(vec![agent_loop_view("bot")], mode);
        handle_paste(&mut app, block);
        let text = match &app.mode {
            UiMode::Sending { input, .. }
            | UiMode::EditingGoal { input, .. }
            | UiMode::EditingDirective { input, .. } => input.text(),
            m => panic!("{name}: paste changed the mode to {m:?}"),
        };
        assert_eq!(
            text, "first\n second\nthird",
            "{name} must keep the line breaks (the TAB still becomes a space)"
        );
    }

    // The one-line fields are unchanged: flattened, because a caret cannot navigate a newline.
    let mut app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::Renaming {
            id: "bot".into(),
            current: None,
            input: Field::new(),
        },
    );
    handle_paste(&mut app, block);
    let UiMode::Renaming { input, .. } = &app.mode else {
        unreachable!();
    };
    assert_eq!(input.as_str(), "first second third");
}
