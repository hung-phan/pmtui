//! Parking the session on a human, and coming back. The routines that synthesize an
//! open stop — an in-pane dialog, a spent budget, a decision the harness will not
//! take — and the `Blocked` round trip that resumes once an answer lands. One unit
//! because a park and its resume have to agree about which answers count, what the
//! answer text becomes, and which counters a human touch resets.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Result;

use crate::advise;
use crate::clock::Epoch;
use crate::job::{
    AgentLoopState, AutopilotEventKind, DecisionKind, DecisionRecord, HoldReason, JobRun,
};
use crate::lease;
use crate::pmstate::{OpenStop, PaneDialogStop, StopKind, StopStatus};
use crate::state::{self, Answer};
use crate::tmux::{self, Driver, PaneActivity};

use super::drive::BUSY_RECHECK_S;
use super::nudge::{DEFAULT_CADENCE_S, append_context};
use super::session::{EnsureOutcome, LAUNCH_GRACE_S, mint_uuid_v4};
use super::supervisor::one_line;
use super::{JobScheduler, JobTick};

pub(super) struct CapabilityStop<'a> {
    pub id_tag: &'a str,
    pub dialog: Option<&'a tmux::PaneDialog>,
    pub question: &'a str,
    pub options: &'a [String],
    pub status: String,
}

