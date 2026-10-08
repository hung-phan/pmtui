use super::*;
use crate::job::{AgentLoopState, WakeReport, WakeState};
use crate::registry::Engine;

fn report(seq: u64, state: WakeState) -> WakeReport {
    WakeReport {
        state,
        seq,
        stops: Vec::new(),
        next_check_s: None,
        cadence_s: None,
        status: Some("implemented the first slice".into()),
        next_step: Some("run the integration suite".into()),
        conversation_id: None,
    }
}

#[test]
fn successful_nudge_turn_closes_on_the_next_fresh_report() {
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    let id = ledger.start_turn(
        20,
        TurnTrigger::Heartbeat {
            pending_context: true,
            marker_recovery: false,
        },
    );

    assert_eq!(id, 1);
    assert!(matches!(
        ledger.turn_trace[0].outcome,
        TurnOutcome::AwaitingReport
    ));
    assert!(!ledger.finish_turn_from_report(
        25,
        &report(0, WakeState::Working),
        TurnDisposition::Working,
    ));
    ledger.report_generation = 1;
    assert!(ledger.finish_turn_from_report(
        30,
        &report(1, WakeState::Working),
        TurnDisposition::Working,
    ));

    assert!(matches!(
        &ledger.turn_trace[0].outcome,
        TurnOutcome::Reported {
            at: 30,
            marker_seq: 1,
            state: WakeState::Working,
            disposition: TurnDisposition::Working,
            status: Some(status),
            next_step: Some(next_step),
            ..
        } if status == "implemented the first slice"
            && next_step == "run the integration suite"
    ));
}

#[test]
fn a_new_turn_supersedes_an_unreported_turn_and_ids_are_monotonic() {
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    ledger.start_turn(
        20,
        TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: false,
        },
    );
    let second = ledger.start_turn(
        40,
        TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: true,
        },
    );

    assert_eq!(second, 2);
    assert!(matches!(
        ledger.turn_trace[0].outcome,
        TurnOutcome::NoReport {
            at: 40,
            reason: TurnNoReportReason::Superseded,
        }
    ));
    assert!(matches!(
        ledger.turn_trace[1].outcome,
        TurnOutcome::AwaitingReport
    ));
}

#[test]
fn turn_trace_is_bounded_and_text_is_capped() {
    let mut ledger = AgentLoopState::fresh(Engine::Codex, None, 10);
    for n in 0..(TURN_TRACE_MAX + 5) {
        ledger.start_turn(
            20 + n as i64,
            TurnTrigger::Heartbeat {
                pending_context: false,
                marker_recovery: false,
            },
        );
    }
    let mut long = report(ledger.last_marker_seq + 1, WakeState::Monitoring);
    long.status = Some("x".repeat(1_000));
    long.next_step = Some("y".repeat(1_000));
    ledger.report_generation = 1;
    ledger.finish_turn_from_report(200, &long, TurnDisposition::Monitoring);

    assert_eq!(ledger.turn_trace.len(), TURN_TRACE_MAX);
    assert_eq!(ledger.turn_trace.first().map(|turn| turn.id), Some(6));
    let TurnOutcome::Reported {
        status, next_step, ..
    } = &ledger.turn_trace.last().expect("last turn").outcome
    else {
        panic!("last turn should be reported");
    };
    assert!(status.as_ref().is_some_and(|value| value.len() < 1_000));
    assert!(next_step.as_ref().is_some_and(|value| value.len() < 1_000));
}

#[test]
fn malformed_trace_entries_are_dropped_without_losing_the_ledger() {
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    ledger.start_turn(
        20,
        TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: false,
        },
    );
    let mut value = serde_json::to_value(&ledger).expect("serialize ledger");
    value["turn_trace"] = serde_json::json!([
        value["turn_trace"][0].clone(),
        {"future_turn_shape": true}
    ]);

    let loaded: AgentLoopState = serde_json::from_value(value).expect("load operational ledger");
    assert_eq!(loaded.turn_trace.len(), 1);
    assert_eq!(loaded.turn_trace[0].id, 1);
}

