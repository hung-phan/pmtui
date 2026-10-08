//! What the harness does when a consult does NOT produce a usable verdict: escalate
//! the worker's REAL question to the human, count the failure toward the latch that
//! latches the broken transport off rather than retrying forever, and handles a consult debt
//! inherited from a daemon that
//! died mid-consult. Kept apart from [`super::supervisor`] because these are the
//! paths that have to be trusted: every one of them must reach a human,
//! and none of them may leave a blanket approval behind.

use anyhow::{Context, Result, bail};

use crate::advise::{Consult, Refusal};
use crate::clock::Epoch;
use crate::job::{self, AgentLoopState, JobRun};
use crate::tmux::{self, Driver};

use super::stops::{CapabilityStop, dashboard_answerable_dialog};
use super::supervisor::{AdviceTarget, AdviseInFlight, AdviseStep};
use super::{JobScheduler, JobTick};

/// Consecutive supervisor invocations that produced no usable result (non-zero exit,
/// orphan, timeout) after which the feature LATCHES OFF for this session and future
/// decisions escalate without a model. Mirrors `DesktopNotifier`'s `DESKTOP_DEAD_AFTER`:
/// one failure could be a blip, three in a row with no success between is a broken
/// transport. A hard SPAWN error latches immediately (nothing transient about "could not
/// start it"), exactly as the notifier treats a spawn error.
pub(super) const SUPERVISOR_DEAD_AFTER: usize = 3;
/// Consecutive REFUSALS after which the feature latches off (Rule 7's second half). A
/// refusal already escalates, so the cap only matters when a human keeps answering "keep
/// going" into the same unanswerable decision; past this the harness stops paying for an
/// opinion it has learned it will not get.
pub(super) const SUPERVISOR_MAX_REFUSALS: u32 = 3;

/// Delivery health of this session's supervisor — the latch that stops retrying a broken
/// transport. Policy-eligible decisions then escalate rather than being auto-approved.
/// Mirrors `escalation::DesktopHealth`, minus the atomics (a `JobScheduler` is owned by
/// one sweep thread, so plain fields suffice).
#[derive(Debug, Default)]
pub(super) struct AdviseHealth {
    /// Latched: stop consulting and escalate decisions that require a verdict.
    pub(super) latched: bool,
    pub(super) reason: Option<String>,
    /// Whether the one-line "supervisor is off" warning has been printed.
    pub(super) warned: bool,
    /// Consecutive no-result outcomes; any usable verdict resets it.
    pub(super) no_result_streak: usize,
    /// Consecutive refusals; any usable verdict resets it.
    pub(super) refusal_streak: u32,
}

impl AdviseHealth {
    /// Print the "the supervisor is switched off" line at most once. A daemon sweeping
    /// every 500ms would otherwise repeat it forever, so the single line has to say
    /// everything a reader needs.
    pub(super) fn latch_off(&mut self, project_id: &str, why: &str) {
        self.latched = true;
        self.reason = Some(why.to_string());
        if !self.warned {
            self.warned = true;
            eprintln!(
                "pmd: session supervisor disabled for {project_id}: {why} — low-stakes \
                 decisions now escalate without a model verdict; this warning is not repeated"
            );
        }
    }

    /// A consult produced a usable verdict: clear both streaks so past blips cannot
    /// accumulate into a latch-off of a supervisor that demonstrably works.
    pub(super) fn note_success(&mut self) {
        self.no_result_streak = 0;
        self.refusal_streak = 0;
    }
}

impl JobScheduler {
    /// Recover a consult debt inherited from a daemon that died mid-consult. The reply cannot be
    /// validated after restart because its nonce and in-memory grant are gone, so recovery
    /// escalates the original audited question instead of manufacturing an approval.
    pub(super) fn recover_parked_advice(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
        parked: &job::ParkedAdvice,
    ) -> Result<AdviseStep> {
        let session = tmux::supervisor_session_name(&self.project_id, &self.work_dir, parked.seq);
        driver
            .terminate(&session)
            .with_context(|| format!("terminate interrupted decider session {session}"))?;
        if !self.cleanup_advice_artifacts(parked.seq) {
            bail!(
                "clean interrupted decider artifacts for {} sequence {}",
                self.project_id,
                parked.seq
            );
        }
        let audit = base
            .decider_runs
            .iter()
            .rev()
            .find(|run| run.seq == parked.seq && run.target == job::DeciderTarget::Marker)
            .cloned();
        let report_seq = base
            .advice_queue
            .iter()
            .find(|queued| parked.stop_ids.contains(&queued.stop_id))
            .map(|queued| queued.report_seq);
        let mut escalated = base.clone();
        escalated
            .advice_queue
            .retain(|queued| !parked.stop_ids.contains(&queued.stop_id));
        escalated.finish_decider_run(
            parked.seq,
            now,
            job::DeciderOutcome::Interrupted {
                reason: "daemon restarted before the decider reply could be validated".into(),
            },
        );
        if parked.pane_dialog {
            let until = now;
            escalated.run = JobRun::Monitoring { until };
            escalated.updated_at = now;
            escalated.last_status = Some(
                "a delegated interactive choice was interrupted by daemon restart; \
                 the live prompt will be rechecked"
                    .into(),
            );
            self.save_ledger(&mut escalated)?;
            self.run = JobRun::Monitoring { until };
            return Ok(AdviseStep::Yields(JobTick::Monitoring { until }));
        }
        if let Some(report_seq) = report_seq {
            escalated.finish_turn_review(report_seq, job::TurnReviewOutcome::Escalated);
        }
        let Some(audit) = audit else {
            let tick = self.park_stuck_kind(
                now,
                &escalated,
                crate::pmstate::StopKind::Capability,
                "a decider consult was interrupted by daemon restart, but its original question \
                 is unavailable; review the worker session"
                    .into(),
            )?;
            return Ok(AdviseStep::Yields(tick));
        };
        let status = "a decider consult was interrupted by daemon restart before its reply could \
                      be validated; the original decision is yours"
            .to_string();
        let tick = self.park_capability_stop(
            now,
            &escalated,
            CapabilityStop {
                id_tag: "advice",
                dialog: None,
                question: &audit.question,
                options: &audit.options,
                status,
            },
        )?;
        Ok(AdviseStep::Yields(tick))
    }

