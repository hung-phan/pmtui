//! Delegating positively identified terminal choice menus to the existing
//! read-only decider, then applying its validated index under the pane input lock.

use std::time::Duration;

use anyhow::Result;

use crate::advise::{self, Consult, Refusal, Verdict};
use crate::clock::Epoch;
use crate::job::{AgentLoopState, AutopilotEventKind, JobRun};
use crate::lease;
use crate::tmux::{self, Driver};

use super::nudge::DEFAULT_CADENCE_S;
use super::session::mint_uuid_v4;
use super::supervisor::{
    AdviceTarget, AdviseInFlight, AdviseStep, DeciderSelection, SUPERVISOR_POLL_S,
    SUPERVISOR_TIMEOUT_S, one_line, project_situation,
};
use super::{JobScheduler, JobTick};

impl JobScheduler {
    /// Ask the existing read-only decider to choose a positively identified,
    /// non-permission in-pane option. `None` means no safe consult can run, and the
    /// caller must park the dialog for the human.
    pub(super) fn spawn_dialog_advice(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        next: &AgentLoopState,
        dialog: tmux::PaneDialog,
        decider_engine: crate::registry::Engine,
        decider_model: Option<&str>,
    ) -> Result<Option<JobTick>> {
        if !self.advise_enabled
            || self.advise_health.latched
            || dialog.class != tmux::PaneDialogClass::DelegableChoice
            || dialog.selected_index.is_none()
            || advise::hard_floor_hit_in(&dialog.question, &dialog.options).is_some()
        {
            return Ok(None);
        }
        let goal =
            advise::clamp_goal(&std::fs::read_to_string(self.paths.brief()).unwrap_or_default());
        if goal.trim().is_empty() {
            return Ok(None);
        }
        let concrete_indices: Vec<usize> = dialog
            .options
            .iter()
            .enumerate()
            .filter_map(|(index, option)| tmux::dialog_option_is_concrete(option).then_some(index))
            .collect();
        if concrete_indices.len() < 2
            || (dialog.mode == tmux::PaneDialogMode::Multiple && concrete_indices.len() > 3)
        {
            return Ok(None);
        }
        let option_sets = dialog_option_sets(dialog.mode, &concrete_indices);
        let consult = Consult {
            nonce: mint_uuid_v4(),
            goal,
            question: dialog.question.clone(),
            options: option_sets
                .iter()
                .map(|indices| {
                    indices
                        .iter()
                        .filter_map(|index| dialog.options.get(*index))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" + ")
                })
                .collect(),
            reported_effect: None,
            situation: project_situation(next),
            directive: advise::clamp_directive(
                &std::fs::read_to_string(self.paths.directive()).unwrap_or_default(),
            ),
        };
        self.spawn_consult(
            driver,
            now,
            next,
            consult,
            AdviceTarget::Dialog {
                dialog,
                option_sets,
            },
            DeciderSelection {
                engine: decider_engine,
                model: decider_model,
                reported_kind: None,
                effect: None,
                policy: crate::job::DeciderPolicy {
                    kind: crate::pmstate::StopKind::Ambiguity,
                    labelled_risk: crate::state::RiskClass::Medium,
                    effective_risk: crate::state::RiskClass::Medium,
                },
            },
        )
    }

    pub(super) fn apply_dialog_verdict(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
        mut inflight: Box<AdviseInFlight>,
        verdict: Verdict,
    ) -> Result<AdviseStep> {
        let (expected, option_sets) = match &inflight.target {
            AdviceTarget::Dialog {
                dialog,
                option_sets,
            } => (dialog.clone(), option_sets.clone()),
            AdviceTarget::Marker { .. } => return Ok(AdviseStep::NotEngaged),
        };
        let (target, option, reason) = match verdict {
            Verdict::Select {
                index,
                option,
                reason,
            } => (index, option, reason),
            Verdict::Answer { .. } => {
                let tick = self.advice_refused(
                    now,
                    base,
                    &inflight,
                    &Refusal::OutsideGrant {
                        action: "answer".to_string(),
                        grant: crate::advise::Grant::PickOption,
                    },
                )?;
                return Ok(AdviseStep::Yields(tick));
            }
        };

        let input_lock = self.paths.input_lock();
        let _input_lease =
            match lease::acquire_with_retry(&input_lock, 3, Duration::from_millis(25))? {
                Some(lease) => lease,
                None => {
                    let until = now + SUPERVISOR_POLL_S;
                    self.advise = Some(inflight);
                    self.persist_run(base, now, JobRun::Monitoring { until }, None)?;
                    return Ok(AdviseStep::Yields(JobTick::Monitoring { until }));
                }
            };

        if self.human_present(driver, now) {
            if !inflight.frozen {
                inflight.frozen = true;
                inflight.deadline = now + SUPERVISOR_TIMEOUT_S;
            }
            let until = now + SUPERVISOR_POLL_S;
            self.advise = Some(inflight);
            self.persist_run(base, now, JobRun::Monitoring { until }, None)?;
            return Ok(AdviseStep::Yields(JobTick::Monitoring { until }));
        }

        let session = self.loop_session();
        let alive = driver.is_alive(&session).unwrap_or(false);
        let dead = driver.pane_dead(&session).unwrap_or(true);
        if !alive || dead {
            let refusal = Refusal::NoResult {
                detail: "the worker pane disappeared before its choice could be selected"
                    .to_string(),
            };
            let audited = audited_outcome(
                base,
                &inflight,
                now,
                crate::job::DeciderOutcome::Failed {
                    reason: refusal.to_string(),
                },
            );
            let tick = self.park_advice_refusal(
                now,
                &audited,
                &inflight.consult,
                &inflight.target,
                &refusal,
            )?;
            return Ok(AdviseStep::Yields(tick));
        }
        let capture = match driver.capture_tail(&session, 40) {
            Ok(capture) => capture,
            Err(e) => {
                let refusal = Refusal::NoResult {
                    detail: format!(
                        "the worker pane could not be re-read before selecting its choice ({e})"
                    ),
                };
                let audited = audited_outcome(
                    base,
                    &inflight,
                    now,
                    crate::job::DeciderOutcome::Failed {
                        reason: refusal.to_string(),
                    },
                );
                let tick = self.park_advice_refusal(
                    now,
                    &audited,
                    &inflight.consult,
                    &inflight.target,
                    &refusal,
                )?;
                return Ok(AdviseStep::Yields(tick));
            }
        };
        let current = tmux::classify_dialog(&capture);
        if current.as_ref() != Some(&expected) {
            let mut changed = base.clone();
            changed.finish_decider_run(
                inflight.parked.seq,
                now,
                crate::job::DeciderOutcome::Interrupted {
                    reason: "the interactive prompt changed before the decider returned".into(),
                },
            );
            changed.run = JobRun::Monitoring { until: now };
            changed.updated_at = now;
            changed.last_status =
                Some("the interactive prompt changed before its delegated choice returned".into());
            self.save_ledger(&mut changed)?;
            self.run = JobRun::Monitoring { until: now };
            return Ok(AdviseStep::Yields(JobTick::Monitoring { until: now }));
        }
        if expected.class != tmux::PaneDialogClass::DelegableChoice
            || advise::hard_floor_hit_in(&expected.question, &expected.options).is_some()
            || expected.selected_index.is_none()
        {
            let audited = audited_outcome(
                base,
                &inflight,
                now,
                crate::job::DeciderOutcome::Refused {
                    reason: "the prompt no longer passed the delegable-choice safety checks".into(),
                },
            );
            let tick = self.park_dialog(now, &audited, expected)?;
            return Ok(AdviseStep::Yields(tick));
        }
        let Some(pane_targets) = option_sets.get(target).cloned() else {
            let tick = self.advice_refused(
                now,
                base,
                &inflight,
                &Refusal::IndexOutOfRange {
                    got: target as i64,
                    options: option_sets.len(),
                },
            )?;
            return Ok(AdviseStep::Yields(tick));
        };
        let summary = selection_summary(&pane_targets, &option, &reason);
        let audit_answer = selection_answer(&pane_targets, &option);
        let fingerprint = (advise::text_hash(&capture), summary.clone());
        if self.advise_last.as_ref() == Some(&fingerprint) {
            let tick = self.advice_refused(now, base, &inflight, &Refusal::NoProgress)?;
            return Ok(AdviseStep::Yields(tick));
        }

        let interactive = match driver.verify_dialog_interactive(&session, &expected) {
            Ok(dialog) => dialog,
            Err(_) => {
                let audited = audited_outcome(
                    base,
                    &inflight,
                    now,
                    crate::job::DeciderOutcome::Refused {
                        reason: "the live prompt did not pass the interactivity proof".into(),
                    },
                );
                let tick = self.park_dialog(now, &audited, expected)?;
                return Ok(AdviseStep::Yields(tick));
            }
        };
        if self.human_present(driver, now) {
            let audited = audited_outcome(
                base,
                &inflight,
                now,
                crate::job::DeciderOutcome::Interrupted {
                    reason: "a human attached after the interactivity proof; automated selection \
                             was cancelled"
                        .into(),
                },
            );
            let tick = self.park_dialog(now, &audited, expected)?;
            return Ok(AdviseStep::Yields(tick));
        }
        if let Err(e) = driver.select_dialog_options(&session, &interactive, &pane_targets) {
            let refusal = Refusal::NoResult {
                detail: format!("the selected option could not be sent to the worker pane ({e})"),
            };
            let audited = audited_outcome(
                base,
                &inflight,
                now,
                crate::job::DeciderOutcome::Failed {
                    reason: refusal.to_string(),
                },
            );
            let tick = self.park_advice_refusal(
                now,
                &audited,
                &inflight.consult,
                &inflight.target,
                &refusal,
            )?;
            return Ok(AdviseStep::Yields(tick));
        }

        self.advise_health.note_success();
        self.busy_since = None;
        self.reset_idle_gate();
        let turn_count = self.turn_count();
        self.turns_at_nudge = Some(turn_count);
        let cadence = base.cadence_s.unwrap_or(DEFAULT_CADENCE_S) as i64;
        let until = now + cadence;
        let mut applied = base.clone();
        applied.nudged_at_seq = Some(applied.last_marker_seq);
        applied.nudged_at_report_generation = Some(applied.report_generation);
        applied.nudged_at = Some(now);
        applied.turn_count_at_nudge = Some(turn_count);
        applied.run = JobRun::Monitoring { until };
        applied.updated_at = now;
        applied.last_status = Some(format!(
            "the session supervisor selected a low-stakes interactive choice: {summary}"
        ));
        applied.finish_decider_run(
            inflight.parked.seq,
            now,
            crate::job::DeciderOutcome::Resolved {
                answer: audit_answer,
                reason: reason.clone(),
            },
        );
        applied.record_event(now, AutopilotEventKind::SupervisorResolved(Some(summary)));
        self.save_ledger(&mut applied)?;
        self.run = JobRun::Monitoring { until };
        self.advise_last = Some(fingerprint);
        Ok(AdviseStep::Yields(JobTick::Monitoring { until }))
    }
}

