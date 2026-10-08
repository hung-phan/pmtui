//! The `Driver` trait the daemon spawns a step through, the handle and outcome
//! types that cross it, and [`observe`], which turns "done-signal present?" plus
//! "session alive?" into an [`Observation`]. The trait and the completion protocol
//! belong in one file because the protocol is what the trait exists for.
//!
//! Completion detection (validated by spike): the step command is wrapped so
//! that on exit it writes its exit code *atomically* (temp + rename) to a
//! done-signal file. The daemon decides from (done-signal present?, alive?):
//!   - done-signal present         -> Completed(exit_code)   [authoritative]
//!   - absent & session alive       -> Running
//!   - absent & session gone        -> Orphaned (crashed/killed before signal)
//!
//! Because a step writes the signal microseconds before its pane dies, "done
//! always wins": [`observe`] re-reads the signal after seeing the session gone,
//! closing the write-then-die race.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::clock::Epoch;

use super::launch::{LaunchError, LaunchOutcome, ManagedEnv};

/// A launched step's handle: the session name plus the files the wrapper writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepHandle {
    pub session: String,
    pub done_signal: PathBuf,
    pub log: PathBuf,
}

/// Outcome of observing a launched step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Running,
    Completed { exit_code: i32 },
    Orphaned,
}

/// The tmux operations the daemon needs. Real: [`TmuxDriver`](super::TmuxDriver); test: `FakeDriver`.
pub trait Driver: Send + Sync {
    /// Spawn `command` (argv) as a detached session named `session`, running in
    /// `cwd`, wrapped so its exit code lands in `done_signal` and its combined
    /// output in `log`.
    fn spawn_step(
        &self,
        session: &str,
        cwd: &Path,
        command: &[String],
        done_signal: &Path,
        log: &Path,
    ) -> Result<StepHandle>;

    /// Is the session still alive?
    fn is_alive(&self, session: &str) -> Result<bool>;

    /// Capture up to the last `lines` of the pane, for diagnostics only.
    fn capture_tail(&self, session: &str, lines: usize) -> Result<String>;

    /// Capture the pane the same way as [`Driver::capture_tail`] but WITH its escape
    /// sequences intact (`capture-pane -e`), for the RENDER path only.
    ///
    /// A SEPARATE method on purpose, and [`Driver::capture_tail`] deliberately keeps its
    /// escape-free output: [`classify_pane`](super::classify_pane), [`classify_dialog`](super::classify_dialog) and the stall/idle
    /// detection all match on literal pane text, so handing them SGR escapes would break
    /// pane classification outright (a colour change dropped into the middle of an
    /// `esc to interrupt` hint stops matching, and the session reads Idle mid-response).
    /// Only pmtui's preview wants the escapes, so only pmtui's preview asks for them.
    ///
    /// Defaults to [`Driver::capture_tail`] so a driver that has not opted in still
    /// renders — unstyled, as before — rather than showing nothing.
    fn capture_tail_styled(&self, session: &str, lines: usize) -> Result<String> {
        self.capture_tail(session, lines)
    }

    /// Kill the session if it exists (idempotent).
    fn terminate(&self, session: &str) -> Result<()>;

    /// Ask the session's process group to stop (SIGTERM), without killing the terminal.
    ///
    /// The POLITE half of cancelling a job, and the reason it is separate from [`Driver::terminate`]:
    /// `claude -p` exits 143 on SIGTERM after running its `SessionEnd` hooks and terminating the
    /// process tree of anything it started, so a signal first is what reaps a child's own children. A
    /// kill is still the fallback — the caller terminates on a later frame if the session is still
    /// alive — so this never has to succeed for a cancel to finish.
    ///
    /// Idempotent and best-effort: a session that is already gone is `Ok`. Defaults to
    /// [`Driver::terminate`] so a driver that has not opted in still stops the job.
    fn request_stop(&self, session: &str) -> Result<()> {
        self.terminate(session)
    }

