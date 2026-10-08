//! The in-memory `Driver` most of this crate's unit tests drive: liveness, pane
//! tails, attachment, `pane_dead`/`pane_in_mode` and the write-during-liveness
//! race are all armed by hand, and every `send_keys`/`launch_interactive` call is
//! recorded for assertions. `#[cfg(test)]`-gated exactly as it has always been, and
//! reached from other modules' tests as `crate::tmux::fake::FakeDriver`.

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::clock::Epoch;

use super::driver::{Driver, StepHandle};
use super::launch::{LaunchError, LaunchOutcome, ManagedEnv};

#[derive(Default)]
struct Inner {
    alive: HashMap<String, bool>,
    commands: HashMap<String, Vec<String>>,
    tails: HashMap<String, String>,
    /// If set, the next `is_alive` call writes this exit code to the given
    /// done-signal and returns false — simulating a step that finished
    /// exactly during the liveness check (exercises the race re-read).
    write_on_is_alive: Option<(PathBuf, i32)>,
    /// Whether a human is attached to a session (drives `has_clients`).
    clients: HashMap<String, bool>,
    /// Simulated `#{session_created}` epoch per session (drives `session_created`).
    created: HashMap<String, Epoch>,
    /// Simulated `#{pane_dead}` per session (drives `pane_dead`). Absent ⇒ not dead,
    /// so every existing test is unaffected. NOTE: a fake cannot establish the tmux
    /// FACT that a corpse still reads Idle — that needs the real-tmux acceptance test
    /// (`harness_does_not_nudge_a_dead_pane`); this only pins the harness's DECISION
    /// once something tells it the pane is dead.
    pane_dead: HashMap<String, bool>,
    /// Simulated `#{pane_in_mode}` per session (drives `pane_in_mode`). Absent ⇒ not in
    /// a mode, so every existing test is unaffected. NOTE: like `pane_dead`, a fake
    /// cannot establish the tmux FACT that copy-mode eats the submit while
    /// `paste-buffer` still exits 0 — that needs the real-tmux acceptance test
    /// (`nudge_reaches_a_pane_left_in_copy_mode`); this only pins the DECISION.
    pane_in_mode: HashMap<String, bool>,
    /// Sessions whose `is_alive` probe is armed to return `Err` (liveness UNKNOWN).
    fail_is_alive: std::collections::HashSet<String>,
    /// Sessions whose `send_keys` is armed to return `Err` — the TRANSIENT tmux
    /// send failure. Nothing is recorded in `sent` for an armed session, so a test
    /// can prove the keystroke never landed AND that the harness re-parked instead.
    fail_send_keys: std::collections::HashSet<String>,
    /// Sessions whose `terminate` call is armed to fail.
    fail_terminate: std::collections::HashSet<String>,
    /// Every `send_keys(session, text)` call, in order, for test assertions.
    sent: Vec<(String, String)>,
    /// Every raw dialog selection as `(session, current, target)`.
    selected_dialog_options: Vec<(String, usize, usize)>,
    /// Structured single/multi selections applied after an interactivity probe.
    applied_dialog_selections: Vec<(String, Vec<usize>)>,
    /// Sessions whose active interactivity probe should fail.
    fail_verify_dialog: std::collections::HashSet<String>,
    /// Sessions where a human attaches immediately after a successful dialog probe.
    attach_after_verify_dialog: std::collections::HashSet<String>,
    /// Sessions whose next dialog selection should fail.
    fail_select_dialog: std::collections::HashSet<String>,
    /// Every `launch_interactive(session, …, argv)` call, in order, as
    /// `(session, argv)` — for asserting the persistent loop session is launched
    /// once with the expected argv.
    launched: Vec<(String, Vec<String>)>,
    /// The [`ManagedEnv`] of every recorded launch, in the same order as `launched`.
    launched_env: Vec<(String, ManagedEnv)>,
    /// One-shot launch failures, keyed by session: the next `launch_interactive` of that
    /// session returns the armed error, starts nothing and records nothing.
    launch_errors: HashMap<String, LaunchError>,
    /// Exact Codex rollout id returned for a session.
    codex_session_ids: HashMap<String, String>,
    /// Sessions whose exact Codex rollout probe should fail.
    fail_codex_session_id: std::collections::HashSet<String>,
}

/// In-memory driver for deterministic scheduler/observe unit tests.
#[derive(Default)]
pub struct FakeDriver {
    inner: Mutex<Inner>,
}