fn audited_outcome(
    base: &AgentLoopState,
    inflight: &AdviseInFlight,
    now: Epoch,
    outcome: crate::job::DeciderOutcome,
) -> AgentLoopState {
    let mut audited = base.clone();
    audited.finish_decider_run(inflight.parked.seq, now, outcome);
    audited
}

fn dialog_option_sets(mode: tmux::PaneDialogMode, indices: &[usize]) -> Vec<Vec<usize>> {
    if mode == tmux::PaneDialogMode::Single {
        return indices.iter().copied().map(|index| vec![index]).collect();
    }
    let mut sets = Vec::new();
    for size in 1..=indices.len() {
        for mask in 1usize..(1usize << indices.len()) {
            if mask.count_ones() as usize == size {
                sets.push(
                    indices
                        .iter()
                        .enumerate()
                        .filter_map(|(bit, index)| ((mask & (1 << bit)) != 0).then_some(*index))
                        .collect(),
                );
            }
        }
    }
    sets
}

fn selection_summary(targets: &[usize], option: &str, reason: &str) -> String {
    let answer = selection_answer(targets, option);
    format!(
        "{answer}{}",
        if reason.trim().is_empty() {
            String::new()
        } else {
            format!(" ({})", one_line(reason))
        }
    )
}

fn selection_answer(targets: &[usize], option: &str) -> String {
    let label = if targets.len() == 1 {
        format!("option {}", targets[0] + 1)
    } else {
        format!(
            "options {}",
            targets
                .iter()
                .map(|index| (index + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!("{label} — {}", one_line(option))
}
