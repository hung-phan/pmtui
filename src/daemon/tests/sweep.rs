//! The sweep's bookkeeping over the registry, ahead of any one scheduler's tick:
//! pruning a removed id, the disabled→enabled edge, the poison threshold, duplicate
//! ids, the cross-process driver lease, and the idle self-exit. Every row is a
//! `Mode::AgentLoop` session driven by the `JobScheduler`.

use super::*;

fn park_on_ambiguity(paths: &ProjectPaths, stop_id: &str) {
    let mut ledger = crate::job::load(paths).unwrap().unwrap();
    ledger.run = crate::job::JobRun::Blocked {
        stop_ids: vec![stop_id.into()],
        since: 1_000,
    };
    ledger.open_stops = vec![crate::pmstate::OpenStop {
        id: stop_id.into(),
        kind: crate::pmstate::StopKind::Ambiguity,
        pane_dialog: None,
        channel: None,
        context_ref: None,
        question: Some("Which option should continue?".into()),
        options: vec!["first".into(), "second".into()],
        authorized_responders: Vec::new(),
        message_id: None,
        first_posted: 1_000,
        last_polled: None,
        last_seen_reply_ts: None,
        status: crate::pmstate::StopStatus::AwaitingReply,
    }];
    crate::job::save(paths, &ledger).unwrap();
}

/// A JOB ROW IS NOT pmd'S. Its life belongs to the dashboard's spawn broker, so pmd builds no runner
/// for it and — the bug this guards — writes NOTHING under it. Observing a job's dead pane used to leave
/// a `driver.json` in the directory the broker had just deleted, which is how a retired child's folder
/// came back holding one file.
#[test]
fn a_job_row_is_skipped_entirely_and_nothing_is_written_under_it() {
    let (dir, mut job) = agent_loop_project("kid", Tier::Autopilot);
    job.launch = Some(crate::registry::LaunchRecord {
        request_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        args_hash: "h".into(),
        state: crate::registry::LaunchState::Started,
        outcome: Some(crate::registry::SpawnOutcome::Ready),
        kind: crate::registry::LaunchKind::Job,
        branch: None,
        base_commit: None,
    });
    let paths = ProjectPaths::for_session(dir.path(), "kid");
    // The broker's retirement: the whole subtree goes while pmd is mid-sweep.
    std::fs::remove_dir_all(paths.state_dir()).unwrap();
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    daemon.sweep(&reg(vec![job]), &driver, &clock, &notif);

    assert!(!daemon.tracks("kid"), "pmd must not take a job as a runner");
    assert!(
        !paths.state_dir().exists(),
        "pmd recreated a retired job's state: {:?}",
        std::fs::read_dir(paths.state_dir())
            .map(|entries| entries.flatten().map(|e| e.path()).collect::<Vec<_>>())
    );
}

/// An ordinary row is still reconciled — the skip above is about `launch.kind`, not about every row
/// whose pane is missing.
#[test]
fn a_chat_row_beside_a_job_is_still_driven() {
    let (_d, chat) = agent_loop_project("human", Tier::Autopilot);
    let (_d2, mut job) = agent_loop_project("kid", Tier::Autopilot);
    job.launch = Some(crate::registry::LaunchRecord {
        request_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        args_hash: "h".into(),
        state: crate::registry::LaunchState::Started,
        outcome: Some(crate::registry::SpawnOutcome::Ready),
        kind: crate::registry::LaunchKind::Job,
        branch: None,
        base_commit: None,
    });
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    daemon.sweep(&reg(vec![chat, job]), &driver, &clock, &notif);

    assert!(daemon.tracks("human"), "the human's row is pmd's to drive");
    assert!(!daemon.tracks("kid"));
}

#[test]
fn prunes_runners_for_removed_projects() {
    let (_d, p) = agent_loop_project("p", Tier::Autopilot);
    let (_d2, p2) = agent_loop_project("q", Tier::Autopilot);
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    daemon.sweep(&reg(vec![p.clone(), p2.clone()]), &driver, &clock, &notif);
    assert!(daemon.tracks("p") && daemon.tracks("q"));
    // Drop q from the registry -> its runner is pruned.
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    assert!(daemon.tracks("p"));
    assert!(
        !daemon.tracks("q"),
        "removed project's runner should be pruned"
    );
}