impl FakeDriver {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_alive(&self, session: &str, alive: bool) {
        self.inner
            .lock()
            .unwrap()
            .alive
            .insert(session.to_string(), alive);
    }
    pub fn set_tail(&self, session: &str, tail: &str) {
        self.inner
            .lock()
            .unwrap()
            .tails
            .insert(session.to_string(), tail.to_string());
    }
    pub fn command_for(&self, session: &str) -> Option<Vec<String>> {
        self.inner.lock().unwrap().commands.get(session).cloned()
    }
    pub fn spawn_count(&self) -> usize {
        self.inner.lock().unwrap().commands.len()
    }
    pub fn arm_write_on_is_alive(&self, done_signal: &Path, code: i32) {
        self.inner.lock().unwrap().write_on_is_alive = Some((done_signal.to_path_buf(), code));
    }
    pub fn set_clients(&self, s: &str, attached: bool) {
        self.inner
            .lock()
            .unwrap()
            .clients
            .insert(s.to_string(), attached);
    }
    pub fn set_created(&self, s: &str, at: Epoch) {
        self.inner.lock().unwrap().created.insert(s.to_string(), at);
    }
    /// Simulate a `remain-on-exit` corpse (`#{pane_dead}` = 1) for `s`.
    pub fn set_pane_dead(&self, s: &str, dead: bool) {
        self.inner
            .lock()
            .unwrap()
            .pane_dead
            .insert(s.to_string(), dead);
    }
    /// Simulate a pane left in copy-mode (`#{pane_in_mode}` = 1) for `s`.
    pub fn set_pane_in_mode(&self, s: &str, in_mode: bool) {
        self.inner
            .lock()
            .unwrap()
            .pane_in_mode
            .insert(s.to_string(), in_mode);
    }
    pub fn fail_is_alive(&self, s: &str) {
        self.inner
            .lock()
            .unwrap()
            .fail_is_alive
            .insert(s.to_string());
    }
    pub fn fail_terminate(&self, s: &str) {
        self.inner
            .lock()
            .unwrap()
            .fail_terminate
            .insert(s.to_string());
    }
    pub fn allow_terminate(&self, s: &str) {
        self.inner.lock().unwrap().fail_terminate.remove(s);
    }
    /// Arm (`true`) or disarm (`false`) a TRANSIENT `send_keys` failure for `s`.
    pub fn fail_send_keys(&self, s: &str, fail: bool) {
        let mut inner = self.inner.lock().unwrap();
        if fail {
            inner.fail_send_keys.insert(s.to_string());
        } else {
            inner.fail_send_keys.remove(s);
        }
    }
    /// Every `(session, text)` passed to `send_keys`, in call order.
    pub fn sent_keys(&self) -> Vec<(String, String)> {
        self.inner.lock().unwrap().sent.clone()
    }
    /// Every `(session, current, target)` passed to `select_dialog_option`.
    pub fn selected_dialog_options(&self) -> Vec<(String, usize, usize)> {
        self.inner.lock().unwrap().selected_dialog_options.clone()
    }
    /// Arm (`true`) or disarm (`false`) a dialog-selection failure for `s`.
    pub fn fail_select_dialog(&self, s: &str, fail: bool) {
        let mut inner = self.inner.lock().unwrap();
        if fail {
            inner.fail_select_dialog.insert(s.to_string());
        } else {
            inner.fail_select_dialog.remove(s);
        }
    }
    pub fn applied_dialog_selections(&self) -> Vec<(String, Vec<usize>)> {
        self.inner.lock().unwrap().applied_dialog_selections.clone()
    }
    pub fn fail_verify_dialog(&self, s: &str, fail: bool) {
        let mut inner = self.inner.lock().unwrap();
        if fail {
            inner.fail_verify_dialog.insert(s.to_string());
        } else {
            inner.fail_verify_dialog.remove(s);
        }
    }
    pub fn attach_after_verify_dialog(&self, s: &str) {
        self.inner
            .lock()
            .unwrap()
            .attach_after_verify_dialog
            .insert(s.to_string());
    }
    /// Every `(session, argv)` a `launch_interactive` started, in call order. A launch on an
    /// already-alive session and an armed failure started nothing, so neither is recorded.
    pub fn launched(&self) -> Vec<(String, Vec<String>)> {
        self.inner.lock().unwrap().launched.clone()
    }
    /// The `(session, env)` of every launch in [`FakeDriver::launched`], in the same order.
    pub fn launched_env(&self) -> Vec<(String, ManagedEnv)> {
        self.inner.lock().unwrap().launched_env.clone()
    }
    /// Make the NEXT `launch_interactive` of `session` fail with `err` (one-shot).
    pub fn arm_launch_error(&self, session: &str, err: LaunchError) {
        self.inner
            .lock()
            .unwrap()
            .launch_errors
            .insert(session.to_string(), err);
    }
    pub fn set_codex_session_id(&self, session: &str, id: &str) {
        self.inner
            .lock()
            .unwrap()
            .codex_session_ids
            .insert(session.to_string(), id.to_string());
    }
    pub fn fail_codex_session_id(&self, session: &str, fail: bool) {
        let mut inner = self.inner.lock().unwrap();
        if fail {
            inner.fail_codex_session_id.insert(session.to_string());
        } else {
            inner.fail_codex_session_id.remove(session);
        }
    }
}

