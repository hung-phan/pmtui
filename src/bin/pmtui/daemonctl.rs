//! What pmtui knows and says about the `pmd` process: the liveness probe over the
//! singleton flock, the outcome type an ensure returns, the timings both use, and the
//! predicate for whether pmd's sweep drives a mode at all. Pure observation and
//! vocabulary — the spawning/stopping is `App`'s, in `app/pmd.rs`.

use crate::*;

/// Idle drains an Autopilot claude arm waits for pmd to pin a conversation id before it
/// gives up (see [`App::armed_drains_left`]). The run loop's idle tick is ~500ms and
/// pmd's default sweep is 500ms, so 60 ≈ 30 SECONDS — ~30x the one sweep it actually
/// takes, generous enough for a loaded machine and short enough that a dead daemon is
/// reported while the human is still looking at the row.
pub(crate) const AUTOPILOT_ARM_DRAINS: u32 = 60;

/// How long a daemon-liveness observation is reused before re-probing (see
/// [`App::daemon_live`]).
///
/// The run loop's idle tick is ~500ms and it RE-RENDERS every tick, so an uncached
/// probe would `open()` + `flock()` twice a second forever — exactly the per-frame
/// syscall storm the render path is built to avoid (`refresh` already limits its tmux
/// probes to idle ticks for the same reason). At 3s one probe covers ~6 frames, and a
/// daemon that dies is reported within ~3s — fast enough that the human is still
/// looking at the row they just acted on.
pub(crate) const DAEMON_PROBE_TTL: Duration = Duration::from_secs(3);

/// Consecutive FRESH `Down` observations (so `>= N * DAEMON_PROBE_TTL` ≈ 9s of
/// continuous DOWN) required before an armed Autopilot auto-open gives up on the
/// daemon it just ensured. See [`armed_wait_decision`] for why one sample is not enough.
pub(crate) const DAEMON_DOWN_DISARM_SAMPLES: u32 = 4;

/// What pmtui last OBSERVED about the daemon singleton flock. Three values, not a
/// bool, because "the probe failed" is a third fact: reporting it as DOWN would invent
/// a daemon outage out of a permissions error, and reporting it as up would be the lie
/// this whole slice exists to remove.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DaemonLive {
    /// Somebody holds the singleton flock ⇒ a `pmd` is running for this
    /// `(registry, socket)`.
    Up,
    /// The lock is free, or was never created ⇒ nothing is driving anything.
    Down,
    /// The probe itself failed. NEVER claim up.
    Unknown,
}

impl DaemonLive {
    /// The status-bar chip: text + a BASIC ANSI colour (so the user's terminal theme
    /// still wins). `DOWN` is shouted because it is the actionable one.
    pub(crate) fn chip(self) -> (&'static str, Color) {
        match self {
            DaemonLive::Up => ("pmd up", agent_manager::theme::live()),
            DaemonLive::Down => ("pmd DOWN", agent_manager::theme::hard()),
            DaemonLive::Unknown => ("pmd ?", agent_manager::theme::soft()),
        }
    }
}

/// Probe the daemon singleton flock ONCE, non-creating. The `(registry, socket)` →
/// path mapping is `lease::daemon_lock_path`, the same one `pmd` itself derives, and
/// `lease::is_held` is the NON-CREATING probe: `ensure_daemon`'s `lease::try_acquire`
/// would create the file, and several pmtui tests read its existence as proof that an
/// ensure ran.
pub(crate) fn probe_daemon_live(registry: &Path, socket: &str) -> DaemonLive {
    match lease::is_held(&lease::daemon_lock_path(registry, socket)) {
        Ok(Some(true)) => DaemonLive::Up,
        // Free and never-created are the same fact for the human: nothing is driving.
        Ok(Some(false)) | Ok(None) => DaemonLive::Down,
        Err(_) => DaemonLive::Unknown,
    }
}

/// The outcome of [`App::ensure_daemon`]. Typed rather than a bare `String` because
/// one caller has to BRANCH on it instead of merely printing it: the Autopilot
/// first-Enter arm (see [`autopilot_first_enter`]) is only honest if a daemon really is
/// up to make it come true, and "pmd binary not found" must NOT be armed behind an
/// encouraging status. `Display` renders exactly the fragments the print-only callers
/// (`cycle_tier`, `submit_create`, `ensure_daemon_for_enabled_autopilot`) have always
/// shown, so their copy is unchanged.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DaemonEnsure {
    /// The singleton flock was free and we spawned a `pmd`.
    Started,
    /// A `pmd` already holds the singleton flock.
    AlreadyRunning,
    /// No daemon is up and we could not start one (missing binary, spawn or flock
    /// error). Carries the human-readable reason.
    Failed(String),
}

impl DaemonEnsure {
    /// Whether a `pmd` is up as a result of this ensure — the PRECONDITION the
    /// Autopilot arm needs before it may promise the human a drive.
    ///
    /// `Started` is the honest best we can know from here: `spawn` succeeded, but a
    /// daemon can still die during boot (a second pmd losing the singleton race exits
    /// cleanly, for instance). That residual is what the BOUNDED arm
    /// ([`App::armed_drains_left`]) exists to catch — not a re-ensure loop.
    pub(crate) fn is_up(&self) -> bool {
        matches!(self, Self::Started | Self::AlreadyRunning)
    }
}

impl std::fmt::Display for DaemonEnsure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Started => f.write_str("daemon started"),
            Self::AlreadyRunning => f.write_str("daemon already running"),
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}

/// Whether `pmd`'s sweep actually DRIVES this mode — i.e. whether a running daemon
/// is what "autopilot on" needs for such a row. Every row is a `Mode::AgentLoop`
/// session that pmd drives (under Autopilot), so this holds for the one mode.
pub(crate) fn pmd_drives(mode: Mode) -> bool {
    matches!(mode, Mode::AgentLoop)
}

/// How long [`App::stop_daemon`] waits for a signalled daemon to release the singleton
/// lock: 20 x 50ms = 1s. Generous against pmd's own sweep, and bounded because a
/// dashboard must never hang on a process that refuses to die.
pub(crate) const DAEMON_STOP_TRIES: u32 = 20;
pub(crate) const DAEMON_STOP_WAIT: Duration = Duration::from_millis(50);