impl JobScheduler {
    /// Blocked between nudges: resume when a human answer at/after `since` resolves
    /// every open stop (or the stop was withdrawn) — resuming by NUDGING the live
    /// session with the answer text (never spawning); otherwise re-emit `Escalated` so
    /// the daemon's dedup keeps it silent (no dedicated "still parked" variant —
    /// mirrors the phase engine).
    pub(super) fn on_blocked(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        ledger: &AgentLoopState,
        stop_ids: &[String],
        since: Epoch,
    ) -> Result<JobTick> {
        let answers: Vec<Answer> = state::read_json_or(&self.paths.answers(), Vec::new())?;
        if let Some(stop) = ledger
            .open_stops
            .iter()
            .find(|stop| stop.is_pane_dialog() && stop_ids.contains(&stop.id))
        {
            if self.human_present(driver, now) {
                return Ok(JobTick::Escalated(stop_ids.to_vec()));
            }
            if stop.status == StopStatus::Held {
                return self.reconcile_dialog_stop(driver, now, ledger, stop_ids, stop);
            }
            if let Some(answer) = answers
                .iter()
                .rev()
                .find(|answer| answer.stop_id == stop.id && answer.answered_at >= since)
            {
                return self.resume_dialog_with_answer(driver, now, ledger, stop_ids, stop, answer);
            }
            return self.reconcile_dialog_stop(driver, now, ledger, stop_ids, stop);
        }

        // Only answers at/after the park start count — a stale answer for a recycled
        // id can't silently resolve a fresh question.
        let answered: HashSet<&str> = answers
            .iter()
            .filter(|a| a.answered_at >= since)
            .map(|a| a.stop_id.as_str())
            .collect();
        let open: HashSet<&str> = ledger.open_stops.iter().map(|s| s.id.as_str()).collect();
        let still_blocking = stop_ids
            .iter()
            .any(|id| open.contains(id.as_str()) && !answered.contains(id.as_str()));
        if still_blocking {
            let remaining: Vec<String> = stop_ids
                .iter()
                .filter(|id| open.contains(id.as_str()) && !answered.contains(id.as_str()))
                .cloned()
                .collect();
            if remaining.len() != stop_ids.len() {
                let resolved: Vec<String> = stop_ids
                    .iter()
                    .filter(|id| answered.contains(id.as_str()))
                    .cloned()
                    .collect();
                let extra = answers_extra(&answers, &resolved, since);
                let mut next = ledger.clone();
                next.open_stops.retain(|stop| remaining.contains(&stop.id));
                next.run = JobRun::Blocked {
                    stop_ids: remaining.clone(),
                    since,
                };
                next.updated_at = now;
                if !extra.trim().is_empty() {
                    next.record_event(now, AutopilotEventKind::Answered(Some(extra.clone())));
                    next.pending_context = Some(append_context(next.pending_context.take(), extra));
                }
                self.save_ledger(&mut next)?;
                self.run = next.run;
            }
            return Ok(JobTick::Escalated(remaining));
        }
        // A human present on this conversation right now (chatting, or attached to the
        // loop session) would collide with the answer's nudge. Defer WITHOUT consuming
        // the answer or resetting budgets: stay Blocked and re-emit Escalated (the
        // daemon's dedup keeps it silent — the same signal as `still_blocking`),
        // re-checked next sweep. The answer stays in answers.json and applies on the
        // first sweep after the human leaves. Returning BEFORE the resets below
        // preserves the answer and both budgets for that post-detach sweep.
        if self.human_present(driver, now) {
            return Ok(JobTick::Escalated(stop_ids.to_vec()));
        }
        // Unblocked: a human decision is a fresh start — reset BOTH bounded-runtime
        // budgets (the wake count AND the wall-clock window, so answering "keep going"
        // on either escalation actually continues) AND the stall backstop window (so a
        // human answer to a `Stuck` escalation actually resumes the loop), then resume by
        // nudging the SAME persistent session with the answer text.
        self.reset_after_human_answer();
        let extra = answers_extra(&answers, stop_ids, since);
        let mut base = ledger.clone();
        reset_persisted_progress(&mut base);
        base.open_stops.clear();
        // The answer rides into the resume as `pending_context` — the ONE durable
        // carrier — so it is on DISK before anything tries to type it. Handing it down
        // as a by-value argument instead is what made a transient `send_keys` error
        // DESTROY a human decision: the error branch re-parked a `base` that did not
        // carry it, the ledger left `Blocked` with the stops already cleared, and `tick`
        // only routes to `on_blocked` FROM `Blocked` — so nothing ever re-read
        // `answers.json` and the answer was gone for good.
        //
        // Anything already parked and UNDELIVERED is appended to, never overwritten. Both
        // are things a human said: an answer can sit undelivered across an escalation (the
        // pane was dead when we tried to type it), and the old by-value `extra` — which
        // simply won over `pending_context` — would have dropped the earlier one. Growth is
        // human-paced and self-limiting: every append needs a fresh human answer, and the
        // first delivered nudge consumes the whole payload at once.
        if !extra.trim().is_empty() {
            // The human's half of the loop, on the feed: without this the feed jumps
            // "needs you → nudged" and the answer that unblocked it leaves no trace.
            base.record_event(now, AutopilotEventKind::Answered(Some(extra.clone())));
            base.pending_context = Some(append_context(base.pending_context.take(), extra));
        }
        // A human decision supersedes any supervisor opinion still in flight about the
        // same session: drop it (and its `pmsup-` session) rather than letting it resolve
        // later and escalate a decision the human has just moved past.
        let superseded_stop_ids = self
            .advise
            .as_ref()
            .and_then(|inflight| match &inflight.target {
                super::supervisor::AdviceTarget::Marker { stop_ids, .. } => Some(stop_ids.clone()),
                super::supervisor::AdviceTarget::Dialog { .. } => None,
            })
            .or_else(|| {
                self.advise_orphaned
                    .as_ref()
                    .map(|parked| parked.stop_ids.clone())
            })
            .unwrap_or_default();
        if let Some(abandoned) = self.abandon_advice(driver, &base)? {
            base.finish_decider_run(
                abandoned.decider_seq,
                now,
                crate::job::DeciderOutcome::Interrupted {
                    reason: "a human answered before the decider returned".into(),
                },
            );
            if let Some(report_seq) = abandoned.report_seq {
                base.finish_turn_review(report_seq, crate::job::TurnReviewOutcome::Interrupted);
            }
            base.advice_queue
                .retain(|queued| !superseded_stop_ids.contains(&queued.stop_id));
        }
        if !base.advice_queue.is_empty() {
            // A co-reported human-owned stop paused the ordinary decision queue. Preserve the
            // human answer in pending_context, leave the worker untouched, and let the due drive
            // tick consult the remaining decisions before delivering the accumulated answers.
            base.run = JobRun::Monitoring { until: now };
            base.updated_at = now;
            self.save_ledger(&mut base)?;
            self.run = JobRun::Monitoring { until: now };
            return Ok(JobTick::Monitoring { until: now });
        }
        self.resume_with_answer(driver, now, &base)
    }

    fn reset_after_human_answer(&mut self) {
        self.wakes = 0;
        self.window_start = None;
        self.busy_since = None;
        self.reset_no_progress();
        self.reset_idle_gate();
    }

