use crate::tmux::Driver;
use crate::tmux::fake::FakeDriver;

#[test]
fn claude_fork_identity_accepts_one_uuid_and_waits_for_absence() {
    let dir = tempfile::tempdir().unwrap();
    let identity = dir.path().join("fork-conversation-id");
    let driver = FakeDriver::default();

    assert_eq!(driver.claude_session_id("fork", &identity).unwrap(), None);
    std::fs::write(&identity, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee\n").unwrap();
    assert_eq!(
        driver.claude_session_id("fork", &identity).unwrap(),
        Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into())
    );
}

#[test]
fn claude_fork_identity_rejects_malformed_hook_output() {
    let dir = tempfile::tempdir().unwrap();
    let identity = dir.path().join("fork-conversation-id");
    std::fs::write(&identity, "source-session\nsecond-line").unwrap();

    let error = FakeDriver::default()
        .claude_session_id("fork", &identity)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("does not contain one session UUID")
    );
}

#[test]
fn claude_fork_identity_surfaces_non_missing_read_errors() {
    let dir = tempfile::tempdir().unwrap();
    let identity = dir.path().join("fork-conversation-id");
    std::fs::create_dir(&identity).unwrap();

    let error = FakeDriver::default()
        .claude_session_id("fork", &identity)
        .unwrap_err();

    assert!(error.to_string().contains("read"), "{error:#}");
    assert!(
        error.to_string().contains(&identity.display().to_string()),
        "{error:#}"
    );
}