    /// Type `text` into a live interactive session and submit it (a nudge into the persistent agent).
    /// Literal text then a SEPARATE Enter, with a short bracketed-paste guard delay between;
    /// multiline/large text goes via a tmux paste-buffer. A long Codex paste receives the second
    /// Enter its collapsed-paste composer requires. Best-effort: a dead session surfaces as `Err`.
    /// Default `Err`s ("this driver can't type") so a driver that hasn't opted in never silently
    /// drops a nudge; [`TmuxDriver`](super::TmuxDriver) and `FakeDriver` override it.
    fn send_keys(&self, session: &str, text: &str) -> Result<()> {
        let _ = text;
        bail!("send_keys is not supported by this Driver (session {session})");
    }

    /// Move a numbered dialog's live highlight from `current` to `target`, then
    /// submit exactly once. This is deliberately separate from [`Driver::send_keys`]:
    /// `Up`/`Down`/`Enter` are terminal key names, not text that may be pasted into a
    /// composer. The caller must hold the session's `input.lock` and revalidate the
    /// dialog immediately before calling.
    fn select_dialog_option(&self, session: &str, current: usize, target: usize) -> Result<()> {
        let _ = (current, target);
        bail!("select_dialog_option is not supported by this Driver (session {session})");
    }

    /// Actively prove that a classified menu is interactive by moving its
    /// highlight without submitting, then returning the fresh classified dialog.
    /// Text alone cannot distinguish a live menu from a transcript quoting one.
    fn verify_dialog_interactive(
        &self,
        session: &str,
        expected: &super::PaneDialog,
    ) -> Result<super::PaneDialog> {
        let _ = expected;
        bail!("verify_dialog_interactive is not supported by this Driver (session {session})");
    }

    /// Apply a validated set of original pane-option indices. Single-choice
    /// dialogs require exactly one target; multi-select dialogs toggle the
    /// checkbox delta and then activate their separate Submit row.
    fn select_dialog_options(
        &self,
        session: &str,
        dialog: &super::PaneDialog,
        targets: &[usize],
    ) -> Result<()> {
        let _ = (dialog, targets);
        bail!("select_dialog_options is not supported by this Driver (session {session})");
    }

    /// Launch (or, if already alive by name, leave running) a long-lived interactive
    /// session running `argv` in `cwd`, with `env` applied to its environment. Idempotent
    /// by name (a session already alive is left as-is and reported
    /// [`LaunchOutcome::AlreadyAlive`]), checks the engine binary is on PATH, re-checks
    /// liveness so an engine that crashes on startup surfaces, and binds the bare-`Ctrl+q`
    /// detach key.
    ///
    /// The error says whether an agent could have started
    /// ([`LaunchError::proven_not_started`]); its `Display` is the text callers show.
    /// Default `Err`s ("this driver can't launch interactive sessions") so a driver that
    /// hasn't opted in never silently no-ops, and as an ambiguous [`LaunchError::Probe`] so
    /// it never claims a proven outcome on a guess; [`TmuxDriver`](super::TmuxDriver) and
    /// `FakeDriver` override it.
    fn launch_interactive(
        &self,
        session: &str,
        cwd: &Path,
        argv: &[String],
        env: &ManagedEnv,
    ) -> std::result::Result<LaunchOutcome, LaunchError> {
        let _ = (cwd, argv, env);
        Err(LaunchError::Probe(format!(
            "launch_interactive is not supported by this Driver (session {session})"
        )))
    }

    /// Install the server-wide bare Ctrl+q detach binding used by pmtui's foreground attach.
    /// Default no-op for in-memory and non-tmux drivers.
    fn ensure_detach_key(&self) {}

    /// Attach the caller's current terminal to `session` until the client detaches or the
    /// session ends. The exit status is intentionally not interpreted: both outcomes return the
    /// dashboard to the human. Default `Err` keeps unsupported drivers from claiming an attach.
    fn attach_interactive(&self, session: &str) -> Result<()> {
        bail!("attach_interactive is not supported by this Driver (session {session})");
    }

    /// Discover the exact user-thread Codex rollout held open by a live session.
    /// `None` means no identity could be proven. The default deliberately knows
    /// nothing, so callers can refuse a destructive restart instead of guessing.
    fn codex_session_id(&self, _session: &str, _cwd: &Path) -> Result<Option<String>> {
        Ok(None)
    }

