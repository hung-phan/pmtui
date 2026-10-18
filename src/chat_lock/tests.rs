use super::*;
use crate::tmux::{Driver, fake::FakeDriver};
use tempfile::tempdir;

const NOW: Epoch = 1_000_000;
const S: &str = "pm-x";

#[test]
fn alive_session_without_attach_marker_does_not_defer() {
    let dir = tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());
    let d = FakeDriver::new();
    d.set_alive(S, true);
    assert!(
        !is_active(&paths, NOW),
        "a unified terminal being alive is normal; only attach intent defers"
    );
}

#[test]
fn fresh_attach_intent_defers_and_keeps_marker() {
    let dir = tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());
    mark(&paths, 1, S, "pmd", NOW).unwrap();
    assert!(is_active(&paths, NOW));
    assert!(
        paths.chat_lock().exists(),
        "fresh attach intent must remain for the pre-client window"
    );
}

#[test]
fn expired_attach_marker_clears_even_while_the_terminal_lives() {
    let dir = tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());
    let d = FakeDriver::new();
    d.set_alive(S, true);
    mark(&paths, 1, S, "pmd", NOW - CHAT_LAUNCH_GRACE_S - 1).unwrap();
    assert!(!is_active(&paths, NOW));
    assert!(!paths.chat_lock().exists(), "expired marker is self-healed");
    assert!(
        d.is_alive(S).unwrap(),
        "the terminal itself is never killed"
    );
}

#[test]
fn attached_client_is_detected_by_the_separate_has_clients_gate() {
    let dir = tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());
    let d = FakeDriver::new();
    d.set_alive(S, true);
    d.set_clients(S, true); // a human is attached
    assert!(!is_active(&paths, NOW));
    assert!(d.has_clients(S).unwrap());
    assert!(
        d.is_alive(S).unwrap(),
        "an attached session is never reaped, even past the cap"
    );
}

#[test]
fn absent_marker_alive_session_is_available_to_the_driver() {
    let dir = tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());
    let d = FakeDriver::new();
    // No marker at all. Terminal liveness does not imply human presence.
    d.set_alive(S, true);
    d.set_clients(S, false);
    assert!(!is_active(&paths, NOW));
    assert!(
        d.is_alive(S).unwrap(),
        "checking attach intent never terminates the terminal"
    );
}
