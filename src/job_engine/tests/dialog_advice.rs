//! Focused tests for dialog-advice eligibility, late safety checks, and process
//! lifecycle failures. The end-to-end happy paths remain in `supervisor.rs`.

use std::path::Path;

use super::*;
use crate::advise::{Refusal, Verdict};

const PERMISSION_DIALOG_PANE: &str = concat!(
    " Do you want to create hello.txt?\n",
    " ❯ 1. Yes\n",
    "   2. Yes, allow all edits during this session\n",
    "   3. No\n",
    "\n",
    " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
);

fn prepared_dialog_inflight() -> (Fx, String, Box<AdviseInFlight>) {
    let (mut fx, session) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(
        &fx,
        "Keep the changelog tooling consistent; dprint is already vendored.",
    );
    fx.driver.set_tail(&session, CHOICE_DIALOG_PANE);
    let dialog = tmux::classify_dialog(CHOICE_DIALOG_PANE).expect("choice dialog");
    let base = ledger(&fx);

    assert_eq!(
        fx.sched
            .spawn_dialog_advice(&fx.driver, START, &base, dialog, Engine::Claude, None)
            .unwrap(),
        Some(JobTick::Monitoring {
            until: START + SUPERVISOR_POLL_S
        })
    );
    let inflight = fx.sched.advise.take().expect("dialog consult in flight");
    (fx, session, inflight)
}

fn select(index: usize, option: &str, reason: &str) -> Verdict {
    Verdict::Select {
        index,
        option: option.to_string(),
        reason: reason.to_string(),
    }
}

fn yielded(step: AdviseStep) -> JobTick {
    match step {
        AdviseStep::Yields(tick) => tick,
        AdviseStep::NotEngaged => panic!("dialog advice unexpectedly declined the target"),
        AdviseStep::Applied => panic!("dialog advice unexpectedly produced marker text"),
    }
}

fn assert_capability_escalation(fx: &Fx, tick: JobTick, status_fragment: &str) {
    assert!(matches!(tick, JobTick::Escalated(_)));
    let current = ledger(fx);
    assert!(matches!(current.run, JobRun::Blocked { .. }));
    assert_eq!(current.open_stops.len(), 1);
    assert!(
        current.open_stops[0].pane_dialog.is_some(),
        "dialog escalation must preserve pane routing metadata"
    );
    assert!(
        current
            .last_status
            .as_deref()
            .is_some_and(|status| status.contains(status_fragment)),
        "unexpected status: {:?}",
        current.last_status
    );
    assert!(fx.driver.applied_dialog_selections().is_empty());
}

#[test]
fn a_marker_target_is_not_consumed_by_the_dialog_verdict_path() {
    let (mut fx, _, mut inflight) = prepared_dialog_inflight();
    inflight.target = AdviceTarget::Marker {
        stop_ids: vec!["stop-1".into()],
        report_seq: 0,
    };
    let before = ledger(&fx);

    let step = fx
        .sched
        .apply_dialog_verdict(
            &fx.driver,
            START + SUPERVISOR_POLL_S,
            &before,
            inflight,
            select(0, "prettier", "consistent"),
        )
        .unwrap();

    assert!(matches!(step, AdviseStep::NotEngaged));
    assert!(fx.driver.applied_dialog_selections().is_empty());
}

#[test]
fn a_free_text_verdict_for_a_choice_escalates_instead_of_typing() {
    let (mut fx, _, inflight) = prepared_dialog_inflight();
    let base = ledger(&fx);

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &fx.driver,
                START + SUPERVISOR_POLL_S,
                &base,
                inflight,
                Verdict::Answer {
                    text: "use dprint".into(),
                    reason: "vendored".into(),
                },
            )
            .unwrap(),
    );

    assert_capability_escalation(&fx, tick, "outside this decision's grant");
}

