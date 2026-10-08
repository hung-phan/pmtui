//! The source's terminal is rechecked under its input lock, and must stay idle across two captures
//! before a fork branches it: one Idle frame can be the gap between two bursts of the same turn.

use super::*;

fn source_captures(fixture: &ForkFixture) -> usize {
    let source_session = session_name("bot", &fixture.root);
    fixture
        .pane
        .captures()
        .iter()
        .filter(|(session, _)| *session == source_session)
        .count()
}

#[test]
fn fork_rechecks_live_source_terminal_state_under_the_input_lock() {
    let mut idle = fork_fixture(Engine::Claude);
    let idle_driver = source_driver(&idle, IDLE_CLAUDE_PANE);
    install_driver(&mut idle, idle_driver);
    idle.app.fork_selected();
    assert_forked(&idle, Engine::Claude);

    let mut attached = fork_fixture(Engine::Claude);
    let attached_driver =
        source_driver(&attached, IDLE_CLAUDE_PANE).with(|inner| inner.attached = true);
    install_driver(&mut attached, attached_driver);
    attached.app.fork_selected();
    assert!(attached.app.status.contains("human is attached"));

    let mut busy = fork_fixture(Engine::Claude);
    let busy_driver = source_driver(&busy, "still streaming without a composer");
    install_driver(&mut busy, busy_driver);
    busy.app.fork_selected();
    assert!(busy.app.status.contains("still working"));

    let mut dead = fork_fixture(Engine::Claude);
    let dead_driver = source_driver(&dead, "").with(|inner| inner.pane_dead = true);
    install_driver(&mut dead, dead_driver);
    dead.app.fork_selected();
    assert_forked(&dead, Engine::Claude);

    let mut unreadable = fork_fixture(Engine::Claude);
    let unreadable_driver = source_driver(&unreadable, "").with(|inner| inner.fail_capture = true);
    install_driver(&mut unreadable, unreadable_driver);
    unreadable.app.fork_selected();
    assert!(
        unreadable
            .app
            .status
            .contains("could not read its terminal")
    );

    let mut dead_probe = fork_fixture(Engine::Claude);
    let dead_probe_driver =
        source_driver(&dead_probe, "").with(|inner| inner.fail_pane_dead = true);
    install_driver(&mut dead_probe, dead_probe_driver);
    dead_probe.app.fork_selected();
    assert!(
        dead_probe
            .app
            .status
            .contains("could not inspect its terminal")
    );

    let mut alive_probe = fork_fixture(Engine::Claude);
    let alive_probe_driver = source_driver(&alive_probe, "").with(|inner| inner.fail_alive = true);
    install_driver(&mut alive_probe, alive_probe_driver);
    alive_probe.app.fork_selected();
    assert!(
        alive_probe
            .app
            .status
            .contains("could not inspect its terminal")
    );

    let mut stage_load_error = fork_fixture(Engine::Claude);
    let registry_path = stage_load_error.registry.clone();
    let stage_error_driver = source_driver(&stage_load_error, IDLE_CLAUDE_PANE)
        .with(|inner| inner.corrupt_registry_on_capture = Some(registry_path));
    install_driver(&mut stage_load_error, stage_error_driver);
    stage_load_error.app.fork_selected();
    assert!(stage_load_error.app.status.contains("could not stage"));
}

#[test]
fn fork_requires_a_live_source_to_stay_idle_across_two_captures() {
    let mut stable = fork_fixture(Engine::Claude);
    let driver = source_driver(&stable, IDLE_CLAUDE_PANE);
    install_driver(&mut stable, driver);
    stable.app.fork_selected();
    assert_eq!(source_captures(&stable), 2, "one Idle frame is not enough");
    assert_forked(&stable, Engine::Claude);

    let mut streaming = fork_fixture(Engine::Claude);
    let source_session = session_name("bot", &streaming.root);
    let frames = vec![
        format!("\u{25cf} A partial answer\n{IDLE_CLAUDE_PANE}"),
        format!("\u{25cf} A partial answer that is still growing\n{IDLE_CLAUDE_PANE}"),
    ];
    let driver = source_driver(&streaming, IDLE_CLAUDE_PANE).with(|inner| {
        inner.tail_frames.insert(source_session, frames);
    });
    install_driver(&mut streaming, driver);
    streaming.app.fork_selected();
    assert_refused_without_launch(&streaming, "still working");

    let mut turned_busy = fork_fixture(Engine::Claude);
    let source_session = session_name("bot", &turned_busy.root);
    let frames = vec![
        IDLE_CLAUDE_PANE.to_string(),
        format!("\u{273b} Mustering\u{2026}\n{IDLE_CLAUDE_PANE}"),
    ];
    let driver = source_driver(&turned_busy, IDLE_CLAUDE_PANE).with(|inner| {
        inner.tail_frames.insert(source_session, frames);
    });
    install_driver(&mut turned_busy, driver);
    turned_busy.app.fork_selected();
    assert_refused_without_launch(&turned_busy, "still working");
}

/// Put the source on Autopilot with a pmd-nudged turn whose completion hook has not fired.
fn outstanding_nudged_turn(fixture: &ForkFixture) {
    let paths = ProjectPaths::for_session(&fixture.root, "bot");
    let mut config: Config = state::read_json(&paths.config()).unwrap();
    config.autonomy = Tier::Autopilot;
    state::write_json_atomic(&paths.config(), &config).unwrap();
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.run = job::JobRun::Monitoring {
        until: SystemClock.now() + 3_600,
    };
    ledger.turn_count_at_nudge = Some(1);
    job::save(&paths, &ledger).unwrap();
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();
    std::fs::write(paths.turn_signal(), "x").unwrap();
}

#[test]
fn fork_refuses_an_idle_looking_claude_source_with_an_outstanding_turn() {
    let mut claude = fork_fixture(Engine::Claude);
    outstanding_nudged_turn(&claude);
    let driver = source_driver(&claude, IDLE_CLAUDE_PANE);
    install_driver(&mut claude, driver);
    claude.app.fork_selected();
    assert_refused_without_launch(&claude, "still working");

    // Codex paints its own busy banner and its notify hook is optional, so a confirmed idle
    // composer outranks a stale completion count, exactly as on the dashboard row.
    let mut codex = fork_fixture(Engine::Codex);
    outstanding_nudged_turn(&codex);
    let driver = live_codex_source(&codex, Some(SOURCE_ID));
    install_driver(&mut codex, driver);
    codex.app.fork_selected();
    assert_forked(&codex, Engine::Codex);
}
