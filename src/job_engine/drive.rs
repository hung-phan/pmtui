//! The DRIVE path: what one tick is allowed to do to the live session. The defer
//! gates that refuse to type at a pane a human owns, the Idle/Busy reading of what
//! the pane is showing, the short rechecks that follow, and the stall backstop that
//! bounds a pane which is Busy forever. One unit because it is one decision — may a
//! nudge be typed right now — and every guard here exists to answer it `no` safely.

use anyhow::Result;

use crate::clock::Epoch;
use crate::job::{self, AgentLoopState, AutopilotEventKind, HoldReason, JobRun};
use crate::pmstate::StopKind;
use crate::registry::Engine;
use crate::state::{Config, RiskClass, TurnSignalHealth, turn_signal_health};
use crate::tmux::{self, Driver, PaneActivity};

use super::marker::UnresolvedAdvice;
use super::session::{EnsureOutcome, LAUNCH_GRACE_S};
use super::supervisor::{AdviceSpawn, AdviseStep, DeciderSkip, SUPERVISOR_TIMEOUT_S};
use super::{JobScheduler, JobTick};

/// Busy-recheck interval: when the pane is mid-response (or a transient tmux
/// capture/send error occurred), re-park `Monitoring` this soon and try again — never
/// nudge into a busy pane, but come back promptly once it goes idle.
pub(crate) const BUSY_RECHECK_S: i64 = 5;
/// Confirmation gate (defense in depth): how many **consecutive** `PaneActivity::Idle`
/// observations the heartbeat requires before it types a nudge into the live pane.
///
/// `classify_pane` separates busy from idle almost entirely via the spinner line, and a
/// spinner-led line only counts as Busy when it carries an in-progress ellipsis or a live
/// counter — the FINISHED forms (`✻ Cogitated for 20s`) deliberately do not. That is
/// measured against real captures and correct today, but it means any future claude state
/// that is genuinely working while showing neither marker would read Idle and the harness
/// would type into a mid-response agent. One such window has already been found and fixed
/// (the turn warm-up `✻ Mustering…`, which had no counter yet), so this class of gap is
/// real rather than hypothetical. Requiring TWO observations makes a single misread
/// harmless: the pane has to look idle across two separate captures
/// [`BUSY_RECHECK_S`] apart.
///
/// COST: a nudge is delayed by at most one [`BUSY_RECHECK_S`] (5s) per cadence tick —
/// i.e. an effective cadence of `cadence + 5s`, which is noise against the 5-minute
/// default. Cheap insurance against the far more expensive failure (a keystroke landing
/// mid-response, which corrupts the agent's turn).
pub(super) const IDLE_CONFIRMATIONS_REQUIRED: u32 = 2;
/// Stall backstop — the THIRD bounded-runtime budget: once the pane has been
/// CONTINUOUSLY Busy with NO progress (no idle prompt, no marker bump, no human
/// present) for this long (30 min), the loop ESCALATES a human-dismissable `Stuck`
/// instead of re-parking [`BUSY_RECHECK_S`] forever. This is the "continuous-Busy,
/// no-progress" bound; it closes the deferred I1 silent-stall (a wedged agent —
/// `classify_pane` = Busy indefinitely) and the bad-`--resume` case (`claude --resume
/// <bad-id>` lands on an ERROR prompt, so `classify_pane` = Busy forever and `max_wakes` —
/// which only advances on a delivered nudge — never fires; `max_wall_clock_s` now also
/// bounds that case, but 24h later, which is not a useful time to hear about it).
/// Deliberately NOT `max_wall_clock_s` (the "session too old even while
/// progressing" budget, a different semantic). Could become a per-session config field
/// later.
pub(super) const DEFAULT_STALL_BUSY_S: u64 = 1800;
/// FO-2 (Milestone D) marker-less-recheck backstop (K): after this many CONSECUTIVE
/// completed-turn-but-marker-less rechecks the loop escalates a `WorkerStuck` — a FAST
/// bound independent of the 30-min `DEFAULT_STALL_BUSY_S` and of `max_wakes`. Reset on any
/// accepted marker bump and on `on_blocked`.
pub(super) const MARKER_LESS_RECHECK_MAX: u32 = 3;

/// How long an OWED REPORT may stand before the human is told: 45 minutes.
///
/// Deliberately LONGER than [`DEFAULT_STALL_BUSY_S`], because this is the OUTER bound rather than the
/// first line of defence. The pane-stall window and the marker-less recovery nudge both act sooner and
/// more precisely; this one exists so that when BOTH are blind — a turn-end hook that never fires, and
/// a pane whose output keeps rebasing the stall window — the session still reaches a person instead of
/// being held in silence (measured: 3033 seconds, and only freed by its pane dying).
///
/// 45 minutes is three missed check-ins at the default cadence
/// ([`super::nudge::DEFAULT_CADENCE_S`] = 300s is shorter still, so this is generous for every cadence
/// a human is likely to set). An agent that has said nothing for that long is worth one dismissable
/// question, and the nudge it was sent asks it to report EACH wake.
pub(super) const REPORT_DEBT_CEILING_S: i64 = 2700;