    fn resume_dialog_with_answer(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        ledger: &AgentLoopState,
        stop_ids: &[String],
        stop: &OpenStop,
        answer: &Answer,
    ) -> Result<JobTick> {
        let Some(snapshot) = stop
            .pane_dialog
            .as_ref()
            .filter(|dialog| dialog.dashboard_answerable)
        else {
            return self.reconcile_dialog_stop(driver, now, ledger, stop_ids, stop);
        };
        let Some(target) = stop
            .options
            .iter()
            .position(|option| option == &answer.answer)
            .filter(|target| tmux::dialog_option_is_concrete(&stop.options[*target]))
        else {
            return self.reconcile_dialog_stop(driver, now, ledger, stop_ids, stop);
        };

        let _input_lease = match lease::acquire_with_retry(
            &self.paths.input_lock(),
            3,
            Duration::from_millis(25),
        )? {
            Some(lease) => lease,
            None => return Ok(JobTick::Escalated(stop_ids.to_vec())),
        };
        if self.human_present(driver, now) {
            return Ok(JobTick::Escalated(stop_ids.to_vec()));
        }

        let session = self.loop_session();
        if !driver.is_alive(&session).unwrap_or(false) || driver.pane_dead(&session).unwrap_or(true)
        {
            return Ok(JobTick::Escalated(stop_ids.to_vec()));
        }
        let capture = match driver.capture_tail(&session, 40) {
            Ok(capture) => capture,
            Err(_) => return Ok(JobTick::Escalated(stop_ids.to_vec())),
        };
        let Some(current) = tmux::classify_dialog(&capture).filter(|dialog| {
            dialog.identity_fingerprint() == snapshot.fingerprint
                && dashboard_answerable_dialog(dialog)
        }) else {
            return self.clear_dialog_stop(
                now,
                ledger,
                "the interactive prompt changed before the human answer was applied",
            );
        };

        let mut held = ledger.clone();
        let Some(held_stop) = held.open_stops.iter_mut().find(|open| open.id == stop.id) else {
            return Ok(JobTick::Escalated(stop_ids.to_vec()));
        };
        held_stop.status = StopStatus::Held;
        held.updated_at = now;
        held.last_status =
            Some("applying the human's interactive choice; attach if it remains blocked".into());
        self.save_ledger(&mut held)?;

        if self.human_present(driver, now) {
            return Ok(JobTick::Escalated(stop_ids.to_vec()));
        }
        let interactive = match driver.verify_dialog_interactive(&session, &current) {
            Ok(dialog) => dialog,
            Err(_) => return Ok(JobTick::Escalated(stop_ids.to_vec())),
        };
        if self.human_present(driver, now) {
            return Ok(JobTick::Escalated(stop_ids.to_vec()));
        }
        if driver
            .select_dialog_options(&session, &interactive, &[target])
            .is_err()
        {
            return Ok(JobTick::Escalated(stop_ids.to_vec()));
        }

        self.reset_after_human_answer();
        let turn_count = self.turn_count();
        self.turns_at_nudge = Some(turn_count);
        let until = now + ledger.cadence_s.unwrap_or(DEFAULT_CADENCE_S) as i64;
        let mut next = held;
        reset_persisted_progress(&mut next);
        next.open_stops.clear();
        next.nudged_at_seq = Some(next.last_marker_seq);
        next.nudged_at_report_generation = Some(next.report_generation);
        next.nudged_at = Some(now);
        next.turn_count_at_nudge = Some(turn_count);
        next.run = JobRun::Monitoring { until };
        next.updated_at = now;
        next.last_status = Some(format!(
            "the human selected an interactive choice: {}",
            one_line(&answer.answer)
        ));
        next.record_event(
            now,
            AutopilotEventKind::Answered(Some(one_line(&answer.answer))),
        );
        self.save_ledger(&mut next)?;
        self.run = JobRun::Monitoring { until };
        Ok(JobTick::Monitoring { until })
    }

    fn reconcile_dialog_stop(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        ledger: &AgentLoopState,
        stop_ids: &[String],
        stop: &OpenStop,
    ) -> Result<JobTick> {
        let session = self.loop_session();
        if !driver.is_alive(&session).unwrap_or(false) || driver.pane_dead(&session).unwrap_or(true)
        {
            return Ok(JobTick::Escalated(stop_ids.to_vec()));
        }
        let capture = match driver.capture_tail(&session, 40) {
            Ok(capture) => capture,
            Err(_) => return Ok(JobTick::Escalated(stop_ids.to_vec())),
        };
        let same_dialog = tmux::classify_dialog(&capture).is_some_and(|dialog| {
            stop.pane_dialog.as_ref().map_or_else(
                || {
                    dialog.question == stop.question.as_deref().unwrap_or_default()
                        && dialog.options == stop.options
                },
                |snapshot| dialog.identity_fingerprint() == snapshot.fingerprint,
            )
        });
        if same_dialog {
            return Ok(JobTick::Escalated(stop_ids.to_vec()));
        }
        self.clear_dialog_stop(
            now,
            ledger,
            "the interactive prompt was resolved or changed in the terminal",
        )
    }

