//! Tests for the completion protocol and the trait's fail-safe defaults: what
//! `observe` reports for each (done-signal, liveness) pair, and that a driver
//! implementing only the four required methods behaves exactly as one without any
//! of the optional probes.

use anyhow::{Result, bail};
use std::path::Path;
use tempfile::tempdir;

use super::*;
use crate::tmux::fake::FakeDriver;
use crate::tmux::{Driver, ManagedEnv, Observation, StepHandle, classify_dialog, observe};

#[test]
fn observe_running_when_alive_and_no_signal() {
    let dir = tempdir().unwrap();
    let h = handle(dir.path(), "s1");
    let d = FakeDriver::new();
    d.set_alive("s1", true);
    assert_eq!(observe(&d, &h).unwrap(), Observation::Running);
}

#[test]
fn observe_completed_when_signal_present() {
    let dir = tempdir().unwrap();
    let h = handle(dir.path(), "s2");
    std::fs::write(&h.done_signal, "0").unwrap();
    let d = FakeDriver::new();
    d.set_alive("s2", true); // signal wins even if still "alive"
    assert_eq!(
        observe(&d, &h).unwrap(),
        Observation::Completed { exit_code: 0 }
    );
}

#[test]
fn observe_completed_reads_nonzero_exit() {
    let dir = tempdir().unwrap();
    let h = handle(dir.path(), "s3");
    std::fs::write(&h.done_signal, "3\n").unwrap();
    let d = FakeDriver::new();
    assert_eq!(
        observe(&d, &h).unwrap(),
        Observation::Completed { exit_code: 3 }
    );
}

#[test]
fn observe_orphaned_when_dead_and_no_signal() {
    let dir = tempdir().unwrap();
    let h = handle(dir.path(), "s4");
    let d = FakeDriver::new();
    d.set_alive("s4", false);
    assert_eq!(observe(&d, &h).unwrap(), Observation::Orphaned);
}

#[test]
fn observe_wins_race_when_signal_written_during_liveness_check() {
    // Signal absent at first read; is_alive writes it and returns false.
    // The re-read must still report Completed, not Orphaned.
    let dir = tempdir().unwrap();
    let h = handle(dir.path(), "s5");
    let d = FakeDriver::new();
    d.arm_write_on_is_alive(&h.done_signal, 0);
    assert_eq!(
        observe(&d, &h).unwrap(),
        Observation::Completed { exit_code: 0 }
    );
}

#[test]
fn corrupt_signal_is_orphaned_not_error() {
    // A present-but-unparseable signal (e.g. ENOSPC half-write) must not
    // wedge the project: observe reports Orphaned, never Err.
    let dir = tempdir().unwrap();
    let h = handle(dir.path(), "s6");
    std::fs::write(&h.done_signal, "not-a-number").unwrap();
    let d = FakeDriver::new();
    d.set_alive("s6", true); // even while "alive", corrupt wins as Orphaned
    assert_eq!(observe(&d, &h).unwrap(), Observation::Orphaned);
}

#[test]
fn empty_signal_is_pending_so_a_live_step_is_still_running() {
    // An empty signal is a partial write mid-rename, not completion.
    let dir = tempdir().unwrap();
    let h = handle(dir.path(), "s7");
    std::fs::write(&h.done_signal, "").unwrap();
    let d = FakeDriver::new();
    d.set_alive("s7", true);
    assert_eq!(observe(&d, &h).unwrap(), Observation::Running);
}

#[test]
fn empty_signal_with_dead_session_is_orphaned() {
    let dir = tempdir().unwrap();
    let h = handle(dir.path(), "s8");
    std::fs::write(&h.done_signal, "   ").unwrap();
    let d = FakeDriver::new();
    d.set_alive("s8", false);
    assert_eq!(observe(&d, &h).unwrap(), Observation::Orphaned);
}

#[test]
fn done_signal_io_errors_keep_the_path_context() {
    let dir = tempdir().unwrap();
    let h = handle(dir.path(), "unreadable");
    std::fs::create_dir(&h.done_signal).unwrap();

    let err = observe(&FakeDriver::new(), &h).unwrap_err();
    assert!(
        err.to_string()
            .contains(&h.done_signal.display().to_string()),
        "{err:#}"
    );
}

// ---- fail-safe trait defaults ---------------------------------------------

/// A `Driver` that implements ONLY the four required methods, so the DEFAULTS are
/// what get exercised. Every probe added to the trait must default toward "behave
/// exactly as a driver without it", or a third impl silently changes harness
/// behaviour — which is the invariant this pins.
struct MinimalDriver;

impl Driver for MinimalDriver {
    fn spawn_step(
        &self,
        _session: &str,
        _cwd: &Path,
        _command: &[String],
        _done_signal: &Path,
        _log: &Path,
    ) -> Result<StepHandle> {
        bail!("MinimalDriver cannot spawn")
    }
    fn is_alive(&self, _session: &str) -> Result<bool> {
        Ok(false)
    }
    fn capture_tail(&self, _session: &str, _lines: usize) -> Result<String> {
        Ok(String::new())
    }
    fn terminate(&self, _session: &str) -> Result<()> {
        Ok(())
    }
}

#[test]
fn every_optional_probe_defaults_fail_safe() {
    let d = MinimalDriver;
    assert_eq!(d.capture_tail_styled("s", 10).unwrap(), "");
    d.resize_window("s", 80, 24).unwrap();
    d.set_window_size_auto("s").unwrap();
    d.clear_history("s").unwrap();
    d.ensure_detach_key();
    // NOT in a mode ⇒ send_keys does not try to cancel anything.
    assert!(
        !d.pane_in_mode("s").unwrap(),
        "pane_in_mode must default false (assume not in a mode)"
    );
    // The pre-existing defaults, asserted alongside so the pattern is visible.
    assert!(
        !d.pane_dead("s").unwrap(),
        "assume NOT dead ⇒ never escalate"
    );
    assert!(d.has_clients("s").unwrap(), "assume attached ⇒ never reap");
    assert!(
        d.session_created("s").unwrap().is_none(),
        "age unknown ⇒ never reap"
    );
    // And the two that must refuse LOUDLY rather than silently no-op.
    assert!(d.send_keys("s", "hi").is_err(), "cannot type ⇒ Err");
    assert!(
        d.select_dialog_option("s", 0, 1).is_err(),
        "cannot send raw dialog keys ⇒ Err"
    );
    let dialog = classify_dialog(concat!(
        " Which path?\n",
        " ❯ 1. A\n",
        "   2. B\n",
        " Enter to select · Esc to cancel\n",
    ))
    .unwrap();
    assert!(
        d.verify_dialog_interactive("s", &dialog).is_err(),
        "cannot prove interactivity ⇒ Err"
    );
    assert!(
        d.select_dialog_options("s", &dialog, &[1]).is_err(),
        "cannot apply a structured selection ⇒ Err"
    );
    let refused = d
        .launch_interactive(
            "s",
            Path::new("/tmp"),
            &["x".into()],
            &ManagedEnv::default(),
        )
        .expect_err("cannot launch ⇒ Err");
    assert_eq!(
        refused.to_string(),
        "launch_interactive is not supported by this Driver (session s)"
    );
    assert!(
        !refused.proven_not_started(),
        "an unsupported driver never claims a proven outcome"
    );
    assert!(
        d.attach_interactive("s").is_err(),
        "cannot attach a foreground terminal ⇒ Err"
    );
    assert_eq!(
        d.codex_session_id("s", Path::new("/tmp")).unwrap(),
        None,
        "unknown Codex identity ⇒ never guess"
    );
}
