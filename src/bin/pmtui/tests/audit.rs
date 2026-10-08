use super::*;

fn reported_turn(id: u64, started_at: Epoch, marker_seq: u64) -> job::TurnTrace {
    job::TurnTrace {
        id,
        started_at,
        marker_baseline: marker_seq.saturating_sub(1),
        report_generation_baseline: Some(marker_seq.saturating_sub(1)),
        trigger: job::TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: false,
        },
        outcome: job::TurnOutcome::Reported {
            at: started_at + 42,
            report_generation: marker_seq,
            marker_seq,
            state: job::WakeState::Working,
            disposition: job::TurnDisposition::Working,
            status: Some("tests are green".into()),
            next_step: Some("update the docs".into()),
        },
    }
}

#[test]
fn audit_tab_switch_preserves_each_scroll_position() {
    let mut app = app_with(vec![autopilot_loop_view("alpha")], UiMode::Normal);
    app.open_decisions();
    let UiMode::Decisions {
        tab,
        scroll,
        other_scroll,
        ..
    } = &mut app.mode
    else {
        panic!("v should open the audit");
    };
    assert_eq!(*tab, AuditTab::Decisions);
    *scroll = 7;
    *other_scroll = 3;

    handle_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(matches!(
        app.mode,
        UiMode::Decisions {
            tab: AuditTab::Turns,
            scroll: 3,
            other_scroll: 7,
            ..
        }
    ));

    handle_key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert!(matches!(
        app.mode,
        UiMode::Decisions {
            tab: AuditTab::Decisions,
            scroll: 7,
            other_scroll: 3,
            ..
        }
    ));
}

#[test]
fn turns_tab_correlates_nudges_reports_and_held_wakes() {
    let mut view = autopilot_loop_view("alpha");
    view.turn_trace = vec![
        reported_turn(1, 100, 7),
        job::TurnTrace {
            id: 2,
            started_at: 200,
            marker_baseline: 7,
            report_generation_baseline: Some(7),
            trigger: job::TurnTrigger::Heartbeat {
                pending_context: true,
                marker_recovery: true,
            },
            outcome: job::TurnOutcome::AwaitingReport,
        },
    ];
    view.autopilot_events = vec![
        job::AutopilotEvent {
            at: 90,
            count: 2,
            kind: job::AutopilotEventKind::Nudged,
        },
        job::AutopilotEvent {
            at: 100,
            count: 1,
            kind: job::AutopilotEventKind::Nudged,
        },
        job::AutopilotEvent {
            at: 180,
            count: 4,
            kind: job::AutopilotEventKind::Held(job::HoldReason::Busy),
        },
    ];
    let mut app = app_with(vec![view], UiMode::Normal);
    app.open_decisions();
    handle_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);

    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);

    assert!(
        screen.contains("[Turns]") && screen.contains("Decisions"),
        "{screen}"
    );
    assert!(
        screen.contains("turn #1") && screen.contains("report #7"),
        "{screen}"
    );
    assert!(
        screen.contains("tests are green") && screen.contains("next: update the docs"),
        "{screen}"
    );
    assert!(
        screen.contains("held ×4") && screen.contains("agent working"),
        "{screen}"
    );
    assert!(screen.contains("nudged ×2"), "{screen}");
    assert_eq!(
        screen.matches("nudged").count(),
        1,
        "the mirrored event at the first traced turn is hidden but older history remains: {screen}"
    );
    assert!(
        screen.contains("turn #2")
            && screen.contains("pending answer")
            && screen.contains("marker recovery")
            && screen.contains("awaiting report"),
        "{screen}"
    );
}

#[test]
fn monitoring_report_and_legacy_event_text_wrap_without_losing_the_tail() {
    let mut view = autopilot_loop_view("alpha");
    view.turn_trace = vec![job::TurnTrace {
        id: 4,
        started_at: 200,
        marker_baseline: 3,
        report_generation_baseline: Some(3),
        trigger: job::TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: false,
        },
        outcome: job::TurnOutcome::Reported {
            at: 242,
            report_generation: 4,
            marker_seq: 4,
            state: job::WakeState::Monitoring,
            disposition: job::TurnDisposition::Monitoring,
            status: Some(
                "monitoring the migration while the detached validator checks every package STATUS_TAIL_VISIBLE"
                    .into(),
            ),
            next_step: Some(
                "inspect the validator output and reconcile every remaining warning NEXT_TAIL_VISIBLE"
                    .into(),
            ),
        },
    }];
    view.autopilot_events = vec![job::AutopilotEvent {
        at: 100,
        count: 1,
        kind: job::AutopilotEventKind::Reported(Some(
            "legacy monitoring detail also keeps its final words EVENT_TAIL_VISIBLE".into(),
        )),
    }];
    let app = app_with(
        vec![view],
        UiMode::Decisions {
            tab: AuditTab::Turns,
            scroll: 0,
            other_scroll: 0,
            since: None,
            id: "alpha".into(),
        },
    );

    let mut terminal = Terminal::new(TestBackend::new(56, 26)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);

    assert!(screen.contains("STATUS_TAIL_VISIBLE"), "{screen}");
    assert!(screen.contains("NEXT_TAIL_VISIBLE"), "{screen}");
    assert!(screen.contains("EVENT_TAIL_VISIBLE"), "{screen}");
}

