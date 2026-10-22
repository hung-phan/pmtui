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
    /// The pane's tail, STYLED (`capture_tail_styled`), because only the styles tell Claude's
    /// dim hint from a person's draft. `None` = the capture failed, which is a REFUSAL (see below).
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

/// How many trailing lines `s` captures, styled, to gate the send. Wider than
/// `PANE_TAIL_LINES` because `classify_dialog` scans a 24-line window for a
/// question-plus-options block, and because the input box can sit above a tall footer: the gate
/// asks only whether a box is there, never whether the agent is idle.
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
    let Some(styled) = p.capture.as_deref() else {
        return SendPlan::Refuse("can't read the pane — nothing sent".into());
    };
    if let Some(refusal) = pane_refusal(styled) {
        return SendPlan::Refuse(refusal.into());
    }
    // Mid-response is FINE and is sent, not refused: the composer accepts it and it
    // becomes the agent's next turn. Refusing here would reproduce "it did nothing", and
    // the send only DELAYS pmd's next nudge (idle confirmations reset naturally).
    if tmux::classify_pane_styled(styled) == tmux::PaneActivity::Busy
        && tmux::has_composer(&tmux::unstyled(styled))
    {
        return SendPlan::Send(format!("sent → {} (it was working)", t.id));
    }
    SendPlan::Send(format!("sent → {}", t.id))
}

/// Probe the target pane for everything [`send_decision`] needs.
///
/// Shared by `App::plan_send` and `pmtui message`, so a keypress and a CLI send can never
/// disagree about what they looked at.
pub(crate) fn probe(d: &dyn tmux::Driver, t: &SendTarget) -> SendProbe {
    SendProbe {
        alive: d.is_alive(&t.session).ok(),
        pane_dead: d.pane_dead(&t.session).unwrap_or(false),
        // `Driver::has_clients` DEFAULTS to `Ok(true)` (assume attached), which is
        // right for the reaper and wrong here — a probe error must not make a
        // human-pressed key dead. An error reads as "not attached"; the pane-state
        // gate below is the one that fails closed.
        attached: d.has_clients(&t.session).unwrap_or(false),
        in_mode: d.pane_in_mode(&t.session).unwrap_or(false),
        capture: d.capture_tail_styled(&t.session, SEND_GATE_LINES).ok(),
    }
}

/// Write the bytes to the pane, under the session's `input.lock`, re-checking everything
/// that can change while the lock is being acquired.
///
/// The ONE place a message reaches a pane, for `s` and for `pmtui message` alike — the
/// invariant is that every pane input owns `input.lock` across the final recheck, the paste
/// and the Enter, so a second writer here would be a second way to interleave with a pmd
/// nudge.
pub(crate) fn write_to_pane(d: &dyn tmux::Driver, t: &SendTarget, text: &str) -> Result<()> {
    // `tmux::sanitize_send_text`, not `advise::sanitize_control_bytes`: newlines are
    // legitimate here (`send_keys` routes them through `load-buffer`/`paste-buffer`),
    // but a raw ESC arrives as shift+tab and cycles claude's permission mode, and a
    // lone CR submits a second message. The inline field cannot produce either, but
    // the `$EDITOR` buffer and a bracketed paste both can.
    let payload = tmux::sanitize_send_text(text);
    if payload.is_empty() {
        anyhow::bail!("nothing left after removing control characters");
    }
    let paths = ProjectPaths::for_session(&t.root, &t.id);
    let _input_lease =
        lease::acquire_with_retry(&paths.input_lock(), 40, Duration::from_millis(25))?
            .ok_or_else(|| anyhow::anyhow!("another input is still being delivered — try again"))?;
    if d.is_alive(&t.session).ok() == Some(false) {
        anyhow::bail!("the agent's session ended while we waited");
    }
    if d.has_clients(&t.session).unwrap_or(true) {
        anyhow::bail!("a client attached while the message was waiting");
    }
    let capture = d
        .capture_tail_styled(&t.session, SEND_GATE_LINES)
        .context("re-read pane before send")?;
    if let Some(why) = pane_refusal(&capture) {
        anyhow::bail!("the pane changed while the message was waiting: {why}");
    }
    d.send_keys(&t.session, &payload)
}

/// Why the pane cannot take a typed Message right now, from a STYLED capture
/// (`capture_tail_styled`), or `None` when it can.
///
/// Shared by [`send_decision`] and `App::deliver`'s final recheck, so the press and the write
/// can never disagree about what is safe. In order: a dialog (the `Enter` would answer it), the
/// feedback survey drawn by the input box (its digit keys would answer it), no prompt at all, and a person's unsent draft in
/// the box (our text would be glued onto theirs and submitted). The dialog check precedes the
/// prompt check so its clearer reason wins; `has_composer_in_tail` refuses a dialog anyway.
pub(crate) fn pane_refusal(styled: &str) -> Option<&'static str> {
    let cap = tmux::unstyled(styled);
    if tmux::classify_dialog(&cap).is_some() {
        return Some("dialog up — press Enter and answer it there");
    }
    if tmux::feedback_survey(&cap) {
        return Some("the agent is asking for feedback — press Enter and answer it there");
    }
    if !tmux::has_composer_in_tail(&cap, SEND_GATE_LINES) {
        return Some("no prompt on screen — press Enter to look");
    }
    if tmux::claude_draft_in_box(styled) {
        return Some(
            "the agent's input box has unsent text — press Enter, then submit or clear it",
        );
    }
    None
}