    /// A consult produced NO usable result (harness deadline, non-zero exit, orphaned
    /// session, unreadable signal). Rule 6: escalate to the human — never a silent retry,
    /// and never the old canned string pretending to be a decision — and count it toward
    /// the latch, which after [`SUPERVISOR_DEAD_AFTER`] in a row stops further model calls;
    /// later decisions still escalate.
    pub(super) fn advice_failed(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
        inflight: &AdviseInFlight,
        detail: String,
    ) -> Result<JobTick> {
        self.advise_health.no_result_streak += 1;
        if self.advise_health.no_result_streak >= SUPERVISOR_DEAD_AFTER {
            self.advise_health.latch_off(
                &self.project_id,
                &format!(
                    "{SUPERVISOR_DEAD_AFTER} consults in a row produced no result \
                     (last: {detail})"
                ),
            );
        }
        let refusal = Refusal::NoResult { detail };
        let mut audited = base.clone();
        audited.finish_decider_run(
            inflight.parked.seq,
            now,
            job::DeciderOutcome::Failed {
                reason: refusal.to_string(),
            },
        );
        self.park_advice_refusal(now, &audited, &inflight.consult, &inflight.target, &refusal)
    }

    /// A consult ANSWERED but the answer is not usable — it refused, echoed the wrong
    /// nonce, picked an out-of-range index, asked for something outside its grant, carried
    /// a control byte, or would merely have repeated itself (Rule 7). Escalate, and count
    /// it toward [`SUPERVISOR_MAX_REFUSALS`]: a refusal already reaches the human, so the
    /// cap only bites when a human keeps answering "keep going" into the same
    /// unanswerable decision, at which point paying for another opinion is waste.
    pub(super) fn advice_refused(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
        inflight: &AdviseInFlight,
        refusal: &Refusal,
    ) -> Result<JobTick> {
        self.advise_health.refusal_streak += 1;
        if self.advise_health.refusal_streak >= SUPERVISOR_MAX_REFUSALS {
            self.advise_health.latch_off(
                &self.project_id,
                &format!(
                    "{SUPERVISOR_MAX_REFUSALS} consults in a row were unusable \
                     (last: {refusal})"
                ),
            );
        }
        let mut audited = base.clone();
        audited.finish_decider_run(
            inflight.parked.seq,
            now,
            job::DeciderOutcome::Refused {
                reason: refusal.to_string(),
            },
        );
        self.park_advice_refusal(now, &audited, &inflight.consult, &inflight.target, refusal)
    }

    /// Park `Blocked` on ONE stop carrying the WORKER's real question + options, so the
    /// human answers the ACTUAL decision rather than a summary of a harness failure. The
    /// refusal reason rides in `last_status`, where the dashboard shows it.
    ///
    /// Kind: [`StopKind::Capability`], and Rule 2 makes that non-negotiable. It must NOT
    /// be `WorkerStuck` (nor `Ambiguity`/`ExpertNeeded`): all three floor to `Medium` in
    /// [`crate::policy::effective_risk_kind`], and `(Autopilot, Medium) => AutoFlow` —
    /// so the refusal would AUTO-APPROVE the very decision being refused, which is the
    /// one outcome that must never happen here. `Capability` is forced `Hard`, and `Hard`
    /// escalates on BOTH tiers. It is also the honest label: it means precisely "the
    /// harness cannot decide this itself". `park_dialog` records the same trap.
    pub(super) fn park_advice_refusal(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
        consult: &Consult,
        target: &AdviceTarget,
        refusal: &Refusal,
    ) -> Result<JobTick> {
        let (id_tag, dialog, question, options, instruction) = match target {
            AdviceTarget::Dialog { dialog, .. } => (
                "dialog",
                Some(dialog),
                dialog.question.as_str(),
                dialog.options.as_slice(),
                if !dashboard_answerable_dialog(dialog) {
                    "Attach to the session and answer it there"
                } else {
                    "Answer it here or attach to the session"
                },
            ),
            AdviceTarget::Marker { .. } => {
                // A worker may raise a stop with options but no prose; say something true
                // rather than persisting a blank question (`open_stop` would normalize it
                // to `None`, leaving the human a list of options and no context).
                let question = if consult.question.trim().is_empty() {
                    "the agent paused on a low-stakes decision it did not phrase as a question"
                } else {
                    &consult.question
                };
                (
                    "advice",
                    None,
                    question,
                    consult.options.as_slice(),
                    "Answer it here",
                )
            }
        };
        let status =
            format!("a low-stakes decision could not be auto-resolved — {refusal}. {instruction}");
        let mut escalated = base.clone();
        if let AdviceTarget::Marker {
            stop_ids,
            report_seq,
        } = target
        {
            escalated
                .advice_queue
                .retain(|queued| !stop_ids.contains(&queued.stop_id));
            escalated.finish_turn_review(*report_seq, job::TurnReviewOutcome::Escalated);
        }
        self.park_capability_stop(
            now,
            &escalated,
            CapabilityStop {
                id_tag,
                dialog,
                question,
                options,
                status,
            },
        )
    }
}
