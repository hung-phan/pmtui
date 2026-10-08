//! Dashboard answers for persisted pane dialogs. The rendered stop drops the pane
//! metadata, so both `a` and submission must re-read the AgentLoop ledger and fail
//! closed unless it still proves an ordinary dashboard-answerable choice.

use super::*;

fn pane_dialog_stop(options: &[&str], dashboard_answerable: bool) -> pmstate::OpenStop {
    let mut stop = open_stop("stop-bot-dialog-1000", pmstate::StopKind::Capability);
    stop.question = Some("Which option should I choose?".into());
    stop.options = options.iter().map(|option| (*option).to_string()).collect();
    stop.pane_dialog = Some(pmstate::PaneDialogStop {
        fingerprint: 42,
        dashboard_answerable,
    });
    stop
}

fn blocked_dialog_app(stop: pmstate::OpenStop) -> (tempfile::TempDir, ProjectPaths, App) {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let paths = ProjectPaths::for_session(&root, "bot");
    set_tier(&paths, Tier::Autopilot);
    let stop_id = stop.id.clone();
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    ledger.run = job::JobRun::Blocked {
        stop_ids: vec![stop_id],
        since: 1000,
    };
    ledger.open_stops = vec![stop];
    job::save(&paths, &ledger).unwrap();
    let app = loop_app(&reg_path);
    (dir, paths, app)
}

fn assert_attach_only(app: &App, paths: &ProjectPaths) {
    assert!(
        matches!(app.mode, UiMode::Normal),
        "attach-only dialogs must not leave an answer overlay open: {:?}",
        app.mode
    );
    assert!(
        app.status.contains("Enter") && app.status.contains("attach"),
        "the refusal must direct the human to Enter/attach: {}",
        app.status
    );
    assert!(
        !paths.answers().exists(),
        "an attach-only dialog must not append an answer"
    );
}

#[test]
fn a_refuses_a_legacy_pane_dialog_without_a_safety_snapshot() {
    let mut stop = pane_dialog_stop(&["prettier", "dprint"], true);
    stop.pane_dialog = None;
    let (_dir, paths, mut app) = blocked_dialog_app(stop);

    app.begin_answer();

    assert_attach_only(&app, &paths);
}

#[test]
fn a_refuses_a_human_only_pane_dialog() {
    let stop = pane_dialog_stop(&["Yes", "No"], false);
    let (_dir, paths, mut app) = blocked_dialog_app(stop);

    app.begin_answer();

    assert_attach_only(&app, &paths);
}

#[test]
fn a_refuses_a_held_pane_dialog_after_terminal_selection_started() {
    let mut stop = pane_dialog_stop(&["prettier", "dprint"], true);
    stop.status = pmstate::StopStatus::Held;
    let (_dir, paths, mut app) = blocked_dialog_app(stop);

    app.begin_answer();

    assert_attach_only(&app, &paths);
}

#[test]
fn a_opens_for_a_persisted_dashboard_answerable_pane_dialog() {
    let stop = pane_dialog_stop(&["prettier", "dprint"], true);
    let (_dir, _paths, mut app) = blocked_dialog_app(stop);

    app.begin_answer();

    assert!(
        matches!(app.mode, UiMode::Answering { .. }),
        "an ordinary persisted choice should open the dashboard overlay: {}",
        app.status
    );
}

#[test]
fn a_refuses_when_the_displayed_dialog_disappeared_from_the_ledger() {
    let stop = pane_dialog_stop(&["prettier", "dprint"], true);
    let (_dir, paths, mut app) = blocked_dialog_app(stop);
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.open_stops.clear();
    job::save(&paths, &ledger).unwrap();

    app.begin_answer();

    assert_attach_only(&app, &paths);
}

#[test]
fn submission_rechecks_held_status_and_does_not_retry_an_answer() {
    let stop = pane_dialog_stop(&["prettier", "dprint"], true);
    let (_dir, paths, mut app) = blocked_dialog_app(stop);
    app.begin_answer();
    assert!(matches!(app.mode, UiMode::Answering { .. }));

    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.open_stops[0].status = pmstate::StopStatus::Held;
    job::save(&paths, &ledger).unwrap();
    app.submit_answer();

    assert_attach_only(&app, &paths);
}

#[test]
fn submission_refuses_when_the_dialog_disappeared_from_the_ledger() {
    let stop = pane_dialog_stop(&["prettier", "dprint"], true);
    let (_dir, paths, mut app) = blocked_dialog_app(stop);
    app.mode = UiMode::Answering {
        input: Field::new(),
        choice: 0,
        scroll: 0,
    };
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.open_stops.clear();
    job::save(&paths, &ledger).unwrap();

    app.submit_answer();

    assert_attach_only(&app, &paths);
}

#[test]
fn submission_rechecks_a_dialog_that_becomes_legacy_or_human_only() {
    for pane_dialog in [
        None,
        Some(pmstate::PaneDialogStop {
            fingerprint: 42,
            dashboard_answerable: false,
        }),
    ] {
        let stop = pane_dialog_stop(&["prettier", "dprint"], true);
        let (_dir, paths, mut app) = blocked_dialog_app(stop);
        app.begin_answer();
        assert!(matches!(app.mode, UiMode::Answering { .. }));

        let mut ledger = job::load(&paths).unwrap().unwrap();
        ledger.open_stops[0].pane_dialog = pane_dialog;
        job::save(&paths, &ledger).unwrap();
        app.submit_answer();

        assert_attach_only(&app, &paths);
    }
}

#[test]
fn submission_refuses_typed_text_for_a_pane_dialog() {
    let stop = pane_dialog_stop(&["prettier", "dprint"], true);
    let (_dir, paths, mut app) = blocked_dialog_app(stop);
    app.mode = UiMode::Answering {
        input: "use rustfmt instead".into(),
        choice: 0,
        scroll: 0,
    };

    app.submit_answer();

    assert_attach_only(&app, &paths);
}

#[test]
fn submission_refuses_meta_and_free_text_pane_options() {
    for option in ["Type something.", "Chat about this", "Custom answer"] {
        let stop = pane_dialog_stop(&[option], true);
        let (_dir, paths, mut app) = blocked_dialog_app(stop);
        app.mode = UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        };

        app.submit_answer();

        assert_attach_only(&app, &paths);
    }
}

#[test]
fn dashboard_answerable_radio_choice_appends_the_exact_option() {
    let stop = pane_dialog_stop(&["prettier", "dprint"], true);
    let (_dir, paths, mut app) = blocked_dialog_app(stop);
    app.mode = UiMode::Answering {
        input: Field::new(),
        choice: 1,
        scroll: 0,
    };

    app.submit_answer();

    let answers: Vec<Answer> = state::read_json(&paths.answers()).unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].answer, "dprint");
}

#[test]
fn dashboard_answerable_single_unchecked_checkbox_appends_the_exact_option() {
    let stop = pane_dialog_stop(&["integration tests"], true);
    let (_dir, paths, mut app) = blocked_dialog_app(stop);
    app.mode = UiMode::Answering {
        input: Field::new(),
        choice: 0,
        scroll: 0,
    };

    app.submit_answer();

    let answers: Vec<Answer> = state::read_json(&paths.answers()).unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].answer, "integration tests");
}