impl JobScheduler {
    /// The DRIVE path (design §2.3), run for `Idle`, a due `Monitoring`, and the
    /// `Running` re-drive. In order:
    ///   1. **Defer gates (consume NO state):** if a human is chatting this
    ///      conversation (`chat_lock::is_active`) OR attached to the loop session
    ///      (`has_clients`), do NOT nudge — return `Monitoring` WITHOUT mutating
    ///      `self.run`, budgets, or any answer. A running wall-clock window is paused
    ///      (`window_start = now`) so a long human session can't trip a spurious stall, and
    ///      an in-flight supervisor consult has its deadline frozen ONCE for the same reason
    ///      (this gate returns before `advise_step`, so an attach used to expire a consult
    ///      that had already answered — m23 bug 3).
    ///   2. `ensure_session`: launch the persistent session if it isn't alive. On a
    ///      JUST-launched session apply cold-start grace (park `Monitoring` and do NOT
    ///      nudge a still-booting `claude`).
    ///      Then, still before anything trusts the pane, in this order:
    ///      [`JobScheduler::budget_backstop`] (this tick is doing work, so it opens the
    ///      wall-clock window and escalates if the budget is spent), the marker disposer
    ///      [`JobScheduler::observe_marker`] (the agent's own self-assessment outranks
    ///      anything we infer from pixels), and [`JobScheduler::dead_pane_escalation`] (ONE
    ///      liveness probe — a dead pane's last frame lies in both directions).
    ///   3. Already up ⇒ classify the pane: `Idle` nudges (budget-gated) only once
    ///      [`IDLE_CONFIRMATIONS_REQUIRED`] CONSECUTIVE Idle observations confirm it (see
    ///      [`JobScheduler::idle_observed`]); `Busy` (or a transient capture error)
    ///      re-parks a short recheck without nudging.
    pub(super) fn drive(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
        config: &Config,
    ) -> Result<JobTick> {
        // 1. Defer gates — never nudge into a chatting/attached pane; consume nothing.
        if self.human_present(driver, now) {
            // Pause the wall-clock window (a human actively here is a "touch"), exactly
            // as the old Monitoring-defer did. `wakes`/answers/`pending_context`/the
            // ledger are all untouched, and `self.run` is NOT mutated.
            if self.window_start.is_some() {
                self.window_start = Some(now);
            }
            // A human attached is not a stall: the pane looks Busy because they're
            // typing/thinking, so clear the stall backstop timer. A human owning the pane
            // also voids any earlier Idle observations (they may have typed since), so the
            // confirmation gate must re-earn both of them after they leave.
            self.busy_since = None;
            self.reset_idle_gate();
            // A human touch is a fresh start for the no-progress run too (slice 1): they may be
            // making the changes themselves, and the next nudge must re-baseline the tree.
            self.reset_no_progress();
            // NOTE: do NOT re-baseline `turns_at_nudge` here. It means "turn count at our last
            // NUDGE", and `turn_in_progress()` (turn_signal <= turns_at_nudge) reads a
            // re-baselined equality as "a turn is in progress". Once the human detaches from an
            // IDLE agent the turn count never climbs past that re-baseline, so `turn_in_progress`
            // stayed true forever, held the drive in `busy_recheck(TurnInProgress)`, and the agent
            // was NEVER nudged again (the 2026-08-21 "monitoring · confirming…" wedge). Human turns
            // cannot cause a spurious `WorkerStuck`: `marker_less_recheck` nudges (resetting the
            // baseline) before it escalates, and a genuinely wedged agent is still caught by the
            // busy-stall backstop. So the nudge baseline is left untouched while a human is here.
            // FREEZE an in-flight supervisor consult, exactly as the wall-clock window above
            // is paused and for the same reason: this gate returns ~44 lines before
            // `advise_step`, so while a client is attached the consult is neither polled nor
            // reaped, and its deadline would run down unattended. The detach tick then read
            // that as "the supervisor produced no result: no answer within 90s" about a
            // supervisor that had answered in ~0s, escalated a decision that was only ever
            // low-stakes, AND counted the attach toward `SUPERVISOR_DEAD_AFTER` — so three
            // ordinary attaches over a session's life switched the feature off with nothing
            // said to the user. Abandoning the consult here instead would be worse: the
            // marker bump is already consumed, so nothing would ever answer the worker.
            //
            // Granted ONCE, not for as long as the human stays — see
            // [`AdviseInFlight::frozen`] for why an unbounded freeze eventually types a
            // stale verdict at a decision the human has already handled by hand.
            if let Some(inflight) = self.advise.as_mut()
                && !inflight.frozen
            {
                inflight.frozen = true;
                inflight.deadline = now + SUPERVISOR_TIMEOUT_S;
            }
            let until = match &self.run {
                JobRun::Monitoring { until } => *until,
                _ => now, // Idle / re-drive → a benign "will re-check" signal
            };
            return Ok(JobTick::Monitoring { until });
        }

        // 2. Ensure the persistent session is alive (launch once).
        match self.ensure_session(driver, now, base)? {
            EnsureOutcome::JustLaunched => {
                // Cold-start grace: `ensure_session` already parked `Monitoring{grace}`
                // + persisted the (possibly minted) conversation id. Do NOT nudge a
                // still-booting claude this tick.
                return Ok(JobTick::Monitoring {
                    until: now + LAUNCH_GRACE_S,
                });
            }
            EnsureOutcome::AlreadyUp => {}
        }

        // 2.2. Bounded-runtime backstop: this tick is doing WORK on a live session, so it
        // opens the wall-clock window and can spend it. Runs BEFORE the marker disposer
        // deliberately — a budget a marker bump can always postpone is not a budget, which
        // is exactly how a self-napping agent used to run unbounded (see
        // `budget_backstop`). Nothing is lost by pre-empting the disposer: the marker
        // watermark is untouched, so the same bump is disposed on the first tick after the
        // human answers.
        if let Some(tick) = self.budget_backstop(now, base)? {
            return Ok(tick);
        }

        // 2.5. Dispose a fresh marker bump (Slice 2). The session is confirmed alive
        // here (past the cold-start early-return) and no human is present (past the
        // defer gate), so the agent's latest self-assessment legitimately decides this
        // tick: a `Blocked`/`Monitoring` bump parks/escalates and WINS the tick (no
        // nudge); a `Working` bump / auto-flow / no-marker falls through to the
        // heartbeat nudge below.
        if let Some(tick) = self.observe_marker(driver, now, base, config)? {
            return Ok(tick);
        }
        // 2.6. The SUPERVISOR step (m20). A consult spawned by the auto-flow branch of
        // `dispose_report` is detached, so it resolves on a LATER sweep — here. It sits
        // AFTER the marker disposer so a fresh marker bump always outranks a pending
        // consult (the agent moving on makes the consult moot, and `dispose_report`
        // abandons it), and BEFORE the pane classify below so a nudge can never be typed
        // while the harness still has nothing to say. `Yields` returns the tick as-is;
        // `Applied` has just written the answer to `pending_context`, so the ordinary
        // idle-gated nudge below delivers it through the ONE carrier.
        let advised = self.advise_step(driver, now, base)?;
        if let AdviseStep::Yields(tick) = advised {
            return Ok(tick);
        }
        // `observe_marker` may have persisted a `Working`/auto-flow disposition
        // (`last_marker_seq`, `last_status`, `continuations`, a codex `conversation_id`,
        // or `pending_context`), and `advise_step` may have appended a supervisor answer
        // to `pending_context`. Reload so the nudge below carries either forward instead
        // of clobbering it with the stale pre-dispose `base`. With no marker file and no
        // applied advice this path is byte-identical to today.
        let base_owned;
        let base = if self.last_marker_stamp.is_some() || matches!(advised, AdviseStep::Applied) {
            base_owned = job::load(&self.paths)?.unwrap_or_else(|| base.clone());
            &base_owned
        } else {
            base
        };

        // A marker may carry several ordinary decisions. Each gets its own consult and verdict;
        // successful answers accumulate in pending_context, and the worker is nudged only after
        // the durable queue is empty. This also resumes queued local decisions after the human
        // answers a co-reported hard stop.
        if !base.advice_queue.is_empty() {
            let queued: Vec<(String, crate::worker::StopDraft)> = base
                .advice_queue
                .iter()
                .map(|item| (item.stop_id.clone(), item.draft.clone()))
                .collect();
            match self.spawn_advice(
                driver,
                now,
                base,
                &queued,
                config.decider_engine,
                config.decider_model.as_deref(),
            )? {
                AdviceSpawn::Spawned(tick) => return Ok(tick),
                AdviceSpawn::Unconsultable => {
                    return self.park_unresolved_auto_stops(
                        now,
                        base,
                        config,
                        UnresolvedAdvice {
                            report_seq: base.advice_queue[0].report_seq,
                            stops: &queued[..1],
                            reason: "the current queued decision is malformed, oversized, or \
                                     otherwise not safely consultable; it needs human review",
                            preserve_queued_siblings: true,
                        },
                    );
                }
                AdviceSpawn::Empty | AdviceSpawn::Unavailable => {}
            }
            let unavailable_reason = self
                .advise_health
                .reason
                .as_deref()
                .map(|reason| {
                    format!("decider is latched off ({reason}); queued decisions need you")
                })
                .unwrap_or_else(|| {
                    "decider unavailable; the remaining queued decisions need you".into()
                });
            return self.park_unresolved_auto_stops(
                now,
                base,
                config,
                UnresolvedAdvice {
                    report_seq: base.advice_queue[0].report_seq,
                    stops: &queued,
                    reason: &unavailable_reason,
                    preserve_queued_siblings: false,
                },
            );
        }

        // 2.9. LIVENESS before pixels: a capture is only evidence about a pane that is
        // still alive, so the dead-pane probe runs BEFORE the capture below is trusted.
        // A `remain-on-exit` corpse keeps drawing the last bare `❯` it painted, which
        // `classify_pane` reads as Idle — so without this the harness cheerfully nudges a
        // process that exited, forever. See `dead_pane_escalation`.
        if let Some(tick) = self.dead_pane_escalation(driver, now, base)? {
            return Ok(tick);
        }

        // 3. Already up: read the pane ONCE, then dispose it.
        let session = self.loop_session();
        match driver.capture_tail(&session, 40) {
            Ok(capture) => {
                // 3a. A blocking DIALOG is checked BEFORE the Idle/Busy disposition,
                // because a dialog pane classifies `Busy` (it draws no bare prompt and no
                // busy marker, so `classify_pane` falls to its conservative default). Left
                // to the heartbeat it re-parks BUSY_RECHECK_S forever and the human hears
                // nothing until DEFAULT_STALL_BUSY_S fires a MISLEADING "wedged / no
                // progress" `Stuck` half an hour late. Escalate immediately instead, with
                // the real question.
                //
                // Ordering: this sits AFTER the human-present defer gate (a human at the
                // pane can just answer the dialog themselves — never park on them for a
                // decision they are already looking at), AFTER `ensure_session` (nothing
                // to capture until the session is alive), and AFTER the marker disposer at
                // 2.5, which keeps its precedence: the agent's own written
                // self-assessment outranks anything we infer from pixels.
                if let Some(dialog) = tmux::classify_dialog(&capture) {
                    let policy_decision = crate::policy::decide_kind(
                        config.autonomy,
                        StopKind::Ambiguity,
                        RiskClass::Medium,
                    );
                    let authority_marker =
                        crate::advise::hard_floor_hit_in(&dialog.question, &dialog.options);
                    if policy_decision == crate::policy::Decision::AutoFlow
                        && dialog.class == tmux::PaneDialogClass::DelegableChoice
                        && authority_marker.is_none()
                        && let Some(tick) = self.spawn_dialog_advice(
                            driver,
                            now,
                            base,
                            dialog.clone(),
                            config.decider_engine,
                            config.decider_model.as_deref(),
                        )?
                    {
                        return Ok(tick);
                    }
                    let reason = if policy_decision != crate::policy::Decision::AutoFlow {
                        "tier policy requires human ownership; no decider was called".into()
                    } else if let Some(marker) = authority_marker {
                        format!(
                            "terminal authority guard matched `{marker}`; no decider was called"
                        )
                    } else if dialog.class != tmux::PaneDialogClass::DelegableChoice {
                        "terminal dialog classification requires human ownership; no decider was \
                         called"
                            .into()
                    } else if dialog.selected_index.is_none() {
                        "terminal dialog has no stable selected option; no decider was called"
                            .into()
                    } else {
                        "decider unavailable or dialog not safely consultable; no decider was called"
                            .into()
                    };
                    let human_owned_surface = authority_marker.is_some()
                        || dialog.class != tmux::PaneDialogClass::DelegableChoice
                        || dialog.selected_index.is_none();
                    let audit_kind = if human_owned_surface {
                        StopKind::Capability
                    } else {
                        StopKind::Ambiguity
                    };
                    let audit_risk = if human_owned_surface {
                        RiskClass::Hard
                    } else {
                        RiskClass::Medium
                    };
                    let mut audited = base.clone();
                    self.record_decider_skip(
                        &mut audited,
                        now,
                        DeciderSkip {
                            engine: config.decider_engine,
                            model: config.decider_model.as_deref(),
                            target: job::DeciderTarget::Dialog,
                            question: &dialog.question,
                            options: &dialog.options,
                            policy: job::DeciderPolicy {
                                kind: audit_kind,
                                labelled_risk: RiskClass::Medium,
                                effective_risk: audit_risk,
                            },
                            reason,
                            reported_kind: None,
                            effect: None,
                        },
                    );
                    return self.park_dialog(now, &audited, dialog);
                }
                match tmux::classify_pane(&capture) {
                    PaneActivity::Idle => self.idle_observed(driver, now, base, &capture),
                    // Busy: re-check soon, consume nothing (no nudge, no budget, no answer)
                    // — but run the stall backstop so a Busy-forever pane can't hang
                    // silently.
                    PaneActivity::Busy => self.busy_pane_recheck(now, base, &capture),
                }
            }
            // A transient capture error must not wedge the session: treat as Busy and
            // re-check next sweep (constraint: transient tmux errors defer, not error).
            // A truly transient error self-heals — the next successful Idle capture
            // resets `busy_since` via `nudge`.
            Err(_) => self.busy_recheck(now, base, HoldReason::Transient),
        }
    }