#[test]
fn a_refused_dialog_consult_preserves_menu_routing_for_the_human_answer() {
    let (mut fx, session, inflight) = prepared_dialog_inflight();
    let expected = match &inflight.target {
        AdviceTarget::Dialog { dialog, .. } => dialog.clone(),
        AdviceTarget::Marker { .. } => panic!("expected dialog advice"),
    };
    let base = ledger(&fx);
    let refusal = Refusal::SupervisorRefused {
        reason: "the goal does not determine the choice".into(),
    };

    let tick = fx
        .sched
        .advice_refused(START + SUPERVISOR_POLL_S, &base, &inflight, &refusal)
        .unwrap();

    assert_capability_escalation(&fx, tick, "does not determine the choice");
    let parked = ledger(&fx);
    let stop = &parked.open_stops[0];
    assert_eq!(stop.question.as_deref(), Some(expected.question.as_str()));
    assert_eq!(stop.options, expected.options);
    assert_eq!(
        stop.pane_dialog.as_ref().map(|dialog| dialog.fingerprint),
        Some(expected.identity_fingerprint())
    );
    assert!(stop.dashboard_answerable_dialog());

    fx.clock.set(START + SUPERVISOR_POLL_S + 1);
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: stop.id.clone(),
            answer: "dprint".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: fx.clock.now(),
        },
    )
    .unwrap();

    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        fx.driver.applied_dialog_selections(),
        vec![(session, vec![1])]
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a menu answer must be applied as a selection, not pasted as prose"
    );
}

#[test]
fn an_attached_human_freezes_the_deadline_and_preserves_the_verdict() {
    let (mut fx, session, inflight) = prepared_dialog_inflight();
    fx.driver.set_clients(&session, true);
    let base = ledger(&fx);
    let now = START + SUPERVISOR_POLL_S;

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &fx.driver,
                now,
                &base,
                inflight,
                select(1, "dprint", "vendored"),
            )
            .unwrap(),
    );

    assert_eq!(
        tick,
        JobTick::Monitoring {
            until: now + SUPERVISOR_POLL_S
        }
    );
    let preserved = fx.sched.advise.as_ref().expect("verdict remains in flight");
    assert!(preserved.frozen);
    assert_eq!(preserved.deadline, now + SUPERVISOR_TIMEOUT_S);
    assert!(ledger(&fx).advice_inflight.is_some());
    assert!(fx.driver.applied_dialog_selections().is_empty());
}

#[test]
fn a_human_attaching_after_the_probe_cancels_selection() {
    let (mut fx, session, inflight) = prepared_dialog_inflight();
    fx.driver.attach_after_verify_dialog(&session);
    let base = ledger(&fx);
    let now = START + SUPERVISOR_POLL_S;

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &fx.driver,
                now,
                &base,
                inflight,
                select(1, "dprint", "vendored"),
            )
            .unwrap(),
    );

    assert!(matches!(tick, JobTick::Escalated(_)));
    assert!(fx.driver.applied_dialog_selections().is_empty());
    let current = ledger(&fx);
    assert!(matches!(
        &current.decider_runs[0].outcome,
        job::DeciderOutcome::Interrupted { reason }
            if reason.contains("attached after the interactivity proof")
    ));
}

#[test]
fn a_disappeared_worker_pane_escalates_before_selection() {
    let (mut fx, session, inflight) = prepared_dialog_inflight();
    fx.driver.set_alive(&session, false);
    let base = ledger(&fx);

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &fx.driver,
                START + SUPERVISOR_POLL_S,
                &base,
                inflight,
                select(1, "dprint", "vendored"),
            )
            .unwrap(),
    );

    assert_capability_escalation(&fx, tick, "worker pane disappeared");
}

#[test]
fn a_liveness_probe_error_fails_closed_as_a_disappeared_pane() {
    let (mut fx, session, inflight) = prepared_dialog_inflight();
    fx.driver.fail_is_alive(&session);
    let base = ledger(&fx);

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &fx.driver,
                START + SUPERVISOR_POLL_S,
                &base,
                inflight,
                select(1, "dprint", "vendored"),
            )
            .unwrap(),
    );

    assert_capability_escalation(&fx, tick, "worker pane disappeared");
}

#[test]
fn a_dead_worker_pane_escalates_even_when_the_session_is_alive() {
    let (mut fx, session, inflight) = prepared_dialog_inflight();
    fx.driver.set_pane_dead(&session, true);
    let base = ledger(&fx);

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &fx.driver,
                START + SUPERVISOR_POLL_S,
                &base,
                inflight,
                select(1, "dprint", "vendored"),
            )
            .unwrap(),
    );

    assert_capability_escalation(&fx, tick, "worker pane disappeared");
}