    fn clear_dialog_stop(
        &mut self,
        now: Epoch,
        ledger: &AgentLoopState,
        status: &str,
    ) -> Result<JobTick> {
        self.reset_after_human_answer();
        let mut changed = ledger.clone();
        reset_persisted_progress(&mut changed);
        changed.open_stops.clear();
        changed.run = JobRun::Monitoring { until: now };
        changed.updated_at = now;
        changed.last_status = Some(status.into());
        self.save_ledger(&mut changed)?;
        self.run = JobRun::Monitoring { until: now };
        Ok(JobTick::Monitoring { until: now })
    }

    /// Resume a just-unblocked session by nudging the live persistent session with the
    /// human's answer. Ensures the session is alive (re-launching it — resuming the SAME
    /// conversation id — if it died during the Blocked wait). Never types into a busy or
    /// DEAD pane.
    ///
    /// Takes no `extra`: the caller has already written the answer onto `base` as
    /// `pending_context`, so EVERY outcome here (relaunch, busy re-park, dead-pane
    /// escalation, transient send failure) persists a `base` that still carries it and
    /// the first successful idle nudge delivers it. That is why this can be so plain —
    /// there is no longer a second, non-durable copy of the answer to keep in sync.
    fn resume_with_answer(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
    ) -> Result<JobTick> {
        match self.ensure_session(driver, now, base)? {
            EnsureOutcome::JustLaunched => {
                // The session died during the Blocked wait and was just relaunched
                // (resuming the same id). `ensure_session` already persisted `base` —
                // `pending_context` and all — alongside the id + Monitoring{grace}, so
                // the first idle nudge after cold-start carries the answer.
                Ok(JobTick::Monitoring {
                    until: now + LAUNCH_GRACE_S,
                })
            }
            EnsureOutcome::AlreadyUp => {
                // A DEAD pane cannot receive the answer (see `dead_pane_escalation`):
                // surface it instead of typing into a corpse. The answer stays parked in
                // `pending_context`, so it is delivered if the human revives the session.
                if let Some(tick) = self.dead_pane_escalation(driver, now, base)? {
                    return Ok(tick);
                }
                let session = self.loop_session();
                match driver.capture_tail(&session, 40) {
                    Ok(capture) if tmux::classify_pane(&capture) == PaneActivity::Idle => {
                        // The resume path: a human answer just landed (it rides in
                        // `pending_context`), so flag it for the "Since last wake" block. Not a
                        // marker-less-finish correction — the answer is the thing to deliver here.
                        self.nudge(driver, now, base, true, false)
                    }
                    // Busy / transient error: re-park a short recheck (the answer is
                    // already on the ledger); the next idle nudge (drive path) delivers it.
                    _ => {
                        let until = now + BUSY_RECHECK_S;
                        // Holding the human's answer for a busy pane is a hold like any other —
                        // record it so the feed shows why the answer hasn't landed yet.
                        self.persist_run(
                            base,
                            now,
                            JobRun::Monitoring { until },
                            Some(AutopilotEventKind::Held(HoldReason::Busy)),
                        )?;
                        Ok(JobTick::Monitoring { until })
                    }
                }
            }
        }
    }