    /// Whether the turn we last nudged is provably STILL running (M71): the turn-complete
    /// signal ([`crate::state::ProjectPaths::turn_signal`]) exists and its byte count — one
    /// per completed turn, appended by the engine's turn-end hook — has NOT advanced past
    /// [`JobScheduler::turns_at_nudge`]. Read-only stat, panic-free.
    ///
    /// Returns `false` when the hook is unwired (no file), there is no nudge baseline yet, OR
    /// a turn HAS completed since our nudge — all of which defer to the fingerprint gate. A
    /// completed turn is deliberately NOT trusted as "idle now": a human attached to the same
    /// session drives the SAME hooked process (pmtui Enter), so a higher count proves only
    /// "a turn ended at some point since my nudge", not "no turn is in flight right now" —
    /// only the content-stability fingerprint can confirm the latter safely.
    fn turn_in_progress(&self) -> bool {
        match (
            std::fs::metadata(self.paths.turn_signal())
                .map(|m| m.len())
                .ok(),
            self.turns_at_nudge,
        ) {
            (Some(now), Some(base)) => now <= base,
            _ => false,
        }
    }

    /// FO-2: whether a turn has COMPLETED since our last nudge — the turn-complete signal
    /// exists AND its byte count has advanced PAST [`JobScheduler::turns_at_nudge`]. The
    /// exact inverse of `turn_in_progress`'s `now <= base`. Returns `false` with no baseline
    /// or no signal file (hook unwired), so a build without the turn-end event holds instead of
    /// relaxing — see the awaiting-report arm for why pixels cannot stand in here, and
    /// `report_debt_backstop` for what bounds that hold.
    fn turn_finished_since_nudge(&self) -> bool {
        match (
            std::fs::metadata(self.paths.turn_signal())
                .map(|m| m.len())
                .ok(),
            self.turns_at_nudge,
        ) {
            (Some(now), Some(base)) => now > base,
            _ => false,
        }
    }

