//! Tests for the view derivation: the tier dial and `read_agent_loop` over a real
//! on-disk session ledger (including the autopilot-off marker gate).

use super::*;
use crate::job::{self, AgentLoopState, JobRun};
use crate::pmstate::{OpenStop, StopKind, StopStatus};
use crate::state::{Config, ProjectPaths, RiskClass, Tier};

#[test]
fn sort_puts_attention_first() {
    let mut v = [
        Posture::Done,
        Posture::NeedsYou,
        Posture::Monitoring,
        Posture::Running,
    ];
    v.sort_by_key(|p| p.sort_rank());
    assert_eq!(v[0], Posture::NeedsYou);
    assert_eq!(v[3], Posture::Done);
}

#[test]
fn tier_cycles() {
    // Two-level toggle: Standard ↔ Autopilot, symmetric next/prev.
    assert_eq!(next_tier(Tier::Standard), Tier::Autopilot);
    assert_eq!(next_tier(Tier::Autopilot), Tier::Standard);
    assert_eq!(prev_tier(Tier::Standard), Tier::Autopilot);
    assert_eq!(prev_tier(Tier::Autopilot), Tier::Standard);
}

#[test]
fn agent_loop_view_projects_the_bounded_turn_trace() {
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    ledger.start_turn(
        1010,
        job::TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: false,
        },
    );
    job::save(&sp, &ledger).unwrap();

    let view = ProjectView::read_agent_loop("bot", &sp, true, 1020);
    assert_eq!(view.turn_trace, ledger.turn_trace);
}

#[test]
fn agent_loop_blocked_on_stuck_kind_is_stuck_and_hard() {
    // A Blocked agent-loop ledger normally reads NeedsYou, but a stuck-kind
    // dead-end burns the alarming Stuck posture, and its display stop is forced
    // Hard even though `stuck` isn't in the reference ALWAYS_HARD_KINDS string set.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = JobRun::Blocked {
        stop_ids: vec!["s".into()],
        since: 1000,
    };
    l.open_stops = vec![OpenStop {
        id: "s".into(),
        kind: StopKind::Stuck,
        pane_dialog: None,
        channel: None,
        context_ref: None,
        question: None,
        options: vec![],
        authorized_responders: vec![],
        message_id: None,
        first_posted: 1000,
        last_polled: None,
        last_seen_reply_ts: None,
        status: StopStatus::AwaitingReply,
    }];
    job::save(&sp, &l).unwrap();

    let v = ProjectView::read_agent_loop("bot", &sp, true, 1500);
    assert_eq!(v.posture, Posture::Stuck);
    assert_eq!(v.stops.len(), 1);
    assert_eq!(v.stops[0].kind, "stuck");
    assert_eq!(v.stops[0].risk_class, RiskClass::Hard);
}

#[test]
fn agent_loop_display_stop_carries_question_and_options() {
    // The ledger→display hop must not drop the agent's ask (it used to hard-code an
    // empty question), and an absent question must stay empty so the renderers'
    // kind fallback still applies.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = JobRun::Blocked {
        stop_ids: vec!["s".into(), "t".into()],
        since: 1000,
    };
    let base = OpenStop {
        id: "s".into(),
        kind: StopKind::ConfirmDone,
        pane_dialog: None,
        channel: None,
        context_ref: None,
        question: Some("Confirm and close, or keep adding?".into()),
        options: vec!["close".into(), "keep going".into()],
        authorized_responders: vec![],
        message_id: None,
        first_posted: 1000,
        last_polled: None,
        last_seen_reply_ts: None,
        status: StopStatus::AwaitingReply,
    };
    // The second stop is the shape an older ledger deserializes to.
    let legacy = OpenStop {
        id: "t".into(),
        question: None,
        options: vec![],
        ..base.clone()
    };
    l.open_stops = vec![base, legacy];
    job::save(&sp, &l).unwrap();

    let v = ProjectView::read_agent_loop("bot", &sp, true, 1500);
    assert_eq!(v.stops[0].question, "Confirm and close, or keep adding?");
    assert_eq!(
        v.stops[0].options,
        vec!["close".to_string(), "keep going".into()]
    );
    assert!(
        v.stops[1].question.is_empty(),
        "no question ⇒ empty, not \"None\""
    );
    assert!(v.stops[1].options.is_empty());
}

