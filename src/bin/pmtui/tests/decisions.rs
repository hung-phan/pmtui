//! The DECISION LANE (`v`) and the VERDICT line — the two dashboard additions for reviewing what
//! autopilot decided and seeing at a glance which sessions need you. The lane is PER-SESSION on
//! purpose (a decision only reads in its own session's context), so these tests pin that it shows
//! the SELECTED session's decisions and never mixes in another's.

use super::*;

/// A one-shot autopilot event at wall-clock `at`.
fn ev(at: Epoch, kind: job::AutopilotEventKind) -> job::AutopilotEvent {
    job::AutopilotEvent { at, count: 1, kind }
}

fn queued(id: &str, question: &str) -> job::QueuedAdvice {
    job::QueuedAdvice {
        stop_id: id.into(),
        report_seq: 9,
        draft: agent_manager::worker::StopDraft {
            kind: agent_manager::pmstate::StopKind::Ambiguity,
            effect: agent_manager::worker::StopEffect::default(),
            question: question.into(),
            options: vec!["yes".into(), "no".into()],
            context_ref: None,
            risk_class: RiskClass::Low,
        },
    }
}

fn audit_run(
    seq: u64,
    started_at: Epoch,
    finished_at: Option<Epoch>,
    outcome: job::DeciderOutcome,
) -> job::DeciderRun {
    job::DeciderRun {
        seq,
        started_at,
        finished_at,
        engine: Engine::Claude,
        model: Some("sonnet-audit".into()),
        target: job::DeciderTarget::Marker,
        question: format!("Which formatter should run for decision {seq}?"),
        options: vec!["prettier".into(), "dprint".into()],
        reported_kind: None,
        effect: None,
        policy: job::DeciderPolicy {
            kind: agent_manager::pmstate::StopKind::Ambiguity,
            labelled_risk: RiskClass::Low,
            effective_risk: RiskClass::Medium,
        },
        outcome,
    }
}