    /// Handle an Idle observation on the DUE heartbeat path, behind the two-observation
    /// confirmation gate ([`IDLE_CONFIRMATIONS_REQUIRED`]). The first Idle only ARMS the
    /// gate: re-park the short [`BUSY_RECHECK_S`] recheck and consume NOTHING (no wake, no
    /// `pending_context`, no wall-clock window, no answer) — byte-for-byte the Busy arm's
    /// re-park — so a single misclassified capture can never land a keystroke in a
    /// mid-response agent. Only a SECOND Idle whose transcript is byte-identical to the first
    /// (the content-stability gate in the body) nudges — a pane still STREAMING an answer
    /// changes between the two captures and re-arms instead, which is the real fix for a
    /// working claude that `classify_pane` reads as Idle mid-stream.
    ///
    /// Deliberately does NOT go through [`JobScheduler::busy_recheck`], so an
    /// awaiting-confirmation tick can neither OPEN nor advance the `busy_since` stall
    /// window: `busy_since` is opened only by a real Busy observation, so a session
    /// sitting Idle awaiting confirmation accumulates no stall time and cannot trip a
    /// bogus `Stuck`. It also does not CLEAR `busy_since` — a bare unconfirmed Idle is not
    /// yet proof of life, and clearing here would disarm the backstop for a pane that
    /// alternates Busy/Idle (never reaching two consecutive Idles, so never nudging)
    /// forever. The nudge on the confirming observation is what clears it.
    fn idle_observed(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
        capture: &str,
    ) -> Result<JobTick> {
        // AWAITING A REPORT ⇒ THE AGENT IS STILL WORKING, whatever the pane looks like.
        //
        // User: *"When the session is chatting or working on sth, and not idle, i don't want
        // autopilot to kick in and queue a prompt. That is not good and can disrupt ongoing work
        // on claude/codex."*
        //
        // The pane heuristic above cannot deliver that on its own, because it infers from pixels:
        // `classify_pane` needs a busy marker INSIDE the captured tail, and an agent deep in a long
        // tool call can draw a bare composer with nothing else in the window. Typing then reaches a
        // CLI that queues the text and runs it after the turn in flight — the disruption the user
        // described. pmd-owned report generation is not an inference: it says whether a complete,
        // semantically new marker was accepted since we last spoke to the worker.
        //
        // NOT A DEADLOCK, deliberately. This routes through `busy_recheck`, the same single funnel
        // the Busy arm uses, so `busy_since` opens and `DEFAULT_STALL_BUSY_S` still escalates a
        // human-dismissable `Stuck` — "busy with no progress for 1800s" is an honest description of
        // an agent that has not reported since it was nudged, and the human gets told rather than
        // the session silently never being nudged again.
        //
        // Costs one nudge of throughput for an agent that finishes a wake WITHOUT writing its
        // marker (the prompt asks for one at every decision point, including plain progress). That
        // trade is the user's: not interrupting work outranks nudging on schedule.
        if base.awaiting_report() {
            // AN OWED REPORT ACCRUES, WHATEVER THE PANE PAINTS — see `report_debt_backstop`. First,
            // because every other branch below parks, and one of them parks in a place that opens no
            // stall window at all.
            if let Some(tick) = self.report_debt_backstop(now, base)? {
                return Ok(tick);
            }
            // WHAT COUNTS AS PROOF THE TURN ENDED, per engine.
            //
            // CLAUDE: only the turn-end hook. Its bare prompt stays visible through a long silent tool
            // call, so an idle-looking pane proves nothing — authorising the recovery nudge on two
            // byte-stable idle captures was tried and reverted, because it is exactly the case
            // `nudge::a_working_agent_is_never_nudged_again_until_it_reports` forbids
            // (*"i don't want autopilot to kick in and queue a prompt"*). When the hook cannot say,
            // claude HOLDS, and `report_debt_backstop` above is what stops that being forever.
            //
            // CODEX: its own pane. Codex paints `esc to interrupt` as FIXED CHROME directly above its
            // composer for as long as a turn runs, which is why the general turn-event gate below
            // already exempts it. MEASURED here rather than assumed, on the risky case: driven through
            // a 25-second SILENT tool call (`sleep 25`), every capture 3s apart carried the hint for 27
            // seconds and dropped it the instant the turn ended. The hint sits 4th from the bottom
            // among non-empty lines — well inside `PANE_TAIL_LINES` (16) and unable to scroll out,
            // because it is chrome rather than transcript. So a CONFIRMED-idle codex pane really does
            // mean the turn is over.
            //
            // That matters because codex's turn-end hook is not dependable: on a live resumed session
            // `turn-complete` grew once across roughly seven accepted reports, while a fresh session
            // fired it every turn. Waiting on an intermittent signal is what held that session 51
            // minutes. Its own screen is the signal that does not go missing.
            if self.engine == Engine::Codex || self.turn_finished_since_nudge() {
                if !self.idle_confirmation_ready(capture) {
                    return self.park_idle_confirmation(now, base);
                }
                return self.marker_less_recheck(driver, now, base);
            }
            return self.pane_progress_recheck(now, base, capture, HoldReason::AwaitingReport);
        }
        // TURN-END EVENT GATE (M71) — a DEFINITIVE "still mid-turn" guard when the engine's
        // turn-end hook is wired (claude `Stop` / codex `notify`; see
        // `worker::build_loop_command`). While NO turn has completed since our last nudge, the
        // agent is provably still working on it — so block, EVEN IF `classify_pane` reads
        // Idle (e.g. a slow tool call that has printed nothing for >BUSY_RECHECK_S, which the
        // fingerprint alone would misread as stable-idle). This is the edge the pane heuristic
        // misses. The POSITIVE direction (a turn HAS completed since our nudge) is NOT trusted
        // as "idle now" — a human on the same hooked session may have started another turn —
        // so it falls through to the content-stability fingerprint below, which re-arms on a
        // still-changing transcript. `turn_in_progress` is also `false` with no hook / no
        // baseline, so an engine/build without the event gets the plain m70 behaviour.
        // Codex paints an explicit busy banner for active turns. Once it has written a fresh
        // report (the awaiting-report guard above is clear) and the pane reaches two stable idle
        // composer captures, an unchanged optional notify byte is stale. Claude's bare prompt can
        // remain visible through silent in-flight work, so Claude keeps the strict event hold.
        if self.engine != Engine::Codex && self.turn_in_progress() {
            return self.pane_progress_recheck(now, base, capture, HoldReason::TurnInProgress);
        }
        // CONTENT-STABILITY GATE (Bug A, FALLBACK): the two confirming observations must not merely
        // both classify Idle — they must show the SAME transcript. `classify_pane` returns
        // Idle for a claude that is STREAMING an answer (no busy marker is on screen during
        // the stream in Claude Code v2.1.x — see [`tmux::idle_fingerprint`]), and does so
        // for many consecutive frames, so a count-only gate types a nudge into a working
        // agent. A streaming pane's transcript GROWS between the two [`BUSY_RECHECK_S`]-apart
        // captures; a genuinely waiting one is byte-stable. So advance the count only when
        // this capture's transcript matches the previous confirmation's; a change RE-ARMS
        // the gate to a single observation. This closes the false-Idle window regardless of
        // the spinner/glyph vocabulary the build happens to use.
        if !self.idle_confirmation_ready(capture) {
            return self.park_idle_confirmation(now, base);
        }
        // NO-PROGRESS circuit breaker (slice 1): the pane is confirmed idle and we are about to
        // nudge — first check whether the agent has actually changed the repository since the last
        // nudge. If the working tree has been byte-identical across `no_progress_threshold`
        // consecutive nudges, the agent is spinning (idle at its prompt but producing nothing), so
        // escalate a human-dismissable `Stuck` instead of nudging it forever. Inert unless the tree
        // is a git repo AND the feature is enabled; a git error disables it for this tick.
        if let Some(tick) = self.no_progress_backstop(now, base)? {
            return Ok(tick);
        }
        // The heartbeat path: no human answer just landed (that is `resume_with_answer`'s job),
        // and this is not a marker-less-finish correction (that is `marker_less_recheck`'s).
        self.nudge(driver, now, base, false, false)
    }