struct CaptureFails<'a>(&'a FakeDriver);

impl Driver for CaptureFails<'_> {
    fn spawn_step(
        &self,
        session: &str,
        cwd: &Path,
        command: &[String],
        done_signal: &Path,
        log: &Path,
    ) -> Result<tmux::StepHandle> {
        self.0.spawn_step(session, cwd, command, done_signal, log)
    }

    fn is_alive(&self, session: &str) -> Result<bool> {
        self.0.is_alive(session)
    }

    fn capture_tail(&self, session: &str, _lines: usize) -> Result<String> {
        anyhow::bail!("capture failed for {session} (armed)")
    }

    fn terminate(&self, session: &str) -> Result<()> {
        self.0.terminate(session)
    }

    fn has_clients(&self, session: &str) -> Result<bool> {
        self.0.has_clients(session)
    }

    fn pane_dead(&self, session: &str) -> Result<bool> {
        self.0.pane_dead(session)
    }
}

#[test]
fn a_pane_reread_error_escalates_the_original_question() {
    let (mut fx, _, inflight) = prepared_dialog_inflight();
    let base = ledger(&fx);
    let driver = CaptureFails(&fx.driver);

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &driver,
                START + SUPERVISOR_POLL_S,
                &base,
                inflight,
                select(1, "dprint", "vendored"),
            )
            .unwrap(),
    );

    assert_capability_escalation(&fx, tick, "could not be re-read");
    assert_eq!(
        ledger(&fx).open_stops[0].question.as_deref(),
        Some("Which formatter should I use for the changelog?")
    );
}

#[test]
fn a_late_human_only_reclassification_parks_the_dialog() {
    let (mut fx, session, mut inflight) = prepared_dialog_inflight();
    let permission =
        tmux::classify_dialog(PERMISSION_DIALOG_PANE).expect("permission dialog recognised");
    assert_eq!(permission.class, tmux::PaneDialogClass::HumanOnly);
    inflight.target = AdviceTarget::Dialog {
        dialog: permission.clone(),
        option_sets: vec![vec![0], vec![1], vec![2]],
    };
    fx.driver.set_tail(&session, PERMISSION_DIALOG_PANE);
    let base = ledger(&fx);

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &fx.driver,
                START + SUPERVISOR_POLL_S,
                &base,
                inflight,
                select(0, "Yes", ""),
            )
            .unwrap(),
    );

    assert_capability_escalation(&fx, tick, "waiting on an interactive prompt");
    assert_eq!(
        ledger(&fx).open_stops[0].question.as_deref(),
        Some(permission.question.as_str())
    );
    let parked = ledger(&fx);
    assert!(parked.open_stops[0].is_pane_dialog());
    assert!(
        !parked.open_stops[0].dashboard_answerable_dialog(),
        "human-only dialogs must remain attach-only"
    );
}

#[test]
fn an_unmapped_validated_index_is_refused_without_selection() {
    let (mut fx, _, inflight) = prepared_dialog_inflight();
    let base = ledger(&fx);

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &fx.driver,
                START + SUPERVISOR_POLL_S,
                &base,
                inflight,
                select(99, "not in the captured menu", "model drift"),
            )
            .unwrap(),
    );

    assert_capability_escalation(&fx, tick, "only 2 options exist");
}

#[test]
fn an_identical_blank_reason_selection_is_refused_as_no_progress() {
    let (mut fx, _, inflight) = prepared_dialog_inflight();
    fx.sched.advise_last = Some((
        advise::text_hash(CHOICE_DIALOG_PANE),
        "option 2 — dprint".into(),
    ));
    let base = ledger(&fx);

    let tick = yielded(
        fx.sched
            .apply_dialog_verdict(
                &fx.driver,
                START + SUPERVISOR_POLL_S,
                &base,
                inflight,
                select(1, "dprint", ""),
            )
            .unwrap(),
    );

    assert_capability_escalation(&fx, tick, "same advice was already delivered");
}