#[test]
fn prune_aborts_in_flight_worker_of_removed_session() {
    // S5b Gap 1 (persistent model): a human closing an agent-loop session removes
    // its registry row. Pruning the runner must ABORT its persistent `pmloop-`
    // session (terminate the pane) — not merely drop it from the map — so the
    // orphaned agent can't keep editing the tree.
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "bot", Tier::Autopilot);
    crate::job::save(
        &ProjectPaths::for_session(dir.path(), "bot"),
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    let p = agent_loop_entry("bot", dir.path());

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    // Sweep 1: the JobScheduler launches the persistent `pmloop-` session.
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    let worker = crate::tmux::session_name("bot", dir.path());
    assert!(daemon.tracks("bot"));
    assert!(
        driver.is_alive(&worker).unwrap(),
        "precondition: the persistent loop session is live"
    );

    // The human closes the session -> its id leaves the registry. The prune
    // must abort the persistent session before dropping the runner.
    daemon.sweep(&reg(vec![]), &driver, &clock, &notif);
    assert!(!daemon.tracks("bot"), "removed session's runner is pruned");
    assert!(
        !driver.is_alive(&worker).unwrap(),
        "the orphaned persistent agent must be TERMINATED, not just dropped"
    );
}

#[test]
fn failed_prune_abort_still_discards_the_removed_rows_autopilot_residency_hint() {
    let (dir, p) = agent_loop_project("bot", Tier::Autopilot);
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1_000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    let worker = crate::tmux::session_name("bot", dir.path());
    daemon
        .runners
        .get_mut("bot")
        .expect("runner exists")
        .waiting_for_human = true;
    driver.fail_terminate(&worker);

    assert!(
        !daemon
            .sweep(&reg(vec![]), &driver, &clock, &notif)
            .idle_expired,
        "the failed abort starts, but does not skip, the idle grace"
    );
    assert!(
        daemon.tracks("bot"),
        "the failed abort keeps the runner for retry"
    );

    clock.set(1_000 + IDLE_EXIT_S);
    assert!(
        daemon
            .sweep(&reg(vec![]), &driver, &clock, &notif)
            .idle_expired,
        "a removed row cannot keep pmd resident through stale cached Autopilot state"
    );
}

#[test]
fn disabled_project_is_not_driven_then_enabling_drives() {
    let (_d, mut p) = agent_loop_project("p", Tier::Autopilot);
    p.enabled = false;
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    assert!(
        driver.launched().is_empty(),
        "disabled project must not be driven"
    );
    // Enable it -> next sweep drives it (launches the persistent loop session).
    p.enabled = true;
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    assert!(
        !driver.launched().is_empty(),
        "an enabled Autopilot row is driven"
    );
}

#[test]
fn poison_pauses_after_threshold_and_notifies_once() {
    // A readable Autopilot config (so the row IS driven) but a MALFORMED ledger ->
    // every tick errors on `job::load`, so the poison counter strikes.
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "poison", Tier::Autopilot);
    std::fs::write(
        ProjectPaths::for_session(dir.path(), "poison").pmstate(),
        "{ this is not json",
    )
    .unwrap();
    let p = agent_loop_entry("poison", dir.path());
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    for _ in 0..POISON_THRESHOLD {
        let report = daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
        assert!(!report.all_enabled_done);
    }
    assert!(daemon.is_poisoned("poison"));
    assert_eq!(notif.count(), 1, "surfaced exactly once");
    // Further sweeps neither drive nor re-notify (no tight loop).
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    assert_eq!(notif.count(), 1);
}

#[test]
fn reenabling_clears_poison() {
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "poison", Tier::Autopilot);
    let ledger_path = ProjectPaths::for_session(dir.path(), "poison").pmstate();
    std::fs::write(&ledger_path, "{ bad").unwrap();
    let mut p = agent_loop_entry("poison", dir.path());
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    for _ in 0..POISON_THRESHOLD {
        daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    }
    assert!(daemon.is_poisoned("poison"));
    // Operator fixes the ledger and toggles disabled -> enabled to unpause.
    crate::job::save(
        &ProjectPaths::for_session(dir.path(), "poison"),
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    p.enabled = false;
    daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    p.enabled = true;
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    assert!(!daemon.is_poisoned("poison"), "re-enable clears poison");
    assert!(!driver.launched().is_empty(), "and it drives again");
}

