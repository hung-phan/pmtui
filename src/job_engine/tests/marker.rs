//! The marker disposer: what each `WakeReport` state does to the ledger, typed effect
//! enforcement, and the malformed/mid-write cases.

use super::*;
use crate::job::{LedgerSituation, WakeState};

#[test]
fn marker_mtime_is_not_cached_when_decider_teardown_fails() {
    let (mut fx, _session) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    let supervisor = sup_session(&fx, seq);
    let previous_revision = fx.sched.last_marker_stamp;
    write_marker(
        &fx,
        r#"{"seq":10,"state":"working","status":"new work","next_step":"continue"}"#,
    );
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(fx.paths.needs_you())
        .unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5)),
    )
    .unwrap();
    fx.driver.fail_terminate(&supervisor);
    fx.clock.set(START + SUPERVISOR_POLL_S);

    assert!(fx.sched.tick(&fx.driver, &fx.clock).is_err());
    assert_eq!(fx.sched.last_marker_stamp, previous_revision);

    fx.driver.allow_terminate(&supervisor);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(ledger(&fx).last_marker_seq, 10);
}
use crate::state::RiskClass;

// --- marker disposer (Slice 2) ------------------------------------------

#[test]
fn accepted_marker_bump_confirms_the_conversation_and_clears_the_fallback_gate() {
    // A registry seed adopted this session and left `resume_unconfirmed = true`. The
    // FIRST accepted marker bump proves the agent actually ran a turn on this
    // conversation, so the gate clears — every future relaunch then RESUMES it rather
    // than firing the one-shot create-fallback.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |l| {
        l.resume_unconfirmed = true;
    });
    // A plain progress bump (any accepted state confirms; Working is the common one).
    report_progress(&fx, 1, 5);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).last_marker_seq, 1, "the bump was accepted");
    assert!(
        !ledger(&fx).resume_unconfirmed,
        "an accepted bump confirms the cid — the conversation is now proven real"
    );
}

#[test]
fn fresh_blocked_marker_escalates_once_and_does_not_reappend_on_reobservation() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, BLOCKED_HARD);
    // First tick disposes the bump: a hard `publish` escalates on every tier.
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let id = format!("stop-{SESSION_ID}-0-1-{START}");
    assert_eq!(out, JobTick::Escalated(vec![id.clone()]));
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
    assert_eq!(l.open_stops[0].kind, StopKind::Publish);
    assert_eq!(l.last_marker_seq, 100, "watermark advanced");
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a Blocked park never nudges"
    );
    // A second tick over the SAME file re-emits Escalated (the daemon dedups) via
    // the Blocked arm and does NOT re-append the stop.
    let out2 = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(out2, JobTick::Escalated(vec![id.clone()]));
    assert_eq!(ledger(&fx).open_stops.len(), 1, "stop not re-appended");
    assert!(fx.driver.sent_keys().is_empty());
    assert_eq!(fx.driver.launched().len(), 0, "no relaunch while parked");
}

/// The per-session `config.json` tier as it stands on disk — the dial pmtui's `m` writes and
/// `daemon::entry_tier` reads, so a test asserting on it is asserting on the real switch.
fn tier_on_disk(fx: &Fx) -> Tier {
    state::read_json::<Config>(&fx.sched.paths.config())
        .expect("config readable")
        .autonomy
}

#[test]
fn a_reached_goal_pauses_the_heartbeat_and_keeps_autopilot_on() {
    // The user's design, REVERSING what m36 built here: *"When its mention need me, may be it should
    // not switch to Standard and still keep autopilot. But it will pause the heartbeat. Press a will
    // answer and re-arm the heartbeat again on next tick."*
    //
    // m36 wrote `autonomy = Standard` on a `confirm_done`, reasoning that the dial was already the
    // stop condition. It was the wrong lever: `a` writes `answers.json`, only pmd reads that, and
    // `pmd_drives_row` is false for a Standard row — so the harness raised a question and disabled
    // the key that answers it in the same tick. The park is the pause; the dial is the human's.
    //
    // All four sentences of the requirement are asserted here, in order, because the first three were
    // already true of `JobRun::Blocked` and only the tier write had to go.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    assert_eq!(tier_on_disk(&fx), Tier::Autopilot, "precondition");
    write_marker(
        &fx,
        r#"{"seq":101,"state":"blocked","status":"5 jokes delivered",
                "stops":[{"kind":"confirm_done","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"medium",
                "question":"jokes.md holds 5 jokes. Confirm and close, or keep adding?",
                "options":["Confirm and close","Keep going"]}]}"#,
    );
    // (a) NOTIFIED ONCE — the human is told, exactly as before.
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));

    // (b) THE DIAL IS UNTOUCHED. This is the reversal, and it is what makes `a` work: pmtui's
    //     `answer_reaches_the_agent` gate is `pmd_drives_row`, so a row whose tier was flipped here
    //     could not be answered from the dashboard at all.
    assert_eq!(
        tier_on_disk(&fx),
        Tier::Autopilot,
        "needing a human must not flip the autonomy dial — that is what made `a` refuse"
    );
    assert!(
        crate::daemon::pmd_drives_row(crate::registry::Mode::AgentLoop, Some(tier_on_disk(&fx))),
        "and the row must still be one pmd drives, or the answer has no deliverer"
    );

    // (c) PARKED, with the claim readable, and the agent's session alive.
    let l = ledger(&fx);
    assert!(matches!(l.run, JobRun::Blocked { .. }), "{:?}", l.run);
    assert_eq!(l.open_stops.len(), 1, "the claim must be readable");
    assert_eq!(
        l.open_stops[0].question.as_deref(),
        Some("jokes.md holds 5 jokes. Confirm and close, or keep adding?")
    );
    assert!(
        fx.driver.is_alive(&sess).unwrap_or(false),
        "parking must not kill the agent's session"
    );

    // (d) THE HEARTBEAT IS PAUSED. Sweeps keep coming (the daemon does not stop calling `tick`), so
    //     the engine itself must not nudge — the regression the user experienced as batch after
    //     batch of jokes. Note this holds with autopilot ON: `Blocked` is the pause, not the dial.
    let typed = fx.driver.sent_keys().len();
    for step in 1..=4 {
        fx.clock.set(START + step * 600);
        let _ = fx.sched.tick(&fx.driver, &fx.clock);
    }
    assert_eq!(
        fx.driver.sent_keys().len(),
        typed,
        "a session waiting on a human must not be nudged: {:?}",
        fx.driver.sent_keys()
    );

    // (e) AND `a` RE-ARMS IT. The answer pmtui writes resumes the heartbeat on the next tick and
    //     reaches the agent — the half that was unreachable while the dial got flipped.
    let id = ledger(&fx).open_stops[0].id.clone();
    let answered_at = START + 4 * 600 + 1;
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: id,
            answer: "Keep going".into(),
            note: None,
            answered_by: "user".into(),
            answered_at,
        },
    )
    .unwrap();
    fx.clock.set(answered_at);
    fx.driver.set_tail(&sess, IDLE_PANE);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    let sent = fx.driver.sent_keys();
    assert_eq!(
        sent.len(),
        typed + 1,
        "the answer re-arms exactly one nudge"
    );
    assert!(
        sent[typed].1.contains("Keep going"),
        "and the human's answer reaches the agent: {:?}",
        sent[typed].1
    );
}

