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
mod tests;
