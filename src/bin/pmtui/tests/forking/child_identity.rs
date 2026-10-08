//! Waiting for the child to report which conversation it opened. Every way that wait can fail —
//! a probe error, an exit, a timeout, a stolen or vanished row — stops the child and discards it.

use super::*;

#[test]
fn fork_identity_probe_errors_stop_the_child_and_discard_its_row() {
    let mut fixture = fork_fixture(Engine::Codex);
    let child_session = session_name("bot-fork", &fixture.root);
    let failing = FakePane::codex_probe_fails(&child_session);
    install_driver(&mut fixture, failing);

    fixture.app.fork_selected();

    assert_fork_discarded(&fixture);
    assert_eq!(fixture.pane.terminated(), vec![child_session]);
    assert!(fixture.app.status.contains("identity probe did not settle"));

    let mut liveness_error = fork_fixture(Engine::Claude);
    let child_session = session_name("bot-fork", &liveness_error.root);
    let failing = FakePane::default().with(|inner| {
        inner.fail_alive_for.insert(child_session.clone());
    });
    install_driver(&mut liveness_error, failing);

    liveness_error.app.fork_selected();

    assert!(
        liveness_error
            .app
            .status
            .contains("inspect fork terminal while waiting for identity")
    );
    assert_eq!(liveness_error.pane.terminated(), vec![child_session]);
    assert_fork_discarded(&liveness_error);
}

#[test]
fn fork_identity_exit_timeout_and_missing_staged_row_are_cleaned_up() {
    let mut exited = fork_fixture(Engine::Claude);
    let exited_driver = FakePane::default().with(|inner| inner.skip_launch_alive = true);
    install_driver(&mut exited, exited_driver);
    exited.app.fork_selected();
    assert!(exited.app.status.contains("exited before reporting"));
    assert_eq!(exited.pane.terminated().len(), 1);
    assert_fork_discarded(&exited);

    let mut timeout = fork_fixture(Engine::Claude);
    let timeout_driver = FakePane::default();
    install_driver(&mut timeout, timeout_driver);
    timeout.app.fork_selected();
    assert!(timeout.app.status.contains("identity probe timed out"));
    assert_eq!(timeout.pane.terminated().len(), 1);
    assert_fork_discarded(&timeout);

    let mut disappeared = fork_fixture(Engine::Claude);
    let child_session = session_name("bot-fork", &disappeared.root);
    let registry_path = disappeared.registry.clone();
    let disappearing_driver = FakePane::with_codex_session(&child_session, CHILD_ID)
        .with(|inner| inner.remove_fork_row_on_identity = Some(registry_path));
    install_driver(&mut disappeared, disappearing_driver);
    disappeared.app.fork_selected();
    assert!(disappeared.app.status.contains("staged row disappeared"));
    assert_eq!(disappeared.pane.terminated(), vec![child_session]);
    assert_fork_discarded(&disappeared);

    let mut corrupt = fork_fixture(Engine::Claude);
    let child_session = session_name("bot-fork", &corrupt.root);
    let registry_path = corrupt.registry.clone();
    let corrupting_driver = FakePane::with_codex_session(&child_session, CHILD_ID)
        .with(|inner| inner.corrupt_registry_on_identity = Some(registry_path));
    install_driver(&mut corrupt, corrupting_driver);
    corrupt.app.fork_selected();
    assert!(corrupt.app.status.contains("could not save"));
    assert!(
        corrupt
            .app
            .status
            .contains("could not remove its disabled row"),
        "an unreadable registry keeps the reservation for the row it may still hold: {}",
        corrupt.app.status
    );
    assert_eq!(corrupt.pane.terminated(), vec![child_session]);
    assert!(
        ProjectPaths::for_session(&corrupt.root, "bot-fork")
            .state_dir()
            .exists()
    );
}

#[test]
fn fork_refuses_an_identity_already_owned_by_another_session() {
    let mut fixture = fork_fixture(Engine::Claude);
    Registry::update(&fixture.registry, |registry| {
        let mut sibling = registry.projects[0].clone();
        sibling.id = "sibling".into();
        sibling.conversation_id = Some(CHILD_ID.into());
        registry.projects.push(sibling);
    })
    .unwrap();

    fixture.app.fork_selected();

    let registry = Registry::load(&fixture.registry).unwrap();
    assert!(
        registry.projects.iter().any(
            |entry| entry.id == "sibling" && entry.conversation_id.as_deref() == Some(CHILD_ID)
        ),
        "discarding the fork must not touch the row that owns the identity"
    );
    assert_fork_discarded(&fixture);
    assert!(fixture.app.status.contains("already belongs"));
    assert_eq!(fixture.pane.terminated().len(), 1);
}

#[test]
fn codex_child_identity_keeps_polling_past_the_source_rollout() {
    let mut fixture = fork_fixture(Engine::Codex);
    let child_session = session_name("bot-fork", &fixture.root);
    let driver = FakePane::default().with(|inner| {
        inner.codex_id_frames.insert(
            child_session.clone(),
            vec![Some(SOURCE_ID.into()), None, Some(CHILD_ID.into())],
        );
    });
    install_driver(&mut fixture, driver);

    fixture.app.fork_selected();

    assert_forked(&fixture, Engine::Codex);
    assert_eq!(
        fixture
            .pane
            .codex_probes()
            .iter()
            .filter(|session| **session == child_session)
            .count(),
        3
    );

    let mut stuck = fork_fixture_with_identity(Engine::Codex, SOURCE_ID);
    stuck.app.fork_selected();
    assert!(
        stuck.app.status.contains("identity probe did not settle")
            && stuck.app.status.contains("source conversation id"),
        "{}",
        stuck.app.status
    );
    assert!(
        stuck.pane.codex_probes().len() > 1,
        "the wait keeps polling"
    );
    assert_fork_discarded(&stuck);
}

#[test]
fn fork_identity_wait_stops_when_the_child_pane_dies() {
    let mut dead = fork_fixture(Engine::Claude);
    install_driver(
        &mut dead,
        FakePane::default().with(|inner| inner.pane_dead = true),
    );
    dead.app.fork_selected();
    assert!(
        dead.app.status.contains("exited before reporting"),
        "a dead pane under remain-on-exit is an exit, not a slow start: {}",
        dead.app.status
    );
    assert_fork_discarded(&dead);

    let mut unreadable = fork_fixture(Engine::Claude);
    install_driver(
        &mut unreadable,
        FakePane::default().with(|inner| inner.fail_pane_dead = true),
    );
    unreadable.app.fork_selected();
    assert!(
        unreadable
            .app
            .status
            .contains("inspect fork terminal while waiting for identity"),
        "{}",
        unreadable.app.status
    );
    assert_fork_discarded(&unreadable);
}