#[test]
fn duplicate_ids_are_reconciled_once() {
    // Two entries with the same id but different roots must not both drive
    // (they'd fight over one runner); the second is ignored.
    let (_d, p) = agent_loop_project("p", Tier::Autopilot);
    let (_d2, mut dup) = agent_loop_project("p", Tier::Autopilot); // same id "p", different root
    dup.root = _d2.path().to_path_buf();
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    daemon.sweep(&reg(vec![p.clone(), dup]), &driver, &clock, &notif);
    // Only the first entry's runner exists, driven exactly once.
    assert!(daemon.tracks("p"));
    assert_eq!(
        driver.launched().len(),
        1,
        "only the first entry for a duplicate id is driven"
    );
}

#[test]
fn foreign_lease_prevents_driving_until_released() {
    let (_d, p) = agent_loop_project("bot", Tier::Autopilot);
    // Simulate another process holding this session's per-session driver lease.
    let lock = ProjectPaths::for_session(&p.root, &p.id)
        .daemon_dir()
        .join("driver.lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let held = crate::lease::try_acquire(&lock).unwrap().unwrap();

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    assert!(
        driver.launched().is_empty(),
        "must not drive a project another process holds"
    );
    // Release the foreign lease -> the daemon can now take it and drive.
    drop(held);
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    assert!(
        !driver.launched().is_empty(),
        "driving resumes once the lease is free"
    );
}

#[test]
fn empty_registry_is_not_all_done() {
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    let report = daemon.sweep(&reg(vec![]), &driver, &clock, &notif);
    assert!(!report.all_enabled_done, "no projects != all done");
}

#[test]
fn pmd_reports_idle_expired_only_after_the_grace_with_nothing_to_drive() {
    // User: *"when there are no session running or needs pmd do we need to keep it around"* — no.
    // With an EMPTY registry nothing needs pmd, but it must not exit on the first idle sweep: the
    // grace is what keeps a quick autopilot off→on toggle on the same daemon.
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    // First idle sweep: the clock STARTS, but the grace has not elapsed.
    let r = daemon.sweep(&reg(vec![]), &driver, &clock, &notif);
    assert!(
        !r.idle_expired,
        "must not exit on the first idle sweep (grace not elapsed)"
    );

    // Still within the grace.
    clock.set(1000 + IDLE_EXIT_S - 1);
    let r = daemon.sweep(&reg(vec![]), &driver, &clock, &notif);
    assert!(!r.idle_expired, "still within the grace window");

    // Past the grace, still nothing to drive: NOW it exits.
    clock.set(1000 + IDLE_EXIT_S);
    let r = daemon.sweep(&reg(vec![]), &driver, &clock, &notif);
    assert!(
        r.idle_expired,
        "idle past the grace with nothing to drive → exit"
    );
}

#[test]
fn a_drivable_row_keeps_pmd_alive_and_resets_the_idle_clock() {
    // The regression guard for the toggle: an Autopilot agent-loop row NEEDS pmd (even parked on
    // cadence), so the idle clock must never accrue while one exists — and a row reappearing must
    // RESET a clock that had started, which is exactly the off→on toggle.
    let (_d, auto) = agent_loop_project("bot", Tier::Autopilot);

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    // An Autopilot row present: never idle, however long we wait.
    clock.set(1000 + IDLE_EXIT_S * 3);
    let r = daemon.sweep(&reg(vec![auto.clone()]), &driver, &clock, &notif);
    assert!(
        !r.idle_expired,
        "a drivable row must keep pmd alive indefinitely"
    );

    // Row goes away (paused/removed): the idle clock starts now, not retroactively.
    let start = clock.now();
    let r = daemon.sweep(&reg(vec![]), &driver, &clock, &notif);
    assert!(!r.idle_expired, "the clock only starts when idle begins");

    // The row comes back BEFORE the grace elapses (the off→on toggle): clock resets…
    clock.set(start + IDLE_EXIT_S - 1);
    let r = daemon.sweep(&reg(vec![auto]), &driver, &clock, &notif);
    assert!(!r.idle_expired);

    // …so a later idle stretch must serve the FULL grace again, not the leftover.
    let restart = clock.now();
    daemon.sweep(&reg(vec![]), &driver, &clock, &notif);
    clock.set(restart + IDLE_EXIT_S - 1);
    let r = daemon.sweep(&reg(vec![]), &driver, &clock, &notif);
    assert!(
        !r.idle_expired,
        "the grace restarted from the new idle transition"
    );
    clock.set(restart + IDLE_EXIT_S);
    let r = daemon.sweep(&reg(vec![]), &driver, &clock, &notif);
    assert!(r.idle_expired);
}

#[test]
fn blocked_autopilot_debt_keeps_pmd_alive_when_the_tier_read_temporarily_fails() {
    let (dir, auto) = agent_loop_project("bot", Tier::Autopilot);
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    park_on_ambiguity(&paths, "stop-bot-answer");

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1_000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    // Establish the row as a live Autopilot responsibility before the config read fails.
    assert!(
        !daemon
            .sweep(&reg(vec![auto.clone()]), &driver, &clock, &notif)
            .idle_expired
    );
    clock.set(1_000 + IDLE_EXIT_S * 2);
    assert!(
        !daemon
            .sweep(&reg(vec![auto.clone()]), &driver, &clock, &notif)
            .idle_expired,
        "a readable blocked Autopilot row remains a live pmd responsibility"
    );
    std::fs::write(paths.config(), "{temporarily-unreadable").unwrap();

    // The daemon already observed this unchanged row on Autopilot. It must remain resident while
    // the current read is unavailable, but the drive path still receives None and stays fail-closed.
    let unreadable_at = clock.now() + 1_000;
    clock.set(unreadable_at);
    assert!(
        !daemon
            .sweep(&reg(vec![auto.clone()]), &driver, &clock, &notif)
            .idle_expired
    );
    clock.set(unreadable_at + IDLE_EXIT_S);
    assert!(
        !daemon
            .sweep(&reg(vec![auto]), &driver, &clock, &notif)
            .idle_expired
    );
    assert_eq!(
        driver.spawn_count(),
        0,
        "a blocked session is not relaunched"
    );
    assert!(
        driver.sent_keys().is_empty(),
        "an unreadable tier never grants permission to type"
    );
}

#[test]
fn an_explicit_standard_tier_overrides_cached_autopilot_blocked_residency() {
    let (dir, standard) = agent_loop_project("std-blocked", Tier::Autopilot);
    let paths = ProjectPaths::for_session(dir.path(), "std-blocked");
    park_on_ambiguity(&paths, "stop-standard");

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1_000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    // First establish both cached Autopilot and a runner that successfully observed Blocked.
    daemon.sweep(&reg(vec![standard.clone()]), &driver, &clock, &notif);
    seed_agent_loop_config(dir.path(), "std-blocked", Tier::Standard);
    daemon.sweep(&reg(vec![standard.clone()]), &driver, &clock, &notif);

    // Even if the next read fails, the explicit Standard observation replaced cached Autopilot.
    // The old waiting flag cannot keep pmd resident after the human turned autonomy off.
    std::fs::write(paths.config(), "{temporarily-unreadable").unwrap();
    clock.set(1_000 + IDLE_EXIT_S);
    assert!(
        daemon
            .sweep(&reg(vec![standard]), &driver, &clock, &notif)
            .idle_expired
    );
}

#[test]
fn a_standard_only_registry_lets_pmd_exit() {
    // A Standard agent-loop row is undriven by design (m15) — the human drives it — so a
    // registry of only Standard rows must go idle and exit after the grace.
    let (_d, standard) = agent_loop_project("std", Tier::Standard);

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    daemon.sweep(&reg(vec![standard.clone()]), &driver, &clock, &notif);
    clock.set(1000 + IDLE_EXIT_S);
    let r = daemon.sweep(&reg(vec![standard]), &driver, &clock, &notif);
    assert!(
        r.idle_expired,
        "a registry with nothing pmd drives must let it exit"
    );
}