#[test]
fn no_stop_kind_ever_writes_the_autonomy_dial() {
    // The dial is the HUMAN's, and since the m36 stand-down was removed no stop kind may write it.
    // Kept as a guard rather than deleted with the feature: writing the tier from the disposer is
    // exactly the mistake that made `a` refuse the question the harness had just raised, and the
    // tempting place to re-introduce it is right here, in the escalate branch.
    //
    // (1) A mid-flight question (`expert_needed`) leaves it ON — turning autopilot off there would
    //     strand a session the human expected to keep running.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_marker(
        &fx,
        r#"{"seq":101,"state":"blocked","stops":[{"kind":"expert_needed","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"hard",
                "question":"Which formatter?","options":["prettier","dprint"]}]}"#,
    );
    let _ = fx.sched.tick(&fx.driver, &fx.clock);
    assert_eq!(
        tier_on_disk(&fx),
        Tier::Autopilot,
        "a mid-flight question must leave autopilot ON"
    );

    // (2) And a COMPLETION claim (`confirm_done`) leaves it wherever it was — here Standard, which
    //     also proves the disposer is not writing the dial "back" to a value it already holds.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(
        &fx,
        r#"{"seq":101,"state":"blocked","stops":[{"kind":"confirm_done","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"medium",
                "question":"Done?","options":["yes","no"]}]}"#,
    );
    let _ = fx.sched.tick(&fx.driver, &fx.clock);
    assert_eq!(tier_on_disk(&fx), Tier::Standard);
}

#[test]
fn blocked_marker_question_and_options_reach_the_ledger() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |ledger| {
        ledger.start_turn(
            START - 10,
            crate::job::TurnTrigger::Heartbeat {
                pending_context: false,
                marker_recovery: false,
            },
        );
    });
    write_marker(
        &fx,
        r#"{"seq":101,"state":"blocked","stops":[{"kind":"confirm_done","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"medium",
                "question":"  jokes.md holds 5 jokes. Confirm and close, or keep adding?  ",
                "options":["Confirm and close","Keep going","Revise some jokes"]}]}"#,
    );
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    let mut persisted = ledger(&fx);
    assert!(matches!(
        persisted.turn_trace.last().map(|turn| &turn.outcome),
        Some(crate::job::TurnOutcome::Reported {
            disposition: crate::job::TurnDisposition::Escalated,
            marker_seq: 101,
            ..
        })
    ));
    let stop = persisted.open_stops.remove(0);
    assert_eq!(
        stop.question.as_deref(),
        Some("jokes.md holds 5 jokes. Confirm and close, or keep adding?"),
        "question persisted verbatim (trimmed)"
    );
    assert_eq!(
        stop.options,
        vec![
            "Confirm and close".to_string(),
            "Keep going".into(),
            "Revise some jokes".into()
        ]
    );
}

#[test]
fn blocked_marker_without_a_question_persists_none() {
    // `StopDraft.question` defaults to "" when the agent omits it; that must land
    // as `None`, so every reader can treat `Some` as "there is text to show".
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(
        &fx,
        r#"{"seq":101,"state":"blocked","stops":[{"kind":"publish","effect":{"scope":"external","reversibility":"irreversible","authority":"ordinary"},"risk_class":"low","question":"  "}]}"#,
    );
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    let stop = ledger(&fx).open_stops.remove(0);
    assert!(
        stop.question.is_none(),
        "blank question ⇒ None, not Some(\"\")"
    );
    assert!(stop.options.is_empty());
}

#[test]
fn pmd_owned_report_identity_accepts_new_content_after_a_future_worker_seq() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_marker(
        &fx,
        r#"{"seq":1790140600,"state":"monitoring","status":"research complete","next_check_s":300}"#,
    );
    backdate_marker(&fx, 2);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let poisoned = ledger(&fx);
    assert_eq!(poisoned.last_marker_seq, 1_790_140_600);
    assert_eq!(poisoned.report_generation, 1);
    assert!(poisoned.last_marker_revision.is_some());

    let recovered = r#"{"seq":1789845066,"state":"monitoring","status":"full gates passed","next_check_s":300}"#;
    write_marker(&fx, recovered);
    backdate_marker(&fx, 2);
    fx.clock.advance(BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let recovered_state = ledger(&fx);
    assert_eq!(
        recovered_state.report_generation, 2,
        "pmd ordering advances even though the worker's diagnostic seq moved backward"
    );
    assert_eq!(recovered_state.last_marker_seq, 1_789_845_066);
    assert_eq!(
        recovered_state.last_status.as_deref(),
        Some("full gates passed")
    );

    fx.sched.last_marker_stamp = None;
    fx.clock.advance(BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        ledger(&fx).report_generation,
        2,
        "persisted file identity prevents replay after a daemon restart loses its file cache"
    );
}

#[test]
fn legacy_future_sequence_poison_recovers_on_the_next_distinct_marker() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |ledger| {
        ledger.last_marker_seq = 1_790_140_600;
        ledger.nudged_at_seq = Some(1_790_140_600);
        ledger.digest.disposed = 437;
        ledger.situation = Some(LedgerSituation {
            state: WakeState::Working,
            status: Some("old accepted report".into()),
            open_stops: Vec::new(),
            seq: 1_790_140_600,
            at: START - 100,
        });
    });
    write_marker(
        &fx,
        r#"{"seq":1789845066,"state":"monitoring","status":"current report","next_check_s":300}"#,
    );
    backdate_marker(&fx, 2);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    let state = ledger(&fx);
    assert_eq!(state.last_marker_seq, 1_789_845_066);
    assert_eq!(
        state.report_generation, 438,
        "the first pmd generation continues from the existing disposed-report count"
    );
    assert_eq!(state.digest.disposed, 438);
    assert!(state.last_marker_revision.is_some());
    assert!(!state.awaiting_report());
    assert_eq!(state.nudged_at_report_generation, Some(437));
}