#[test]
fn audit_header_names_decider_state_and_lifetime_counters() {
    let mut alpha = autopilot_loop_view("alpha");
    alpha.decider_engine = Some(Engine::Codex);
    alpha.decider_model = Some("gpt-5.5".into());
    alpha.decision_digest = job::DecisionCounters {
        disposed: 12,
        working: 3,
        monitoring: 2,
        auto_flow: 4,
        escalated: 2,
        stalled: 1,
    };
    alpha.advice_inflight = Some(job::ParkedAdvice {
        seq: 9,
        stop_ids: vec!["stop-9".into()],
        pane_dialog: false,
    });
    alpha.advice_queue = vec![
        queued("stop-9", "Which formatter?"),
        queued("stop-10", "Sort imports?"),
    ];
    alpha.decider_runs = vec![audit_run(9, 100, None, job::DeciderOutcome::Consulting)];
    let app = app_with(vec![alpha], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();

    terminal
        .draw(|frame| {
            render_decisions(frame, &app, 0, None, "alpha", frame.area());
        })
        .unwrap();

    let screen = screen_text(&terminal);
    for expected in [
        "codex/gpt-5.5",
        "consulting #9",
        "1 next",
        "disposed 12",
        "working 3",
        "monitoring 2",
        "auto 4",
        "escalated 2",
        "stalled 1",
        "decision queue (2)",
        "Which formatter?",
        "Sort imports?",
    ] {
        assert!(screen.contains(expected), "missing {expected:?}: {screen}");
    }
}

#[test]
fn completed_audit_expands_the_record_without_hiding_compatibility_events() {
    use agent_manager::job::AutopilotEventKind as K;

    let mut alpha = autopilot_loop_view("alpha");
    let mut resolved = audit_run(
        7,
        100,
        Some(105),
        job::DeciderOutcome::Resolved {
            answer: "option 2 - dprint".into(),
            reason: "dprint is already vendored".into(),
        },
    );
    resolved.reported_kind = Some(agent_manager::pmstate::StopKind::Ambiguity);
    resolved.effect = Some(agent_manager::worker::StopEffect::default());
    alpha.decider_runs = vec![resolved];
    alpha.autopilot_events = vec![
        ev(
            105,
            K::SupervisorResolved(Some("legacy duplicate summary".into())),
        ),
        ev(
            106,
            K::Escalated(Some("keep this non-decider event".into())),
        ),
    ];
    let app = app_with(vec![alpha], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

    terminal
        .draw(|frame| {
            render_decisions(frame, &app, 0, None, "alpha", frame.area());
        })
        .unwrap();

    let screen = screen_text(&terminal);
    for expected in [
        "Asked",
        "Which formatter should run for decision 7?",
        "1) prettier",
        "2) dprint",
        "Reported",
        "scope=unknown",
        "Policy",
        "ambiguity",
        "low -> medium",
        "Result",
        "option 2 - dprint",
        "Reason",
        "dprint is already vendored",
        "Duration",
        "5s",
        "keep this non-decider event",
    ] {
        assert!(screen.contains(expected), "missing {expected:?}: {screen}");
    }
    assert!(screen.contains("legacy duplicate summary"), "{screen}");
}

#[test]
fn deterministic_preflight_skip_explains_why_no_decider_was_called() {
    let mut alpha = autopilot_loop_view("alpha");
    let mut skipped = audit_run(
        3,
        100,
        Some(100),
        job::DeciderOutcome::Skipped {
            reason: "typed effect `scope=external, reversibility=unknown, authority=privileged` \
                 requires human ownership; no decider was called"
                .into(),
            reported_kind: Some(agent_manager::pmstate::StopKind::ExpertNeeded),
            effect: Some(agent_manager::worker::StopEffect {
                scope: agent_manager::worker::EffectScope::External,
                reversibility: agent_manager::worker::EffectReversibility::Unknown,
                authority: agent_manager::worker::EffectAuthority::Privileged,
                unrecognized_metadata: false,
            }),
        },
    );
    skipped.policy = job::DeciderPolicy {
        kind: agent_manager::pmstate::StopKind::Capability,
        labelled_risk: RiskClass::Low,
        effective_risk: RiskClass::Hard,
    };
    alpha.decider_runs = vec![skipped];
    alpha.autopilot_events = vec![ev(
        100,
        job::AutopilotEventKind::Escalated(Some("legacy typed-policy escalation".into())),
    )];
    let app = app_with(vec![alpha], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();

    terminal
        .draw(|frame| {
            render_decisions(frame, &app, 0, None, "alpha", frame.area());
        })
        .unwrap();

    let screen = screen_text(&terminal);
    for expected in [
        "preflight #3",
        "Reported",
        "expert_needed",
        "capability",
        "low -> hard",
        "Result    not called",
        "authority=privileged",
        "no decider was called",
        "0s",
    ] {
        assert!(screen.contains(expected), "missing {expected:?}: {screen}");
    }
    assert!(
        !screen.contains("auto-flow"),
        "a typed-policy escalation was never auto-flow: {screen}"
    );
    assert!(
        screen.contains("legacy typed-policy escalation"),
        "timestamp-only dedup must not hide an unrelated escalation: {screen}"
    );
}

#[test]
fn unfinished_audits_distinguish_live_consults_from_interrupted_ones() {
    let mut live = autopilot_loop_view("live");
    live.advice_inflight = Some(job::ParkedAdvice {
        seq: 4,
        stop_ids: vec!["stop-4".into()],
        pane_dialog: false,
    });
    live.decider_runs = vec![audit_run(4, 100, None, job::DeciderOutcome::Consulting)];
    let mut interrupted = autopilot_loop_view("interrupted");
    interrupted.decider_runs = vec![audit_run(5, 200, None, job::DeciderOutcome::Consulting)];
    let app = app_with(vec![live, interrupted], UiMode::Normal);

    let mut live_terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    live_terminal
        .draw(|frame| {
            render_decisions(frame, &app, 0, None, "live", frame.area());
        })
        .unwrap();
    let live_screen = screen_text(&live_terminal);
    assert!(live_screen.contains("consulting #4"), "{live_screen}");
    assert!(
        live_screen.contains("Result    consulting"),
        "{live_screen}"
    );
    assert!(live_screen.contains("awaiting verdict"), "{live_screen}");
    assert!(!live_screen.contains("interrupted"), "{live_screen}");

    let mut interrupted_terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    interrupted_terminal
        .draw(|frame| {
            render_decisions(frame, &app, 0, None, "interrupted", frame.area());
        })
        .unwrap();
    let interrupted_screen = screen_text(&interrupted_terminal);
    assert!(interrupted_screen.contains("idle"), "{interrupted_screen}");
    assert!(
        interrupted_screen.contains("Result    interrupted"),
        "{interrupted_screen}"
    );
    assert!(
        interrupted_screen.contains("escalate"),
        "{interrupted_screen}"
    );
}

#[test]
fn non_resolved_audit_does_not_hide_same_second_resolution_event() {
    let mut alpha = autopilot_loop_view("alpha");
    alpha.decider_runs = vec![audit_run(
        4,
        100,
        Some(105),
        job::DeciderOutcome::Refused {
            reason: "not enough authority".into(),
        },
    )];
    alpha.autopilot_events = vec![ev(
        105,
        job::AutopilotEventKind::SupervisorResolved(Some("separate resolution event".into())),
    )];
    let app = app_with(vec![alpha], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();

    terminal
        .draw(|frame| {
            render_decisions(frame, &app, 0, None, "alpha", frame.area());
        })
        .unwrap();

    let screen = screen_text(&terminal);
    assert!(screen.contains("not enough authority"), "{screen}");
    assert!(screen.contains("separate resolution event"), "{screen}");
}

#[test]
fn audit_view_tail_follows_and_is_safe_on_a_tiny_terminal() {
    let mut alpha = autopilot_loop_view("alpha");
    alpha.decider_runs = vec![
        audit_run(
            1,
            100,
            Some(101),
            job::DeciderOutcome::Refused {
                reason: "oldest audit reason".into(),
            },
        ),
        audit_run(
            2,
            200,
            Some(202),
            job::DeciderOutcome::Failed {
                reason: "newest audit reason".into(),
            },
        ),
    ];
    let app = app_with(vec![alpha], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    let mut max_scroll = 0;

    terminal
        .draw(|frame| {
            max_scroll = render_decisions(frame, &app, 0, None, "alpha", frame.area());
        })
        .unwrap();
    let tail = screen_text(&terminal);
    assert!(max_scroll > 0, "two expanded audits should scroll");
    assert!(tail.contains("newest audit reason"), "{tail}");
    assert!(tail.contains("escalate"), "{tail}");
    assert!(!tail.contains("oldest audit reason"), "{tail}");

    terminal
        .draw(|frame| {
            render_decisions(frame, &app, usize::MAX, None, "alpha", frame.area());
        })
        .unwrap();
    assert!(
        screen_text(&terminal).contains("oldest audit reason"),
        "scrolling to the top reveals the oldest audit"
    );

    let mut tiny = Terminal::new(TestBackend::new(12, 4)).unwrap();
    tiny.draw(|frame| {
        render_decisions(frame, &app, usize::MAX, None, "alpha", frame.area());
    })
    .expect("tiny decision audit remains panic-free");
}

#[test]
fn audit_renderer_handles_every_outcome_policy_label_and_zero_width() {
    use agent_manager::pmstate::StopKind;

    let kinds = [
        StopKind::Publish,
        StopKind::Merge,
        StopKind::ConfirmDone,
        StopKind::Ambiguity,
        StopKind::Stuck,
        StopKind::ExpertNeeded,
        StopKind::WorkerStuck,
        StopKind::Capability,
    ];
    let mut alpha = autopilot_loop_view("alpha");
    alpha.decider_runs = kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let mut run = audit_run(
                index as u64 + 1,
                100,
                Some(
                    100 + match index {
                        0 => 3_661,
                        1 => 61,
                        2 => 3,
                        _ => 1,
                    },
                ),
                match index {
                    0 => job::DeciderOutcome::Recovered {
                        answer: "recovered approval".into(),
                    },
                    1 => job::DeciderOutcome::Interrupted {
                        reason: "the worker moved on".into(),
                    },
                    _ => job::DeciderOutcome::Resolved {
                        answer: "a deliberately long answer that wraps in a narrow audit pane"
                            .into(),
                        reason: "verified from bounded evidence".into(),
                    },
                },
            );
            run.policy.kind = kind;
            run.policy.labelled_risk = RiskClass::Hard;
            run.policy.effective_risk = RiskClass::Hard;
            run.question =
                "A deliberately long audit question that must wrap under its field label".into();
            run
        })
        .collect();
    let mut live_dialog = audit_run(99, SystemClock.now(), None, job::DeciderOutcome::Consulting);
    live_dialog.target = job::DeciderTarget::Dialog;
    alpha.decider_runs.push(live_dialog);
    alpha.advice_inflight = Some(job::ParkedAdvice {
        seq: 99,
        stop_ids: Vec::new(),
        pane_dialog: true,
    });
    alpha.decider_model = Some("company.bedrock.global.anthropic.claude-sonnet-4-6".into());
    alpha.autopilot_events = vec![ev(300, job::AutopilotEventKind::Launched)];
    let app = app_with(vec![alpha], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(72, 160)).unwrap();

    terminal
        .draw(|frame| {
            render_decisions(frame, &app, 0, None, "alpha", frame.area());
        })
        .unwrap();
    let screen = screen_text(&terminal);
    for expected in [
        "all 0 · work 0 · watch 0 · auto 0 · ask 0 · stuck 0",
        "claude/claude-sonnet-4-6",
        "recovered approval",
        "the worker moved on",
        "dialog",
        "3s",
        "1m 01s",
        "1h 01m 01s",
        "publish",
        "merge",
        "confirm_done",
        "stuck",
        "expert_needed",
        "worker_stuck",
        "capability",
        "hard -> hard",
        "launched",
        "Home/End",
    ] {
        assert!(screen.contains(expected), "missing {expected:?}: {screen}");
    }

    terminal
        .draw(|frame| {
            render_decisions(frame, &app, 0, None, "alpha", Rect::new(0, 0, 0, 4));
        })
        .expect("zero-width layout remains panic-free");
}

#[test]
fn is_decision_splits_decisions_from_heartbeat_noise() {
    use agent_manager::job::AutopilotEventKind as K;
    use agent_manager::job::HoldReason;
    // Decisions the human might want to review — pmd acting for them, or the escalation lifecycle.
    for k in [
        K::Launched,
        K::AutoAnswered(None),
        K::SupervisorResolved(None),
        K::CadenceChanged("5m → 1m".into()),
        K::Escalated(None),
        K::Stuck(None),
        K::Answered(None),
    ] {
        assert!(k.is_decision(), "{k:?} should count as a decision");
    }
    // Routine heartbeat activity — noise the lane filters out.
    for k in [K::Nudged, K::Held(HoldReason::Busy), K::Reported(None)] {
        assert!(!k.is_decision(), "{k:?} should NOT count as a decision");
    }
}

#[test]
fn the_lane_shows_only_the_selected_sessions_decisions_oldest_first() {
    use agent_manager::job::AutopilotEventKind as K;
    let mut alpha = autopilot_loop_view("alpha");
    alpha.autopilot_events = vec![
        ev(
            100,
            K::AutoAnswered(Some("chose dprint over prettier".into())),
        ),
        ev(300, K::Escalated(Some("run the migration".into()))),
    ];
    let mut beta = autopilot_loop_view("beta");
    beta.autopilot_events = vec![ev(200, K::AutoAnswered(Some("beta only detail".into())))];
    let mut app = app_with(vec![alpha, beta], UiMode::Normal); // selected = alpha (index 0)
    app.open_decisions();

    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render decision lane");
    let s = screen_text(&t);

    assert!(
        s.contains("Decisions") && s.contains("alpha"),
        "titled for the session: {s}"
    );
    assert!(
        s.contains("chose dprint") && s.contains("run the migration"),
        "alpha's decisions show: {s}"
    );
    assert!(
        !s.contains("beta only detail"),
        "another session's decisions are NOT mixed in: {s}"
    );
    // Oldest first (a log, read top-down): the 100 auto-answer sits ABOVE the 300 escalation.
    let esc = s.find("run the migration").expect("escalation shown");
    let auto = s.find("chose dprint").expect("auto-answer shown");
    assert!(
        auto < esc,
        "oldest decision first, newest at the bottom: {s}"
    );
}

#[test]
fn the_lane_hides_heartbeat_noise() {
    use agent_manager::job::AutopilotEventKind as K;
    use agent_manager::job::HoldReason;
    let mut a = autopilot_loop_view("alpha");
    a.autopilot_events = vec![
        ev(100, K::Nudged),
        ev(200, K::Held(HoldReason::Busy)),
        ev(300, K::AutoAnswered(Some("the one real decision".into()))),
    ];
    let mut app = app_with(vec![a], UiMode::Normal);
    app.open_decisions();
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app)).unwrap();
    let s = screen_text(&t);
    assert!(
        s.contains("the one real decision"),
        "the decision shows: {s}"
    );
    assert!(
        !s.contains("nudged") && !s.contains("held"),
        "heartbeat noise is filtered: {s}"
    );
}

#[test]
fn an_empty_or_unknown_decision_lane_degrades_to_calm_copy() {
    let app = app_with(vec![autopilot_loop_view("alpha")], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    let mut max = usize::MAX;
    terminal
        .draw(|frame| {
            max = render_decisions(frame, &app, 99, Some(100), "missing", frame.area());
        })
        .unwrap();

    let screen = screen_text(&terminal);
    assert_eq!(max, 0);
    assert!(screen.contains("nothing yet"), "{screen}");
    assert!(screen.contains("missing"), "{screen}");
}

#[test]
fn coalesced_decisions_wrap_with_a_hanging_indent() {
    use agent_manager::job::AutopilotEventKind as K;
    let mut alpha = autopilot_loop_view("alpha");
    alpha.autopilot_events = vec![job::AutopilotEvent {
        at: 100,
        count: 12,
        kind: K::Escalated(Some(
            "first line\nwith a deliberately long explanation that wraps".into(),
        )),
    }];
    let app = app_with(vec![alpha], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(52, 12)).unwrap();
    terminal
        .draw(|frame| {
            render_decisions(frame, &app, 3, None, "alpha", frame.area());
        })
        .unwrap();
    let screen = screen_text(&terminal);

    assert!(screen.contains("×12"), "{screen}");
    assert!(screen.contains("first line with a"), "{screen}");
    assert!(
        screen.contains("deliberately long") && screen.contains("explanation that"),
        "continuation rows should preserve the sanitized detail: {screen}"
    );
}

#[test]
fn the_new_since_divider_tracks_the_per_session_watermark() {
    use agent_manager::job::AutopilotEventKind as K;
    let mut a = autopilot_loop_view("alpha");
    a.autopilot_events = vec![ev(500, K::AutoAnswered(Some("recent choice".into())))];
    let mut app = app_with(vec![a], UiMode::Normal);
    // A prior look at 400 → the 500 decision is NEW since.
    app.decisions_seen.insert("alpha".into(), 400);
    app.open_decisions();
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app)).unwrap();
    assert!(
        screen_text(&t).contains("new since"),
        "a decision after the watermark is flagged new"
    );
}

