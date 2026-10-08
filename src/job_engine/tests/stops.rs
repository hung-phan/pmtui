//! Parking on a human and resuming: the `Blocked` round trip, an answer that unblocks
//! (and a stale one that does not), and the in-pane dialog that escalates once.

use super::*;
use crate::pmstate::StopStatus;

// --- Blocked / escalation preserved -------------------------------------

#[test]
fn blocked_reemits_escalated_silently_without_nudging() {
    let mut fx = blocked_fx();
    assert!(matches!(fx.sched.run, JobRun::Blocked { .. }));
    let ids = match fx.sched.tick(&fx.driver, &fx.clock).unwrap() {
        JobTick::Escalated(ids) => ids,
        other => panic!("expected Escalated, got {other:?}"),
    };
    assert_eq!(ids, vec!["stop-x".to_string()]);
    // Re-emitted every parked tick; never launches or nudges.
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(ids)
    );
    assert!(fx.driver.launched().is_empty());
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn answer_unblocks_and_nudges_the_live_session_with_the_answer() {
    let mut fx = blocked_fx();
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true); // the persistent session survived the wait
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.set(START + 5);
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: "stop-x".into(),
            answer: "use postgres".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: START + 5,
        },
    )
    .unwrap();
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    let sent = fx.driver.sent_keys();
    assert_eq!(sent.len(), 1, "resume nudges the live session once");
    assert_eq!(sent[0].0, sess);
    assert!(
        sent[0].1.contains("use postgres"),
        "answer fed into the nudge"
    );
    assert!(
        fx.driver.launched().is_empty(),
        "session already alive ⇒ no relaunch"
    );
    assert!(ledger(&fx).open_stops.is_empty(), "resolved stop cleared");
}

#[test]
fn latest_answer_wins_when_a_stop_was_answered_more_than_once() {
    let mut fx = blocked_fx();
    let session = loop_session(&fx);
    fx.driver.set_alive(&session, true);
    fx.driver.set_tail(&session, IDLE_PANE);
    for (answer, at) in [("old answer", START + 1), ("corrected answer", START + 2)] {
        state::append_answer(
            &fx.paths,
            &Answer {
                stop_id: "stop-x".into(),
                answer: answer.into(),
                note: None,
                answered_by: "user".into(),
                answered_at: at,
            },
        )
        .unwrap();
    }
    fx.clock.set(START + 2);

    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    let sent = &fx.driver.sent_keys()[0].1;
    assert!(sent.contains("corrected answer"), "{sent}");
    assert!(!sent.contains("old answer"), "{sent}");
}

#[test]
fn multiple_stops_become_answerable_one_at_a_time_without_losing_answers() {
    let mut fx = blocked_fx();
    let mut current = ledger(&fx);
    current.open_stops = vec![
        open_stop(
            "stop-a".into(),
            StopKind::Capability,
            None,
            "First decision?",
            &[],
            START,
        ),
        open_stop(
            "stop-b".into(),
            StopKind::Capability,
            None,
            "Second decision?",
            &[],
            START,
        ),
    ];
    current.run = JobRun::Blocked {
        stop_ids: vec!["stop-a".into(), "stop-b".into()],
        since: START,
    };
    job::save(&fx.paths, &current).unwrap();
    fx.sched.run = current.run;
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: "stop-a".into(),
            answer: "first answer".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: START + 1,
        },
    )
    .unwrap();
    fx.clock.set(START + 1);

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec!["stop-b".into()])
    );
    let partial = ledger(&fx);
    assert_eq!(partial.open_stops.len(), 1);
    assert_eq!(partial.open_stops[0].id, "stop-b");
    assert!(
        partial
            .pending_context
            .as_deref()
            .is_some_and(|text| text.contains("first answer"))
    );

    let session = loop_session(&fx);
    fx.driver.set_alive(&session, true);
    fx.driver.set_tail(&session, IDLE_PANE);
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: "stop-b".into(),
            answer: "second answer".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: START + 2,
        },
    )
    .unwrap();
    fx.clock.set(START + 2);

    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    let sent = &fx.driver.sent_keys()[0].1;
    assert!(sent.contains("first answer"), "{sent}");
    assert!(sent.contains("second answer"), "{sent}");
}