    /// Read the Claude conversation identity written by the fork process's
    /// injected `SessionStart` hook. The file is accepted only when it contains
    /// one UUID, so a malformed or stale hook result cannot publish a child.
    fn claude_session_id(&self, _session: &str, identity_file: &Path) -> Result<Option<String>> {
        let raw = match std::fs::read_to_string(identity_file) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", identity_file.display()));
            }
        };
        let id = raw.trim();
        if !super::codex::is_uuid(id) {
            bail!(
                "{} does not contain one session UUID",
                identity_file.display()
            );
        }
        Ok(Some(id.to_string()))
    }

    /// Resize a session's window to `cols`x`rows`. pmtui calls this on the render tick to
    /// fit the DETACHED agent pane to the live preview area, so a wide terminal shows a
    /// wide transcript instead of tmux's 80-col create default. Best-effort: a dead/missing
    /// session is not worth an error (it will be sized when it next exists).
    ///
    /// SIDE EFFECT (why [`set_window_size_auto`](Driver::set_window_size_auto) exists):
    /// tmux flips the session's `window-size` to `manual` here, which disables
    /// resize-to-attached-client — so a human who then attaches (pmtui's `Enter`) would keep
    /// this preview size instead of their real terminal's. Callers MUST re-enable auto before
    /// attaching. Default no-op so non-tmux drivers ignore the fit.
    fn resize_window(&self, _session: &str, _cols: u16, _rows: u16) -> Result<()> {
        Ok(())
    }

    /// Re-enable tmux's automatic resize-to-attached-client (`window-size latest`) WITHOUT
    /// changing the current size — the counterpart to [`resize_window`](Driver::resize_window),
    /// called just before attaching so `Enter` shows the pane at the human's real terminal
    /// size rather than the preview's. Default no-op.
    fn set_window_size_auto(&self, _session: &str) -> Result<()> {
        Ok(())
    }

    /// Drop the pane's SCROLLBACK, keeping only the current visible screen. Called right after
    /// a [`resize_window`](Driver::resize_window): a full-screen TUI redraws by clearing, which
    /// pushes the pre-resize frame into scrollback, so a plain `resize` leaves a STACK of stale
    /// frames that a deep `capture-pane -S` then pulls back into the preview (the "multiple
    /// claude panels" a resize shows). Clearing history collapses the capture to the one live
    /// frame — the "repaint". Default no-op.
    fn clear_history(&self, _session: &str) -> Result<()> {
        Ok(())
    }

    /// How many tmux clients are attached to `session` right now. Distinguishes an
    /// IN-USE REPL (a human is attached) from a DETACHED-and-forgotten one, so the
    /// orphan-reap never kills a session someone is typing in. Default `Ok(true)` =
    /// "assume attached" = never reap (fail-safe).
    fn has_clients(&self, _session: &str) -> Result<bool> {
        Ok(true)
    }

    /// The wall-clock epoch the tmux session was CREATED (`#{session_created}`), or
    /// `None` if unknown/dead. Anchors orphan-reaping on the session's OWN age,
    /// independent of the on-disk marker (so a lost marker cannot wedge the poll).
    /// Default `Ok(None)` = "age unknown" = never reap (fail-safe).
    fn session_created(&self, _session: &str) -> Result<Option<Epoch>> {
        Ok(None)
    }

    /// Whether the session's pane is a CORPSE: its process exited but tmux kept the pane
    /// (`#{pane_dead}`, i.e. `remain-on-exit`). The liveness probe that must be consulted
    /// BEFORE a [`Driver::capture_tail`] is trusted, because a capture cannot tell the
    /// difference: a dead pane still shows the last frame the process painted, and if that
    /// frame ends at a bare prompt [`classify_pane`](super::classify_pane) reports `Idle` — so the harness would
    /// type nudges into a process that is gone. (The opposite misread is just as bad: an
    /// EMPTY capture hits `classify_pane`'s conservative `Busy` default and the session
    /// waits out the whole 30-minute stall backstop before saying anything.)
    ///
    /// Default `Ok(false)` = "assume NOT dead" = behave exactly as a driver without this
    /// probe, i.e. never escalate on a guess — the same fail-safe direction as
    /// [`Driver::has_clients`] (assume attached ⇒ never reap) and
    /// [`Driver::session_created`] (age unknown ⇒ never reap).
    fn pane_dead(&self, _session: &str) -> Result<bool> {
        Ok(false)
    }

    /// Whether the session's pane is sitting in a tmux MODE (`#{pane_in_mode}`) —
    /// in practice COPY-MODE, left behind when a human scrolls back in an attached
    /// session and detaches without pressing `q`.
    ///
    /// The probe [`Driver::send_keys`] must consult, because a pane in copy-mode
    /// SILENTLY DESTROYS a nudge. Measured on tmux 3.6a against a `cat` pane that
    /// records what actually reaches the pty:
    ///   - `paste-buffer` exits **0** and the text lands in the pane;
    ///   - the following `send-keys Enter` is swallowed by the copy-mode key table —
    ///     the mode flips 1→0 and **no newline ever reaches the pty**. Of a
    ///     two-line payload the pty saw only the first line (19 of 34 bytes), and
    ///     nothing was submitted. With an `-X cancel` first, all 34 bytes arrive.
    ///
    /// Both calls therefore return success, `send_keys` returns `Ok`, and
    /// `JobScheduler::nudge` reads that as delivered and clears `pending_context` —
    /// so a human's parked answer is gone and autopilot "does nothing". Every
    /// production nudge takes this path, because `loop_nudge_prompt` is multi-line.
    /// (The literal path fails louder but still loses the text: `send-keys -l` in
    /// copy-mode exits **1** with `no current client`/`not in a mode` and 0 bytes
    /// reach the pty, which `nudge` at least re-parks on rather than consuming.)
    ///
    /// Default `Ok(false)` = "assume NOT in a mode" = behave exactly as a driver
    /// without this probe, i.e. never act on a guess — the same fail-safe direction
    /// as [`Driver::has_clients`] (assume attached ⇒ never reap),
    /// [`Driver::session_created`] (age unknown ⇒ never reap) and
    /// [`Driver::pane_dead`] (assume not dead ⇒ never escalate).
    fn pane_in_mode(&self, _session: &str) -> Result<bool> {
        Ok(false)
    }
}