#[test]
fn legacy_equal_worker_seq_with_changed_status_is_not_dropped() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |ledger| {
        ledger.last_marker_seq = 42;
        ledger.digest.disposed = 3;
        ledger.last_status = Some("old status".into());
        ledger.last_plan = Some("old plan".into());
        ledger.situation = Some(LedgerSituation {
            state: WakeState::Working,
            status: Some("old status".into()),
            open_stops: Vec::new(),
            seq: 42,
            at: START - 100,
        });
    });
    write_marker(
        &fx,
        r#"{"seq":42,"state":"working","status":"new status","next_step":"new plan"}"#,
    );
    backdate_marker(&fx, 2);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    let state = ledger(&fx);
    assert_eq!(state.report_generation, 4);
    assert_eq!(state.last_status.as_deref(), Some("new status"));
    assert_eq!(state.last_plan.as_deref(), Some("new plan"));
}

#[test]
fn report_generation_not_worker_seq_controls_outstanding_nudge_debt() {
    let mut state = AgentLoopState::fresh(Engine::Codex, Some(300), START);
    state.last_marker_seq = 1_790_140_600;
    state.report_generation = 7;
    state.nudged_at_seq = Some(1_790_140_600);
    state.nudged_at_report_generation = Some(7);
    assert!(state.awaiting_report());

    state.last_marker_seq = 1_789_845_066;
    assert!(
        state.awaiting_report(),
        "a backward audit timestamp cannot clear report debt"
    );
    state.report_generation = 8;
    assert!(
        !state.awaiting_report(),
        "one accepted pmd generation clears the outstanding nudge"
    );
}

#[test]
fn malformed_marker_fresh_is_a_recheck_not_a_stall() {
    // OQ3 hardening: a parse error whose file was JUST written (fresh mtime) is a
    // plausibly-mid-write read → recheck at BUSY_RECHECK_S WITHOUT bumping the stall
    // counter (and without recording the mtime, so it is re-read next tick).
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, "{ not json"); // fresh real mtime
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        }
    );
    assert_eq!(
        ledger(&fx).continuations,
        0,
        "a fresh (mid-write) malformed marker is not counted"
    );
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn malformed_marker_counts_after_grace_and_stalls_at_threshold() {
    // A settled (not-fresh) malformed marker is counted once per NEW mtime; three
    // (stuck_threshold) counted observations raise a Stuck escalation.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {}); // stuck_threshold = 3
    write_marker(&fx, "{ not json");
    backdate_marker(&fx, 30);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        ledger(&fx).continuations,
        1,
        "settled malformed counts once"
    );
    // The SAME file (unchanged mtime) is not double-counted within a sweep.
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(ledger(&fx).continuations, 1, "unchanged mtime is skipped");
    // A NEW settled malformed write counts again → 2.
    write_marker(&fx, "{ still not json");
    backdate_marker(&fx, 20);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(ledger(&fx).continuations, 2);
    // The third counted observation hits stuck_threshold → Stuck / Blocked.
    write_marker(&fx, "{ nope");
    backdate_marker(&fx, 10);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Stuck(_)
    ));
    assert!(matches!(ledger(&fx).run, JobRun::Blocked { .. }));
    assert_eq!(ledger(&fx).continuations, 3);
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a stall parks, never nudges"
    );
}

#[test]
fn monitoring_marker_parks_the_self_scheduled_nap_without_nudging() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |ledger| {
        ledger.start_turn(
            START - 10,
            crate::job::TurnTrigger::Heartbeat {
                pending_context: false,
                marker_recovery: false,
            },
        );
    });
    write_marker(&fx, r#"{"seq":5,"state":"monitoring","next_check_s":900}"#);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(out, JobTick::Monitoring { until: START + 900 });
    let l = ledger(&fx);
    assert_eq!(l.run, JobRun::Monitoring { until: START + 900 });
    assert_eq!(l.continuations, 0);
    assert_eq!(l.last_marker_seq, 5);
    assert!(
        fx.driver.sent_keys().is_empty(),
        "monitoring skips the nudge this tick"
    );
    assert!(matches!(
        l.turn_trace.last().map(|turn| &turn.outcome),
        Some(crate::job::TurnOutcome::Reported {
            disposition: crate::job::TurnDisposition::Monitoring,
            marker_seq: 5,
            ..
        })
    ));
}

#[test]
fn monitoring_marker_clamps_an_over_long_self_scheduled_nap() {
    // `next_check_s` is worker-supplied and was UNBOUNDED: a report of
    // `next_check_s: 999_999_999` parked the session ~31 years out — a silent
    // self-eviction from the fleet no human asked for. It is now clamped to the same
    // ceiling `cadence_s` is held to (`CADENCE_MAX_S`).
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(
        &fx,
        r#"{"seq":6,"state":"monitoring","next_check_s":999999999}"#,
    );
    let max = crate::job_engine::CADENCE_MAX_S as i64;
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(out, JobTick::Monitoring { until: START + max });
    assert_eq!(ledger(&fx).run, JobRun::Monitoring { until: START + max });
}

#[test]
fn monitoring_marker_clamps_a_sub_minute_self_scheduled_nap_to_the_floor() {
    // The OTHER end of the same clamp: `next_check_s: 5` used to park the session ~5s out, so it
    // expired almost immediately, re-nudged, and re-reported the same nap — the sub-minute nudge
    // storm `CADENCE_MIN_S` exists to forbid (the same floor `cadence_s` is held to in
    // `adopt_cadence`). It is now floored, not just ceilinged.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, r#"{"seq":6,"state":"monitoring","next_check_s":5}"#);
    let min = crate::job_engine::CADENCE_MIN_S as i64;
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(out, JobTick::Monitoring { until: START + min });
    assert_eq!(ledger(&fx).run, JobRun::Monitoring { until: START + min });
}

#[test]
fn working_marker_keeps_nudging_on_cadence_and_records_status() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |ledger| {
        ledger.start_turn(
            START - 10,
            crate::job::TurnTrigger::Heartbeat {
                pending_context: false,
                marker_recovery: false,
            },
        );
    });
    write_marker(&fx, r#"{"seq":7,"state":"working","status":"indexing"}"#);
    let out = tick_confirmed(&mut fx);
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S + 300
        },
        "working falls through to the heartbeat nudge (once the idle prompt is confirmed)"
    );
    assert_eq!(fx.driver.sent_keys().len(), 1, "working nudges once");
    let l = ledger(&fx);
    assert_eq!(l.last_status.as_deref(), Some("indexing"));
    assert_eq!(l.continuations, 0);
    assert_eq!(l.last_marker_seq, 7);
    assert!(!matches!(l.run, JobRun::Blocked { .. }));
    assert!(matches!(
        l.turn_trace.first().map(|turn| &turn.outcome),
        Some(crate::job::TurnOutcome::Reported {
            disposition: crate::job::TurnDisposition::Working,
            marker_seq: 7,
            ..
        })
    ));
}