#[test]
fn a_human_answer_lands_on_the_autopilot_feed_as_you_answered() {
    // The human's half of the loop, on the feed: unblocking a parked session records
    // `Answered` so the feed doesn't read as pmd talking to itself (needs you → nudged).
    let mut fx = blocked_fx();
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.set(START + 5);
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: "stop-x".into(),
            answer: "use postgres".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: START + 5,
        },
    )
    .unwrap();
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        ledger(&fx).events.iter().any(|e| matches!(
            &e.kind,
            job::AutopilotEventKind::Answered(Some(a)) if a.contains("use postgres")
        )),
        "the human's answer is on the feed: {:?}",
        ledger(&fx).events
    );
}

#[test]
fn stale_answer_does_not_unblock() {
    let mut fx = blocked_fx();
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    // An answer BEFORE the park (stale) must not resolve the fresh question.
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: "stop-x".into(),
            answer: "old".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: START - 500,
        },
    )
    .unwrap();
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn chat_active_defers_the_blocked_answer_resume() {
    let mut fx = blocked_fx();
    let sess = loop_session(&fx);
    let chat = chat_session(&fx);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.set(START + 5);
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: "stop-x".into(),
            answer: "use postgres".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: START + 5,
        },
    )
    .unwrap();
    // Human chatting ⇒ the answer's nudge would collide: defer, consume nothing.
    crate::chat_lock::mark(&fx.paths, 1, &chat, "test", START + 5).unwrap();
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert!(fx.driver.sent_keys().is_empty(), "no nudge while chatting");
    assert!(
        matches!(fx.sched.run, JobRun::Blocked { .. }),
        "stays Blocked so the answer still applies after the human leaves"
    );
    // Detach ⇒ the answer's resume nudge finally fires.
    crate::chat_lock::clear(&fx.paths);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert!(fx.driver.sent_keys()[0].1.contains("use postgres"));
}

#[test]
fn transient_send_error_never_destroys_the_humans_answer() {
    // THE BUG (m18/1): `on_blocked` used to hand the human's answer to `nudge` as a
    // by-value `extra` argument while persisting a `base` that did NOT carry it. A
    // TRANSIENT `send_keys` error then re-parked `persist_run(base, …)` — flipping the
    // ledger out of `Blocked` with the stops already cleared and `pending_context`
    // still `None`. `tick` only routes to `on_blocked` from `Blocked`, so the answer
    // could never be re-read: one dropped keystroke silently ate a human decision.
    //
    // The fix makes `pending_context` the ONE durable carrier, which is what this
    // asserts: after a failed send the answer is ON DISK, and it is delivered intact
    // by the next tick that actually succeeds.
    let mut fx = blocked_fx();
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.driver.fail_send_keys(&sess, true); // the transient tmux failure
    fx.clock.set(START + 5);
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: "stop-x".into(),
            answer: "use postgres".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: START + 5,
        },
    )
    .unwrap();
    // The resume tries to type the answer, the send fails, the tick re-parks.
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + 5 + BUSY_RECHECK_S
        },
        "a transient send error re-parks a short recheck"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "nothing reached the pane — that is the premise of this test"
    );
    // THE ASSERTION: the answer survived the failed send, durably.
    assert_eq!(
        ledger(&fx).pending_context.as_deref(),
        Some(
            "The human resolved these — treat them as binding and continue:\n- stop-x: use postgres"
        ),
        "a dropped keystroke must not destroy a human answer"
    );
    // And it is actually delivered once tmux recovers (two confirmed Idle
    // observations later — the failed send deliberately consumed no confirmation).
    fx.driver.fail_send_keys(&sess, false);
    fx.clock.set(START + 5 + BUSY_RECHECK_S);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    let sent = fx.driver.sent_keys();
    assert_eq!(sent.len(), 1, "delivered exactly once");
    assert!(
        sent[0].1.contains("use postgres"),
        "the answer reaches the agent verbatim: {}",
        sent[0].1
    );
    // The invariant, machine-checked: `pending_context` is cleared ONLY by a tick that
    // actually delivered it. It was `Some` across every non-delivering tick above and
    // is `None` only now, after the send that succeeded.
    assert!(
        ledger(&fx).pending_context.is_none(),
        "a delivered answer is consumed exactly once"
    );
}