#[test]
fn opening_the_lane_advances_the_sessions_watermark() {
    let mut app = app_with(vec![autopilot_loop_view("alpha")], UiMode::Normal);
    assert!(
        !app.decisions_seen.contains_key("alpha"),
        "no watermark before the first open"
    );
    app.open_decisions();
    assert!(
        app.decisions_seen.contains_key("alpha"),
        "opening the lane records the session as seen"
    );
    // The mode carries the PREVIOUS (absent) watermark, so the first-ever open shows no divider.
    match &app.mode {
        UiMode::Decisions { since, id, .. } => {
            assert_eq!(id, "alpha");
            assert!(
                since.is_none(),
                "first open has no prior look to be new against"
            );
        }
        m => panic!("expected the decision lane, got {m:?}"),
    }
}

#[test]
fn opening_status_counts_structured_audits_and_compatibility_events() {
    use agent_manager::job::AutopilotEventKind as K;

    let mut alpha = autopilot_loop_view("alpha");
    alpha.decider_runs = vec![audit_run(
        3,
        100,
        Some(105),
        job::DeciderOutcome::Resolved {
            answer: "dprint".into(),
            reason: "configured".into(),
        },
    )];
    alpha.autopilot_events = vec![ev(
        105,
        K::SupervisorResolved(Some("legacy duplicate".into())),
    )];
    alpha.advice_queue = vec![queued("stop-next", "Sort imports?")];
    let mut app = app_with(vec![alpha], UiMode::Normal);

    app.open_decisions();

    assert_eq!(
        app.status,
        "reviewing 2 audit rows and 1 queued decision for alpha"
    );
}