#[test]
fn auto_flow_without_a_consultable_question_escalates() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[{"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low"}]}"#,
    );
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    let l = ledger(&fx);
    assert!(matches!(l.run, JobRun::Blocked { .. }));
    assert!(l.pending_context.is_none());
    assert_eq!(l.open_stops[0].kind, StopKind::Capability);
    assert_eq!(l.last_marker_seq, 9);
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn blocked_without_a_stop_parks_stuck() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, r#"{"seq":12,"state":"blocked","stops":[]}"#);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Stuck(_)
    ));
    assert!(matches!(ledger(&fx).run, JobRun::Blocked { .. }));
    assert_eq!(ledger(&fx).last_marker_seq, 12);
}

#[test]
fn answer_past_since_resumes_a_marker_parked_blocked() {
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, BLOCKED_HARD);
    // Park Blocked via the marker.
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    let id = ledger(&fx).open_stops[0].id.clone();
    // A human answer at/after `since` (START) resumes via the SURVIVING on_blocked
    // path, nudging the live session with the answer text.
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: id,
            answer: "ship it".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: START + 1,
        },
    )
    .unwrap();
    fx.clock.set(START + 1);
    fx.driver.set_tail(&sess, IDLE_PANE);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    let sent = fx.driver.sent_keys();
    assert_eq!(sent.len(), 1, "resume nudges the live session once");
    assert!(sent[0].1.contains("ship it"), "answer fed into the nudge");
    assert!(ledger(&fx).open_stops.is_empty(), "resolved stop cleared");
    assert!(matches!(ledger(&fx).run, JobRun::Monitoring { .. }));
}

#[test]
fn human_present_defers_a_fresh_blocked_marker_until_detach() {
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, BLOCKED_HARD);
    fx.driver.set_clients(&sess, true); // a human is attached RIGHT NOW
    // The human-present gate (before step 2.5) defers: no nudge, no marker dispose.
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert!(fx.driver.sent_keys().is_empty(), "attached ⇒ no nudge");
    assert!(
        !matches!(ledger(&fx).run, JobRun::Blocked { .. }),
        "the marker is not disposed while a human is present"
    );
    // Detach ⇒ the next tick parks the Blocked (the documented one-tick defer).
    fx.driver.set_clients(&sess, false);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert!(matches!(ledger(&fx).run, JobRun::Blocked { .. }));
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn oq6_codex_conversation_id_is_captured_when_the_ledger_has_none() {
    // A codex session has no harness-pinned id; the ledger's conversation_id is
    // None until the agent reports its rollout id in a marker. OQ6: capture it.
    let fx = setup_with(Tier::Standard, Engine::Codex, Some(300), |l| {
        l.conversation_id = None;
        l.run = JobRun::Monitoring { until: START };
    });
    let sess = tmux::session_name(&fx.sched.project_id, &fx.sched.work_dir);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    let mut fx = fx;
    write_marker(
        &fx,
        r#"{"seq":11,"state":"working","status":"warming up","conversation_id":"codex-rollout-42"}"#,
    );
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        ledger(&fx).conversation_id.as_deref(),
        Some("codex-rollout-42"),
        "OQ6: the codex rollout id is persisted so a relaunch --resumes it"
    );
}

#[test]
fn a_privileged_effect_labelled_low_is_forced_capability_and_never_consulted() {
    // The worker's prose is irrelevant. A privileged effect is a typed policy input,
    // so `expert_needed`/`low` cannot auto-flow it under Autopilot.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(&fx, "Ship the uploader.");
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[{"kind":"expert_needed","effect":{"scope":"external","reversibility":"unknown","authority":"privileged"},"risk_class":"low","question":"I need a credential for the S3 bucket — which one?","options":["use mine","use CI's"]}]}"#,
    );
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Escalated(_)),
        "a credentials request must reach the human, got {out:?}"
    );
    let l = ledger(&fx);
    assert!(matches!(l.run, JobRun::Blocked { .. }), "{:?}", l.run);
    assert_eq!(l.open_stops.len(), 1);
    assert_eq!(
        l.open_stops[0].kind,
        StopKind::Capability,
        "forced Hard, so NO tier can auto-flow it"
    );
    assert!(
        l.pending_context.is_none(),
        "nothing was approved, so nothing may be queued for the agent: {:?}",
        l.pending_context
    );
    assert_eq!(
        fx.driver.spawn_count(),
        0,
        "the floor must SKIP the consult entirely — the outcome is already decided, so \
         there is no opinion worth buying (and none to be talked out of it by)"
    );
    assert_eq!(l.decider_runs.len(), 1);
    let audit = &l.decider_runs[0];
    assert_eq!(audit.finished_at, Some(START));
    assert_eq!(audit.policy.kind, StopKind::Capability);
    assert_eq!(audit.policy.labelled_risk, crate::state::RiskClass::Low);
    assert_eq!(audit.policy.effective_risk, crate::state::RiskClass::Hard);
    assert_eq!(audit.engine, Engine::Claude);
    assert!(
        matches!(
            &audit.outcome,
            crate::job::DeciderOutcome::Skipped { reason, .. }
                if reason.contains("authority=privileged")
                    && reason.contains("no decider was called")
        ),
        "{:?}",
        audit.outcome
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "nothing typed at the agent"
    );
}

#[test]
fn local_effect_does_not_escalate_because_of_words_in_the_question() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(&fx, "Choose formatting for the local admin UI.");
    fx.driver.set_tail(&sess, BUSY_PANE);
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[{
            "kind":"ambiguity",
            "effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},
            "risk_class":"low",
            "question":"Should the local admin UI use tabs or spaces?",
            "options":["tabs","spaces"]
        }]}"#,
    );

    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    let current = ledger(&fx);
    assert!(current.open_stops.is_empty());
    assert_eq!(current.decider_runs.len(), 1);
    assert!(matches!(
        current.decider_runs[0].outcome,
        crate::job::DeciderOutcome::Consulting
    ));
    assert_eq!(fx.driver.spawn_count(), 1);
}