#[test]
fn pending_context_is_cleared_only_by_a_tick_that_delivered_it() {
    // The same invariant from the other side, over the paths that do NOT deliver:
    // a busy pane, an unconfirmed Idle, and an attached human must all leave a parked
    // `pending_context` untouched. (`deny_nudge_consumes_nothing` covers the attached
    // case for the run/wake state; this one is specifically about the answer payload
    // surviving every non-delivering disposition.)
    let mut fx = setup_with(Tier::Standard, Engine::Claude, Some(300), |l| {
        l.conversation_id = Some("convo-1".into());
        l.run = JobRun::Monitoring { until: START };
        l.pending_context = Some("carry me".into());
    });
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    // (a) Busy pane: re-park, no delivery.
    fx.driver.set_tail(&sess, BUSY_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).pending_context.as_deref(), Some("carry me"));
    // (b) First (unconfirmed) Idle: arms the gate only, no delivery.
    fx.clock.set(START + BUSY_RECHECK_S);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).pending_context.as_deref(), Some("carry me"));
    assert!(fx.driver.sent_keys().is_empty());
    // (c) A human attaches on the confirming tick: defer, no delivery.
    fx.clock.set(START + 2 * BUSY_RECHECK_S);
    fx.driver.set_clients(&sess, true);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).pending_context.as_deref(), Some("carry me"));
    assert!(fx.driver.sent_keys().is_empty());
    // (d) They leave: the next confirmed nudge delivers it, and only THEN is it gone.
    fx.driver.set_clients(&sess, false);
    fx.clock.set(START + 3 * BUSY_RECHECK_S);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    assert!(fx.driver.sent_keys()[0].1.contains("carry me"));
    assert!(ledger(&fx).pending_context.is_none());
}

// --- blocking dialog detection -------------------------------------------

/// The salient tail of the real `claude --permission-mode default` capture (the
/// full trimmed pane lives in `tmux`'s `FIXTURE_REAL_DIALOG_PERMISSION`; this is
/// just enough of it to drive the engine).
const DIALOG_PANE: &str = concat!(
    "● Write(hello.txt)\n",
    "\n",
    " Do you want to create hello.txt?\n",
    " ❯ 1. Yes\n",
    "   2. Yes, allow all edits during this session (shift+tab)\n",
    "   3. No\n",
    "\n",
    " Esc to cancel · Tab to amend\n",
);

#[test]
fn an_autopilot_goal_choice_starts_a_consult_without_selecting_or_escalating() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_dialog_consult(&mut fx, &sess);
    assert_eq!(seq, 1);
    assert_eq!(fx.driver.spawn_count(), 1);
    let l = ledger(&fx);
    assert!(!matches!(l.run, JobRun::Blocked { .. }));
    assert!(l.open_stops.is_empty());
    assert!(l.pending_context.is_none());
}

#[test]
fn a_standard_goal_choice_never_starts_a_consult_or_selects() {
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, CHOICE_DIALOG_PANE);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    assert_eq!(fx.driver.spawn_count(), 0);
    assert!(fx.driver.selected_dialog_options().is_empty());
}

