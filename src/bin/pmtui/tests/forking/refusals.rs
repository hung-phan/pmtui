//! Every precondition a fork checks before it reserves anything — an unknown or disagreeing
//! conversation, a working, attached or input-locked source, unreadable human inputs — plus the
//! reservation and staging failures that must leave no invisible state behind.

use super::*;

#[test]
fn fork_refuses_unknown_disagreeing_and_working_sources_without_launching() {
    let mut missing = fork_fixture(Engine::Claude);
    Registry::update(&missing.registry, |registry| {
        registry.projects[0].conversation_id = None;
    })
    .unwrap();
    let paths = ProjectPaths::for_session(&missing.root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.conversation_id = None;
    job::save(&paths, &ledger).unwrap();
    missing.app.fork_selected();
    assert!(missing.app.status.contains("no conversation to fork"));
    assert!(missing.pane.launches().is_empty());

    let mut disagreeing = fork_fixture(Engine::Claude);
    let paths = ProjectPaths::for_session(&disagreeing.root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.conversation_id = Some("ffffffff-eeee-4ddd-8ccc-bbbbbbbbbbbb".into());
    job::save(&paths, &ledger).unwrap();
    disagreeing.app.fork_selected();
    assert!(disagreeing.app.status.contains("disagrees"));
    assert!(disagreeing.pane.launches().is_empty());

    let mut working = fork_fixture(Engine::Claude);
    working.app.projects[0].session_live = true;
    working.app.projects[0].agent_working = Some(true);
    working.app.fork_selected();
    assert!(working.app.status.contains("still working"));
    assert!(working.pane.launches().is_empty());

    let mut corrupt = fork_fixture(Engine::Claude);
    let paths = ProjectPaths::for_session(&corrupt.root, "bot");
    std::fs::write(paths.pmstate(), "{not json").unwrap();
    corrupt.app.fork_selected();
    assert!(corrupt.app.status.contains("runtime state unreadable"));
    assert!(corrupt.pane.launches().is_empty());

    let mut engine_mismatch = fork_fixture(Engine::Claude);
    Registry::update(&engine_mismatch.registry, |registry| {
        registry.projects[0].engine = Some(Engine::Codex);
    })
    .unwrap();
    engine_mismatch.app.fork_selected();
    assert!(engine_mismatch.app.status.contains("engine"));
    assert!(engine_mismatch.app.status.contains("disagrees"));
    assert!(engine_mismatch.pane.launches().is_empty());
}

#[test]
fn fork_refuses_attached_or_input_locked_sources_and_discards_failed_children() {
    let mut attached = fork_fixture(Engine::Claude);
    attached.app.projects[0].human_attached = true;
    attached.app.fork_selected();
    assert!(attached.app.status.contains("human is attached"));
    assert!(attached.pane.launches().is_empty());

    let mut locked = fork_fixture(Engine::Claude);
    let source_paths = ProjectPaths::for_session(&locked.root, "bot");
    let held = lease::try_acquire(&source_paths.input_lock())
        .unwrap()
        .expect("source input lock");
    locked.app.fork_selected();
    assert!(locked.app.status.contains("receiving input"));
    assert!(locked.pane.launches().is_empty());
    drop(held);

    let mut lock_error = fork_fixture(Engine::Claude);
    let source_paths = ProjectPaths::for_session(&lock_error.root, "bot");
    std::fs::create_dir_all(source_paths.input_lock()).unwrap();
    lock_error.app.fork_selected();
    assert!(lock_error.app.status.contains("could not lock its input"));

    let mut failed_launch = fork_fixture(Engine::Claude);
    let child_session = session_name("bot-fork", &failed_launch.root);
    let failed_driver = FakePane::with_codex_session(&child_session, CHILD_ID)
        .with(|inner| inner.fail_launch = true);
    failed_launch.app.agent_tmux = Box::new(failed_driver.clone());
    failed_launch.pane = failed_driver;
    failed_launch.app.fork_selected();
    assert_fork_discarded(&failed_launch);
    assert_eq!(
        failed_launch.pane.terminated(),
        vec![session_name("bot-fork", &failed_launch.root)]
    );
    assert!(
        failed_launch
            .app
            .status
            .contains("could not start bot-fork")
    );

    let reused_id = SOURCE_ID.to_ascii_uppercase();
    let mut reused_source = fork_fixture_with_identity(Engine::Claude, &reused_id);
    let child_session = session_name("bot-fork", &reused_source.root);
    let cleanup_fails = FakePane::with_codex_session(&child_session, &reused_id)
        .with(|inner| inner.fail_terminate = true);
    install_driver(&mut reused_source, cleanup_fails);
    reused_source.app.fork_selected();
    let registry = Registry::load(&reused_source.registry).unwrap();
    let child = registry
        .projects
        .iter()
        .find(|entry| entry.id == "bot-fork")
        .expect("a child whose terminal could not be stopped stays visible");
    assert!(!child.enabled && child.conversation_id.is_none());
    assert!(reused_source.pane.terminated().len() == 1);
    assert!(
        reused_source
            .app
            .status
            .contains("engine returned the source conversation id"),
        "{}",
        reused_source.app.status
    );
    assert!(
        reused_source
            .app
            .status
            .contains("could not stop its terminal")
            && reused_source.app.status.contains("delete it"),
        "{}",
        reused_source.app.status
    );
}

#[test]
fn fork_refuses_missing_selection_registry_row_and_corrupt_human_inputs() {
    let mut no_selection = app_with(Vec::new(), UiMode::Normal);
    no_selection.fork_selected();
    assert_eq!(no_selection.status, "no session selected");

    let mut unreadable_registry = fork_fixture(Engine::Claude);
    unreadable_registry.app.registry_path = unreadable_registry._dir.path().to_path_buf();
    unreadable_registry.app.fork_selected();
    assert!(
        unreadable_registry
            .app
            .status
            .contains("not creating a fork")
    );

    let mut missing_row = fork_fixture(Engine::Claude);
    Registry::update(&missing_row.registry, |registry| registry.projects.clear()).unwrap();
    missing_row.app.fork_selected();
    assert!(
        missing_row
            .app
            .status
            .contains("gone from the session list")
    );

    let mut corrupt_config = fork_fixture(Engine::Claude);
    let paths = ProjectPaths::for_session(&corrupt_config.root, "bot");
    std::fs::write(paths.config(), "{").unwrap();
    corrupt_config.app.fork_selected();
    assert!(corrupt_config.app.status.contains("settings unreadable"));

    let mut bad_goal = fork_fixture(Engine::Claude);
    let paths = ProjectPaths::for_session(&bad_goal.root, "bot");
    std::fs::remove_file(paths.brief()).unwrap();
    std::fs::create_dir(paths.brief()).unwrap();
    bad_goal.app.fork_selected();
    assert!(bad_goal.app.status.contains("goal unreadable"));

    let mut bad_directive = fork_fixture(Engine::Claude);
    let paths = ProjectPaths::for_session(&bad_directive.root, "bot");
    std::fs::remove_file(paths.directive()).unwrap();
    std::fs::create_dir(paths.directive()).unwrap();
    bad_directive.app.fork_selected();
    assert!(bad_directive.app.status.contains("directive unreadable"));
}

#[test]
fn fork_handles_missing_optional_text_and_reservation_or_staging_failures() {
    let mut no_text = fork_fixture(Engine::Claude);
    let paths = ProjectPaths::for_session(&no_text.root, "bot");
    std::fs::remove_file(paths.brief()).unwrap();
    std::fs::remove_file(paths.directive()).unwrap();
    no_text.app.fork_selected();
    let child = ProjectPaths::for_session(&no_text.root, "bot-fork");
    assert_eq!(std::fs::read_to_string(child.brief()).unwrap(), "");
    assert!(!child.directive().exists());

    let mut reserve_failure = fork_fixture(Engine::Claude);
    let sessions = reserve_failure.root.join(state::STATE_DIR).join("sessions");
    use std::os::unix::fs::PermissionsExt;
    let original = std::fs::metadata(&sessions).unwrap().permissions();
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o555)).unwrap();
    reserve_failure.app.fork_selected();
    std::fs::set_permissions(&sessions, original).unwrap();
    assert!(reserve_failure.app.status.contains("could not reserve"));

    let mut staging_failure = fork_fixture(Engine::Claude);
    let registry_parent = staging_failure.registry.parent().unwrap();
    let original = std::fs::metadata(registry_parent).unwrap().permissions();
    std::fs::set_permissions(registry_parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    staging_failure.app.fork_selected();
    std::fs::set_permissions(registry_parent, original).unwrap();
    assert!(staging_failure.app.status.contains("could not stage"));
    assert!(
        !ProjectPaths::for_session(&staging_failure.root, "bot-fork")
            .state_dir()
            .exists(),
        "an unstaged reservation must not become invisible preserved state"
    );
}