#[test]
fn legacy_stop_without_effect_metadata_reaches_the_goal_aware_decider() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(&fx, "Use the formatter that already fits this repository.");
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[{
            "kind":"ambiguity",
            "risk_class":"low",
            "question":"Which formatter?",
            "options":["prettier","dprint"]
        }]}"#,
    );

    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + SUPERVISOR_POLL_S
        }
    );
    let current = ledger(&fx);
    assert!(current.open_stops.is_empty());
    assert_eq!(fx.driver.spawn_count(), 1);
    assert_eq!(current.decider_runs.len(), 1);
    assert!(matches!(
        current.decider_runs[0].outcome,
        crate::job::DeciderOutcome::Consulting
    ));
    let prompt = consult_argv(&fx, 1).last().cloned().unwrap_or_default();
    assert!(
        prompt.contains("scope=unknown, reversibility=unknown, authority=unknown"),
        "{prompt}"
    );
}

#[test]
fn worker_labelled_hard_ordinary_decision_still_reaches_the_goal_aware_decider() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(
        &fx,
        "Use the formatter already established by the repository.",
    );
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[{
            "kind":"ambiguity",
            "effect":{"scope":"unknown","reversibility":"unknown","authority":"ordinary"},
            "risk_class":"hard",
            "question":"Which formatter?",
            "options":["prettier","dprint"]
        }]}"#,
    );

    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.driver.spawn_count(), 1);
    let current = ledger(&fx);
    assert!(current.open_stops.is_empty());
    assert!(matches!(
        current.decider_runs[0].outcome,
        crate::job::DeciderOutcome::Consulting
    ));
    assert_eq!(
        current.decider_runs[0].policy.labelled_risk,
        RiskClass::Hard,
        "the audit preserves the worker's caution even though it does not force escalation"
    );
}

#[test]
fn a_typed_external_irreversible_effect_escalates_on_both_tiers() {
    // Routing is independent of prose and tier: the typed external/irreversible effect
    // forces `Capability`, and Capability is Hard everywhere.
    for tier in [Tier::Autopilot, Tier::Standard] {
        let (mut fx, _sess) = marker_fx(tier, |_| {});
        write_goal(&fx, "Get the release out.");
        write_marker(
            &fx,
            r#"{"seq":9,"state":"blocked","stops":[{"kind":"ambiguity","effect":{"scope":"external","reversibility":"irreversible","authority":"ordinary"},"risk_class":"low","question":"Ready?","options":["deploy now","wait"]}]}"#,
        );
        let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
        assert!(matches!(out, JobTick::Escalated(_)), "{tier:?}: {out:?}");
        assert_eq!(
            ledger(&fx).open_stops[0].kind,
            StopKind::Capability,
            "{tier:?}"
        );
        assert_eq!(fx.driver.spawn_count(), 0, "{tier:?}");
    }
}

#[test]
fn an_escalating_stop_never_reaches_the_supervisor() {
    // `decide_kind` => Escalate. The consult exists only to fill in the CONTENT of an
    // approval, so with nothing approved there is nothing to consult about.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(&fx, "Ship it.");
    write_marker(&fx, BLOCKED_HARD); // publish/low -> forced Hard by policy, not by the floor
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
    assert_eq!(
        fx.driver.spawn_count(),
        0,
        "an escalation must not spend a consult"
    );
}

#[test]
fn dispose_records_reported_and_escalated_on_the_autopilot_feed() {
    // The marker disposer is the source of most feed entries. A `working` report lands as
    // `Reported` carrying the agent's own status; a `blocked` one lands as `Escalated`
    // carrying the question the human must answer.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_marker(
        &fx,
        r#"{"seq":7,"state":"working","status":"indexing src/"}"#,
    );
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        ledger(&fx).events.iter().any(|e| matches!(
            &e.kind,
            job::AutopilotEventKind::Reported(Some(s)) if s == "indexing src/"
        )),
        "a working report is on the feed with its status: {:?}",
        ledger(&fx).events
    );
    // A typed external/irreversible effect forces escalation, recorded verbatim.
    // On a FRESH session, so the single marker is unambiguously the one disposed (writing a
    // second marker into the same fx risks a coarse-granularity mtime collision that makes
    // `observe_marker` short-circuit — a test artifact, not the behaviour under test).
    let (mut fx2, _sess2) = marker_fx(Tier::Autopilot, |_| {});
    write_marker(
        &fx2,
        r#"{"seq":8,"state":"blocked","stops":[{"kind":"ambiguity","effect":{"scope":"external","reversibility":"irreversible","authority":"ordinary"},"risk_class":"medium",
            "question":"deploy to production now?","options":["yes","no"]}]}"#,
    );
    fx2.sched.tick(&fx2.driver, &fx2.clock).unwrap();
    assert!(
        ledger(&fx2).events.iter().any(|e| matches!(
            &e.kind,
            job::AutopilotEventKind::Escalated(Some(q)) if q == "deploy to production now?"
        )),
        "a blocked report is on the feed as an escalation with its question: {:?}",
        ledger(&fx2).events
    );
}

#[test]
fn next_step_is_mirrored_and_kept_when_a_later_report_omits_it() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_s| {});
    // First report carries a next_step.
    write_marker(
        &fx,
        r#"{"state":"working","seq":10,"status":"a","next_step":"wire the alert"}"#,
    );
    backdate_marker(&fx, 3);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).last_plan.as_deref(), Some("wire the alert"));
    // Second report omits next_step -> the prior plan is KEPT, not cleared.
    write_marker(&fx, r#"{"state":"working","seq":11,"status":"b"}"#);
    backdate_marker(&fx, 2);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).last_plan.as_deref(), Some("wire the alert"));
    assert_eq!(ledger(&fx).last_status.as_deref(), Some("b"));
    // Third report PRESENTS next_step but EMPTY ("") -> present-but-empty is not an
    // instruction to forget: the "last NON-EMPTY, kept-prior when omitted" contract
    // means an empty string must NOT clobber the plan the agent last stated.
    write_marker(
        &fx,
        r#"{"state":"working","seq":12,"status":"c","next_step":""}"#,
    );
    backdate_marker(&fx, 1);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        ledger(&fx).last_plan.as_deref(),
        Some("wire the alert"),
        "an empty next_step must leave the prior plan intact, not clear it"
    );
    assert_eq!(ledger(&fx).last_status.as_deref(), Some("c"));
}

// --- Milestone B: the decider-lane ledger ------------------------------------

