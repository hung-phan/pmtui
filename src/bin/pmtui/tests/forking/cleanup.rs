//! Removing the state a fork reserved before it failed, and saying so when the directory itself
//! cannot go.

use super::*;

#[test]
fn unstaged_fork_cleanup_handles_present_missing_and_unremovable_state() {
    let dir = tempfile::tempdir().unwrap();
    let missing = ProjectPaths::for_session(dir.path(), "missing");
    cleanup_unstaged_fork(&missing).unwrap();

    let present = ProjectPaths::for_session(dir.path(), "present");
    std::fs::create_dir_all(present.state_dir()).unwrap();
    cleanup_unstaged_fork(&present).unwrap();
    assert!(!present.state_dir().exists());

    let blocked = ProjectPaths::for_session(dir.path(), "blocked");
    std::fs::create_dir_all(blocked.state_dir()).unwrap();
    std::fs::write(blocked.state_dir().join("keep"), "x").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(blocked.state_dir(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let error = cleanup_unstaged_fork(&blocked).unwrap_err();
    assert!(error.to_string().contains("remove"), "{error:#}");
    std::fs::set_permissions(blocked.state_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    cleanup_unstaged_fork(&blocked).unwrap();
}

#[test]
fn a_discarded_fork_reports_reserved_state_it_could_not_remove() {
    let mut fixture = fork_fixture(Engine::Claude);
    let child_state = ProjectPaths::for_session(&fixture.root, "bot-fork").state_dir();
    let driver = FakePane::default().with(|inner| {
        inner.fail_launch = true;
        inner.freeze_dir_on_launch = Some(child_state.clone());
    });
    install_driver(&mut fixture, driver);

    fixture.app.fork_selected();

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&child_state, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        fixture
            .app
            .status
            .contains("could not remove its reserved state"),
        "{}",
        fixture.app.status
    );
    let registry = Registry::load(&fixture.registry).unwrap();
    assert!(
        !registry.projects.iter().any(|entry| entry.id == "bot-fork"),
        "the row goes even when its directory cannot"
    );
}