    fn idle_confirmation_ready(&mut self, capture: &str) -> bool {
        let fingerprint = tmux::idle_fingerprint(capture);
        if self.last_idle_fingerprint == Some(fingerprint) {
            self.idle_confirmations = self.idle_confirmations.saturating_add(1);
        } else {
            self.idle_confirmations = 1;
        }
        self.last_idle_fingerprint = Some(fingerprint);
        self.idle_confirmations >= IDLE_CONFIRMATIONS_REQUIRED
    }

    fn park_idle_confirmation(&mut self, now: Epoch, base: &AgentLoopState) -> Result<JobTick> {
        let until = now + BUSY_RECHECK_S;
        self.persist_run(
            base,
            now,
            JobRun::Monitoring { until },
            Some(AutopilotEventKind::Held(HoldReason::IdleUnconfirmed)),
        )?;
        Ok(JobTick::Monitoring { until })
    }

    /// FO-2: a turn COMPLETED since our last nudge but the agent wrote NO fresh marker (a
    /// "marker-less finish"). The pane is IDLE and the turn-end hook has PROVEN the turn ended,
    /// so — unlike an in-flight turn — nudging cannot interrupt work. Rather than sit silent and
    /// count down to bothering the human, deliver ONE targeted nudge that names the miss and asks
    /// for a marker (the `marker_less_finish` signal flag). The agent gets a real chance to
    /// self-correct; only if it keeps finishing turns without ever reporting do we escalate.
    ///
    /// The counter now bounds MARKER-LESS FINISHES, not passive rechecks: each nudge resets
    /// `turns_at_nudge`, so it can only re-trigger after the agent completes ANOTHER whole turn
    /// still without a marker. At [`MARKER_LESS_RECHECK_MAX`] such finishes we escalate a
    /// human-dismissable `WorkerStuck` — "finished N turns without ever reporting" is now a true
    /// statement of not-reporting, earned across real turns rather than seconds of rechecks. A
    /// fresh marker resets the counter (`marker.rs`), and a human answer resets it and resumes
    /// (`stops.rs`), so a reporting agent never reaches the bound. Does NOT open `busy_since`
    /// (an idle pane is not a Busy stall). With no turn-end hook this path is never reached, so
    /// an engine/build without the event is byte-for-byte unchanged.
    fn marker_less_recheck(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
    ) -> Result<JobTick> {
        // Void any armed Idle observations, exactly as busy_recheck does: the turn-end hook — not
        // the pixel gate — is what proved this finish, and whichever branch we take next
        // (escalate, or nudge which resets it again) must not inherit a stale confirmation.
        self.reset_idle_gate();
        let mut next = base.clone();
        next.marker_less_rechecks = next.marker_less_rechecks.saturating_add(1);
        if next.marker_less_rechecks >= MARKER_LESS_RECHECK_MAX {
            // The bounded backstop, independent of DEFAULT_STALL_BUSY_S / max_wakes. Reached only
            // after the agent has finished this many turns and reported on NONE of them, despite
            // being nudged each time. `park_stuck_kind` clones `next` (carrying the final count)
            // and persists it.
            let reason = format!(
                "finished {} turns without ever writing its decision marker — it may be stuck or \
                 not reporting; close it, or answer to keep it going",
                next.marker_less_rechecks
            );
            return self.park_stuck_kind(now, &next, StopKind::WorkerStuck, reason);
        }
        // Under the bound: nudge with the `marker_less_finish` flag set. `nudge` clones `next`
        // (carrying the incremented count — it never resets `marker_less_rechecks`), resets the
        // idle gate, parks the cadence, and bumps `wakes`, so this correction is a first-class
        // nudge that still counts toward the wake budget.
        self.nudge(driver, now, &next, false, true)
    }

