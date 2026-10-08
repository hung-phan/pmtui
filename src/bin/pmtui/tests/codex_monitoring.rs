use super::*;

fn set_worker_engine(registry_path: &Path, engine: Engine) {
    let mut registry = Registry::load(registry_path).unwrap();
    registry.projects[0].engine = Some(engine);
    registry.save(registry_path).unwrap();
}

fn accepted_monitoring_ledger(paths: &ProjectPaths, now: i64) {
    let mut ledger = job::load(paths).unwrap().unwrap();
    ledger.run = job::JobRun::Monitoring { until: now + 300 };
    ledger.turn_count_at_nudge = Some(1);
    ledger.nudged_at_seq = Some(10);
    ledger.last_marker_seq = 11;
    ledger.situation = Some(job::LedgerSituation {
        state: job::WakeState::Monitoring,
        status: Some("external build is running".into()),
        open_stops: Vec::new(),
        seq: 11,
        at: now,
    });
    job::save(paths, &ledger).unwrap();
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();
    std::fs::write(paths.turn_signal(), ".").unwrap();
}

fn accepted_working_ledger(paths: &ProjectPaths, now: i64) {
    let mut ledger = job::load(paths).unwrap().unwrap();
    ledger.run = job::JobRun::Monitoring { until: now + 300 };
    ledger.turn_count_at_nudge = Some(1);
    ledger.nudged_at_seq = Some(10);
    ledger.last_marker_seq = 11;
    ledger.situation = Some(job::LedgerSituation {
        state: job::WakeState::Working,
        status: Some("checked the external build".into()),
        open_stops: Vec::new(),
        seq: 11,
        at: now,
    });
    job::save(paths, &ledger).unwrap();
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();
    std::fs::write(paths.turn_signal(), ".").unwrap();
}

fn launch_pane(driver: &TmuxDriver, session: &str, root: &Path, pane: &str) {
    let fixture = root.join("codex-pane.txt");
    std::fs::write(&fixture, pane).unwrap();
    driver
        .launch_interactive(
            session,
            root,
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("cat {}; sleep 3600", fixture.display()),
            ],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();
}

#[test]
fn stable_idle_codex_shows_the_monitoring_countdown_after_a_missed_notify() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    set_worker_engine(&registry, Engine::Codex);
    let paths = ProjectPaths::for_session(&root, "bot");
    let now = SystemClock.now();
    accepted_monitoring_ledger(&paths, now);

    let socket = TmuxSocket::new("pmtui-codex-monitoring-idle");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("bot", &root);
    launch_pane(
        &driver,
        &session,
        &root,
        "› Ask Codex to do anything\n\n  model footer\n",
    );

    let mut app = loop_app(&registry);
    app.socket = socket.name().to_string();
    app.refresh();
    app.refresh();
    let view = app.projects[0].clone();
    let _ = driver.terminate(&session);

    assert_eq!(view.agent_working, Some(false));
    assert!(
        view.next_action.starts_with("waiting · check in 00:0"),
        "stable idle pane should show the running countdown: {}",
        view.next_action
    );
}

#[test]
fn stable_idle_codex_overrides_a_stale_working_hook() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    set_worker_engine(&registry, Engine::Codex);
    let paths = ProjectPaths::for_session(&root, "bot");
    accepted_working_ledger(&paths, SystemClock.now());

    let socket = TmuxSocket::new("pmtui-codex-working-hook-idle");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("bot", &root);
    launch_pane(
        &driver,
        &session,
        &root,
        "› Ask Codex to do anything\n\n  model footer\n",
    );

    let mut app = loop_app(&registry);
    app.socket = socket.name().to_string();
    app.refresh();
    app.refresh();
    let view = app.projects[0].clone();
    let _ = driver.terminate(&session);

    assert_eq!(
        view.agent_working,
        Some(false),
        "a stable Codex composer is stronger than a missed optional notify hook"
    );
    assert!(
        view.next_action.starts_with("waiting · check in 00:0"),
        "the idle row should expose its scheduled heartbeat: {}",
        view.next_action
    );
}

#[test]
fn busy_codex_still_overrides_an_accepted_monitoring_report() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    set_worker_engine(&registry, Engine::Codex);
    let paths = ProjectPaths::for_session(&root, "bot");
    accepted_monitoring_ledger(&paths, SystemClock.now());

    let socket = TmuxSocket::new("pmtui-codex-monitoring-busy");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("bot", &root);
    launch_pane(
        &driver,
        &session,
        &root,
        "• Working (3s • esc to interrupt)\n\n› Ask Codex to do anything\n\n  model footer\n",
    );

    let mut app = loop_app(&registry);
    app.socket = socket.name().to_string();
    app.refresh();
    let view = app.projects[0].clone();
    let _ = driver.terminate(&session);

    assert_eq!(view.agent_working, Some(true));
    assert_eq!(
        view.next_action, "working",
        "live pane activity must override the accepted monitoring report"
    );
}