#[test]
fn a_choice_naming_a_hard_floor_action_escalates_without_a_consult() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(
        &sess,
        concat!(
            " Which cleanup should I perform?\n",
            " ❯ 1. Delete the generated directory\n",
            "   2. Keep it\n",
            "\n",
            " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
        ),
    );
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    assert_eq!(fx.driver.spawn_count(), 0);
    assert!(fx.driver.selected_dialog_options().is_empty());
}

#[test]
fn an_authority_grant_choice_is_forced_hard_before_the_decider() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(&fx, "Keep local tests moving.");
    fx.driver.set_tail(
        &sess,
        concat!(
            " Grant AdministratorAccess to the test role?\n",
            " ❯ 1. Yes\n",
            "   2. No\n",
            " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
        ),
    );
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    assert_eq!(fx.driver.spawn_count(), 0);
    assert!(fx.driver.applied_dialog_selections().is_empty());
    let current = ledger(&fx);
    assert_eq!(current.decider_runs.len(), 1);
    let audit = &current.decider_runs[0];
    assert_eq!(audit.target, job::DeciderTarget::Dialog);
    assert_eq!(audit.finished_at, Some(START));
    assert!(matches!(
        &audit.outcome,
        job::DeciderOutcome::Skipped {
            reason,
            reported_kind: None,
            effect: None,
        } if reason.contains("terminal authority guard matched `grant`")
    ));
    let id = parked_dialog_id(&fx);
    assert!(
        !ledger(&fx).open_stops[0].dashboard_answerable_dialog(),
        "a hard-floor authority prompt must remain attach-only"
    );
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "Yes", fx.clock.now());
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec![id])
    );
    assert!(fx.driver.applied_dialog_selections().is_empty());
}

#[test]
fn a_choice_without_an_available_decider_escalates_instead_of_guessing() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, CHOICE_DIALOG_PANE);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    assert_eq!(fx.driver.spawn_count(), 0);
    assert!(fx.driver.selected_dialog_options().is_empty());
    let current = ledger(&fx);
    assert_eq!(current.decider_runs[0].policy.kind, StopKind::Ambiguity);
    assert_eq!(
        current.decider_runs[0].policy.effective_risk,
        crate::state::RiskClass::Medium
    );
}

#[test]
fn a_choice_without_a_goal_escalates_because_the_decider_has_no_mandate() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, CHOICE_DIALOG_PANE);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    assert_eq!(fx.driver.spawn_count(), 0);
    assert!(fx.driver.selected_dialog_options().is_empty());
}

#[test]
fn a_choice_with_fewer_than_two_concrete_options_escalates() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(&fx, "Choose a concrete test strategy.");
    fx.driver.set_tail(
        &sess,
        concat!(
            " Which test strategy should I use?\n",
            " ❯ 1. Unit only\n",
            "   2. Type something.\n",
            "\n",
            " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
        ),
    );
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    assert_eq!(fx.driver.spawn_count(), 0);
}

