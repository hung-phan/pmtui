//! Tests for the `pmtui` binary. Split out of `main.rs`, which had grown to 18.7k
//! lines; a child module still sees its ancestors' private items, so this is a pure
//! move. `use super::*` became `use crate::*` — same items, the crate root IS `main.rs`.
//!
//! Split again by what each test exercises, mirroring the binary's own modules: a test
//! lives in the file named after the behaviour it pins. The fixtures every one of those
//! files builds on live here — the [`FakePane`] preview driver, the `App`/`ProjectView`
//! builders, the on-disk registry and session writers, and the `TestBackend` screen
//! readers — because sharing them is what keeps the tests short enough to read as
//! statements about behaviour.

use crate::*;

/// RAII holder for a real-tmux test's private server socket — the twin of the integration
/// suite's `probe::TmuxSocket`, since this binary's tests cannot reach that crate.
///
/// The real-tmux tests here (`pmtui-loop`, `pmtui-armed`, `pmtui-chat`, `pmtui-del`,
/// `pmtui-aprun`) each kill their server at the end but never removed the socket FILE, and
/// tmux leaves the 0-byte file behind on `kill-server` — hundreds piled up under
/// `/tmp/tmux-<uid>/`. Holding this for the test's scope removes the file on drop (and on a
/// panicking assertion, which the manual end-of-test cleanup missed).
pub(crate) struct TmuxSocket(String);

impl TmuxSocket {
    pub(crate) fn new(prefix: &str) -> Self {
        Self(format!("{prefix}-{}", std::process::id()))
    }

    pub(crate) fn name(&self) -> &str {
        &self.0
    }
}

impl Drop for TmuxSocket {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux")
            .args(["-L", &self.0, "kill-server"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "rm -f \"${{TMUX_TMPDIR:-/tmp}}/tmux-$(id -u)/{}\"",
                self.0
            ))
            .status();
    }
}
use agent_manager::pmstate;
use agent_manager::state::Stop;
use agent_manager::tmux::StepHandle;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// A writable status-log path, unique per fixture, inside ONE temp directory shared by this test
/// binary.
///
/// The log is a file now (`status_log`), so a test that asserts what the dashboard recorded has to
/// read one. Unique per fixture because the suite runs in parallel and two `App`s appending to the
/// same file would read each other's lines; one shared directory because a `TempDir` per fixture
/// would be a thousand directories per run.
fn test_log_path() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("status-log temp dir"));
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dir.path().join(format!("pmtui-{n}.log"))
}

/// The status log's lines with their timestamps stripped — the assertion surface the in-memory
/// `App::log` used to be.
fn log_lines(app: &App) -> Vec<String> {
    std::fs::read_to_string(&app.status_log)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            line.split_once(' ')
                .map_or(line, |(_timestamp, rest)| rest)
                .to_string()
        })
        .collect()
}

/// Put `lines` in the log as if the dashboard had recorded them, for tests that need history to
/// already exist.
fn seed_log(app: &App, lines: &[&str]) {
    for line in lines {
        crate::status_log::append(&app.status_log, line);
    }
}

fn test_turn_signal() -> &'static Path {
    Path::new("/tmp/pmtui-test-turn-complete")
}

// One file per behaviour the dashboard has. Each reaches the fixtures below through its
// own `use super::*;`, exactly as this module reaches the binary through `use crate::*`.
mod answer_overlay;
mod answering;
mod args;
mod arm;
mod audit;
mod autopilot;
mod board;
mod board_actions;
mod board_cards;
mod board_chords;
mod board_status;
mod board_viewport;
mod cadence;
mod chat;
mod checkpoint_preview;
mod chrome;
mod codex_monitoring;
mod composer;
mod create;
mod create_form;
mod daemonctl;
mod decider_activity;
mod decisions;
mod directive;
mod display;
mod edit;
mod enter;
mod events;
mod forking;
mod goal;
mod help;
mod keybar;
mod keys;
mod launch_env;
mod lifecycle;
mod log;
mod overlays;
mod pane_dialog_answering;
mod pmd;
mod preview;
mod rename;
mod rows;
mod seed;
mod send;
mod session;
mod settings;
mod spawn_broker;
mod spawn_cli;
mod spawn_jobs;
mod spawn_rows;
mod spawn_skill;
mod status;
mod status_log;
mod stops;
mod switcher;
mod takeover;
mod theming;
mod undriven_activity;
mod wake;

#[derive(Default)]
struct PaneInner {
    alive: std::sync::Mutex<std::collections::HashSet<String>>,
    fail_alive: bool,
    fail_alive_for: std::collections::HashSet<String>,
    tails: std::collections::HashMap<String, String>,
    /// When set, `capture_tail` returns `Err` (tmux itself unusable).
    fail_capture: bool,
    captures: std::sync::Mutex<Vec<(String, usize)>>,
    /// Every `(session, text)` handed to `send_keys`, in call order — the only way
    /// to prove `s` wrote to the pane it claimed to, and to nothing else.
    sends: std::sync::Mutex<Vec<(String, String)>>,
    /// When set, `send_keys` returns `Err` (the failure `s` must report verbatim).
    fail_send: bool,
    /// Every `(session, cwd, argv)` handed to `launch_interactive`, in call order — the only way
    /// to prove a create STARTED an agent, in the right tree, with the right posture.
    launches: std::sync::Mutex<Vec<(String, String, Vec<String>)>>,
    /// The [`tmux::ManagedEnv`] of every `launch_interactive` call, in the same order as
    /// `launches` — how a test proves a launch told the agent which session it is in.
    launch_envs: std::sync::Mutex<Vec<(String, tmux::ManagedEnv)>>,
    /// One-shot typed launch failures keyed by session: the next launch of that session is
    /// recorded, then returns the armed error without starting anything.
    armed_launch_errors: std::sync::Mutex<std::collections::HashMap<String, tmux::LaunchError>>,
    /// Every session handed to `terminate`, in call order — how a test proves the autopilot flip
    /// really handed the conversation over rather than only saying so.
    terminated: std::sync::Mutex<Vec<String>>,
    /// Every session handed to `request_stop`, in call order — how a cancel test tells the POLITE
    /// signal from the kill that follows it.
    stop_requests: std::sync::Mutex<Vec<String>>,
    /// Every session handed to the foreground attach operation.
    foreground_attaches: std::sync::Mutex<Vec<String>>,
    fail_terminate: bool,
    fail_launch: bool,
    fail_attach: bool,
    /// Every `(session, cols, rows)` handed to `resize_window`, in call order — how the
    /// pane-fit dedupe test proves the render tick reflows the detached pane ONLY when the
    /// size actually changed (and re-fires after `forget_agent_pane_fit`).
    resizes: std::sync::Mutex<Vec<(String, u16, u16)>>,
    /// Every session handed to `clear_history`, in call order — proof the fit "repaints" by
    /// dropping stale scrollback right after it resizes.
    clears: std::sync::Mutex<Vec<String>>,
    /// Every `(session, cwd, argv)` passed to `spawn_step`, in call order.
    steps: std::sync::Mutex<Vec<(String, String, Vec<String>)>>,
    /// A tmux client is attached. Modelled explicitly because the TRAIT default is
    /// `Ok(true)` ("assume attached", right for the orphan reaper), so a double that
    /// forgets to override it silently takes the attached branch in every test.
    attached: bool,
    /// The pane is in copy-mode/scrollback.
    in_mode: bool,
    /// The pane's process exited but the pane lingers (`remain-on-exit`).
    pane_dead: bool,
    fail_pane_dead: bool,
    /// Exact live Codex rollout IDs, keyed by tmux session.
    codex_session_ids: std::collections::HashMap<String, String>,
    /// Sessions whose exact Codex rollout probe returns an error.
    fail_codex_probe: std::collections::HashSet<String>,
    /// Fault injection for a child that exits during launch before identity capture.
    skip_launch_alive: bool,
    /// Fault injection for a staged fork row disappearing before promotion.
    remove_fork_row_on_identity: Option<PathBuf>,
    /// Fault injection for registry corruption between staging and promotion.
    corrupt_registry_on_identity: Option<PathBuf>,
    /// Fault injection for registry corruption after source revalidation.
    corrupt_registry_on_capture: Option<PathBuf>,
    /// Successive captures of one session: capture `n` returns frame `n`, and the last frame
    /// repeats. Overrides `tails` for that session.
    tail_frames: std::collections::HashMap<String, Vec<String>>,
    /// Successive exact Codex probe results for one session, in the same repeat-last shape.
    /// Overrides `codex_session_ids` for that session.
    codex_id_frames: std::collections::HashMap<String, Vec<Option<String>>>,
    /// Every session handed to `codex_session_id`, in call order.
    codex_probes: std::sync::Mutex<Vec<String>>,
    /// Fault injection: a launch makes this directory unwritable before it reports.
    freeze_dir_on_launch: Option<PathBuf>,
}