    /// Park `Blocked` on a dialog that cannot be delegated, carrying the agent's
    /// real question + options to the human, and surface it ONCE.
    ///
    /// Kind: [`StopKind::Capability`] — the honest one. This is not `Stuck` (the agent
    /// is not wedged; it is working correctly and waiting, and calling it "no progress"
    /// is the misleading message this feature exists to replace) and not `Ambiguity` /
    /// `ExpertNeeded` (both floor to `Medium`, which **auto-flows under Autopilot** — the
    /// one outcome that must never happen here). `Capability` is forced `Hard` by
    /// [`crate::policy::effective_risk_kind`], so every tier escalates and no tier can
    /// auto-flow it, and it means precisely "the harness cannot do this itself": only a
    /// human at the terminal may answer a permission prompt.
    ///
    /// Positively identified goal-choice dialogs are intercepted earlier by
    /// `spawn_dialog_advice`: Autopilot may delegate them to the read-only decider,
    /// then select its validated index under `input.lock`. This function is the
    /// fail-closed remainder: permission/trust/unknown chrome, hard-floor wording,
    /// Standard mode, no goal, no decider, and every refusal still reach the human.
    ///
    /// Notify-once + idempotence come from the existing machinery rather than new state:
    /// this parks `JobRun::Blocked`, so every later tick routes to
    /// [`JobScheduler::on_blocked`] (which re-emits `Escalated` for the SAME stop id and
    /// never re-classifies the pane) — no second stop is ever created, and the daemon's
    /// id-keyed dedup keeps it silent after the first notification.
    pub(super) fn park_dialog(
        &mut self,
        now: Epoch,
        ledger: &AgentLoopState,
        dialog: tmux::PaneDialog,
    ) -> Result<JobTick> {
        let status = if dashboard_answerable_dialog(&dialog) {
            format!(
                "waiting on an interactive prompt in its own session: {} — answer it from the \
                 dashboard or attach to the session",
                dialog.question
            )
        } else {
            format!(
                "waiting on an interactive prompt in its own session: {} — attach to the \
                 session and answer it there",
                dialog.question
            )
        };
        self.park_capability_stop(
            now,
            ledger,
            CapabilityStop {
                id_tag: "dialog",
                dialog: Some(&dialog),
                question: &dialog.question,
                options: &dialog.options,
                status,
            },
        )
    }

    /// Park `JobRun::Blocked` on ONE synthesized [`StopKind::Capability`] stop carrying
    /// `question` + `options`, with `status` as the dashboard line, and surface it once as
    /// `Escalated`. The shared persistence core behind [`JobScheduler::park_dialog`] (an
    /// in-pane permission prompt) and [`JobScheduler::park_advice_refusal`] (the
    /// supervisor declined or was unusable).
    ///
    /// Shared rather than duplicated because the two state resets below are load-bearing
    /// and easy to forget: a copy that omitted them would let the stall backstop fire a
    /// second, contradictory escalation on top of this one.
    ///
    /// `Capability` for both callers, and for the same reason — see
    /// [`JobScheduler::park_advice_refusal`] for the `WorkerStuck`/`Medium`/`AutoFlow`
    /// trap that rules the alternatives out.
    pub(super) fn park_capability_stop(
        &mut self,
        now: Epoch,
        ledger: &AgentLoopState,
        stop: CapabilityStop<'_>,
    ) -> Result<JobTick> {
        let stop_id = format!(
            "stop-{}-{}-{now}-{}",
            self.project_id,
            stop.id_tag,
            mint_uuid_v4()
        );
        let mut open = open_stop(
            stop_id.clone(),
            StopKind::Capability,
            None,
            stop.question,
            stop.options,
            now,
        );
        open.pane_dialog = stop.dialog.map(|dialog| PaneDialogStop {
            fingerprint: dialog.identity_fingerprint(),
            dashboard_answerable: dashboard_answerable_dialog(dialog),
        });
        let mut next = ledger.clone();
        next.open_stops = vec![open];
        next.run = JobRun::Blocked {
            stop_ids: vec![stop_id.clone()],
            since: now,
        };
        next.last_status = Some(stop.status);
        next.updated_at = now;
        next.record_event(
            now,
            AutopilotEventKind::Escalated(Some(stop.question.to_string())),
        );
        next.record_decision(DecisionRecord::at(
            now,
            None,
            DecisionKind::Escalated,
            Some(stop.question.to_string()),
            vec![stop_id.clone()],
        ));
        self.save_ledger(&mut next)?;
        self.run = JobRun::Blocked {
            stop_ids: vec![stop_id.clone()],
            since: now,
        };
        // A pane parked on a dialog (or on a decision the harness just handed the human) is
        // NOT stalling: close the stall window so the busy_since backstop can never
        // additionally fire a bogus "wedged / no progress" `Stuck` on top of this
        // escalation (belt-and-braces — while the dialog is up `drive` returns here before
        // `busy_recheck`, and the `Blocked` run doesn't reach `drive` at all — and so that a
        // window opened by the Busy ticks BEFORE the dialog appeared cannot survive the
        // human's answer). It also voids any armed Idle observations.
        self.busy_since = None;
        self.reset_idle_gate();
        Ok(JobTick::Escalated(vec![stop_id]))
    }