#[test]
fn dialog_pane_escalates_on_the_first_tick_carrying_the_question_and_options() {
    // The bug this closes: this pane classifies Busy, so before the dialog check the
    // harness re-parked BUSY_RECHECK_S forever and said nothing for 30 minutes.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, DIALOG_PANE);
    assert_eq!(
        tmux::classify_pane(DIALOG_PANE),
        PaneActivity::Busy,
        "sanity: without the dialog check this pane is just 'Busy'"
    );
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let JobTick::Escalated(ids) = &out else {
        panic!("dialog should escalate: {out:?}");
    };
    let id = ids[0].clone();
    assert!(
        id.starts_with(&format!("stop-{SESSION_ID}-dialog-{START}-")),
        "dialog stop ids carry a nonce: {id}"
    );
    assert_eq!(
        out,
        JobTick::Escalated(vec![id.clone()]),
        "a dialog escalates on the FIRST tick, not after the stall bound"
    );
    let l = ledger(&fx);
    assert_eq!(
        l.run,
        JobRun::Blocked {
            stop_ids: vec![id.clone()],
            since: START
        }
    );
    assert_eq!(l.open_stops.len(), 1);
    assert_eq!(l.open_stops[0].id, id);
    // Autopilot is the tier that auto-flows Medium, so this pins the kind choice:
    // `Capability` is forced Hard and escalates even here. A `Medium` kind
    // (Ambiguity/ExpertNeeded) would have auto-flowed — i.e. silently swallowed a
    // permission prompt only a human may answer.
    assert_eq!(l.open_stops[0].kind, StopKind::Capability);
    assert!(
        l.open_stops[0].pane_dialog.is_some(),
        "the persisted stop must retain its pane-dialog origin"
    );
    assert!(
        !l.open_stops[0].dashboard_answerable_dialog(),
        "permission prompts remain attach-only"
    );
    assert_eq!(
        policy::decide_kind(
            Tier::Autopilot,
            l.open_stops[0].kind,
            crate::state::RiskClass::Low
        ),
        Decision::Escalate,
        "the dialog kind must never auto-flow, on any tier"
    );
    // The human must see the actual choice, not just "capability".
    assert_eq!(
        l.open_stops[0].question.as_deref(),
        Some("Do you want to create hello.txt?")
    );
    assert_eq!(
        l.open_stops[0].options,
        vec![
            "Yes".to_string(),
            "Yes, allow all edits during this session (shift+tab)".to_string(),
            "No".to_string(),
        ]
    );
    // ...and the status text tells them how to answer it.
    let status = l.last_status.clone().unwrap_or_default();
    assert!(
        status.contains("attach"),
        "status must say attach: {status}"
    );
    // This is a permission prompt, not a goal-choice UI: it remains human-only.
    assert!(
        fx.driver.sent_keys().is_empty(),
        "permission prompts must not send keys / select an option"
    );
}

fn answer_dialog(fx: &Fx, stop_id: &str, answer: &str, now: Epoch) {
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: stop_id.to_string(),
            answer: answer.to_string(),
            note: None,
            answered_by: "user".into(),
            answered_at: now,
        },
    )
    .unwrap();
}

fn parked_dialog_id(fx: &Fx) -> String {
    ledger(fx)
        .open_stops
        .first()
        .expect("dialog stop")
        .id
        .clone()
}

#[test]
fn a_human_answer_to_a_radio_dialog_selects_the_live_option_without_nudging_prose() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |ledger| {
        ledger.continuations = 3;
        ledger.stale_plan_streak = 4;
        ledger.marker_less_rechecks = 5;
    });
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, CHOICE_DIALOG_PANE);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    let id = parked_dialog_id(&fx);
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "dprint", fx.clock.now());

    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        fx.driver.applied_dialog_selections(),
        vec![(sess, vec![1])],
        "the human's radio choice is applied by option index"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a pane-dialog answer must not be pasted as prose"
    );
    let l = ledger(&fx);
    assert!(l.open_stops.is_empty());
    assert!(l.pending_context.is_none());
    assert_eq!(l.continuations, 0);
    assert_eq!(l.stale_plan_streak, 0);
    assert_eq!(l.marker_less_rechecks, 0);
}

#[test]
fn a_dialog_answer_waits_for_an_attached_human_to_leave() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, CHOICE_DIALOG_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let id = parked_dialog_id(&fx);
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "dprint", fx.clock.now());
    fx.driver.set_clients(&sess, true);

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec![id])
    );
    assert!(fx.driver.applied_dialog_selections().is_empty());
    assert_eq!(ledger(&fx).open_stops[0].status, StopStatus::AwaitingReply);

    fx.driver.set_clients(&sess, false);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.driver.applied_dialog_selections(), vec![(sess, vec![1])]);
}