#[test]
fn autopilot_off_blocked_marker_reads_needs_you_with_no_answerable_stop() {
    // THE BUG: with autopilot OFF (`Tier::Standard`) the daemon skips this session's
    // WHOLE tick by design (`daemon::pmd_drives_row`), so a marker the agent wrote
    // while off is never folded into the ledger. The ledger stayed `Idle` with no
    // open stops, so the row drew an idle glyph and the header tally counted the
    // session as idle — while the agent was actually blocked, waiting on a human.
    // The row lied about not needing you.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    // Idle ledger, no open stops — exactly what an OFF session's ledger looks like.
    let l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    assert!(matches!(l.run, JobRun::Idle), "precondition: idle ledger");
    job::save(&sp, &l).unwrap();
    // Autopilot OFF.
    state::write_json_atomic(
        &sp.config(),
        &Config {
            autonomy: Tier::Standard,
            step_timeout_s: 100,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    // The agent's own machine report: blocked, waiting on a decision.
    std::fs::write(
        sp.needs_you(),
        r#"{ "state": "blocked", "seq": 7,
             "stops": [{ "kind": "confirm_done",
                         "effect": { "scope": "local", "reversibility": "reversible",
                                     "authority": "ordinary" },
                         "question": "ship it?",
                         "risk_class": "medium" }],
             "status": "waiting on you" }"#,
    )
    .unwrap();

    let v = ProjectView::read_agent_loop("bot", &sp, true, 1500);
    assert_eq!(
        v.posture,
        Posture::NeedsYou,
        "a blocked marker must make the row honest even with the daemon not ticking"
    );
    // ...and NOTHING became answerable. Only `pmd` may fold a marker into the
    // ledger, and only `pmd` writes `state.json`; inventing an open stop here would
    // offer an answer no one would ever deliver.
    assert!(
        v.stops.is_empty(),
        "no stop is invented from the marker: {:?}",
        v.stops
    );
}

#[test]
fn autopilot_off_marker_does_not_override_a_working_ledger_or_autopilot_row() {
    // The marker is a LAST RESORT, not an override. Two non-regressions:
    //  1. Autopilot ON ⇒ the daemon does tick, so the ledger is authoritative and a
    //     stale marker must not repaint the row.
    //  2. A ledger with open stops already drives the posture (and carries the
    //     stuck-kind burn), so the marker must not touch it.
    let blocked_marker = r#"{ "state": "blocked", "seq": 7, "stops": [] }"#;

    // (1) Autopilot ON + idle ledger ⇒ still Fresh.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    job::save(&sp, &AgentLoopState::fresh(Engine::Claude, Some(300), 1000)).unwrap();
    state::write_json_atomic(
        &sp.config(),
        &Config {
            autonomy: Tier::Autopilot,
            step_timeout_s: 100,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    std::fs::write(sp.needs_you(), blocked_marker).unwrap();
    assert_eq!(
        ProjectView::read_agent_loop("bot", &sp, true, 1500).posture,
        Posture::Fresh,
        "autopilot ON ⇒ the ledger is authoritative; the marker must not repaint"
    );

    // (2) Autopilot OFF but the ledger already carries a stuck-kind open stop ⇒ the
    // alarming Stuck posture survives; a `blocked` marker must not soften it.
    let dir2 = tempfile::tempdir().unwrap();
    let sp2 = ProjectPaths::for_session(dir2.path(), "bot");
    std::fs::create_dir_all(sp2.state_dir()).unwrap();
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = JobRun::Blocked {
        stop_ids: vec!["s".into()],
        since: 1000,
    };
    l.open_stops = vec![OpenStop {
        id: "s".into(),
        kind: StopKind::Stuck,
        pane_dialog: None,
        channel: None,
        context_ref: None,
        question: None,
        options: vec![],
        authorized_responders: vec![],
        message_id: None,
        first_posted: 1000,
        last_polled: None,
        last_seen_reply_ts: None,
        status: StopStatus::AwaitingReply,
    }];
    job::save(&sp2, &l).unwrap();
    state::write_json_atomic(
        &sp2.config(),
        &Config {
            autonomy: Tier::Standard,
            step_timeout_s: 100,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    std::fs::write(sp2.needs_you(), blocked_marker).unwrap();
    let v2 = ProjectView::read_agent_loop("bot", &sp2, true, 1500);
    assert_eq!(
        v2.posture,
        Posture::Stuck,
        "stuck must not soften to NeedsYou"
    );
    assert_eq!(
        v2.stops.len(),
        1,
        "the ledger's real stop still comes through"
    );
}

#[test]
fn agent_loop_missing_ledger_renders_fresh() {
    // A registered session whose ledger isn't on disk yet still renders (Fresh,
    // no stops) rather than crashing the TUI.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    let v = ProjectView::read_agent_loop("bot", &sp, true, 1500);
    assert_eq!(v.posture, Posture::Fresh);
    assert!(v.stops.is_empty());
    assert!(v.tier.is_none());
}

#[test]
fn a_countdown_is_only_shown_when_something_will_actually_happen_at_zero() {
    // User: *"i change cadence value to 1m from 5m. The check in go to 0 but no message is sent"*.
    //
    // Half of that was a real brake left engaged (fixed in `AgentLoopState::retime`), and half was
    // this: the row printed `monitoring · check in 0s` while `awaiting_report()` held, and `drive`
    // sends NOTHING in that state whatever the park says — it re-parks a few seconds at a time until
    // the agent reports. A timer counting down to an event that will not happen is the same class of
    // lie as the Standard-row check-in removed in m41, so it says the real reason instead.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(60), 1000);
    l.run = JobRun::Monitoring { until: 1000 };
    l.last_marker_seq = 7;
    l.nudged_at_seq = Some(7); // nudged at 7, nothing newer reported
    job::save(&sp, &l).unwrap();

    let v = ProjectView::read_agent_loop("bot", &sp, true, 1000);
    assert_eq!(
        v.next_action, "waiting for the agent to report",
        "a park that cannot fire must say why, not count down to it"
    );

    // THE AGENT REPORTS ⇒ the brake is off, so the countdown means something again and comes back.
    l.last_marker_seq = 8;
    job::save(&sp, &l).unwrap();
    let v = ProjectView::read_agent_loop("bot", &sp, true, 940);
    assert_eq!(
        v.next_action, "monitoring · check in 00:01:00",
        "…and once it can fire, the timer is the honest thing to show"
    );
}

#[test]
fn countdown_uses_unbounded_zero_padded_hours_minutes_and_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    for (remaining, want) in [
        (97_445, "27:04:05"),
        (359_999, "99:59:59"),
        (360_000, "100:00:00"),
    ] {
        let mut l = AgentLoopState::fresh(Engine::Claude, Some(remaining as u64), 1000);
        l.run = JobRun::Monitoring {
            until: 1000 + remaining,
        };
        job::save(&sp, &l).unwrap();
        assert_eq!(
            ProjectView::read_agent_loop("bot", &sp, true, 1000).next_action,
            format!("monitoring · check in {want}")
        );
    }
}

#[test]
fn the_countdown_never_resets_through_zero_it_shows_confirming_in_the_recheck_window() {
    // User: *"Timer sometimes is odd, when the timer countdown to 0s on autopilot, it
    // doesn't send immediately. it resets to 5 or 10s then it sends another one. This sets
    // false expectation."*
    //
    // The last `BUSY_RECHECK_S` of any monitoring park is `drive`'s confirmation/recheck
    // brake: at expiry it re-reads the pane and, if it is still confirming (the first of two
    // consecutive Idle observations, a still-changing transcript, or a Busy pane), re-parks
    // ANOTHER short recheck and sends nothing. Counting that window down to `0s` promised a
    // send the drive path withholds, so the row shows `confirming…` there and NEVER a
    // countdown resetting through zero.
    use crate::job_engine::BUSY_RECHECK_S;
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    let cadence_end = 1000 + 300;
    l.run = JobRun::Monitoring { until: cadence_end };
    job::save(&sp, &l).unwrap();

    // A cadence with time to spare counts down honestly.
    assert_eq!(
        ProjectView::read_agent_loop("bot", &sp, true, 1000).next_action,
        "monitoring · check in 00:05:00",
        "a park with real time left is an honest countdown"
    );
    // Anywhere inside the recheck window (at the floor, below it, and at the very zero the
    // user watched) the row must say it is confirming, never count down to/through 0.
    for remaining in [BUSY_RECHECK_S, BUSY_RECHECK_S - 1, 1, 0] {
        let now = cadence_end - remaining;
        let action = ProjectView::read_agent_loop("bot", &sp, true, now).next_action;
        assert_eq!(
            action, "monitoring · confirming…",
            "inside the recheck window the row must not promise a send (remaining={remaining}s)"
        );
        assert!(
            !action.contains("check in"),
            "no countdown may show inside the recheck window: {action}"
        );
    }
    // Past expiry (drive has not yet re-parked this tick) is still the honest state, never
    // the phantom `check in 0s` the user reported.
    let overrun = ProjectView::read_agent_loop("bot", &sp, true, cadence_end + 3);
    assert_eq!(overrun.next_action, "monitoring · confirming…");
    assert!(!overrun.next_action.contains("check in 00:00:00"));
}

#[test]
fn a_driven_session_shows_working_mid_turn_then_waiting_once_the_turn_ends() {
    // User: *"after the claude finish and wait for, why don't we update the status"*.
    //
    // A DRIVEN (Monitoring) session's working-vs-waiting comes from the LIVE turn-end
    // signal file (one byte per completed turn) compared against `turn_count_at_nudge`,
    // the count captured at the last nudge:
    //   size == baseline ⇒ the nudged turn has not finished ⇒ WORKING (row reads `working`);
    //   size >  baseline ⇒ a turn completed since  ⇒ idle at prompt, WAITING.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    std::fs::create_dir_all(sp.daemon_dir()).unwrap();
    let cadence_end = 1000 + 300;
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = JobRun::Monitoring { until: cadence_end };
    l.turn_count_at_nudge = Some(2); // two turns had completed when we nudged
    job::save(&sp, &l).unwrap();

    // MID-TURN: the signal file still holds the baseline count of bytes.
    std::fs::write(sp.turn_signal(), "xx").unwrap(); // size 2 == baseline
    let v = ProjectView::read_agent_loop("bot", &sp, true, 1000);
    assert_eq!(v.agent_working, Some(true), "size == baseline ⇒ working");
    assert_eq!(
        v.next_action, "working",
        "mid-turn says working plainly, above every countdown"
    );

    // TURN ENDS: the hook appended a byte, so size > baseline ⇒ idle at its prompt.
    std::fs::write(sp.turn_signal(), "xxx").unwrap(); // size 3 > baseline
    let v = ProjectView::read_agent_loop("bot", &sp, true, 1000);
    assert_eq!(v.agent_working, Some(false), "size > baseline ⇒ waiting");
    assert_eq!(
        v.next_action, "waiting · check in 00:05:00",
        "a waiting session still shows an honest countdown to the next nudge"
    );
}

#[test]
fn working_vs_waiting_is_unknown_without_the_hook_and_keeps_the_running_default() {
    // No signal file (the hook never fired) or no baseline (a pre-m73 ledger) ⇒
    // `agent_working` is None, and the row keeps the m72 default of reading as a
    // monitoring/running session rather than guessing idle.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    let cadence_end = 1000 + 300;
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = JobRun::Monitoring { until: cadence_end };

    // Baseline set, but the signal file is absent ⇒ can't tell ⇒ None, neutral text.
    l.turn_count_at_nudge = Some(2);
    job::save(&sp, &l).unwrap();
    let v = ProjectView::read_agent_loop("bot", &sp, true, 1000);
    assert_eq!(v.agent_working, None, "no signal file ⇒ unknown");
    assert_eq!(v.next_action, "monitoring · check in 00:05:00");

    // No baseline at all (a pre-m73 ledger) ⇒ still None even with a signal file present.
    std::fs::create_dir_all(sp.daemon_dir()).unwrap();
    std::fs::write(sp.turn_signal(), "xxx").unwrap();
    l.turn_count_at_nudge = None;
    job::save(&sp, &l).unwrap();
    let v = ProjectView::read_agent_loop("bot", &sp, true, 1000);
    assert_eq!(v.agent_working, None, "no baseline ⇒ unknown");
    assert_eq!(v.next_action, "monitoring · check in 00:05:00");
}

#[test]
fn working_vs_waiting_is_only_read_for_a_monitoring_run() {
    // `agent_working` is a Monitoring-only notion: an Idle/Running/Blocked ledger reports
    // None regardless of any stray signal file, so only a session pmd is actively driving
    // ever flips between working and waiting.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    std::fs::create_dir_all(sp.daemon_dir()).unwrap();
    std::fs::write(sp.turn_signal(), "xxxx").unwrap();
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.turn_count_at_nudge = Some(1);
    // run stays Idle (fresh), so this is not a driven row.
    job::save(&sp, &l).unwrap();
    let v = ProjectView::read_agent_loop("bot", &sp, true, 1000);
    assert_eq!(
        v.agent_working, None,
        "a non-Monitoring run has no working sub-state"
    );
}

#[test]
fn a_standard_row_shows_no_pmd_cadence_hint_and_no_working_substate() {
    // m74. A STANDARD (human-driven) session is never nudged by pmd (`daemon::pmd_drives_row`),
    // so its ledger `run` is a FROZEN snapshot and any "monitoring · check in Ns" /
    // working-vs-waiting hint would promise pmd activity that never comes (user: *"when i use
    // standard, the status on the top bar doesn't really reflect correctly"*). So read_agent_loop
    // reports `agent_working: None` and a `next_action` of "you drive it" for every non-blocked
    // run — even a leftover `Monitoring` from a former Autopilot stint — while a real blocked ask
    // still names itself.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(sp.state_dir()).unwrap();
    state::write_json_atomic(
        &sp.config(),
        &Config {
            autonomy: Tier::Standard,
            step_timeout_s: 100,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();

    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);

    // The normal never-driven Standard state (ledger Idle).
    job::save(&sp, &l).unwrap();
    let v = ProjectView::read_agent_loop("bot", &sp, true, 1000);
    assert_eq!(v.next_action, "you drive it");
    assert_eq!(v.agent_working, None);

    // A leftover Monitoring from a former Autopilot stint — WITH a turn baseline and a signal
    // file that would read "working" on an autopilot row — must NOT resurrect a pmd countdown or
    // a working/waiting sub-state on a Standard row.
    l.run = JobRun::Monitoring { until: 1_000_000 };
    l.turn_count_at_nudge = Some(3);
    job::save(&sp, &l).unwrap();
    std::fs::create_dir_all(sp.daemon_dir()).unwrap();
    std::fs::write(sp.turn_signal(), "xxx").unwrap(); // size 3 == baseline ⇒ "working" if driven
    let v = ProjectView::read_agent_loop("bot", &sp, true, 1000);
    assert_eq!(
        v.next_action, "you drive it",
        "no pmd check-in countdown on a Standard row"
    );
    assert_eq!(
        v.agent_working, None,
        "working/waiting is a pmd-driven notion, off for Standard"
    );

    // A stale Running ledger — same honest hint, not "working".
    l.run = JobRun::Running {
        seq: 0,
        session: "x".into(),
        deadline: 9999,
    };
    job::save(&sp, &l).unwrap();
    assert_eq!(
        ProjectView::read_agent_loop("bot", &sp, true, 1000).next_action,
        "you drive it"
    );

    // A REAL blocked ask still names itself (posture drives the glyph; next_action names the kind).
    l.run = JobRun::Blocked {
        stop_ids: vec!["s".into()],
        since: 1000,
    };
    l.open_stops = vec![OpenStop {
        id: "s".into(),
        kind: StopKind::Publish,
        pane_dialog: None,
        channel: None,
        context_ref: None,
        question: None,
        options: vec![],
        authorized_responders: vec![],
        message_id: None,
        first_posted: 1000,
        last_polled: None,
        last_seen_reply_ts: None,
        status: StopStatus::AwaitingReply,
    }];
    job::save(&sp, &l).unwrap();
    let v = ProjectView::read_agent_loop("bot", &sp, true, 1000);
    assert_eq!(v.posture, Posture::NeedsYou);
    assert_eq!(v.next_action, "blocked · publish");

    l.open_stops.clear();
    job::save(&sp, &l).unwrap();
    assert_eq!(
        ProjectView::read_agent_loop("bot", &sp, true, 1000).next_action,
        "blocked · decision"
    );
}