#[test]
fn digest_counters_are_monotonic_and_never_double_count() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    // Bump 1: a working report.
    report_progress(&fx, 7, 3);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).digest.disposed, 1);
    assert_eq!(ledger(&fx).digest.working, 1);
    // Bump 2: another working report (distinct, older-backdated mtime so it is re-observed).
    report_progress(&fx, 8, 2);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        ledger(&fx).digest.disposed,
        2,
        "each accepted bump counts once"
    );
    // A deliberate identical rewrite is a new wake even though its semantic hash is unchanged:
    // atomic revision metadata distinguishes it from replaying the same file after restart.
    report_progress(&fx, 8, 1);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        ledger(&fx).digest.disposed,
        3,
        "an identical atomic rewrite is still a newly reported wake"
    );
    fx.sched.last_marker_stamp = None;
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        ledger(&fx).digest.disposed,
        3,
        "re-reading the exact same revision after restart is not a fourth report"
    );
    // Exactly one raw.jsonl line per DISPOSED bump.
    let raw = std::fs::read_to_string(fx.sched.paths.raw_jsonl()).unwrap();
    assert_eq!(
        raw.lines().count(),
        3,
        "one raw line per accepted bump: {raw:?}"
    );
}

#[test]
fn consult_path_bumps_disposed_but_not_autoflow_before_a_verdict() {
    // CRITICAL-1: the auto-flow+supervisor sub-path returns Ok(Some(tick)) before the
    // arm-local save; the real save is spawn_advice's `parked = next.clone()`. The seam
    // bumps `disposed`/`situation` and emits the raw line on `next` BEFORE the branch, so
    // the clone carries them. Pre-fix this path recorded nothing.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |ledger| {
        ledger.start_turn(
            START - 10,
            job::TurnTrigger::Heartbeat {
                pending_context: false,
                marker_recovery: false,
            },
        );
    });
    let _seq = start_consult(&mut fx); // spawns a real consult over AUTOFLOW_ASKS (seq 9)
    let l = ledger(&fx);
    assert_eq!(
        l.digest.disposed, 1,
        "the auto-flow accepted bump counts once"
    );
    assert_eq!(
        l.digest.auto_flow, 0,
        "eligibility and consult spawn are not approval"
    );
    assert!(matches!(
        l.turn_trace.last().map(|turn| &turn.outcome),
        Some(job::TurnOutcome::Reported {
            disposition: job::TurnDisposition::Reviewing,
            ..
        })
    ));
    let raw = std::fs::read_to_string(fx.sched.paths.raw_jsonl()).unwrap();
    assert_eq!(raw.lines().count(), 1);
    assert!(
        raw.contains("\"seq\":9"),
        "the raw line carries the disposed report: {raw}"
    );
}

#[test]
fn blocked_no_stop_park_bumps_disposed_and_records_stalled() {
    // CRITICAL-2: a blocked report with no routable stop escalates via park_stuck, which
    // never reaches the four arms — but it DOES pass the top-of-dispose seam.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |ledger| {
        ledger.start_turn(
            START - 10,
            job::TurnTrigger::Heartbeat {
                pending_context: false,
                marker_recovery: false,
            },
        );
    });
    write_marker(&fx, r#"{"seq":12,"state":"blocked","stops":[]}"#);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Stuck(_)
    ));
    let l = ledger(&fx);
    assert_eq!(l.digest.disposed, 1, "the park is an accepted bump");
    assert_eq!(l.digest.stalled, 1);
    assert!(matches!(
        l.turn_trace.last().map(|turn| &turn.outcome),
        Some(job::TurnOutcome::Reported {
            disposition: job::TurnDisposition::Stalled,
            ..
        })
    ));
    assert!(
        l.decisions
            .iter()
            .any(|d| d.kind == job::DecisionKind::Stalled && d.seq == Some(12)),
        "a Stalled decision is recorded with the accepted seq: {:?}",
        l.decisions
    );
    let md = std::fs::read_to_string(fx.sched.paths.decisions()).unwrap();
    assert!(
        md.contains("pmd stalled:"),
        "the stall is source-attributed: {md}"
    );
}

#[test]
fn situation_reflects_the_last_disposed_report_even_on_the_park_path() {
    // A working bump: situation mirrors the report's state/status/seq.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, r#"{"seq":7,"state":"working","status":"indexing"}"#);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let sit = ledger(&fx)
        .situation
        .expect("situation set on a working bump");
    assert_eq!(sit.state, job::WakeState::Working);
    assert_eq!(sit.seq, 7);
    assert_eq!(sit.status.as_deref(), Some("indexing"));
    // The blocked-NO-STOP park path (Missing-3): situation must NOT go stale even though
    // this path skips every arm and parks via park_stuck. Guaranteed by the seam.
    let (mut fx2, _s2) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx2, r#"{"seq":12,"state":"blocked","stops":[]}"#);
    assert!(matches!(
        fx2.sched.tick(&fx2.driver, &fx2.clock).unwrap(),
        JobTick::Stuck(_)
    ));
    let sit2 = ledger(&fx2)
        .situation
        .expect("situation set on the park path");
    assert_eq!(sit2.state, job::WakeState::Blocked);
    assert_eq!(sit2.seq, 12);
}