#[test]
fn a_human_answer_to_a_checkbox_dialog_toggles_the_choice_and_submits() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, MULTI_CHOICE_DIALOG_PANE);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    let id = parked_dialog_id(&fx);
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "integration", fx.clock.now());

    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        fx.driver.applied_dialog_selections(),
        vec![(sess, vec![1])],
        "the human's checkbox choice is applied and the driver owns Submit"
    );
    assert!(fx.driver.sent_keys().is_empty());
    assert!(ledger(&fx).open_stops.is_empty());
}

#[test]
fn a_human_answer_is_not_applied_to_a_changed_dialog() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, CHOICE_DIALOG_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let id = parked_dialog_id(&fx);
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "dprint", fx.clock.now());
    fx.driver.set_tail(&sess, CHANGED_CHOICE_DIALOG_PANE);

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: START + 1 }
    );
    assert!(fx.driver.applied_dialog_selections().is_empty());
    let l = ledger(&fx);
    assert!(l.open_stops.is_empty());
    assert!(
        l.last_status
            .as_deref()
            .is_some_and(|status| status.contains("prompt changed")),
        "{:?}",
        l.last_status
    );
}

#[test]
fn a_human_answer_is_not_applied_when_only_dialog_context_changed() {
    let first = concat!(
        " Which deploy target should I use?\n",
        " Account: test\n",
        " ❯ 1. alpha\n",
        "   2. beta\n",
        " Enter to select · Esc to cancel\n",
    );
    let second = concat!(
        " Which deploy target should I use?\n",
        " Account: production\n",
        " ❯ 1. alpha\n",
        "   2. beta\n",
        " Enter to select · Esc to cancel\n",
    );
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, first);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let id = parked_dialog_id(&fx);
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "beta", fx.clock.now());
    fx.driver.set_tail(&sess, second);

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: START + 1 }
    );
    assert!(fx.driver.applied_dialog_selections().is_empty());
}

#[test]
fn a_failed_human_dialog_selection_is_held_without_replaying() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, CHOICE_DIALOG_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let id = parked_dialog_id(&fx);
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "dprint", fx.clock.now());
    fx.driver.fail_select_dialog(&sess, true);

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec![id.clone()])
    );
    let l = ledger(&fx);
    assert_eq!(l.open_stops.len(), 1);
    assert_eq!(l.open_stops[0].id, id);
    assert_eq!(l.open_stops[0].status, StopStatus::Held);
    assert!(matches!(l.run, JobRun::Blocked { .. }));

    fx.driver.fail_select_dialog(&sess, false);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec![id])
    );
    assert!(
        fx.driver.applied_dialog_selections().is_empty(),
        "a held answer must never be replayed"
    );

    fx.driver.set_tail(&sess, CHANGED_CHOICE_DIALOG_PANE);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: START + 1 }
    );
    assert!(ledger(&fx).open_stops.is_empty());
}

#[test]
fn a_dashboard_answer_never_selects_a_permission_dialog() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, DIALOG_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let id = parked_dialog_id(&fx);
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "No", fx.clock.now());

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec![id])
    );
    assert!(fx.driver.applied_dialog_selections().is_empty());
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn a_meta_choice_stays_attach_only() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, CHOICE_DIALOG_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let id = parked_dialog_id(&fx);
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "Type something.", fx.clock.now());

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec![id])
    );
    assert!(fx.driver.applied_dialog_selections().is_empty());
}

#[test]
fn a_prechecked_checkbox_dialog_stays_attach_only() {
    let pane = concat!(
        " Which test layers should I run?\n",
        " ❯ 1. [✔] unit\n",
        "   2. [ ] integration\n",
        "   3. [ ] Type something\n",
        "      Submit\n",
        " Enter to select · Esc to cancel\n",
    );
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, pane);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let id = parked_dialog_id(&fx);
    assert!(!ledger(&fx).open_stops[0].dashboard_answerable_dialog());
    fx.clock.set(START + 1);
    answer_dialog(&fx, &id, "integration", fx.clock.now());

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec![id])
    );
    assert!(fx.driver.applied_dialog_selections().is_empty());
}