    /// Handle a Busy observation (the pane is mid-response, or a transient tmux capture
    /// error is treated as Busy for the timer too): re-park a short [`BUSY_RECHECK_S`]
    /// Recheck a positively captured Busy pane while accounting for meaningful transcript
    /// progress. A changed normalized fingerprint restarts inactivity from `now`; spinner timers,
    /// token counters, and footer churn normalize away and cannot keep a wedged pane alive.
    pub(super) fn busy_pane_recheck(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
        capture: &str,
    ) -> Result<JobTick> {
        self.pane_progress_recheck(now, base, capture, HoldReason::Busy)
    }

    fn pane_progress_recheck(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
        capture: &str,
        reason: HoldReason,
    ) -> Result<JobTick> {
        let fingerprint = tmux::progress_fingerprint(capture);
        if self.busy_since.is_none() || self.last_busy_progress_fingerprint != Some(fingerprint) {
            self.busy_since = Some(now);
            self.last_busy_progress_fingerprint = Some(fingerprint);
        }
        self.busy_recheck(now, base, reason)
    }

    /// THE CEILING ON AN OWED REPORT — a SECOND clock, because it measures a different thing.
    ///
    /// `busy_since` answers "has this pane shown no progress for a while", and
    /// `pane_progress_recheck` rightly rebases it whenever the transcript grows: a claude streaming a
    /// long answer IS working, and escalating a `Stuck` at it would be a lie
    /// (`drive::changing_claude_false_idle_transcript_restarts_inactivity_without_nudging` pins that).
    ///
    /// "The agent has not reported since we spoke to it" is a different claim, and output does not
    /// settle it — only a report does. Measuring it on `busy_since` is why a live session held
    /// `awaiting_report` for 3033 seconds against the 1800s threshold with nothing said to the human:
    /// its codex pane kept redrawing while it shut down, restarting that window faster than it could
    /// ever fill. Proven, not guessed: the same hold with a byte-stable pane escalates.
    ///
    /// The ceiling is [`REPORT_DEBT_CEILING_S`], above the pane-stall threshold on purpose: this is the
    /// outer bound for when both finer mechanisms are blind, not the first thing to fire.
    ///
    /// Measured on [`AgentLoopState::nudged_at`], which is written at every site that OPENS a debt.
    /// The open `turn_trace` entry carries the same instant, but only for heartbeat nudges — its
    /// trigger enum is `Heartbeat`-only — so reading it would have left the dialog-answer and
    /// applied-verdict paths unbounded. It is also DURABLE where `busy_since` is not: a daemon restart
    /// forgets an in-memory window, and a session held in silence outlives one.
    fn report_debt_backstop(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
    ) -> Result<Option<JobTick>> {
        let Some(since) = base.nudged_at else {
            return Ok(None);
        };
        let owed_for = now - since;
        if owed_for < REPORT_DEBT_CEILING_S {
            return Ok(None);
        }
        // WHOSE SILENCE IS IT? The ceiling fires for two different faults, and blaming the agent for
        // ours is how a session degrades for weeks without anyone looking: with a dead turn-end hook
        // the hold is not an agent that went quiet, it is pmd unable to see that any turn ended, and
        // it recurs EVERY cadence for as long as the session lives. Naming the cause is the whole
        // point — `pmd doctor` already knows this fault, but a diagnostic you have to think to run
        // never reaches the human watching a dashboard say "not reporting".
        if let Some(reason) = self.blind_turn_signal_reason(base) {
            let tick = self.park_stuck_kind(now, base, StopKind::Capability, reason)?;
            return Ok(Some(tick));
        }
        // Human-dismissable, and named for what it IS: silence since the last nudge, not a verdict
        // about the work. The agent may well be mid-task; what it has not done is say so.
        let tick = self.park_stuck(
            now,
            base,
            format!(
                "the agent has not reported in {owed_for}s since it was last nudged — it may be \
                 stuck or not reporting; close it, or answer to keep it going"
            ),
        )?;
        Ok(Some(tick))
    }

