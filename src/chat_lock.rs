//! Short-lived attach intent for the single project terminal.
//!
//! pmtui writes this marker immediately before attaching, closing the small race before
//! tmux reports a client. pmd also checks `has_clients`; terminal liveness by itself is
//! never human presence. The marker expires after the launch grace so a crashed pmtui
//! cannot park Autopilot indefinitely.

use serde::{Deserialize, Serialize};

use crate::clock::Epoch;
use crate::state::{self, ProjectPaths};

/// Maximum attach-intent window before a stale marker is cleared.
pub const CHAT_LAUNCH_GRACE_S: i64 = 30;
/// Version-tolerant attach marker shared by pmtui and pmd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMarker {
    /// Diagnostics only now (no longer the liveness key).
    pub pid: u32,
    /// Launch epoch — anchors the LAUNCH-GRACE check only.
    pub since: Epoch,
    /// The unified project terminal name, for diagnostics.
    #[serde(default)]
    pub session: String,
    /// The `-L` socket it launched on — DIAGNOSTIC ONLY.
    #[serde(default)]
    pub socket: String,
}

/// Write attach intent for `paths`, creating `daemon_dir()` if missing. Called by
/// pmtui immediately before attach so the marker bridges the pre-client window.
pub fn mark(
    paths: &ProjectPaths,
    pid: u32,
    session: &str,
    socket: &str,
    now: Epoch,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(paths.daemon_dir())?;
    state::write_json_atomic(
        &paths.chat_lock(),
        &ChatMarker {
            pid,
            since: now,
            session: session.into(),
            socket: socket.into(),
        },
    )
}

/// Remove attach intent. Missing is already clear.
pub fn clear(paths: &ProjectPaths) {
    let _ = std::fs::remove_file(paths.chat_lock());
}

/// Whether a fresh attach-intent marker requires pmd to defer. Expired markers
/// are cleared. The terminal process and tmux liveness are deliberately irrelevant.
pub fn is_active(paths: &ProjectPaths, now: Epoch) -> bool {
    let marker = state::read_json_opt::<ChatMarker>(&paths.chat_lock())
        .ok()
        .flatten();
    match marker {
        Some(m) if now.saturating_sub(m.since) < CHAT_LAUNCH_GRACE_S => true,
        Some(_) => {
            clear(paths);
            false
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
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
}