#[test]
fn a_dialog_answered_directly_in_tmux_clears_after_detach() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, DIALOG_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    fx.driver.set_tail(&sess, IDLE_PANE);

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: START }
    );
    assert!(ledger(&fx).open_stops.is_empty());
}

#[test]
fn a_legacy_dialog_stop_reconciles_by_question_and_options() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, CHOICE_DIALOG_PANE);
    let dialog = tmux::classify_dialog(CHOICE_DIALOG_PANE).unwrap();
    let id = format!("stop-{SESSION_ID}-dialog-{START}");
    let stop = open_stop(
        id.clone(),
        StopKind::Capability,
        None,
        &dialog.question,
        &dialog.options,
        START,
    );
    let mut current = ledger(&fx);
    current.open_stops = vec![stop];
    current.run = JobRun::Blocked {
        stop_ids: vec![id.clone()],
        since: START,
    };
    job::save(&fx.paths, &current).unwrap();
    fx.sched.run = current.run;

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec![id])
    );
    assert!(
        ledger(&fx).open_stops[0].pane_dialog.is_none(),
        "the test exercises a pre-snapshot stop"
    );
}

#[test]
fn dialog_stop_ids_do_not_reuse_an_answer_within_the_same_second() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, DIALOG_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let first = parked_dialog_id(&fx);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    fx.driver.set_tail(&sess, DIALOG_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let second = parked_dialog_id(&fx);

    assert_ne!(first, second, "dialog stop ids need a per-stop nonce");
}

#[test]
fn synthetic_capability_stop_ids_are_unique_within_the_same_second() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let base = ledger(&fx);
    let options = vec!["continue".to_string(), "hold".to_string()];
    let mut park = || {
        let JobTick::Escalated(ids) = fx
            .sched
            .park_capability_stop(
                START,
                &base,
                CapabilityStop {
                    id_tag: "advice",
                    dialog: None,
                    question: "What next?",
                    options: &options,
                    status: "needs a decision".into(),
                },
            )
            .unwrap()
        else {
            panic!("capability stop should escalate");
        };
        ids[0].clone()
    };

    assert_ne!(park(), park());
}

#[test]
fn synthetic_stuck_stop_ids_are_unique_within_the_same_second() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let base = ledger(&fx);
    fx.sched
        .park_stuck(START, &base, "first stall".into())
        .unwrap();
    let first = ledger(&fx).open_stops[0].id.clone();
    fx.sched
        .park_stuck(START, &base, "second stall".into())
        .unwrap();
    let second = ledger(&fx).open_stops[0].id.clone();

    assert_ne!(first, second);
}

#[test]
fn a_second_tick_on_the_same_dialog_neither_duplicates_the_stop_nor_renotifies() {
    // Notify-once + idempotence, inherited from the `Blocked` arm rather than new
    // state: the park routes every later tick to `on_blocked`, which re-emits the
    // SAME id (the daemon's id-keyed dedup then stays silent) and never re-classifies
    // the pane, so no second stop can be minted.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, DIALOG_PANE);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    let id = parked_dialog_id(&fx);
    // Later ticks, dialog still up, clock advanced (a fresh park would mint a
    // DIFFERENT id — asserting the same id is what proves no re-park happened).
    for t in [START + BUSY_RECHECK_S, START + 600] {
        fx.clock.set(t);
        assert_eq!(
            fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
            JobTick::Escalated(vec![id.clone()]),
            "re-emits the same id ⇒ the daemon dedups ⇒ no re-notify"
        );
        let l = ledger(&fx);
        assert_eq!(l.open_stops.len(), 1, "stop not duplicated");
        assert_eq!(l.open_stops[0].id, id, "stop not re-minted");
        assert_eq!(
            l.run,
            JobRun::Blocked {
                stop_ids: vec![id.clone()],
                since: START
            },
            "the park is not re-based"
        );
    }
    assert!(fx.driver.sent_keys().is_empty(), "still never auto-answers");
}