#[test]
fn opening_the_lane_without_a_selection_explains_the_required_action() {
    let mut app = app_with(vec![], UiMode::Normal);

    app.open_decisions();

    assert!(matches!(app.mode, UiMode::Normal));
    assert!(
        app.status.contains("nothing is selected")
            && app.status.contains("j/k")
            && app.status.contains('v'),
        "{}",
        app.status
    );
}

#[test]
fn the_verdict_line_names_the_needy_sessions_and_spares_the_calm() {
    let stuck = {
        let mut v = autopilot_loop_view("infra");
        v.posture = Posture::Stuck;
        v
    };
    let needy = {
        let mut v = autopilot_loop_view("api");
        v.posture = Posture::NeedsYou;
        v
    };
    let working = autopilot_loop_view("jokes"); // Working — calm
    let app = app_with(vec![working, needy, stuck], UiMode::Normal);
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    t.draw(|f| render(f, &app)).unwrap();
    let rows = screen_rows(&t);
    let verdict = &rows[1].0; // status bar is row 0; the verdict sits directly beneath it
    assert!(
        verdict.contains("infra") && verdict.contains("api"),
        "the verdict names the needy sessions: {verdict:?}"
    );
    assert!(
        !verdict.contains("jokes"),
        "the calm session is not on the verdict line: {verdict:?}"
    );
    assert!(
        rows[2].0.contains("SESSIONS"),
        "the SESSIONS pane follows the verdict line: {:?}",
        rows[2].0
    );
}