#[test]
fn turns_tab_is_useful_when_tiny_and_safe_when_zero_sized() {
    let mut view = autopilot_loop_view("alpha");
    view.turn_trace = vec![reported_turn(1, 100, 7)];
    let app = app_with(
        vec![view],
        UiMode::Decisions {
            tab: AuditTab::Turns,
            scroll: usize::MAX,
            other_scroll: 0,
            since: None,
            id: "alpha".into(),
        },
    );

    let mut tiny = Terminal::new(TestBackend::new(12, 4)).unwrap();
    tiny.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&tiny);
    assert!(
        screen.contains("Audit") || screen.contains("turn"),
        "{screen}"
    );

    let mut zero = Terminal::new(TestBackend::new(1, 1)).unwrap();
    zero.draw(|frame| render(frame, &app)).unwrap();
}

#[test]
fn turn_render_helpers_cover_every_wire_label_and_size_bucket() {
    assert_eq!(wake_label(job::WakeState::Working), "working");
    assert_eq!(wake_label(job::WakeState::Monitoring), "monitoring");
    assert_eq!(wake_label(job::WakeState::Blocked), "blocked");

    for (disposition, label) in [
        (job::TurnDisposition::Working, "working"),
        (job::TurnDisposition::Monitoring, "monitoring"),
        (job::TurnDisposition::Reviewing, "reviewing"),
        (job::TurnDisposition::AutoFlow, "auto-flow"),
        (job::TurnDisposition::Escalated, "needs you"),
        (job::TurnDisposition::Interrupted, "interrupted"),
        (job::TurnDisposition::Stalled, "stalled"),
    ] {
        assert_eq!(disposition_label(disposition), label);
    }
    for (reason, label) in [
        (job::TurnNoReportReason::Superseded, "superseded"),
        (job::TurnNoReportReason::Relaunched, "worker relaunched"),
        (
            job::TurnNoReportReason::TerminalUnavailable,
            "terminal unavailable",
        ),
    ] {
        assert_eq!(no_report_label(reason), label);
    }

    assert_eq!(duration(-1), "0s");
    assert_eq!(duration(59), "59s");
    assert_eq!(duration(61), "1m01s");
    assert_eq!(duration(3_661), "1h01m");
    assert_eq!(clipped("abc", 0), "");
    assert_eq!(clipped("abc", 1), "…");
    assert_eq!(clipped("abc", 3), "abc");
    assert_eq!(clipped("abcdef", 4), "abc…");
}

#[test]
fn turns_tab_renders_no_report_variants_events_divider_and_scroll_state() {
    let mut view = autopilot_loop_view("alpha");
    view.turn_trace = [
        job::TurnNoReportReason::Superseded,
        job::TurnNoReportReason::Relaunched,
        job::TurnNoReportReason::TerminalUnavailable,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, reason)| job::TurnTrace {
        id: index as u64 + 1,
        started_at: 100 + index as i64 * 100,
        marker_baseline: index as u64,
        report_generation_baseline: Some(index as u64),
        trigger: job::TurnTrigger::Heartbeat {
            pending_context: index == 0,
            marker_recovery: index == 1,
        },
        outcome: job::TurnOutcome::NoReport {
            at: 110 + index as i64 * 100,
            reason,
        },
    })
    .collect();
    view.autopilot_events = vec![
        job::AutopilotEvent {
            at: 550,
            count: 1,
            kind: job::AutopilotEventKind::Launched,
        },
        job::AutopilotEvent {
            at: 560,
            count: 2,
            kind: job::AutopilotEventKind::CadenceChanged("5m → 1m".into()),
        },
    ];
    let app = app_with(vec![view], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    terminal
        .draw(|frame| {
            render_turns(frame, &app, 0, Some(250), "alpha", frame.area());
        })
        .unwrap();
    let newest = screen_text(&terminal);
    assert!(newest.contains("new since"), "{newest}");

    terminal
        .draw(|frame| {
            render_turns(frame, &app, 3, Some(250), "alpha", frame.area());
        })
        .unwrap();
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("worker relaunched") || screen.contains("terminal unavailable"),
        "{screen}"
    );
    assert!(screen.contains("↑2"), "{screen}");
    assert!(
        screen.contains("wheel/j/k"),
        "scroll hint missing: {screen}"
    );
}

#[test]
fn turns_tab_falls_back_to_legacy_events_and_handles_missing_session() {
    let mut view = autopilot_loop_view("alpha");
    view.autopilot_events = vec![
        job::AutopilotEvent {
            at: 100,
            count: 1,
            kind: job::AutopilotEventKind::Nudged,
        },
        job::AutopilotEvent {
            at: 110,
            count: 1,
            kind: job::AutopilotEventKind::Reported(None),
        },
    ];
    let app = app_with(vec![view], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    terminal
        .draw(|frame| {
            render_turns(frame, &app, 0, None, "alpha", frame.area());
        })
        .unwrap();
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("nudged") && screen.contains("reported"),
        "{screen}"
    );

    terminal
        .draw(|frame| {
            render_turns(frame, &app, 0, None, "missing", frame.area());
        })
        .unwrap();
    assert!(screen_text(&terminal).contains("nothing yet"));
}