#[test]
fn a_dialog_escalation_cannot_also_trip_the_bogus_wedged_stall() {
    // The stall backstop must not pile a misleading "busy with no progress" Stuck on
    // top of a session already escalated on a dialog: the window is closed at the
    // park, and the `Blocked` run never reaches the busy recheck again.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    // Busy first, so a stall window is genuinely open before the dialog appears.
    fx.driver.set_tail(&sess, BUSY_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.sched.busy_since, Some(START));
    fx.driver.set_tail(&sess, DIALOG_PANE);
    fx.clock.set(START + BUSY_RECHECK_S);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert_eq!(
        fx.sched.busy_since, None,
        "the dialog park closes the stall window"
    );
    // Far past the stall bound: still the same single dialog escalation, never Stuck.
    fx.clock.set(START + DEFAULT_STALL_BUSY_S as i64 * 2);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Escalated(_)),
        "must not become a bogus wedged Stuck: {out:?}"
    );
    assert_eq!(ledger(&fx).open_stops.len(), 1);
    assert_eq!(ledger(&fx).open_stops[0].kind, StopKind::Capability);
}

#[test]
fn a_dialog_does_not_escalate_while_a_human_is_attached() {
    // Precedence: the defer gate wins. Someone looking at the pane can answer the
    // dialog themselves — parking them on a decision already on their screen would be
    // noise, and `drive` must consume nothing while they are there.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, DIALOG_PANE);
    fx.driver.set_clients(&sess, true);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: START },
        "human attached ⇒ defer, no escalation"
    );
    assert!(ledger(&fx).open_stops.is_empty());
    assert!(!matches!(fx.sched.run, JobRun::Blocked { .. }));
    // Once they detach, the same pane escalates.
    fx.driver.set_clients(&sess, false);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
}

#[test]
fn a_marker_bump_still_outranks_the_dialog_check() {
    // Ordering guard: step 2.5 (the agent's own written self-assessment) keeps its
    // precedence over anything inferred from the pane. A fresh `blocked` marker
    // disposes as its OWN stop kind, not as a dialog capability stop.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, DIALOG_PANE);
    write_marker(&fx, BLOCKED_HARD);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Escalated(vec![format!("stop-{SESSION_ID}-0-1-{START}")])
    );
    let l = ledger(&fx);
    assert_eq!(l.open_stops.len(), 1);
    assert_eq!(
        l.open_stops[0].kind,
        StopKind::Publish,
        "the marker's stop wins, not the dialog's"
    );
}

// --- FO-1 / FO-2 counter resets on a human answer (Milestone D) --------------

#[test]
fn human_answer_resets_plan_staleness_in_on_blocked() {
    // A session parked Blocked with an accrued streak: a human answer that unblocks it
    // resets the streak (beside continuations), so answering "keep going" clears it.
    let mut fx = blocked_fx();
    let mut l = ledger(&fx);
    l.stale_plan_streak = 4;
    l.continuations = 3;
    job::save(&fx.paths, &l).unwrap();
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    // Answer the open stop at/after the park start.
    fx.clock.set(START + 1);
    push_answer(&fx, "stop-x", Some("go ahead"), fx.clock.now());
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let after = ledger(&fx);
    assert_eq!(
        after.stale_plan_streak, 0,
        "on_blocked resets the plan-staleness streak"
    );
    assert_eq!(
        after.continuations, 0,
        "…beside continuations (unchanged behaviour)"
    );
}

#[test]
fn human_answer_resets_the_marker_less_counter_in_on_blocked() {
    let mut fx = blocked_fx();
    let mut l = ledger(&fx);
    l.marker_less_rechecks = 2;
    job::save(&fx.paths, &l).unwrap();
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.set(START + 1);
    push_answer(&fx, "stop-x", Some("go ahead"), fx.clock.now());
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        ledger(&fx).marker_less_rechecks,
        0,
        "on_blocked resets the marker-less counter"
    );
}
