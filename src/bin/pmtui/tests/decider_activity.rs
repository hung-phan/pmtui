use super::*;
use agent_manager::worker;

fn consulting_view(now: Epoch) -> ProjectView {
    let mut view = view("alpha", Posture::Monitoring, Vec::new());
    view.next_action = "monitoring · check in 00:10:00".into();
    view.advice_inflight = Some(job::ParkedAdvice {
        seq: 9,
        stop_ids: vec!["decision-1".into()],
        pane_dialog: false,
    });
    view.decider_live = true;
    view.advice_queue = vec![job::QueuedAdvice {
        stop_id: "decision-2".into(),
        report_seq: 4,
        draft: worker::StopDraft {
            kind: pmstate::StopKind::Ambiguity,
            risk_class: RiskClass::Low,
            question: "use the compact format?".into(),
            options: vec!["Use it".into(), "Keep the current format".into()],
            context_ref: None,
            effect: worker::StopEffect::default(),
        },
    }];
    view.decider_runs.push(job::DeciderRun {
        seq: 9,
        started_at: now - 65,
        finished_at: None,
        engine: Engine::Claude,
        model: None,
        target: job::DeciderTarget::Marker,
        question: "which parser should the worker use?".into(),
        options: vec!["Use serde".into(), "Keep the custom parser".into()],
        reported_kind: Some(pmstate::StopKind::Ambiguity),
        effect: None,
        policy: job::DeciderPolicy {
            kind: pmstate::StopKind::Ambiguity,
            labelled_risk: RiskClass::Low,
            effective_risk: RiskClass::Low,
        },
        outcome: job::DeciderOutcome::Consulting,
    });
    view
}

#[test]
fn an_inflight_decider_is_active_and_has_a_primary_status() {
    let now = 10_000;
    let mut view = consulting_view(now);
    view.agent_working = Some(false);

    assert_eq!(
        status_category(&view),
        2,
        "the worker glyph remains idle while the detached decider reviews"
    );
    assert_eq!(primary_status_label(&view), "reviewing");
    assert_eq!(
        current_decider_activity(&view, now).as_deref(),
        Some("decision #9 · reviewing 00:01:05 elapsed · 1 queued")
    );
}

#[test]
fn human_attention_outranks_decider_activity() {
    let now = 10_000;
    let mut view = consulting_view(now);
    view.posture = Posture::NeedsYou;

    assert_eq!(primary_status_label(&view), "needs you");
}

#[test]
fn queued_and_recovered_consults_have_truthful_fallback_text() {
    let now = 10_000;
    let mut view = consulting_view(now);
    view.decider_runs.clear();
    assert_eq!(
        current_decider_activity(&view, now).as_deref(),
        Some("decision #9 · reviewing in progress · 1 queued")
    );

    view.advice_inflight = None;
    assert_eq!(
        current_decider_activity(&view, now).as_deref(),
        Some("1 decision queued")
    );
    view.advice_queue.push(view.advice_queue[0].clone());
    assert_eq!(
        current_decider_activity(&view, now).as_deref(),
        Some("2 decisions queued")
    );
    view.advice_queue.clear();
    assert_eq!(current_decider_activity(&view, now), None);
}

#[test]
fn durable_consult_debt_without_a_live_terminal_is_pending() {
    let now = 10_000;
    let mut view = consulting_view(now);
    view.decider_live = false;

    assert_eq!(primary_status_label(&view), "review pending");
    assert_eq!(
        current_decider_activity(&view, now).as_deref(),
        Some("decision #9 · pending 00:01:05 elapsed · 1 queued")
    );
}

#[test]
fn corrupt_future_or_extreme_decider_timestamps_cannot_overflow() {
    let mut view = consulting_view(0);
    view.decider_runs[0].started_at = i64::MIN;

    let activity = current_decider_activity(&view, i64::MAX).expect("decider activity");

    assert!(
        activity.starts_with("decision #9 · reviewing "),
        "{activity}"
    );
    assert!(activity.ends_with("elapsed · 1 queued"), "{activity}");
}

#[test]
fn the_session_row_and_preview_show_decider_activity() {
    let now = SystemClock.now();
    let app = app_with(vec![consulting_view(now)], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(120, 26)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);

    assert!(screen.contains("reviewing"), "{screen}");
    assert!(screen.contains("decision #9"), "{screen}");
    assert!(screen.contains("1 queued"), "{screen}");
}

#[test]
fn refresh_verifies_the_deterministic_decider_terminal() {
    if !tmux_available() {
        eprintln!("skipping decider refresh test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_agent_loop(dir.path(), "alpha");
    let paths = ProjectPaths::for_session(&root, "alpha");
    set_tier(&paths, Tier::Autopilot);
    let mut ledger = job::load(&paths).unwrap().expect("seeded ledger");
    ledger.advice_inflight = Some(job::ParkedAdvice {
        seq: 9,
        stop_ids: vec!["decision-1".into()],
        pane_dialog: false,
    });
    job::save(&paths, &ledger).unwrap();

    let socket = TmuxSocket::new("pmtui-decider-live");
    let driver = TmuxDriver::with_socket(socket.name());
    let supervisor = tmux::supervisor_session_name("alpha", &root, 9);
    driver
        .launch_interactive(
            &supervisor,
            &root,
            &["sh".to_string()],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.registry_path = registry;
    app.socket = socket.name().to_string();

    app.refresh();
    assert!(app.projects[0].decider_live);

    driver.terminate(&supervisor).unwrap();
    app.refresh();
    assert!(!app.projects[0].decider_live);
}
