//! `s` — deliver ONE message to a row's already-running agent. Everything the
//! decision needs and nothing that performs it: the resolved target, what a probe
//! observed, the two limits, and the two PURE functions that turn those facts into a
//! send or a refusal. The bytes are written by `App::deliver` in `app/sending.rs`.

use crate::*;

/// A queued `$EDITOR` compose-then-send, drained by `run()` (the editor needs the real
/// tty, so it cannot run inside the key handler).
pub(crate) struct SendReq {
    pub(crate) target: SendTarget,
    /// What had been typed inline when `Ctrl+E` was pressed, used to seed the buffer.
    pub(crate) seed: String,
    /// The caret the composer had, as `(row, column)`, restored when the editor produces no send.
    /// Two coordinates rather than one offset because the composer is multi-line now.
    pub(crate) cursor: (usize, usize),
}

/// Everything `s` needs to deliver one message, resolved when the field opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendTarget {
    pub(crate) id: String,
    /// The PROJECT ROOT (the path the human entered on create), from which the
    /// interlock rebuilds the per-session state dir and the `pmchat-…` name.
    pub(crate) root: PathBuf,
    /// The tmux session the bytes go to: the daemon's persistent `pmloop-…` for an
    /// agent-loop row, or the interactive session for an interactive one. Always from
    /// `tmux::session_name`/`session_name`, never re-derived here, so
    /// pmtui and pmd can never disagree about which pane belongs to this session.
    pub(crate) session: String,
    /// An agent-loop row, which has a per-session state dir and therefore a chat
    /// interlock. Interactive rows have neither — pmd never sends them keys.
    pub(crate) agent_loop: bool,
    /// pmd DRIVES this row (`daemon::pmd_drives_row`), so a nudge may be in flight.
    pub(crate) driven: bool,
    /// `session` above is the CHAT pane, not the loop pane, because that is where this
    /// conversation currently lives — set by `App::resolve_send_pane` when the `pmloop-`
    /// session is down but a detached `pmchat-` REPL is up. It changes two things: the chat
    /// interlock is not a refusal (we ARE typing there), and `deliver` must not claim that
    /// interlock, because releasing it afterwards would clear the real chat's own marker.
    pub(crate) in_chat: bool,
}

/// What one press of `s` observed about the target pane. Every field is what a probe
/// ACTUALLY returned, so [`send_decision`] is pure and every arm is unit-testable.
#[derive(Debug, Clone, Default)]
pub(crate) struct SendProbe {
    /// `is_alive`. `None` = the probe itself failed.
    pub(crate) alive: Option<bool>,
    /// The pane's process has exited but the pane lingers (`remain-on-exit`). Such a
    /// corpse still shows `❯` and reads Idle — the reason `dead_pane_escalation` exists.
    pub(crate) pane_dead: bool,
    /// A tmux client is attached right now.
    pub(crate) attached: bool,
    /// The pane is in copy-mode/scrollback.
    pub(crate) in_mode: bool,
    /// The pane's tail. `None` = the capture failed, which is a REFUSAL (see below).
    pub(crate) capture: Option<String>,
}

/// What `s` decided to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SendPlan {
    /// Deliver it, then show this receipt.
    Send(String),
    /// Do not deliver it. Say this, and KEEP what was typed.
    Refuse(String),
}

/// How many trailing lines `s` captures to gate the send. Wider than
/// `PANE_TAIL_LINES` because `classify_dialog` scans a 24-line window for a
/// question-plus-options block.
pub(crate) const SEND_GATE_LINES: usize = 40;

/// Largest message `s` will deliver in one press. Past this, `Enter` and a paste into
/// the agent's own pane is the honest route.
pub(crate) const SEND_MAX_BYTES: usize = 8 * 1024;