/// What a done-signal file currently says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoneSignal {
    /// File does not exist yet.
    Absent,
    /// File exists but is empty/whitespace — a partial write mid-rename, not done.
    Pending,
    /// File exists but does not hold an integer — corrupt (e.g. ENOSPC left a
    /// half-written signal). The step's true fate is unknowable; treat as a crash.
    Corrupt,
    /// A parsed exit code.
    Code(i32),
}

/// Observe a launched step, combining the done-signal and liveness.
///
/// Robust to a corrupt/empty done-signal: a present-but-unparseable signal is
/// reported as [`Observation::Orphaned`] (never a hard error), so a step that
/// finished but left a garbage signal still routes through failure/retry/`Stuck`
/// rather than wedging the project. `read_done_signal` only `Err`s on a genuine
/// I/O fault, and the scheduler's timeout check runs independently of `observe`.
pub fn observe(driver: &dyn Driver, handle: &StepHandle) -> Result<Observation> {
    match read_done_signal(&handle.done_signal)? {
        DoneSignal::Code(code) => return Ok(Observation::Completed { exit_code: code }),
        DoneSignal::Corrupt => return Ok(Observation::Orphaned),
        DoneSignal::Absent | DoneSignal::Pending => {}
    }
    if driver.is_alive(&handle.session)? {
        return Ok(Observation::Running);
    }
    // Session gone with no signal seen yet — re-read to catch a step that wrote
    // its signal and died between our two checks. Done always wins; anything else
    // (absent, still-partial, or corrupt) is an orphan.
    match read_done_signal(&handle.done_signal)? {
        DoneSignal::Code(code) => Ok(Observation::Completed { exit_code: code }),
        _ => Ok(Observation::Orphaned),
    }
}

/// Read a done-signal file. Only a genuine I/O fault is an `Err`; a
/// missing/empty/corrupt file maps to a [`DoneSignal`] variant so callers never
/// abort on bad content.
fn read_done_signal(path: &Path) -> Result<DoneSignal> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let t = s.trim();
            if t.is_empty() {
                Ok(DoneSignal::Pending)
            } else {
                match t.parse::<i32>() {
                    Ok(code) => Ok(DoneSignal::Code(code)),
                    Err(_) => Ok(DoneSignal::Corrupt),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DoneSignal::Absent),
        Err(e) => Err(e).with_context(|| format!("read done-signal {}", path.display())),
    }
}