#[test]
fn the_verdict_line_skips_paused_sessions_even_when_their_ledger_is_blocked() {
    // A paused row (enabled=false) can still hold a NeedsYou/Stuck posture from a Blocked ledger,
    // but `status_category` buckets any paused row as idle, so it never enters the need-you COUNT
    // or the CTA chip. The verdict line must agree with that count — name only the enabled needy
    // session, never the paused one (which would otherwise draw with the idle glyph on a
    // "who needs you" line). Regression guard for the enabled-filter bug.
    let live = {
        let mut v = autopilot_loop_view("api");
        v.posture = Posture::NeedsYou;
        v
    };
    let paused = {
        let mut v = autopilot_loop_view("infra");
        v.posture = Posture::Stuck; // ledger still Blocked...
        v.enabled = false; // ...but the human paused it
        v
    };
    let app = app_with(vec![live, paused], UiMode::Normal);
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    t.draw(|f| render(f, &app)).unwrap();
    let rows = screen_rows(&t);
    let verdict = &rows[1].0; // the enabled needy row makes the verdict render
    assert!(
        verdict.contains("api"),
        "the enabled needy session is named: {verdict:?}"
    );
    assert!(
        !verdict.contains("infra"),
        "a paused session is NOT surfaced on the verdict line: {verdict:?}"
    );
}

#[test]
fn the_verdict_line_is_absent_when_all_is_calm() {
    let app = app_with(vec![autopilot_loop_view("jokes")], UiMode::Normal); // Working, nobody needy
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    t.draw(|f| render(f, &app)).unwrap();
    let rows = screen_rows(&t);
    // No verdict row: the body (the SESSIONS pane) starts immediately under the status bar.
    assert!(
        rows[1].0.contains("SESSIONS"),
        "an all-clear fleet spends no row on a verdict: {:?}",
        rows[1].0
    );
}