/// After claiming the chat interlock, how long to let an in-flight pmd nudge finish
/// before writing our own bytes.
///
/// This does NOT close the interleaving window, and saying otherwise would be the
/// mistake. `JobScheduler::drive` reads `human_present` ONCE at the top of a tick, so a
/// marker written after that read is seen by nobody — and between that gate and pmd's
/// final `Enter` there are several tmux spawns plus the post-paste submit check,
/// inside a 500ms sweep. Unguarded, the failure is concrete: pmd pastes a nudge, we
/// append our text, and pmd's `Enter` submits the concatenation.
///
/// So the marker stops the NEXT tick and this delay outlasts a tick already in flight.
/// The residual window is small and accepted; it is also exactly zero on the row this
/// key matters most for, because with autopilot OFF `pmd_drives_row` returns false and
/// the sweep never touches that pane at all (hence the `driven` gate on the sleep).
/// The refusals that need no tmux at all. Checked FIRST so a dead-end press costs
/// nothing and, in the empty case, so `send-keys -l -- ""` plus `Enter` never submits an
/// empty turn to a live agent.
pub(crate) fn send_precheck(text: &str) -> Option<SendPlan> {
    if text.trim().is_empty() {
        return Some(SendPlan::Refuse("nothing typed — nothing sent".into()));
    }
    if text.len() > SEND_MAX_BYTES {
        let kib = text.len().div_ceil(1024);
        return Some(SendPlan::Refuse(format!(
            "too long ({kib} KiB) — press Enter and paste it there"
        )));
    }
    None
}

/// Whether one press of `s` may write to the pane, given what the probes saw.
///
/// PURE, so all of it is unit-testable — and it has to be, because `FakeDriver` cannot
/// reach most of these states and the dangerous one is invisible to a passing test.
///
/// # Fail-safe polarity
///
/// Mixed, deliberately, and each direction is argued:
///
/// * The PANE-STATE gate fails **closed**. A capture we cannot read, a dialog on screen,
///   or no composer at all ⇒ refuse. `Driver::send_keys` always finishes with a separate
///   `send-keys Enter`, so with a numbered permission dialog up the typed characters go
///   to a select widget's keyboard accelerators and the `Enter` CONFIRMS whatever option
///   was pre-highlighted — granting a write, or worse, "allow all edits during this
///   session" if the text happened to contain a `2`. The human's message is destroyed
///   with `send_keys` returning `Ok`. That is not a hypothetical: `park_dialog` exists
///   because "typing a choice into a live agent is the riskiest action in the system",
///   and `is_codex_composer` excludes numbered lines for exactly this reason. It is also
///   the only gate that protects an autopilot-OFF row, where `pmd_drives_row` returns
///   before the tick so nothing ever parks a stop and `v.stops` is EMPTY BY DESIGN — the
///   ledger cannot warn us there.
/// * LIVENESS fails **open**. An `is_alive` probe that errors means tmux itself is
///   unwell; `send_keys` will then fail loudly and report its own error, which is better
///   than a refusal that guesses.
pub(crate) fn send_decision(t: &SendTarget, p: &SendProbe) -> SendPlan {
    if p.pane_dead {
        return SendPlan::Refuse("agent exited — nothing to type into".into());
    }
    if p.alive == Some(false) {
        return SendPlan::Refuse(if t.driven {
            "agent not up yet — pmd starts it".into()
        } else {
            "no agent running — press Enter to open one".into()
        });
    }
    if p.attached {
        // `has_clients` cannot see whether the attached human has a half-typed draft in
        // the composer; appending to it and submitting the concatenation would mangle
        // in-progress input in another terminal. Same reasoning as the copy-mode arm.
        return SendPlan::Refuse("attached there — type it in that pane".into());
    }
    if p.in_mode {
        // `send_keys` leaves copy-mode with `-X cancel`, but that is justified in its own
        // doc comment by "the drive path defers entirely while a human is attached" — an
        // argument `s` does not inherit.
        return SendPlan::Refuse("scrolled back there — send it in that pane".into());
    }
    let Some(cap) = p.capture.as_deref() else {
        return SendPlan::Refuse("can't read the pane — nothing sent".into());
    };
    if tmux::classify_dialog(cap).is_some() {
        return SendPlan::Refuse("dialog up — press Enter and answer it there".into());
    }
    if !tmux::has_composer(cap) {
        return SendPlan::Refuse("no prompt on screen — press Enter to look".into());
    }
    // Mid-response is FINE and is sent, not refused: the composer accepts it and it
    // becomes the agent's next turn. Refusing here would reproduce "it did nothing", and
    // the send only DELAYS pmd's next nudge (idle confirmations reset naturally).
    if tmux::classify_pane(cap) == tmux::PaneActivity::Busy {
        return SendPlan::Send(format!("sent → {} (it was working)", t.id));
    }
    SendPlan::Send(format!("sent → {}", t.id))
}