impl Driver for FakeDriver {
    fn spawn_step(
        &self,
        session: &str,
        _cwd: &Path,
        command: &[String],
        done_signal: &Path,
        log: &Path,
    ) -> Result<StepHandle> {
        let mut inner = self.inner.lock().unwrap();
        inner.alive.insert(session.to_string(), true);
        inner.commands.insert(session.to_string(), command.to_vec());
        Ok(StepHandle {
            session: session.to_string(),
            done_signal: done_signal.to_path_buf(),
            log: log.to_path_buf(),
        })
    }
    fn is_alive(&self, session: &str) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_is_alive.contains(session) {
            return Err(anyhow::anyhow!("is_alive probe failed"));
        }
        if let Some((path, code)) = inner.write_on_is_alive.take() {
            std::fs::write(&path, code.to_string())?;
            return Ok(false);
        }
        Ok(inner.alive.get(session).copied().unwrap_or(false))
    }
    fn capture_tail(&self, session: &str, _lines: usize) -> Result<String> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .tails
            .get(session)
            .cloned()
            .unwrap_or_default())
    }
    fn terminate(&self, session: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_terminate.contains(session) {
            return Err(anyhow::anyhow!("terminate failed (armed)"));
        }
        inner.alive.insert(session.to_string(), false);
        // A reaped session reads dead + ageless + unattached next tick.
        inner.clients.remove(session);
        inner.created.remove(session);
        Ok(())
    }
    fn send_keys(&self, session: &str, text: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_send_keys.contains(session) {
            return Err(anyhow::anyhow!("send_keys failed (armed)"));
        }
        inner.sent.push((session.to_string(), text.to_string()));
        Ok(())
    }
    fn select_dialog_option(&self, session: &str, current: usize, target: usize) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_select_dialog.contains(session) {
            return Err(anyhow::anyhow!("select_dialog_option failed (armed)"));
        }
        inner
            .selected_dialog_options
            .push((session.to_string(), current, target));
        Ok(())
    }
    fn verify_dialog_interactive(
        &self,
        session: &str,
        expected: &super::PaneDialog,
    ) -> Result<super::PaneDialog> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_verify_dialog.contains(session) {
            return Err(anyhow::anyhow!("dialog interactivity probe failed (armed)"));
        }
        if inner.attach_after_verify_dialog.remove(session) {
            inner.clients.insert(session.to_string(), true);
        }
        drop(inner);
        let current = expected
            .selected_index
            .ok_or_else(|| anyhow::anyhow!("dialog has no selected option"))?;
        let target = if current + 1 < expected.options.len() {
            current + 1
        } else {
            current
                .checked_sub(1)
                .ok_or_else(|| anyhow::anyhow!("dialog has no probe target"))?
        };
        let mut moved = expected.clone();
        moved.selected_index = Some(target);
        Ok(moved)
    }
    fn select_dialog_options(
        &self,
        session: &str,
        _dialog: &super::PaneDialog,
        targets: &[usize],
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_select_dialog.contains(session) {
            return Err(anyhow::anyhow!("select_dialog_options failed (armed)"));
        }
        inner
            .applied_dialog_selections
            .push((session.to_string(), targets.to_vec()));
        Ok(())
    }
    fn launch_interactive(
        &self,
        session: &str,
        _cwd: &Path,
        argv: &[String],
        env: &ManagedEnv,
    ) -> std::result::Result<LaunchOutcome, LaunchError> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(error) = inner.launch_errors.remove(session) {
            return Err(error);
        }
        // Idempotent by name, as in production: an alive session keeps its process.
        if inner.alive.get(session).copied().unwrap_or(false) {
            return Ok(LaunchOutcome::AlreadyAlive);
        }
        // Mark the session created + alive and record the launched argv and env for assertions.
        inner.alive.insert(session.to_string(), true);
        inner.launched.push((session.to_string(), argv.to_vec()));
        inner.launched_env.push((session.to_string(), env.clone()));
        Ok(LaunchOutcome::Started)
    }
    fn codex_session_id(&self, session: &str, _cwd: &Path) -> Result<Option<String>> {
        let inner = self.inner.lock().unwrap();
        if inner.fail_codex_session_id.contains(session) {
            return Err(anyhow::anyhow!("codex session id probe failed (armed)"));
        }
        Ok(inner.codex_session_ids.get(session).cloned())
    }
    fn has_clients(&self, session: &str) -> Result<bool> {
        Ok(*self
            .inner
            .lock()
            .unwrap()
            .clients
            .get(session)
            .unwrap_or(&false))
    }
    fn session_created(&self, session: &str) -> Result<Option<Epoch>> {
        Ok(self.inner.lock().unwrap().created.get(session).copied())
    }
    fn pane_dead(&self, session: &str) -> Result<bool> {
        Ok(*self
            .inner
            .lock()
            .unwrap()
            .pane_dead
            .get(session)
            .unwrap_or(&false))
    }
    fn pane_in_mode(&self, session: &str) -> Result<bool> {
        Ok(*self
            .inner
            .lock()
            .unwrap()
            .pane_in_mode
            .get(session)
            .unwrap_or(&false))
    }
}