    /// Why the ceiling was reached when the cause is OURS rather than the agent's: the turn-end hook
    /// this session depends on is not firing, so `turn_finished_since_nudge` can never go true and
    /// every awaiting-report hold runs the full [`REPORT_DEBT_CEILING_S`].
    ///
    /// `None` — keep the generic "the agent has not reported" wording — in the three cases where the
    /// hook is not the provable cause:
    ///
    /// - **Codex**, which does not depend on the hook at all: the awaiting-report arm ends its hold on
    ///   its own `esc to interrupt` chrome, so a codex session reaching the ceiling had a pane that
    ///   never confirmed idle. That is a different fault, and the hook's state says nothing about it.
    /// - **A fresh session** ([`TurnSignalHealth::Fresh`]), where no signal file and almost no reports
    ///   is indistinguishable from a session that simply has not finished a turn yet. Diagnosing a
    ///   dead hook there would be a guess dressed as a finding.
    /// - **A healthy hook**, where the silence really is the agent's.
    ///
    /// Filed as [`StopKind::Capability`] rather than `Stuck` for the reason `park_dialog` gives: the
    /// agent is not wedged, the harness cannot do something, and "no progress" would be the misleading
    /// half of the message. Both kinds are always-`Hard`, so this changes the FRAMING a human reads,
    /// never whether they are told.
    fn blind_turn_signal_reason(&self, base: &AgentLoopState) -> Option<String> {
        if self.engine == Engine::Codex {
            return None;
        }
        match turn_signal_health(&self.paths.turn_signal(), base.report_generation) {
            TurnSignalHealth::NeverFired { reports } => Some(format!(
                "this session's turn-end hook has never fired across {reports} reports, so pmd \
                 cannot tell when a turn ends and every check-in now waits the full \
                 {REPORT_DEBT_CEILING_S}s — restart the session to relaunch it with the hook"
            )),
            TurnSignalHealth::Partial { turns, reports } => Some(format!(
                "this session's turn-end hook has fired only {turns} times across {reports} \
                 reports, so pmd misses most turn ends and waits the full {REPORT_DEBT_CEILING_S}s \
                 instead — restart the session to relaunch it with the hook"
            )),
            TurnSignalHealth::Fresh | TurnSignalHealth::Healthy { .. } => None,
        }
    }

    /// recheck WITHOUT nudging or consuming any budget/answer, but FIRST run the stall
    /// backstop. The first Busy observation opens the `busy_since` window; once the pane
    /// has been continuously Busy with no intervening progress for
    /// [`DEFAULT_STALL_BUSY_S`], escalate a human-dismissable `Stuck` (mirrors the other
    /// budget callers of [`JobScheduler::park_stuck`]) instead of re-parking forever.
    /// Every sign of progress / life resets `busy_since` elsewhere, so a healthy agent
    /// never trips this.
    pub(super) fn busy_recheck(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
        reason: HoldReason,
    ) -> Result<JobTick> {
        // Busy voids the confirmation gate: the two Idle observations that authorise a
        // nudge must be CONSECUTIVE. This is the single funnel for BOTH Busy arms in
        // `drive` (the classified `PaneActivity::Busy` and the capture-error-treated-as-Busy
        // one), so both reset here.
        self.reset_idle_gate();
        if let Some(since) = self.busy_since {
            if now - since >= DEFAULT_STALL_BUSY_S as i64 {
                // Wedged: escalate a human-dismissable Stuck (the I1 silent-stall +
                // bad-`--resume` closure). No `Held` event is recorded here — the escalation
                // itself is recorded as `Stuck` by `park_stuck`.
                return self.park_stuck(
                    now,
                    base,
                    format!(
                        "agent-loop session has been busy with no progress for {}s — it may be \
                         wedged (or resuming a stale conversation); close it, or answer to keep \
                         it going",
                        now - since
                    ),
                );
            }
        } else {
            // First Busy observation opens the stall window.
            self.busy_since = Some(now);
        }
        let until = now + BUSY_RECHECK_S;
        // The HELD decision rides onto the ledger's autopilot feed, coalesced by `reason`
        // so a run of identical rechecks is one "held ×N" line, not one per sweep.
        self.persist_run(
            base,
            now,
            JobRun::Monitoring { until },
            Some(AutopilotEventKind::Held(reason)),
        )?;
        Ok(JobTick::Monitoring { until })
    }