#[test]
fn malformed_trace_containers_are_treated_as_empty_audit_data() {
    for malformed in [
        serde_json::json!({"not": "an array"}),
        serde_json::json!("not an array"),
        serde_json::Value::Null,
    ] {
        let ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
        let mut value = serde_json::to_value(&ledger).expect("serialize ledger");
        value["turn_trace"] = malformed;

        let loaded: AgentLoopState =
            serde_json::from_value(value).expect("load operational ledger");
        assert!(loaded.turn_trace.is_empty());
        assert_eq!(loaded.engine, Engine::Claude);
    }
}

#[test]
fn no_report_completion_is_explicit_and_requires_a_pending_turn() {
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    assert!(!ledger.finish_turn_without_report(20, TurnNoReportReason::Relaunched));
    assert!(!ledger.finish_turn_from_report(
        20,
        &report(1, WakeState::Working),
        TurnDisposition::Working,
    ));

    ledger.start_turn(
        30,
        TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: true,
        },
    );
    assert!(ledger.finish_turn_without_report(40, TurnNoReportReason::Relaunched));
    assert!(!ledger.finish_turn_without_report(50, TurnNoReportReason::TerminalUnavailable));
    assert!(matches!(
        ledger.turn_trace[0].outcome,
        TurnOutcome::NoReport {
            at: 40,
            reason: TurnNoReportReason::Relaunched,
        }
    ));
}

#[test]
fn reviewed_turns_finish_only_with_review_outcomes() {
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    ledger.start_turn(
        20,
        TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: false,
        },
    );
    ledger.report_generation = 1;
    assert!(ledger.finish_turn_from_report(
        30,
        &report(1, WakeState::Blocked),
        TurnDisposition::Reviewing,
    ));
    assert!(!ledger.finish_turn_review(2, TurnReviewOutcome::Escalated));
    assert!(ledger.finish_turn_review(1, TurnReviewOutcome::AutoFlow));
    assert!(!ledger.finish_turn_review(1, TurnReviewOutcome::Escalated));
    assert!(matches!(
        ledger.turn_trace[0].outcome,
        TurnOutcome::Reported {
            disposition: TurnDisposition::AutoFlow,
            ..
        }
    ));
}

#[test]
fn legacy_future_marker_baseline_does_not_block_pmd_generation_correlation() {
    let mut ledger = AgentLoopState::fresh(Engine::Codex, None, 10);
    ledger.report_generation = 438;
    ledger.turn_trace.push(TurnTrace {
        id: 1,
        started_at: 20,
        marker_baseline: 1_790_140_600,
        report_generation_baseline: None,
        trigger: TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: false,
        },
        outcome: TurnOutcome::AwaitingReport,
    });

    assert!(ledger.finish_turn_from_report(
        30,
        &report(1_789_845_066, WakeState::Working),
        TurnDisposition::Working,
    ));
    assert!(matches!(
        ledger.turn_trace[0].outcome,
        TurnOutcome::Reported {
            report_generation: 438,
            marker_seq: 1_789_845_066,
            ..
        }
    ));
}

#[test]
fn lenient_loading_bounds_an_oversized_trace() {
    let ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    let oversized = (1..=(TURN_TRACE_MAX as u64 + 6))
        .map(|id| TurnTrace {
            id,
            started_at: 20 + id as i64,
            marker_baseline: id - 1,
            report_generation_baseline: Some(id - 1),
            trigger: TurnTrigger::Heartbeat {
                pending_context: false,
                marker_recovery: false,
            },
            outcome: TurnOutcome::AwaitingReport,
        })
        .collect::<Vec<_>>();
    let mut value = serde_json::to_value(&ledger).expect("serialize ledger");
    value["turn_trace"] = serde_json::to_value(oversized).expect("serialize oversized trace");

    let loaded: AgentLoopState = serde_json::from_value(value).expect("load bounded ledger");
    assert_eq!(loaded.turn_trace.len(), TURN_TRACE_MAX);
    assert_eq!(loaded.turn_trace.first().map(|turn| turn.id), Some(7));
    assert_eq!(
        loaded.turn_trace.last().map(|turn| turn.id),
        Some(TURN_TRACE_MAX as u64 + 6)
    );
}