    /// Park `Blocked` on a synthetic `Stuck` stop and surface it once. Restart
    /// resumes it as `Blocked` (no re-notify); a human answering it (pmtui) or
    /// closing the session unblocks it, exactly like any escalation.
    pub(super) fn park_stuck(
        &mut self,
        now: Epoch,
        ledger: &AgentLoopState,
        reason: String,
    ) -> Result<JobTick> {
        self.park_stuck_kind(now, ledger, StopKind::Stuck, reason)
    }

    /// As [`park_stuck`] but with an explicit stop `kind` (e.g. `Capability` for a
    /// codex-id-capture failure). Both kinds are always-`Hard`, so both escalate on
    /// every tier and re-emit as `Escalated([id])` on later parked ticks (daemon
    /// dedups). Returns `JobTick::Stuck` so the daemon notifies once.
    pub(super) fn park_stuck_kind(
        &mut self,
        now: Epoch,
        ledger: &AgentLoopState,
        kind: StopKind,
        reason: String,
    ) -> Result<JobTick> {
        let stop_id = format!("stop-{}-stuck-{now}-{}", self.project_id, mint_uuid_v4());
        // No draft behind a synthesized stall stop, but the `reason` is exactly what
        // the human needs to read, so it doubles as the question.
        let stop = open_stop(stop_id.clone(), kind, None, &reason, &[], now);
        let mut next = ledger.clone();
        next.open_stops = vec![stop];
        next.run = JobRun::Blocked {
            stop_ids: vec![stop_id.clone()],
            since: now,
        };
        next.last_status = Some(reason.clone());
        next.updated_at = now;
        // A `Capability` park (dead pane, codex-id-capture failure) is NOT a stall: surface
        // it as "needs you" (yellow), matching `park_capability_stop`, not the red "stuck /
        // no progress" framing `dead_pane_escalation` deliberately avoids. Only a genuine
        // `Stuck` kind reads as stuck.
        let event = match kind {
            StopKind::Capability => AutopilotEventKind::Escalated(Some(reason.clone())),
            _ => AutopilotEventKind::Stuck(Some(reason.clone())),
        };
        next.record_event(now, event);
        self.save_ledger(&mut next)?;
        self.run = JobRun::Blocked {
            stop_ids: vec![stop_id],
            since: now,
        };
        Ok(JobTick::Stuck(reason))
    }
}

/// Summarize human answers for `stop_ids` (at/after `since`) into a wake-prompt
/// `extra` block, so the resumed agent sees the resolved decisions as binding.
/// Mirrors `phase_engine::answers_extra`.
fn answers_extra(answers: &[Answer], stop_ids: &[String], since: Epoch) -> String {
    let mut lines: Vec<String> = Vec::new();
    for stop_id in stop_ids {
        let Some(a) = answers
            .iter()
            .rev()
            .find(|answer| answer.answered_at >= since && answer.stop_id == *stop_id)
        else {
            continue;
        };
        let note = a.note.as_deref().unwrap_or("").trim();
        if note.is_empty() {
            lines.push(format!("- {}: {}", a.stop_id, a.answer));
        } else {
            lines.push(format!("- {}: {} ({note})", a.stop_id, a.answer));
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!(
            "The human resolved these — treat them as binding and continue:\n{}",
            lines.join("\n")
        )
    }
}

pub(super) fn dashboard_answerable_dialog(dialog: &tmux::PaneDialog) -> bool {
    dialog.dashboard_answerable()
        && advise::hard_floor_hit_in(&dialog.question, &dialog.options).is_none()
}

fn reset_persisted_progress(state: &mut AgentLoopState) {
    state.continuations = 0;
    state.stale_plan_streak = 0;
    state.marker_less_rechecks = 0;
}

/// Construct an `AwaitingReply` open stop with the common defaults (mirrors
/// `phase_engine::open_stop`, including how `question`/`options` carry the marker's
/// drafted ask into the ledger and how a blank question normalises to `None`).
pub(super) fn open_stop(
    id: String,
    kind: StopKind,
    context_ref: Option<String>,
    question: &str,
    options: &[String],
    now: Epoch,
) -> OpenStop {
    OpenStop {
        id,
        kind,
        pane_dialog: None,
        channel: None,
        context_ref,
        question: crate::pmstate::normalize_question(question),
        options: options.to_vec(),
        authorized_responders: Vec::new(),
        message_id: None,
        first_posted: now,
        last_polled: None,
        last_seen_reply_ts: None,
        status: StopStatus::AwaitingReply,
    }
}