/// Frame `index` of a scripted sequence, repeating its last frame; `None` when unscripted.
fn scripted_frame<T: Clone>(frames: Option<&Vec<T>>, index: usize) -> Option<T> {
    let frames = frames?;
    frames.get(index).or(frames.last()).cloned()
}

/// A minimal in-memory [`Driver`] for the PREVIEW's read-only pane capture.
///
/// The library's `tmux::fake::FakeDriver` is `#[cfg(test)]`-gated INSIDE the lib
/// crate, so this separate binary crate cannot see it. This is the same shape,
/// trimmed to what the preview actually calls (`is_alive` + `capture_tail`), plus
/// an armable capture failure. Cheaply cloneable (shared `Arc` state) so a test
/// can keep a handle after boxing one into `App::agent_tmux`, and it RECORDS
/// every capture — that is how a test proves pmtui captures the SELECTED row's
/// `pmloop-…` session and nothing else.
#[derive(Clone, Default)]
struct FakePane(std::sync::Arc<PaneInner>);

impl FakePane {
    /// A driver reporting `session` ALIVE with `tail` on its pane.
    fn live(session: &str, tail: &str) -> Self {
        Self(std::sync::Arc::new(PaneInner {
            alive: std::sync::Mutex::new([session.to_string()].into_iter().collect()),
            tails: [(session.to_string(), tail.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        }))
    }
    /// A driver reporting `session` alive with an EMPTY pane (launched, silent).
    fn live_quiet(session: &str) -> Self {
        Self::live(session, "   \n \n")
    }
    /// A driver reporting `session` alive but whose capture always fails.
    fn capture_fails(session: &str) -> Self {
        Self(std::sync::Arc::new(PaneInner {
            alive: std::sync::Mutex::new([session.to_string()].into_iter().collect()),
            fail_capture: true,
            ..Default::default()
        }))
    }
    fn with_codex_session(session: &str, id: &str) -> Self {
        Self(std::sync::Arc::new(PaneInner {
            alive: std::sync::Mutex::new([session.to_string()].into_iter().collect()),
            codex_session_ids: [(session.to_string(), id.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        }))
    }
    fn codex_probe_fails(session: &str) -> Self {
        Self(std::sync::Arc::new(PaneInner {
            alive: std::sync::Mutex::new([session.to_string()].into_iter().collect()),
            fail_codex_probe: [session.to_string()].into_iter().collect(),
            ..Default::default()
        }))
    }
    /// Every `(session, lines)` passed to `capture_tail`, in call order.
    fn captures(&self) -> Vec<(String, usize)> {
        self.0.captures.lock().unwrap().clone()
    }
    /// Every `(session, text)` passed to `send_keys`, in call order.
    fn sends(&self) -> Vec<(String, String)> {
        self.0.sends.lock().unwrap().clone()
    }
    fn launches(&self) -> Vec<(String, String, Vec<String>)> {
        self.0.launches.lock().unwrap().clone()
    }
    /// Every `(session, cwd, argv)` passed to `spawn_step` — a JOB child's launch.
    fn steps(&self) -> Vec<(String, String, Vec<String>)> {
        self.0.steps.lock().unwrap().clone()
    }
    /// A driver reporting every session dead, with a human attached to whatever is asked about.
    fn attached() -> Self {
        Self(std::sync::Arc::new(PaneInner {
            attached: true,
            ..Default::default()
        }))
    }
    /// Every `(session, env)` passed to `launch_interactive`, in call order.
    fn launched_env(&self) -> Vec<(String, tmux::ManagedEnv)> {
        self.0.launch_envs.lock().unwrap().clone()
    }
    /// Make the NEXT launch of `session` fail with `err` (one-shot).
    fn arm_launch_error(&self, session: &str, err: tmux::LaunchError) {
        self.0
            .armed_launch_errors
            .lock()
            .unwrap()
            .insert(session.to_string(), err);
    }
    /// Every session passed to `codex_session_id`, in call order.
    fn codex_probes(&self) -> Vec<String> {
        self.0.codex_probes.lock().unwrap().clone()
    }
    fn terminated(&self) -> Vec<String> {
        self.0.terminated.lock().unwrap().clone()
    }
    /// Every session passed to `request_stop` (SIGTERM), in call order.
    fn stop_requests(&self) -> Vec<String> {
        self.0.stop_requests.lock().unwrap().clone()
    }
    fn foreground_attaches(&self) -> Vec<String> {
        self.0.foreground_attaches.lock().unwrap().clone()
    }
    /// Every `(session, cols, rows)` passed to `resize_window`, in call order.
    fn resizes(&self) -> Vec<(String, u16, u16)> {
        self.0.resizes.lock().unwrap().clone()
    }
    /// Every session passed to `clear_history`, in call order.
    fn clears(&self) -> Vec<String> {
        self.0.clears.lock().unwrap().clone()
    }
    /// Alive, showing `tail`, unattached and not scrolled back — the ordinary state
    /// of a pane `s` may write to.
    fn sendable(session: &str, tail: &str) -> Self {
        Self(std::sync::Arc::new(PaneInner {
            alive: std::sync::Mutex::new([session.to_string()].into_iter().collect()),
            tails: [(session.to_string(), tail.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        }))
    }
    /// Adjust one field of an otherwise-sendable pane.
    fn with(mut self, f: impl FnOnce(&mut PaneInner)) -> Self {
        let inner = std::sync::Arc::get_mut(&mut self.0).expect("uniquely owned");
        f(inner);
        self
    }
}

impl Driver for FakePane {
    /// A JOB child's launch. Recorded the same way `launch_interactive` is — `(session, cwd, argv)` —
    /// so a test can assert the one-shot argv, and it marks the session ALIVE, because a job that was
    /// just started is running. `fail_step` arms a failure, which the broker must treat as ambiguous.
    fn spawn_step(
        &self,
        session: &str,
        cwd: &Path,
        command: &[String],
        done_signal: &Path,
        log: &Path,
    ) -> Result<StepHandle> {
        self.0.steps.lock().unwrap().push((
            session.to_string(),
            cwd.display().to_string(),
            command.to_vec(),
        ));
        // The SAME arming as `launch_interactive`, so a test that stages a launch failure or a session
        // that never comes up gets the same behaviour whichever kind of child it staged. A job's
        // failure is ambiguous either way, which is what the broker is told.
        if let Some(dir) = &self.0.freeze_dir_on_launch {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500))?;
        }
        if let Some(error) = self.0.armed_launch_errors.lock().unwrap().remove(session) {
            anyhow::bail!("{error}");
        }
        if self.0.fail_launch {
            anyhow::bail!("new-session failed ({session})")
        }
        // tmux refuses a duplicate session name, so a step over a live session fails rather than
        // reporting `AlreadyAlive` the way an interactive launch does. Ambiguous either way.
        if self.0.alive.lock().unwrap().contains(session) {
            anyhow::bail!("duplicate session: {session}")
        }
        if !self.0.skip_launch_alive {
            self.0.alive.lock().unwrap().insert(session.to_string());
        }
        Ok(StepHandle {
            session: session.to_string(),
            done_signal: done_signal.to_path_buf(),
            log: log.to_path_buf(),
        })
    }
    fn is_alive(&self, session: &str) -> Result<bool> {
        if self.0.fail_alive || self.0.fail_alive_for.contains(session) {
            anyhow::bail!("has-session failed");
        }
        Ok(self.0.alive.lock().unwrap().contains(session))
    }
    fn capture_tail(&self, session: &str, lines: usize) -> Result<String> {
        let index = {
            let mut captures = self.0.captures.lock().unwrap();
            captures.push((session.to_string(), lines));
            captures.iter().filter(|(seen, _)| seen == session).count() - 1
        };
        if self.0.fail_capture {
            anyhow::bail!("capture-pane failed");
        }
        if let Some(registry_path) = &self.0.corrupt_registry_on_capture {
            std::fs::write(registry_path, "{")?;
        }
        if let Some(frame) = scripted_frame(self.0.tail_frames.get(session), index) {
            return Ok(frame);
        }
        Ok(self.0.tails.get(session).cloned().unwrap_or_default())
    }
    /// Recorded, and the session stays ALIVE: SIGTERM asks, it does not guarantee, which is exactly the
    /// case the broker's "kill on the next frame" fallback exists for.
    fn request_stop(&self, session: &str) -> Result<()> {
        self.0
            .stop_requests
            .lock()
            .unwrap()
            .push(session.to_string());
        Ok(())
    }

    fn terminate(&self, session: &str) -> Result<()> {
        self.0.terminated.lock().unwrap().push(session.to_string());
        if self.0.fail_terminate {
            anyhow::bail!("kill-session failed");
        }
        self.0.alive.lock().unwrap().remove(session);
        Ok(())
    }
    fn resize_window(&self, session: &str, cols: u16, rows: u16) -> Result<()> {
        self.0
            .resizes
            .lock()
            .unwrap()
            .push((session.to_string(), cols, rows));
        Ok(())
    }
    fn clear_history(&self, session: &str) -> Result<()> {
        self.0.clears.lock().unwrap().push(session.to_string());
        Ok(())
    }
    fn send_keys(&self, session: &str, text: &str) -> Result<()> {
        self.0
            .sends
            .lock()
            .unwrap()
            .push((session.to_string(), text.to_string()));
        if self.0.fail_send {
            anyhow::bail!("send-keys: no such session");
        }
        Ok(())
    }
    fn launch_interactive(
        &self,
        session: &str,
        cwd: &Path,
        argv: &[String],
        env: &tmux::ManagedEnv,
    ) -> std::result::Result<tmux::LaunchOutcome, tmux::LaunchError> {
        self.0.launches.lock().unwrap().push((
            session.to_string(),
            cwd.display().to_string(),
            argv.to_vec(),
        ));
        self.0
            .launch_envs
            .lock()
            .unwrap()
            .push((session.to_string(), env.clone()));
        if let Some(dir) = &self.0.freeze_dir_on_launch {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500))
                .map_err(|error| tmux::LaunchError::Probe(error.to_string()))?;
        }
        if let Some(error) = self.0.armed_launch_errors.lock().unwrap().remove(session) {
            return Err(error);
        }
        if self.0.fail_launch {
            return Err(tmux::LaunchError::NewSessionFailed(
                "new-session failed".into(),
            ));
        }
        if self.0.alive.lock().unwrap().contains(session) {
            return Ok(tmux::LaunchOutcome::AlreadyAlive);
        }
        if !self.0.skip_launch_alive {
            self.0.alive.lock().unwrap().insert(session.to_string());
        }
        Ok(tmux::LaunchOutcome::Started)
    }
    fn attach_interactive(&self, session: &str) -> Result<()> {
        if self.0.fail_attach {
            anyhow::bail!("attach-session failed");
        }
        if !self.0.alive.lock().unwrap().contains(session) {
            anyhow::bail!("attach-session: no such session");
        }
        self.0
            .foreground_attaches
            .lock()
            .unwrap()
            .push(session.to_string());
        Ok(())
    }
    fn codex_session_id(&self, session: &str, _cwd: &Path) -> Result<Option<String>> {
        let index = {
            let mut probes = self.0.codex_probes.lock().unwrap();
            probes.push(session.to_string());
            probes.iter().filter(|seen| *seen == session).count() - 1
        };
        if self.0.fail_codex_probe.contains(session) {
            anyhow::bail!("codex session id probe failed (armed)");
        }
        if let Some(frame) = scripted_frame(self.0.codex_id_frames.get(session), index) {
            return Ok(frame);
        }
        Ok(self.0.codex_session_ids.get(session).cloned())
    }
    fn claude_session_id(&self, session: &str, _identity_file: &Path) -> Result<Option<String>> {
        if let Some(registry_path) = &self.0.remove_fork_row_on_identity {
            Registry::update(registry_path, |registry| {
                registry.projects.retain(|entry| entry.id != "bot-fork")
            })?;
        }
        if let Some(registry_path) = &self.0.corrupt_registry_on_identity {
            std::fs::write(registry_path, "{")?;
        }
        Ok(self.0.codex_session_ids.get(session).cloned())
    }
    fn has_clients(&self, _session: &str) -> Result<bool> {
        Ok(self.0.attached)
    }
    fn pane_in_mode(&self, _session: &str) -> Result<bool> {
        Ok(self.0.in_mode)
    }
    fn pane_dead(&self, _session: &str) -> Result<bool> {
        if self.0.fail_pane_dead {
            anyhow::bail!("pane-dead probe failed");
        }
        Ok(self.0.pane_dead)
    }
}

/// Acquire a lock a test EXPECTS to be free, tolerating the fork→exec window.
///
/// A single-shot `lease::try_acquire` right after the SAME lock was opened+released earlier in a
/// test is flaky in this multi-threaded test binary: sibling tests fork+exec (`spawn_daemon`, tmux
/// probes, the escalation notifier), and a forked child transiently inherits every open fd —
/// including a just-held lease fd — until it `exec`s (CLOEXEC fires only at exec, not fork), so the
/// just-freed `flock` reads as still-held for the fork→exec window (µs, widening to ms under load).
/// This is the exact false positive `lease::acquire_with_retry` filters for pmd; a REAL owner holds
/// the lock for life and still loses every retry, so this stays honest — it tests "becomes free",
/// not "free on the very first syscall".
pub(crate) fn acquire_free_lease(lock: &std::path::Path) -> Option<lease::ProjectLease> {
    lease::acquire_with_retry(lock, 50, std::time::Duration::from_millis(10)).unwrap()
}

/// Poll until the lock at `lock` is observably FREE/absent (or panic once the budget is spent) —
/// the `is_held` sibling of [`acquire_free_lease`], for the same fork→exec fd-inheritance reason
/// above. Use it after DROPPING a lease when the code you run next does its OWN single-shot
/// `try_acquire`/`is_held` (e.g. a second `request_attach`, a `daemon_live` probe): you cannot
/// retry that downstream branch, so the lock must be actually free before it runs. A spent budget
/// means the lock never freed — a real leak, not the µs fork window.
pub(crate) fn wait_until_free(lock: &std::path::Path) {
    for _ in 0..50 {
        match lease::is_held(lock) {
            Ok(Some(false)) | Ok(None) => return,
            _ => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    panic!(
        "lock at {lock:?} never became free within the retry budget (a real leak, not the fork window)"
    );
}

fn view(id: &str, posture: Posture, stops: Vec<Stop>) -> ProjectView {
    ProjectView {
        id: id.into(),
        display_name: None,
        work_summary: None,
        project_name: None,
        forked_from: None,
        incomplete_fork: false,
        spawned_by: None,
        spawned_by_label: None,
        spawn_staged: false,
        job: false,
        job_commit: None,
        job_branch: None,
        enabled: true,
        // A driven (Autopilot) agent-loop row: bucketed by POSTURE, the generic "some
        // project" row most tests want. A human-driven Standard row (session-liveness
        // bucketing, "you drive it") is `agent_loop_view`, which pins Standard explicitly.
        tier: Some(Tier::Autopilot),
        posture,
        next_action: "dispatch task-4".into(),
        step_id: 2,
        last_activity: Some(100),
        // A fixture's stops carry no age unless the test sets one; the preview's
        // waiting-age line is therefore off by default and opted into explicitly.
        oldest_stop_since: None,
        stops,
        mode: Mode::AgentLoop,
        engine: None,
        session_live: false,
        human_attached: false,
        agent_working: None,
        autopilot_events: Vec::new(),
        turn_trace: Vec::new(),
        decision_digest: job::DecisionCounters::default(),
        advice_inflight: None,
        decider_live: false,
        advice_queue: Vec::new(),
        decider_runs: Vec::new(),
        decider_engine: Some(Engine::Claude),
        decider_model: None,
    }
}

/// An App with no live tmux anywhere: the preview seam is a [`FakePane`] that
/// reports every session dead, so no test ever shells out to tmux from `render`
/// (and the log falls through to the step-log/placeholder paths).
fn app_with(projects: Vec<ProjectView>, mode: UiMode) -> App {
    app_with_driver(projects, mode, Box::new(FakePane::default()))
}

/// [`app_with`] with an explicit preview driver — the seam the live-pane tests
/// inject a canned/failing capture through.
fn app_with_driver(projects: Vec<ProjectView>, mode: UiMode, agent_tmux: Box<dyn Driver>) -> App {
    App {
        registry_path: PathBuf::from("/nonexistent"),
        socket: "pm-test".into(),
        projects,
        selected: 0,
        mode,
        board_detail_open: false,
        return_to_board_after_create: false,
        return_to_board_after_send: false,
        return_to_board_after_answer: false,
        return_to_board_after_switch: false,
        return_to_board_after_action: false,
        answering_stop_id: None,
        status: "ready".into(),
        should_quit: false,
        dashboard_owner_nonce: None,
        status_log: test_log_path(),
        last_logged: None,
        // Preferences beside the throwaway log, for the same reason: a fixture must never read or
        // write the human's `pmtui.json`. Unique per fixture like the log itself, so two parallel
        // tests saving preferences cannot read each other's file.
        settings_path: test_log_path().with_extension("json"),
        settings: crate::settings::Settings::default(),
        themes: agent_manager::theme::available(),
        detail_scroll: 0,
        detail_max: std::cell::Cell::new(0),
        panes: std::cell::Cell::new(PaneRects::default()),
        agent_pane_fit: std::cell::RefCell::new(None),
        idle_gate: std::collections::HashMap::new(),
        decisions_seen: std::collections::HashMap::new(),
        row_hits: std::cell::RefCell::new(Vec::new()),
        settings_hits: std::cell::RefCell::new(Vec::new()),
        create_hits: std::cell::RefCell::new(Vec::new()),
        key_hits: std::cell::RefCell::new(Vec::new()),
        top_hits: std::cell::RefCell::new(Vec::new()),
        preview_attach_hit: std::cell::Cell::new(Rect::ZERO),
        composer_hit: std::cell::Cell::new(Rect::ZERO),
        switch_hits: std::cell::RefCell::new(Vec::new()),
        board_hits: std::cell::RefCell::new(Vec::new()),
        board_column_hits: std::cell::RefCell::new(Vec::new()),
        board_column_start: std::cell::Cell::new(0),
        board_lane_offsets: std::cell::Cell::new([0; BoardColumn::ALL.len()]),
        preview_capture: std::cell::RefCell::new(None),
        // Empty by default — tests that exercise the model picker PRE-SEED this so the pick list is
        // deterministic and never depends on the host's real claude/codex CLIs.
        model_catalog: std::collections::HashMap::new(),
        // A home with no `projects/` dir ⇒ the resume-vs-create probe answers `None` ⇒ the
        // optimistic-resume default, so these tests don't depend on the CI machine's real
        // `~/.claude`. The ghost-id create path is exercised by its own test, which points
        // `claude_home` at a scratch `projects/`.
        claude_home: Some(PathBuf::from("/nonexistent/.claude")),
        pmtui_bin: None,
        pending_brief_edit: None,
        pending_directive_edit: None,
        pending_send: None,
        initial_message_retries: std::collections::HashMap::new(),
        message_drafts: std::collections::HashMap::new(),
        pending_chat: None,
        pending_create_chat: None,
        pending_attach_loop: None,
        pending_fork: None,
        pending_first_chat: None,
        armed_drains_left: None,
        scroll_max: std::cell::Cell::new(usize::MAX),
        answer_scroll_top: std::cell::Cell::new(0),
        daemon_live: std::cell::Cell::new(None),
        daemon_down_streak: std::cell::Cell::new(0),
        spawned_pmd: std::cell::RefCell::new(Vec::new()),
        spawn: SpawnBroker::default(),
        agent_tmux,
    }
}

fn stop(id: &str, kind: &str, rc: RiskClass) -> Stop {
    Stop {
        id: id.into(),
        kind: kind.into(),
        risk_class: rc,
        question: "which path?".into(),
        options: vec![],
        context_ref: None,
        status: "awaiting_reply".into(),
    }
}

/// `(row text, per-cell (fg, modifiers))` for every row of the rendered buffer.
///
/// The fill assertions need MODIFIERS, not just colour: a reverse-video field and
/// plain hued text share a foreground and differ only here.
fn screen_rows_styled(terminal: &Terminal<TestBackend>) -> Vec<(String, Vec<(Color, Modifier)>)> {
    let buf = terminal.backend().buffer();
    (buf.area.top()..buf.area.bottom())
        .map(|y| {
            let mut text = String::new();
            let mut styles = Vec::new();
            for x in buf.area.left()..buf.area.right() {
                if let Some(c) = buf.cell((x, y)) {
                    text.push_str(c.symbol());
                    styles.push((c.fg, c.modifier));
                }
            }
            (text, styles)
        })
        .collect()
}

/// The style of the cells under the FIRST occurrence of `needle` on the first row
/// that contains it. `None` when no row does.
fn styles_under(terminal: &Terminal<TestBackend>, needle: &str) -> Option<Vec<(Color, Modifier)>> {
    styles_under_row(terminal, "", needle)
}

/// [`styles_under`], restricted to rows containing `row_contains` — needed because a
/// word like `stuck` appears in the header count as well as in the row, and the
/// first match would silently be the wrong surface.
fn styles_under_row(
    terminal: &Terminal<TestBackend>,
    row_contains: &str,
    needle: &str,
) -> Option<Vec<(Color, Modifier)>> {
    screen_rows_styled(terminal)
        .into_iter()
        .find_map(|(t, st)| {
            if !t.contains(row_contains) {
                return None;
            }
            let byte = t.find(needle)?;
            // TestBackend cells are one symbol each and these fixtures are ASCII in the
            // region under test, so a char offset is a cell offset.
            let start = t[..byte].chars().count();
            Some(st[start..start + needle.chars().count()].to_vec())
        })
}

/// Flatten a rendered TestBackend buffer into row-major text for title checks.
/// The four fixture rows the section tests share: two pmd-driven, one hand-driven, one
/// paused — deliberately handed to the app in the WRONG order, so a render that trusted the
/// vec instead of grouping it would draw duplicate headers.
fn sectioned_app() -> App {
    let loop_row = |id: &str, posture: Posture, tier: Tier, stops: Vec<Stop>| {
        let mut v = view(id, posture, stops);
        v.mode = Mode::AgentLoop;
        v.tier = Some(tier);
        v
    };
    let mut paused = loop_row("stopped", Posture::Working, Tier::Standard, vec![]);
    paused.enabled = false;
    app_with(
        vec![
            loop_row("driven", Posture::Working, Tier::Autopilot, vec![]),
            loop_row("mine", Posture::Monitoring, Tier::Standard, vec![]),
            paused,
            loop_row(
                "asking",
                Posture::NeedsYou,
                Tier::Autopilot,
                vec![stop("s1", "publish", RiskClass::Hard)],
            ),
        ],
        UiMode::Normal,
    )
}

/// The SESSIONS pane's rows, from the header rule down, with the pane frame stripped — so a
/// section test asserts on what is inside the box and nothing else.
///
/// Callers pass a ROOMY terminal (100x20) on purpose: below `NARROW_W` the layout stacks the
/// preview UNDER the list, which leaves the list four rows and silently truncates the
/// sections a test is trying to read.
fn session_rows(app: &App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| render(f, app)).expect("render");
    screen_rows(&t)
        .into_iter()
        .map(|(text, _)| text)
        .skip_while(|r| !r.contains("SESSIONS"))
        .skip(1)
        .take_while(|r| !r.starts_with('└'))
        .map(|r| {
            // Strip the left border column and any right-hand pane that shares the row.
            let inner: String = r.chars().skip(1).collect();
            inner
                .split('│')
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn screen_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}

/// A couple of real stream-json event lines — the LEGACY per-wake transcript
/// shape written by the old ephemeral worker to `steps/<seq>.log`. Its
/// `All done here` result line is the marker tests grep for to prove the
/// `render_transcript` fallback (not the live pane) produced the Log section.
const STEP_LOG_FIXTURE: &str = concat!(
    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Working on the task"}]}}"#,
    "\n",
    r#"{"type":"result","subtype":"success","is_error":false,"result":"All done here"}"#,
    "\n",
);

/// A registry + seeded agent-loop session whose PREVIEW has something to show:
/// the agent's marker report (`needs-you.json`, the file the persistent loop
/// actually writes) and a `steps/0.log` transcript. Returns the app (registry
/// wired) and the session paths so a test can tweak the fixture. The preview
/// driver reports nothing alive, so the Log section takes the step-log path.
fn app_with_wake_fixture(dir: &Path, marker: Option<&str>) -> (App, ProjectPaths) {
    // AUTOPILOT, because the preview only shows a SCHEDULE for a row something drives: since
    // m41 an undriven row hides the check-in countdown rather than counting down to an event
    // nobody will run. Every test on this fixture is about what a driven session displays.
    let (reg_path, root) = reg_with_tier(dir, "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    std::fs::create_dir_all(sp.steps_dir()).unwrap();
    if let Some(body) = marker {
        std::fs::write(sp.needs_you(), body).unwrap();
    }
    std::fs::write(sp.step_log(0), STEP_LOG_FIXTURE).unwrap();
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;
    (app, sp)
}

/// A canned `capture-pane` snapshot of a persistent agent's tmux pane, shaped
/// like the real thing: transcript rows, then claude's own chrome (the finished
/// `✻ Cogitated` spinner line, the input box, a statusline row) and TRAILING
/// BLANK ROWS — which the Log region must trim so the tail sits flush.
const PANE_FIXTURE: &str = concat!(
    "> tighten the stall backstop\n",
    "\n",
    "● Reading src/job_engine.rs\n",
    "  read 412 lines\n",
    "\n",
    "● busy_since never resets\n",
    "\n",
    "* Cogitated for 20s\n",
    "──────────────────\n",
    " > \n",
    "──────────────────\n",
    "  auto mode on\n",
    "\n",
    "   \n",
    "\n",
);

/// A MINIMAL `capture-pane -e` snapshot — the styled counterpart of [`PANE_FIXTURE`].
///
/// Byte-exact, hand-written, and only the forms that actually occur: an indexed-colour
/// run that CONTINUES onto the next row with no SGR of its own (the wrapped-run case
/// that a per-line parser renders white), a bold tool row whose filename is wrapped in
/// OSC 8, a dim footer, and trailing blank rows. NOT a screen dump — no model names,
/// hostnames or machine-specific paths, so it runs anywhere.
const PANE_FIXTURE_STYLED: &str = concat!(
    "\u{1b}[38;5;153m* reading the stall backstop\n",
    "  and its wrapped continuation\u{1b}[39m\n",
    "\u{1b}[1mWrite\u{1b}[0m(\u{1b}]8;;file:///p.txt\u{1b}\\p.txt\u{1b}]8;;\u{1b}\\)\n",
    "\u{1b}[2m  auto mode on\u{1b}[0m\n",
    "\n",
    "   \n",
);

/// `(row text, per-cell fg)` for every row of the rendered buffer. Row text alone
/// cannot tell colour from white, so the colour assertions read the CELLS.
fn screen_rows(terminal: &Terminal<TestBackend>) -> Vec<(String, Vec<Color>)> {
    let buf = terminal.backend().buffer();
    (buf.area.top()..buf.area.bottom())
        .map(|y| {
            let mut text = String::new();
            let mut fgs = Vec::new();
            for x in buf.area.left()..buf.area.right() {
                if let Some(c) = buf.cell((x, y)) {
                    text.push_str(c.symbol());
                    fgs.push(c.fg);
                }
            }
            (text, fgs)
        })
        .collect()
}

/// Seed a registry + agent-loop session for the LIVE-PANE tests. `step_log` also
/// writes the legacy `steps/0.log` transcript so a test can prove which source
/// wins. Returns the registry path and the session's `pmloop-…` tmux session
/// name — taken from `tmux::session_name`, never hand-derived, so the test
/// fails loudly if pmtui ever starts guessing the name itself.
fn loop_pane_fixture(dir: &Path, step_log: bool) -> (PathBuf, String) {
    let (reg_path, root) = reg_with_agent_loop(dir, "bot");
    if step_log {
        let sp = ProjectPaths::for_session(&root, "bot");
        std::fs::create_dir_all(sp.steps_dir()).unwrap();
        std::fs::write(sp.step_log(0), STEP_LOG_FIXTURE).unwrap();
    }
    (reg_path, session_name("bot", &root))
}

/// An app selected on the [`loop_pane_fixture`] session, previewing through `driver`.
fn pane_app(reg_path: &Path, driver: FakePane) -> App {
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(driver),
    );
    app.registry_path = reg_path.to_path_buf();
    app
}

/// The plain text of each styled row, for the row-windowing assertions below.
fn rows(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// [`stop`] plus the agent's own question and offered choices — the shape a real
/// decision marker produces once the ledger carries them.
fn stop_asking(id: &str, kind: &str, question: &str, options: &[&str]) -> Stop {
    Stop {
        question: question.into(),
        options: options.iter().map(|o| o.to_string()).collect(),
        ..stop(id, kind, RiskClass::Hard)
    }
}

/// The real question observed live: the agent had met the goal and asked whether
/// to close, offering three choices — and pmtui showed only `hard confirm_done`.
const LIVE_QUESTION: &str = "jokes.md now holds 5 numbered jokes (goal threshold \
     met). Confirm and close, or should I keep adding more?";

/// A stop with NO draft text: what the harness SYNTHESIZES (`phase_engine`'s
/// `confirm_done`/`stuck` parks pass `""`), and what an agent marker that omitted
/// its `question` leaves behind. [`stop`]'s canned question is blanked here.
fn stop_textless(id: &str, kind: &str) -> Stop {
    Stop {
        question: String::new(),
        options: vec![],
        ..stop(id, kind, RiskClass::Hard)
    }
}

/// Every synthesized (text-less) kind, and the leading words of the product string
/// it must render. Short enough to sit inside ONE rendered row at the widths used
/// below — `screen_text` concatenates rows with no separator, so an assertion that
/// spans a wrap can never match.
/// The leading words of each synthesized stop's product string. Leading, because the
/// `Stops` block truncates a row's TAIL, so what matters is that the human sees the
/// SUBJECT first — and because every one of these strings is now <= 74 chars, the
/// measured row budget at 120 columns, the ACTION half survives too. The first drafts
/// were ~95 chars and lost "Close the session, or tell it to keep going" on screen.
const SYNTHESIZED_STOP_COPY: [(&str, &str); 3] = [
    ("confirm_done", "Agent says the goal is met"),
    ("capability", "Needs your decision"),
    ("stuck", "No progress after repeated tries"),
];

/// A stop with a real multi-option question, shaped like the one the user was looking at.
fn confirm_done_view() -> ProjectView {
    view(
        "test",
        Posture::NeedsYou,
        vec![stop_asking(
            "stop-test-0",
            "confirm_done",
            "Goal 'tell me animal joke' looks satisfied: joke told in chat, written to \
             /workplace/phahng/test/jokes.md, and sent via Slack self-DM. Close the session?",
            &[
                "Close the session - the joke was delivered",
                "Keep going: more animal jokes (different animals / styles)",
                "Keep going: a specific animal I name",
            ],
        )],
    )
}

/// Build an App parked on the create form for a loop session at the given
/// Autonomy tier (goal set — the happy path). Every session seeds a
/// `Mode::AgentLoop` session; the tier (the one dial) is set explicitly so a
/// test can exercise any of the levels.
fn creating_loop_app(
    reg_path: &Path,
    dir: &Path,
    engine: Engine,
    tier: Tier,
    goal: &str,
    cadence_s: u64,
) -> App {
    App {
        registry_path: reg_path.to_path_buf(),
        socket: "pm-test".into(),
        // No preview is drawn behind the create form; a dead-everything fake
        // keeps this construction tmux-free like `app_with`.
        agent_tmux: Box::new(FakePane::default()),
        projects: vec![],
        selected: 0,
        mode: UiMode::Creating(CreateForm {
            task_mode: false,
            field: CreateForm::GOAL,
            engine,
            worker_model: None,
            model_choices: Vec::new(),
            dir: dir.display().to_string().into(),
            name: Field::new(),
            tier,
            goal: goal.into(),
            cadence_s,
            decider_engine: Engine::Claude,
            decider_model: None,
            decider_model_choices: Vec::new(),
        }),
        board_detail_open: false,
        return_to_board_after_create: false,
        return_to_board_after_send: false,
        return_to_board_after_answer: false,
        return_to_board_after_switch: false,
        return_to_board_after_action: false,
        answering_stop_id: None,
        status: String::new(),
        should_quit: false,
        dashboard_owner_nonce: None,
        status_log: test_log_path(),
        last_logged: None,
        // Preferences beside the throwaway log, for the same reason: a fixture must never read or
        // write the human's `pmtui.json`. Unique per fixture like the log itself, so two parallel
        // tests saving preferences cannot read each other's file.
        settings_path: test_log_path().with_extension("json"),
        settings: crate::settings::Settings::default(),
        themes: agent_manager::theme::available(),
        detail_scroll: 0,
        detail_max: std::cell::Cell::new(0),
        panes: std::cell::Cell::new(PaneRects::default()),
        agent_pane_fit: std::cell::RefCell::new(None),
        idle_gate: std::collections::HashMap::new(),
        decisions_seen: std::collections::HashMap::new(),
        row_hits: std::cell::RefCell::new(Vec::new()),
        settings_hits: std::cell::RefCell::new(Vec::new()),
        create_hits: std::cell::RefCell::new(Vec::new()),
        key_hits: std::cell::RefCell::new(Vec::new()),
        top_hits: std::cell::RefCell::new(Vec::new()),
        preview_attach_hit: std::cell::Cell::new(Rect::ZERO),
        composer_hit: std::cell::Cell::new(Rect::ZERO),
        switch_hits: std::cell::RefCell::new(Vec::new()),
        board_hits: std::cell::RefCell::new(Vec::new()),
        board_column_hits: std::cell::RefCell::new(Vec::new()),
        board_column_start: std::cell::Cell::new(0),
        board_lane_offsets: std::cell::Cell::new([0; BoardColumn::ALL.len()]),
        preview_capture: std::cell::RefCell::new(None),
        // Empty by default — tests that exercise the model picker PRE-SEED this so the pick list is
        // deterministic and never depends on the host's real claude/codex CLIs.
        model_catalog: std::collections::HashMap::new(),
        // A home with no `projects/` dir ⇒ the resume-vs-create probe answers `None` ⇒ the
        // optimistic-resume default, so these tests don't depend on the CI machine's real
        // `~/.claude`. The ghost-id create path is exercised by its own test, which points
        // `claude_home` at a scratch `projects/`.
        claude_home: Some(PathBuf::from("/nonexistent/.claude")),
        pmtui_bin: None,
        pending_brief_edit: None,
        pending_directive_edit: None,
        pending_send: None,
        initial_message_retries: std::collections::HashMap::new(),
        message_drafts: std::collections::HashMap::new(),
        pending_chat: None,
        pending_create_chat: None,
        pending_attach_loop: None,
        pending_fork: None,
        pending_first_chat: None,
        armed_drains_left: None,
        scroll_max: std::cell::Cell::new(usize::MAX),
        answer_scroll_top: std::cell::Cell::new(0),
        daemon_live: std::cell::Cell::new(None),
        daemon_down_streak: std::cell::Cell::new(0),
        spawned_pmd: std::cell::RefCell::new(Vec::new()),
        spawn: SpawnBroker::default(),
    }
}

/// Build an App parked on the create form for a loop session (goal set — the
/// happy path) at the default Standard Autonomy tier. Every session seeds the
/// same Mode::AgentLoop session.
fn creating_agent_loop_app(
    reg_path: &Path,
    dir: &Path,
    engine: Engine,
    goal: &str,
    cadence_s: u64,
) -> App {
    creating_loop_app(reg_path, dir, engine, Tier::Standard, goal, cadence_s)
}

fn tmux_available() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A `Config` at `tier` with the standard harness defaults (the same set
/// `seed_agent_loop` writes). For tests needing a config OUTSIDE a seeded agent-loop
/// session — e.g. a shared ROOT `config.json`, where seeding a whole session (brief +
/// ledger) would be wrong.
fn config_at(tier: Tier) -> Config {
    Config {
        autonomy: tier,
        step_timeout_s: 1800,
        max_failures: 3,
        stuck_threshold: 3,
        coordinator_lease_s: 1860,
        decider_engine: Engine::Claude,
        decider_model: None,
    }
}

fn agent_loop_view(id: &str) -> ProjectView {
    let mut v = view(id, Posture::Working, vec![]);
    v.mode = Mode::AgentLoop;
    v.engine = Some(Engine::Claude);
    // The human-driven default: Standard means pmd does not drive it, so the row buckets
    // on session liveness rather than posture. `autopilot_loop_view` overrides to Autopilot.
    v.tier = Some(Tier::Standard);
    v
}

/// [`agent_loop_view`] on AUTOPILOT — a row pmd actually drives, which is what the
/// autopilot-gated keys (`c`) and chips require.
fn autopilot_loop_view(id: &str) -> ProjectView {
    let mut v = agent_loop_view(id);
    v.tier = Some(Tier::Autopilot);
    v
}

/// [`agent_loop_view`] as a spawn request staged it: disabled, Standard, and still owed its one
/// launch by the broker, so every start path refuses it (`start_refusal`).
fn staged_spawn_view(id: &str) -> ProjectView {
    let mut v = agent_loop_view(id);
    v.enabled = false;
    v.spawn_staged = true;
    v.spawned_by = Some("parent".into());
    v.spawned_by_label = Some("parent".into());
    v
}

/// A registry with one agent-loop entry (root `<dir>/<id>`) plus its seeded
/// per-session state (config + brief + fresh Idle ledger). Returns the registry
/// path and the session root.
fn reg_with_agent_loop(dir: &Path, id: &str) -> (PathBuf, PathBuf) {
    let root = dir.join(id);
    let reg_path = dir.join("registry.json");
    let mut reg = Registry::default();
    reg.projects.push(ProjectEntry {
        id: id.into(),
        display_name: None,
        root: root.clone(),
        enabled: true,
        mode: Mode::AgentLoop,
        engine: Some(Engine::Claude),
        worker_model: None,
        initial_prompt: None,
        task_title: None,
        forked_from: None,
        spawned_by: None,
        launch: None,
        conversation_id: None,
        cadence_s: Some(300),
    });
    reg.save(&reg_path).unwrap();
    let session_paths = ProjectPaths::for_session(&root, id);
    seed_agent_loop(
        &session_paths,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        1000,
    )
    .unwrap();
    job::save(
        &session_paths,
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    let skill_file = session_paths.canonical_worker_skill_file();
    std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
    std::fs::write(skill_file, agent_manager::skills::WORKER_SKILL_MD).unwrap();
    agent_manager::skills::ensure_claude_worker_skill_link(&session_paths).unwrap();
    (reg_path, root)
}

/// Write a `driver.json` for an agent-loop session under `sp` with the given
/// worker pane + exit reason (mirrors what the daemon records at spawn/reap).
fn write_driver_json(sp: &ProjectPaths, pane: &str, reason: ExitReason) {
    state::write_json_atomic(
        &sp.driver(),
        &DriverState {
            step_id: 1,
            pane: pane.into(),
            spawned_at: 1000,
            deadline: 5000,
            ended_at: None,
            exit_code: None,
            exit_reason: reason,
            consecutive_failures: 0,
            observed_at: 1000,
        },
    )
    .unwrap();
}

/// Flip a seeded session's recorded tier through the SAME path `cycle_tier` writes,
/// so a test fixture and the real `m` key agree on where the dial lives.
fn set_tier(sp: &ProjectPaths, tier: Tier) {
    let mut c: Config = state::read_json(&sp.config()).unwrap();
    c.autonomy = tier;
    state::write_json_atomic(&sp.config(), &c).unwrap();
}

/// Age the cached daemon-liveness sample past [`DAEMON_PROBE_TTL`], so the next
/// `daemon_live()` re-probes — the test-time equivalent of an idle tick a few
/// seconds later, without sleeping. (Subtracting from an `Instant` is safe here:
/// `CLOCK_MONOTONIC` is already seconds past boot by the time cargo has built.)
fn expire_daemon_cache(app: &App) {
    let (at, seen) = app.daemon_live.get().expect("a sample to expire");
    let stale = at
        .checked_sub(DAEMON_PROBE_TTL + Duration::from_millis(1))
        .expect("monotonic clock is past the TTL");
    app.daemon_live.set(Some((stale, seen)));
}

/// `reg_with_agent_loop`, but with the autonomy dial set — pause/resume care about the tier
/// they must NOT change, and the shared helper always seeds Standard.
fn reg_with_tier(dir: &Path, id: &str, tier: Tier) -> (PathBuf, PathBuf) {
    let (reg_path, root) = reg_with_agent_loop(dir, id);
    let sp = ProjectPaths::for_session(&root, id);
    let mut cfg = state::read_json::<Config>(&sp.config()).unwrap();
    cfg.autonomy = tier;
    state::write_json_atomic(&sp.config(), &cfg).unwrap();
    (reg_path, root)
}

fn enabled_on_disk(reg_path: &Path, id: &str) -> bool {
    Registry::load(reg_path)
        .unwrap()
        .projects
        .iter()
        .find(|p| p.id == id)
        .expect("row present")
        .enabled
}

fn tier_on_disk(root: &Path, id: &str) -> Tier {
    state::read_json::<Config>(&ProjectPaths::for_session(root, id).config())
        .unwrap()
        .autonomy
}

/// A minimal awaiting-reply [`pmstate::OpenStop`] of `kind` (id `stop-1` by
/// default), matching the daemon.rs / job_engine.rs Blocked fixtures.
fn open_stop(id: &str, kind: pmstate::StopKind) -> pmstate::OpenStop {
    pmstate::OpenStop {
        id: id.into(),
        kind,
        pane_dialog: None,
        channel: None,
        context_ref: None,
        question: None,
        options: vec![],
        authorized_responders: vec![],
        message_id: None,
        first_posted: 1000,
        last_polled: None,
        last_seen_reply_ts: None,
        status: pmstate::StopStatus::AwaitingReply,
    }
}

/// An App pointed at `reg_path` and refreshed from disk (so `app.projects` is
/// built from the on-disk registry + per-session ledgers).
fn loop_app(reg_path: &Path) -> App {
    let mut app = app_with(vec![], UiMode::Normal);
    app.registry_path = reg_path.to_path_buf();
    app.refresh();
    app
}

/// Stand in for a live pmd by holding its singleton lease.
fn hold_current_daemon(registry: &Path, socket: &str) -> ProjectLease {
    lease::try_acquire(&lease::daemon_lock_path(registry, socket))
        .unwrap()
        .expect("daemon lock is free")
}

/// Write a per-session AgentLoop ledger with `run` + `open_stops` under a fresh
/// temp session, then read it back into a view (mirrors what `refresh` does).
/// The `TempDir` is returned so the caller keeps the files alive if needed.
fn loop_view(
    run: job::JobRun,
    open_stops: Vec<pmstate::OpenStop>,
    now: Epoch,
) -> (tempfile::TempDir, ProjectView) {
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    // AUTOPILOT on purpose: this helper exists to test that the row/preview reflects the
    // ledger `run` (Running → working, Monitoring → a check-in countdown), and the ledger is
    // the truth ONLY for a session pmd actually drives. A Standard row is human-driven and its
    // ledger is a frozen snapshot, so it reflects live REPL liveness instead (m74) — covered by
    // its own tests, not this one.
    seed_agent_loop(
        &sp,
        Tier::Autopilot,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        1000,
    )
    .unwrap();
    job::save(&sp, &AgentLoopState::fresh(Engine::Claude, Some(300), 1000)).unwrap();
    let mut l = job::load(&sp).unwrap().unwrap();
    l.run = run;
    l.open_stops = open_stops;
    job::save(&sp, &l).unwrap();
    let mut v = ProjectView::read_agent_loop("bot", &sp, true, now);
    v.engine = Some(Engine::Claude);
    (dir, v)
}

/// A claude pane sitting at its bare prompt, with the statusline footer that makes
/// the prompt NOT the last line — minimal, and no machine-specific chrome.
const IDLE_CLAUDE_PANE: &str = "\u{2500}\u{2500}\u{2500}\u{2500}\n\
     \u{276f}  \n\
     \u{2500}\u{2500}\u{2500}\u{2500}\n\
     \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)\n";

/// The permission dialog, from the project's own captured shape: a question, a
/// pre-highlighted numbered choice, and no composer anywhere.
const CLAUDE_DIALOG_PANE: &str = "\u{25cf} Write(hello.txt)\n\
     Do you want to create hello.txt?\n\
     \u{276f} 1. Yes\n\
     2. Yes, allow all edits during this session (shift+tab)\n\
     3. No\n\
     Esc to cancel \u{b7} Tab to amend\n";

/// codex's approval dialog: its selected choice is led by the SAME `\u{203a}` as its
/// composer, and carries a bare `y` accelerator.
const CODEX_DIALOG_PANE: &str = "\u{2022} Allow running `git push`?\n\
     \u{203a} 1. Yes, proceed (y)\n\
     2. No, and tell Codex what to do differently (esc)\n";

/// An App whose selected row is a registered agent-loop session, with `tail` on its
/// pane. Returns the app, the driver handle, the `pmloop-` session name and the
/// project root.
fn send_app(dir: &Path, tail: &str) -> (App, FakePane, String, PathBuf) {
    let (reg_path, root) = reg_with_agent_loop(dir, "bot");
    let session = session_name("bot", &root);
    let pane = FakePane::sendable(&session, tail);
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
    app.registry_path = reg_path;
    app.refresh();
    (app, pane, session, root)
}

/// Press `s`, type `text` one key at a time, then Enter — the real key path, so
/// nothing here can pass by poking `UiMode` directly.
fn send_via_keys(app: &mut App, text: &str) {
    handle_key(app, KeyCode::Char('s'), KeyModifiers::NONE);
    for c in text.chars() {
        handle_key(app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(app, KeyCode::Enter, KeyModifiers::NONE);
}

/// A target for the pure-decision tests.
fn send_target(driven: bool) -> SendTarget {
    SendTarget {
        id: "bot".into(),
        root: PathBuf::from("/nonexistent"),
        session: "pmloop-bot".into(),
        agent_loop: true,
        driven,
        in_chat: false,
    }
}

/// Press `G`, then type `text` into the inline field, one key at a time — the real
/// key path, so nothing here can pass by poking `UiMode` directly.
fn open_and_type(app: &mut App, text: &str) {
    handle_key(app, KeyCode::Char('g'), KeyModifiers::NONE);
    for c in text.chars() {
        handle_key(app, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

/// Press `^X^E`, the editor chord the three prose buffers share: `^X` arms the prefix, `^E` completes
/// it. Bare `^E` is end-of-line INSIDE those buffers now, which is why the chord exists.
fn press_editor_chord(app: &mut App) {
    handle_key(app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    handle_key(app, KeyCode::Char('e'), KeyModifiers::CONTROL);
}

/// Press `^X^R` — the directive field's RESCIND.
fn press_rescind_chord(app: &mut App) {
    handle_key(app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    handle_key(app, KeyCode::Char('r'), KeyModifiers::CONTROL);
}

/// A buffer for each prose surface, NAMED. Deliberately not a `From<&str>` impl: a [`Composer`] carries
/// the prompt an EMPTY one shows, and a goal field asking "message this session" is a visible bug that
/// no assertion about its text would catch. Naming the surface at each construction is the point.
fn goal_buf(text: impl Into<String>) -> Composer {
    Composer::seeded(text.into(), crate::composer::GOAL)
}

fn directive_buf(text: impl Into<String>) -> Composer {
    Composer::seeded(text.into(), crate::composer::DIRECTIVE)
}

fn msg_buf(text: impl Into<String>) -> Composer {
    Composer::seeded(text.into(), crate::composer::MESSAGE)
}

fn goal_input(app: &App) -> String {
    match &app.mode {
        UiMode::EditingGoal { input, .. } => input.text(),
        m => panic!("expected the inline goal field, got {m:?}"),
    }
}

/// Flatten a rendered [`Line`] to plain text (what the terminal would show).
fn line_text(l: &Line) -> String {
    l.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// One selected agent-loop row with an open stop, so EVERY context-sensitive chip
/// applies and the width tiers are what is actually under test.
fn keybar_app() -> App {
    // Autopilot, because only a row pmd drives offers `s Answer` for its open stop.
    let mut v = autopilot_loop_view("auth-rewrite");
    v.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    app_with(vec![v], UiMode::Normal)
}