    /// The OQ1 disposer-ONLY fast-path: run for a NOT-yet-due `Monitoring{until}` whose
    /// marker revision advanced, so a mid-cadence `Blocked`/`Monitoring` bump parks or
    /// escalates within ONE sweep instead of waiting out the full cadence. Unlike
    /// [`JobScheduler::drive`] it NEVER nudges: a `Working` / auto-flow / duplicate-marker
    /// marker (`observe_marker` → `None`) returns the CURRENT parked cadence without a
    /// heartbeat keystroke (auto-flow re-parks `now+cadence`; Working/duplicate-marker leave
    /// `until` unchanged). A mid-write malformed marker is the one non-`None` benign
    /// case: `observe_marker` returns `Some(Monitoring{now+BUSY_RECHECK_S})` (an OQ3
    /// short recheck), which is returned as-is. This keeps the daemon's 500ms liveness
    /// sweep from collapsing the nudge cadence (regression I1): the full nudge heartbeat
    /// stays gated on `Monitoring.until` becoming due.
    pub(super) fn drive_marker_only(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
        config: &Config,
        until: Epoch,
    ) -> Result<JobTick> {
        // Human present: never process a marker while a human owns the pane. DEFER,
        // consuming NO state — and crucially do NOT record `last_marker_stamp`.
        // `observe_marker` short-circuits on an unchanged revision BEFORE parsing/disposal,
        // check, so recording it here would mark a mid-cadence `Blocked` bump "seen"
        // and DROP it after the human detaches (the block would never escalate). The
        // process-local revision cache is only advanced when the marker is actually processed. The
        // cost is that this fast-path re-probes each 500ms sweep while a human is
        // attached (accepted Minor — the DUE path already re-probes every sweep during
        // attachment); once the human detaches the next tick still sees the revision
        // advanced and disposes the pending block within one sweep.
        if self.human_present(driver, now) {
            // Pause the wall-clock window while a human owns the pane, exactly as `drive`'s
            // defer gate does. Without it, a long human attachment reached ONLY through this
            // marker-only fast-path leaves `window_start` running, so `budget_backstop` trips
            // a spurious 24h `Stuck` on a session the human was actively holding open.
            if self.window_start.is_some() {
                self.window_start = Some(now);
            }
            // A human attached is not a stall, and voids any earlier Idle observations
            // (parity with `drive`'s defer gate).
            self.busy_since = None;
            self.reset_idle_gate();
            // NOTE: like `drive`'s defer gate, do NOT re-baseline `turns_at_nudge` here — that
            // wedged `turn_in_progress()` once a human detached from an idle agent (see the note
            // there). The nudge baseline is only ever set by an actual nudge.
            return Ok(JobTick::Monitoring { until });
        }
        // Ensure the session is alive; a JUST-relaunched session applies cold-start grace
        // exactly like the due path (never read a marker a still-booting agent hasn't
        // written yet).
        match self.ensure_session(driver, now, base)? {
            EnsureOutcome::JustLaunched => {
                return Ok(JobTick::Monitoring {
                    until: now + LAUNCH_GRACE_S,
                });
            }
            EnsureOutcome::AlreadyUp => {}
        }
        // Bounded-runtime backstop, exactly as on the due path: this fast-path is the ONLY
        // thing that runs for a session whose agent keeps self-scheduling naps, so leaving
        // the budget out here is what made `max_wall_clock_s` unenforceable (m18/2). An
        // escalation is not a nudge and does not shrink the cadence, so the I1 cadence
        // guard this function exists for is untouched: `park_stuck` parks `Blocked`, after
        // which every tick routes to `on_blocked` instead of back here.
        //
        // Deliberately NO dead-pane probe here (unlike `drive`): this path never reads the
        // pane, so it has nothing to distrust, and the daemon sweeps every 500ms — adding a
        // subprocess to a path that currently costs one `stat` would be the subprocess
        // storm this function was written to avoid. A dead pane still surfaces from `drive`
        // on the next DUE tick (and a dead agent cannot bump the marker that gets us here).
        if let Some(tick) = self.budget_backstop(now, base)? {
            return Ok(tick);
        }
        // Dispose ONLY. `Some(tick)` is a real disposer decision returned as-is: a
        // Blocked escalation / a Monitoring nap within one sweep (the intended win), or
        // an OQ3 mid-write `Monitoring{now+BUSY_RECHECK_S}` recheck. `None` (Working /
        // auto-flow / duplicate-marker) does NOT classify, nudge, or re-park to BUSY_RECHECK —
        // it returns the CURRENT parked cadence. An all-auto-flow disposition persisted
        // `self.run = Monitoring{now+cadence}`, so read the until back from `self.run`
        // (Working/duplicate-marker leave it unchanged, so it still equals the passed-in
        // `until`); fall back to the passed-in `until` if `self.run` isn't `Monitoring`
        // (unreachable on a `None` return, but keeps the emit consistent by contract).
        match self.observe_marker(driver, now, base, config)? {
            Some(tick) => Ok(tick),
            None => {
                let until = if let JobRun::Monitoring { until: current } = self.run {
                    current
                } else {
                    until
                };
                Ok(JobTick::Monitoring { until })
            }
        }
    }

    /// Whether a human is chatting this conversation (`chat_lock`) or attached to the
    /// persistent loop session (`has_clients`) right now — the nudge-defer gate. Both
    /// fail safe toward "present" (defer) so a probe error never nudges into a pane
    /// someone is typing in.
    pub(super) fn human_present(&self, driver: &dyn Driver, now: Epoch) -> bool {
        crate::chat_lock::is_active(&self.paths, now)
            || driver.has_clients(&self.loop_session()).unwrap_or(true)
    }

    /// The dead-pane guard: probe [`Driver::pane_dead`] and, when the pane is CONFIRMED
    /// dead, escalate honestly instead of typing into a corpse. Returns `Some(tick)` when
    /// the pane is dead (the caller returns it as-is), else `None`.
    ///
    /// A dead pane is misclassified in BOTH directions without this. tmux with
    /// `remain-on-exit on` keeps the session and leaves the last frame the process painted
    /// on screen — usually a bare `❯` — so [`tmux::classify_pane`] says **Idle** and the
    /// heartbeat nudges a process that has exited, every cadence, forever. And when the
    /// capture comes back EMPTY, `classify_pane` falls to its conservative **Busy**
    /// default, so the session re-parks [`BUSY_RECHECK_S`] until
    /// [`DEFAULT_STALL_BUSY_S`] finally fires a *misleading* "busy with no progress"
    /// `Stuck` half an hour late. Neither direction tells the human what actually happened.
    ///
    /// Cost: exactly ONE extra tmux call, and only on ticks that were already going to run
    /// `capture-pane` (never on the 500ms marker fast-path) — the sweep deliberately avoids
    /// subprocess storms.
    ///
    /// Fail-safe: [`Driver::pane_dead`] defaults to "not dead" and a probe ERROR is read
    /// the same way, so an unknown answer behaves exactly as before this guard existed —
    /// the harness never escalates on a guess. The remaining ambiguity (the whole SESSION
    /// vanished between `ensure_session` and here, where tmux can report nothing at all)
    /// self-heals in one tick: the next `is_alive` fails and `ensure_session` relaunches.
    ///
    /// Deliberately does NOT kill and relaunch the session. `is_alive` (`has-session`) is
    /// TRUE for a corpse, so the harness cannot revive it by its normal path, and a pane
    /// that died on startup (a bad `--resume`, a crash, a human `/exit`) would relaunch-die
    /// in a loop. Whether to restart or close is exactly the kind of thing a human should
    /// decide, so it is surfaced as [`StopKind::Capability`] — always-`Hard`, so no tier
    /// auto-flows it (`Stuck` would read as "wedged / no progress", which is untrue and is
    /// the misleading message this guard exists to replace).
    pub(super) fn dead_pane_escalation(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
    ) -> Result<Option<JobTick>> {
        let session = self.loop_session();
        if !driver.pane_dead(&session).unwrap_or(false) {
            return Ok(None);
        }
        // Not a stall: close the stall window and void any armed Idle observations of the
        // (now dead) pane, exactly as `park_dialog` does for its own escalation.
        self.busy_since = None;
        self.reset_idle_gate();
        let reason = format!(
            "the agent process in this session's pane has EXITED — {session} is still there \
             but its pane is dead, so there is nothing left to nudge (the pane still shows \
             the last prompt it painted, which is why this is not \"idle\"). Close the \
             session, or attach and restart the agent in it"
        );
        let mut next = base.clone();
        next.finish_turn_without_report(now, crate::job::TurnNoReportReason::TerminalUnavailable);
        Ok(Some(self.park_stuck_kind(
            now,
            &next,
            StopKind::Capability,
            reason,
        )?))
    }
}