#[test]
fn decisions_md_is_sparse_and_source_attributed() {
    // Working → NOT notable: no decisions.md line (raw.jsonl only).
    let (mut fxw, _sw) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fxw, r#"{"seq":7,"state":"working","status":"indexing"}"#);
    fxw.sched.tick(&fxw.driver, &fxw.clock).unwrap();
    let mdw = std::fs::read_to_string(fxw.sched.paths.decisions()).unwrap_or_default();
    assert!(
        !mdw.contains("pmd "),
        "a working decision is raw-only, never on decisions.md: {mdw:?}"
    );
    // Blocked-escalating (hard publish) → notable: a `pmd escalated:` line appears.
    let (mut fxb, _sb) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fxb, BLOCKED_HARD);
    assert!(matches!(
        fxb.sched.tick(&fxb.driver, &fxb.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    let mdb = std::fs::read_to_string(fxb.sched.paths.decisions()).unwrap();
    assert!(
        mdb.contains("pmd escalated:"),
        "escalation is attributed: {mdb}"
    );
    // AutoFlow (consultable) → ABSENT from decisions.md but PRESENT in raw.jsonl.
    let (mut fxa, _sa) = marker_fx(Tier::Autopilot, |_| {});
    let _seq = start_consult(&mut fxa);
    let mda = std::fs::read_to_string(fxa.sched.paths.decisions()).unwrap_or_default();
    assert!(!mda.contains("auto_flow"), "auto-flow is raw-only: {mda:?}");
    let rawa = std::fs::read_to_string(fxa.sched.paths.raw_jsonl()).unwrap();
    assert!(
        rawa.contains("\"seq\":9"),
        "auto-flow IS in raw.jsonl: {rawa}"
    );
}

#[test]
fn side_file_error_never_fails_the_state_json_write() {
    // Make raw.jsonl a DIRECTORY so the append open() fails (EISDIR). The dispose must
    // still succeed: the side-file failure is logged and swallowed.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    std::fs::create_dir_all(fx.sched.paths.raw_jsonl()).unwrap();
    write_marker(&fx, r#"{"seq":7,"state":"working","status":"x"}"#);
    // tick() must NOT error (unwrap proves it) despite the raw.jsonl append failing.
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let l = ledger(&fx);
    assert_eq!(l.last_marker_seq, 7, "the operational ledger still saved");
    assert_eq!(
        l.digest.disposed, 1,
        "dispose ran; only the side file failed"
    );
}

#[test]
fn raw_jsonl_rotates_past_the_ceiling() {
    // A tiny injected ceiling forces a rotation between two short lines: the second append
    // sees the file at/over the ceiling, renames it to `.1`, then writes into a fresh file.
    let (fx, _sess) = marker_fx(Tier::Standard, |_| {});
    fx.sched.append_raw("line-1", 4).unwrap();
    fx.sched.append_raw("line-2", 4).unwrap();
    let raw = std::fs::read_to_string(fx.sched.paths.raw_jsonl()).unwrap();
    let rotated = std::fs::read_to_string(fx.sched.paths.raw_jsonl_rotated()).unwrap();
    assert!(
        raw.contains("line-2") && !raw.contains("line-1"),
        "raw holds only the newest"
    );
    assert!(
        rotated.contains("line-1"),
        "the ceiling rotated the old generation to .1"
    );
}

// --- FO-1 plan-staleness streak (Milestone D) --------------------------------

/// Drive ONE `working` bump carrying `status` + `next_step` on an idle pane, one cadence
/// later; returns the tick. Plain tick (not `tick_confirmed`), so it works whether the
/// disposition arms/monitors (below threshold) or escalates (at threshold).
fn dispose_plan(
    fx: &mut Fx,
    sess: &str,
    seq: u64,
    status: &str,
    next_step: &str,
    age_s: u64,
) -> JobTick {
    write_marker(
        fx,
        &format!(
            r#"{{"seq":{seq},"state":"working","status":"{status}","next_step":"{next_step}"}}"#
        ),
    );
    backdate_marker(fx, age_s);
    fx.driver.set_tail(sess, IDLE_PANE);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap()
}

#[test]
fn restated_plan_bumps_dedicated_streak_not_continuations() {
    // Seed the prior status+plan so the FIRST bump below is already a repeat.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("indexing".into());
        l.last_plan = Some("wire the alert".into());
    });
    for seq in 1..=3u64 {
        dispose_plan(&mut fx, &sess, seq, "indexing", "wire the alert", 10 - seq);
    }
    let l = ledger(&fx);
    assert_eq!(
        l.stale_plan_streak, 3,
        "three identical restatements => streak 3"
    );
    // A DEDICATED counter: a stream of VALID bumps never touches the malformed-marker
    // stall counter `continuations`.
    assert_eq!(l.continuations, 0, "valid bumps never touch continuations");
}

#[test]
fn changing_the_plan_resets_the_streak() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("indexing".into());
        l.last_plan = Some("wire the alert".into());
    });
    for seq in 1..=2u64 {
        dispose_plan(&mut fx, &sess, seq, "indexing", "wire the alert", 10 - seq);
    }
    assert_eq!(ledger(&fx).stale_plan_streak, 2);
    // A DIFFERENT next_step resets the streak to 0.
    dispose_plan(&mut fx, &sess, 3, "indexing", "NOW something else", 2);
    assert_eq!(
        ledger(&fx).stale_plan_streak,
        0,
        "a changed plan resets the streak"
    );
}

#[test]
fn a_terse_bump_neither_bumps_nor_resets() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("indexing".into());
        l.last_plan = Some("wire the alert".into());
        l.stale_plan_streak = 2; // pre-seed a streak
    });
    // A TERSE bump (no next_step) — exactly what `report_progress` writes.
    write_marker(&fx, r#"{"seq":1,"state":"working","status":"indexing"}"#);
    backdate_marker(&fx, 4);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let l = ledger(&fx);
    assert_eq!(
        l.stale_plan_streak, 2,
        "a terse bump leaves the streak UNTOUCHED"
    );
    assert_eq!(
        l.last_plan.as_deref(),
        Some("wire the alert"),
        "keep-prior preserves the plan"
    );
}

#[test]
fn same_plan_but_changed_status_does_not_accrue() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("step 0".into());
        l.last_plan = Some("wire the alert".into());
    });
    // Same next_step, MOVING status each bump — a healthy long grind.
    for seq in 1..=4u64 {
        dispose_plan(
            &mut fx,
            &sess,
            seq,
            &format!("step {seq}"),
            "wire the alert",
            10 - seq,
        );
    }
    assert_eq!(
        ledger(&fx).stale_plan_streak,
        0,
        "a moving status keeps the streak at 0 (BOTH fields must match)"
    );
}

use super::super::marker::DEFAULT_STALE_PLAN_STALL;

#[test]
fn repeated_plan_past_threshold_escalates_worker_stuck() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("indexing".into());
        l.last_plan = Some("wire the alert".into());
    });
    let t2 = DEFAULT_STALE_PLAN_STALL as u64;
    // Drive to ONE below the threshold: every tick still Monitors, streak just under T2.
    for seq in 1..t2 {
        let tick = dispose_plan(
            &mut fx,
            &sess,
            seq,
            "indexing",
            "wire the alert",
            (t2 + 2 - seq) * 2,
        );
        assert!(
            matches!(tick, JobTick::Monitoring { .. }),
            "pre-threshold monitors: {tick:?}"
        );
    }
    assert_eq!(
        ledger(&fx).stale_plan_streak,
        DEFAULT_STALE_PLAN_STALL - 1,
        "one below T2"
    );
    // The threshold bump escalates — it is the COUNT that triggers, not this bump's content.
    let tick = dispose_plan(&mut fx, &sess, t2, "indexing", "wire the alert", 2);
    assert!(
        matches!(tick, JobTick::Stuck(_)),
        "reaching T2 escalates: {tick:?}"
    );
    let l = ledger(&fx);
    assert!(
        matches!(l.run, JobRun::Blocked { .. }),
        "escalation parks Blocked"
    );
    assert_eq!(l.open_stops.len(), 1);
    assert_eq!(
        l.open_stops[0].kind,
        StopKind::WorkerStuck,
        "the honest 'alive but not advancing' kind"
    );
}

#[test]
fn monitoring_state_plan_staleness_also_escalates() {
    // The threshold check is PRE-MATCH, so a self-napping Monitoring agent restating the
    // same plan escalates too — not only the Working arm.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("polling".into());
        l.last_plan = Some("poll the queue".into());
    });
    let t2 = DEFAULT_STALE_PLAN_STALL as u64;
    let mut last = JobTick::WaitingForIntake;
    for seq in 1..=t2 {
        write_marker(
            &fx,
            &format!(
                r#"{{"seq":{seq},"state":"monitoring","status":"polling","next_step":"poll the queue","next_check_s":120}}"#
            ),
        );
        backdate_marker(&fx, (t2 + 2 - seq) * 2);
        fx.driver.set_tail(&sess, IDLE_PANE);
        fx.clock.advance(300 + BUSY_RECHECK_S);
        last = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }
    assert!(
        matches!(last, JobTick::Stuck(_)),
        "a stale plan across monitoring reports escalates: {last:?}"
    );
    assert!(matches!(ledger(&fx).run, JobRun::Blocked { .. }));
}

// --- audit fixes (2026-08-21): silent-loss regressions ----------------------

#[test]
fn a_blocked_marker_with_an_unknown_field_is_recovered_and_still_escalates() {
    // A blocked publish escalation carrying a typo'd/extra key fails the strict
    // deny_unknown_fields parse. The recovery parse must strip the stray key and PRESERVE the
    // escalation — not silently downgrade it to a lenient `working` continuation.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(
        &fx,
        r#"{"seq":100,"state":"blocked","status":"ready to ship","next_step":"await sign-off","stops":[{"kind":"publish","effect":{"scope":"external","reversibility":"irreversible","authority":"ordinary"},"risk_class":"low","question":"ship it?"}],"stauts_typo":"oops"}"#,
    );
    backdate_marker(&fx, 5); // settle past the mid-write grace ⇒ the Err path is reached
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Escalated(_)),
        "the unknown field is stripped and the escalation preserved: {out:?}"
    );
    let l = ledger(&fx);
    assert!(
        matches!(l.run, JobRun::Blocked { .. }),
        "parked Blocked, not downgraded: {:?}",
        l.run
    );
    assert_eq!(l.open_stops[0].kind, StopKind::Publish);
    assert_eq!(
        l.last_marker_seq, 100,
        "the recovered bump advanced the watermark"
    );
    // The other real fields survived the strip too (proves the recovery keeps known keys).
    assert_eq!(l.last_status.as_deref(), Some("ready to ship"));
    assert_eq!(l.last_plan.as_deref(), Some("await sign-off"));
}

#[test]
fn a_genuinely_malformed_marker_still_downgrades_not_escalates() {
    // The recovery is not a free pass: invalid JSON fails the lenient parse too and downgrades
    // to a lenient working re-park — so genuine drift/corruption still surfaces.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, r#"{"seq":100,"state":"blocked", THIS IS NOT JSON"#);
    backdate_marker(&fx, 5);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Monitoring { .. }),
        "unparseable ⇒ lenient working re-park: {out:?}"
    );
    assert!(
        !matches!(ledger(&fx).run, JobRun::Blocked { .. }),
        "garbage never escalates"
    );
}

#[test]
fn a_terse_marker_bump_keeps_the_prior_status() {
    // A state-only bump with no `status` must KEEP the prior status (symmetric with last_plan),
    // not wipe the dashboard chip + nudge echo to None.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |l| {
        l.last_status = Some("driving the E2E thread".into());
        l.last_marker_seq = 10;
    });
    write_marker(&fx, r#"{"seq":11,"state":"working"}"#);
    backdate_marker(&fx, 5);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let l = ledger(&fx);
    assert_eq!(l.last_marker_seq, 11, "the bump was accepted");
    assert_eq!(
        l.last_status.as_deref(),
        Some("driving the E2E thread"),
        "a terse bump keeps the prior status — it does not wipe it to None"
    );
}

#[test]
fn a_mixed_report_escalates_only_the_human_owned_stop_and_queues_the_ordinary_one() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_marker(
        &fx,
        r#"{"seq":100,"state":"blocked","stops":[
            {"kind":"publish","effect":{"scope":"external","reversibility":"irreversible","authority":"ordinary"},"risk_class":"low","question":"ship it?"},
            {"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low","question":"tabs or spaces?","options":["tabs","spaces"]}
        ]}"#,
    );
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let esc_ids = vec![format!("stop-{SESSION_ID}-0-1-{START}")];
    assert_eq!(
        out,
        JobTick::Escalated(esc_ids),
        "only the explicitly human-owned action should escalate"
    );
    let l = ledger(&fx);
    assert!(matches!(l.run, JobRun::Blocked { .. }));
    assert_eq!(l.open_stops.len(), 1);
    assert_eq!(
        l.open_stops[0].kind,
        StopKind::Publish,
        "the publish stop (a force-escalate kind) is what parks"
    );
    assert_eq!(l.advice_queue.len(), 1);
    assert_eq!(l.advice_queue[0].draft.kind, StopKind::Ambiguity);
    assert!(l.pending_context.is_none());
    assert_eq!(l.decider_runs.len(), 1);
}

#[test]
fn an_oversized_decision_batch_bounds_the_queue_and_audits_only_the_overflow() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let stops = (0..=crate::job::ADVICE_QUEUE_MAX)
        .map(|index| {
            serde_json::json!({
                "kind": "ambiguity",
                "risk_class": "low",
                "question": format!("Decision {index}?"),
                "options": ["yes", "no"]
            })
        })
        .collect::<Vec<_>>();
    write_marker(
        &fx,
        &serde_json::json!({
            "seq": 100,
            "state": "blocked",
            "stops": stops
        })
        .to_string(),
    );

    let JobTick::Escalated(ids) = fx.sched.tick(&fx.driver, &fx.clock).unwrap() else {
        panic!("the overflow decision should reach the human");
    };
    assert_eq!(ids.len(), 1);
    let current = ledger(&fx);
    assert_eq!(current.advice_queue.len(), crate::job::ADVICE_QUEUE_MAX);
    assert_eq!(current.open_stops.len(), 1);
    assert_eq!(current.decider_runs.len(), 1);
    assert!(matches!(
        &current.decider_runs[0].outcome,
        crate::job::DeciderOutcome::Skipped { reason, .. }
            if reason.contains("bounded 8-decision review queue")
    ));
}
