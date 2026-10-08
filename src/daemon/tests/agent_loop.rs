//! The `Mode::AgentLoop` route to the `JobScheduler`: the persistent per-session
//! launch, the per-session lease two sessions in one folder must not share, the
//! registry conversation-id seed a first wake adopts, and the same notify-once
//! dedup as the native path (its scheduler also re-emits every parked tick).

use super::*;

// --- agent-loop (JobScheduler) routing -------------------------------------

fn daemon_with_inflight_decider() -> (
    tempfile::TempDir,
    ProjectEntry,
    ProjectPaths,
    FakeDriver,
    FakeClock,
    CaptureNotifier,
    Daemon,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let id = "bot";
    seed_agent_loop_config(dir.path(), id, Tier::Autopilot);
    let paths = ProjectPaths::for_session(dir.path(), id);
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    ledger.conversation_id = Some("conversation".into());
    ledger.run = JobRun::Monitoring { until: 1000 };
    crate::job::save(&paths, &ledger).unwrap();
    std::fs::write(paths.brief(), "keep formatting consistent").unwrap();
    std::fs::write(
        paths.needs_you(),
        r#"{"seq":1,"state":"blocked","stops":[{"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low","question":"Which formatter?","options":["prettier","dprint"]}]}"#,
    )
    .unwrap();
    let entry = agent_loop_entry(id, dir.path());
    let driver = FakeDriver::new();
    let worker = crate::tmux::session_name(id, dir.path());
    driver.set_alive(&worker, true);
    driver.set_tail(&worker, "✻ Working… (esc to interrupt)");
    let clock = FakeClock::new(1000);
    let notifier = CaptureNotifier::new();
    let mut daemon = Daemon::with_supervisor_enabled(true);
    daemon
        .runners
        .insert(id.into(), Runner::build(&entry, Some(true), None));
    let Driven::Job(scheduler) = &mut daemon.runners.get_mut(id).unwrap().driven;
    scheduler.set_decider_binary_available(true);
    daemon.sweep(&reg(vec![entry.clone()]), &driver, &clock, &notifier);
    let parked = crate::job::load(&paths)
        .unwrap()
        .unwrap()
        .advice_inflight
        .expect("decider debt is parked");
    let supervisor = crate::tmux::supervisor_session_name(id, dir.path(), parked.seq);
    assert!(driver.is_alive(&supervisor).unwrap());
    (
        dir, entry, paths, driver, clock, notifier, daemon, supervisor,
    )
}

#[test]
fn every_runner_launches_its_terminal_with_the_daemons_pmtui() {
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "bot", Tier::Autopilot);
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    crate::job::save(
        &paths,
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    let p = agent_loop_entry("bot", dir.path());

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new().with_pmtui_bin(Some(PathBuf::from("/opt/am/pmtui")));
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    assert_eq!(
        driver.launched_env(),
        vec![(
            crate::tmux::session_name("bot", dir.path()),
            crate::tmux::ManagedEnv {
                session_id: "bot".into(),
                state_dir: paths.state_dir(),
                pmtui_bin: Some(PathBuf::from("/opt/am/pmtui")),
            }
        )]
    );
}

#[test]
fn agent_loop_project_is_driven_by_job_scheduler() {
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "bot", Tier::Autopilot);
    // A fresh Idle per-session ledger (what pmtui's create writes at S4).
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
    let report = daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    assert_eq!(
        driver.launched().len(),
        1,
        "an AgentLoop project is driven by the JobScheduler (launches the persistent session)"
    );
    assert!(
        !report.all_enabled_done,
        "an agent-loop session never gates the all-done exit"
    );
}

#[test]
fn one_shot_posture_without_a_decider_escalates_instead_of_auto_approving() {
    let dir = tempfile::tempdir().unwrap();
    let id = "once";
    seed_agent_loop_config(dir.path(), id, Tier::Autopilot);
    let paths = ProjectPaths::for_session(dir.path(), id);
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    ledger.conversation_id = Some("conversation".into());
    ledger.run = JobRun::Monitoring { until: 1000 };
    crate::job::save(&paths, &ledger).unwrap();
    std::fs::write(
        paths.needs_you(),
        r#"{"seq":1,"state":"blocked","stops":[{"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low","question":"Which formatter?","options":["prettier","dprint"]}]}"#,
    )
    .unwrap();
    let session = crate::tmux::session_name(id, dir.path());
    let driver = FakeDriver::new();
    driver.set_alive(&session, true);
    driver.set_tail(&session, "✻ Working… (esc to interrupt)");
    let clock = FakeClock::new(1000);
    let notifier = CaptureNotifier::new();
    let mut daemon = Daemon::with_supervisor_enabled(false);

    daemon.sweep(
        &reg(vec![agent_loop_entry(id, dir.path())]),
        &driver,
        &clock,
        &notifier,
    );

    assert_eq!(driver.spawn_count(), 0, "no detached pmsup session");
    let current = crate::job::load(&paths).unwrap().unwrap();
    assert!(matches!(current.run, JobRun::Blocked { .. }));
    assert_eq!(current.open_stops[0].kind, StopKind::Capability);
    assert!(current.pending_context.is_none());
}

#[test]
fn leaving_autopilot_interrupts_the_decider_without_killing_the_worker() {
    let (_dir, entry, paths, driver, clock, notifier, mut daemon, supervisor) =
        daemon_with_inflight_decider();
    seed_agent_loop_config(entry.root.as_path(), &entry.id, Tier::Standard);

    daemon.sweep(&reg(vec![entry.clone()]), &driver, &clock, &notifier);

    assert!(!driver.is_alive(&supervisor).unwrap());
    assert!(
        crate::job::load(&paths)
            .unwrap()
            .unwrap()
            .advice_inflight
            .is_none()
    );
    assert!(
        driver
            .is_alive(&crate::tmux::session_name(&entry.id, &entry.root))
            .unwrap()
    );
}

#[test]
fn daemon_shutdown_interrupts_the_decider() {
    let (_dir, _entry, paths, driver, clock, _notifier, mut daemon, supervisor) =
        daemon_with_inflight_decider();

    daemon.shutdown_advice(&driver, &clock).unwrap();

    assert!(!driver.is_alive(&supervisor).unwrap());
    assert!(
        crate::job::load(&paths)
            .unwrap()
            .unwrap()
            .advice_inflight
            .is_none()
    );
}

#[test]
fn project_root_change_interrupts_the_old_decider_before_rebuild() {
    let (_dir, mut entry, _paths, driver, clock, notifier, mut daemon, supervisor) =
        daemon_with_inflight_decider();
    let replacement = tempfile::tempdir().unwrap();
    seed_agent_loop_config(replacement.path(), &entry.id, Tier::Autopilot);
    crate::job::save(
        &ProjectPaths::for_session(replacement.path(), &entry.id),
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    entry.root = replacement.path().to_path_buf();

    daemon.sweep(&reg(vec![entry.clone()]), &driver, &clock, &notifier);

    assert!(!driver.is_alive(&supervisor).unwrap());
    assert_eq!(daemon.runners.get(&entry.id).unwrap().root, entry.root);
}

#[test]
fn leaving_autopilot_retries_when_decider_termination_fails() {
    let (_dir, entry, paths, driver, clock, notifier, mut daemon, supervisor) =
        daemon_with_inflight_decider();
    driver.fail_terminate(&supervisor);
    seed_agent_loop_config(entry.root.as_path(), &entry.id, Tier::Standard);

    daemon.sweep(&reg(vec![entry.clone()]), &driver, &clock, &notifier);

    assert!(driver.is_alive(&supervisor).unwrap());
    assert!(
        crate::job::load(&paths)
            .unwrap()
            .unwrap()
            .advice_inflight
            .is_some()
    );

    driver.allow_terminate(&supervisor);
    daemon.sweep(&reg(vec![entry]), &driver, &clock, &notifier);
    assert!(!driver.is_alive(&supervisor).unwrap());
}

#[test]
fn a_paused_row_releases_its_driver_lock_so_resume_wins_the_first_try() {
    // Bug (user): pause an autopilot, press Enter to resume → "pmd is already starting this
    // session", and only a SECOND Enter works. pmd holds the per-session driver.lock while
    // driving and a plain early-return kept holding it for a PAUSED row until a later sweep
    // rebuilt the runner, so pmtui's resume raced the stale lock and lost. The sweep that
    // OBSERVES the pause must free the lock.
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "bot", Tier::Autopilot);
    crate::job::save(
        &ProjectPaths::for_session(dir.path(), "bot"),
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    let lock = ProjectPaths::for_session(dir.path(), "bot")
        .daemon_dir()
        .join("driver.lock");
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    // Sweep 1: pmd drives the autopilot row and ACQUIRES the per-session driver.lock.
    daemon.sweep(
        &reg(vec![agent_loop_entry("bot", dir.path())]),
        &driver,
        &clock,
        &notif,
    );
    assert!(
        crate::lease::try_acquire(&lock).unwrap().is_none(),
        "precondition: pmd holds the driver.lock while driving"
    );

    // Pause: the row is disabled in the registry, and the next sweep observes it.
    let mut paused = agent_loop_entry("bot", dir.path());
    paused.enabled = false;
    daemon.sweep(&reg(vec![paused]), &driver, &clock, &notif);
    assert!(
        lock_is_free(&lock),
        "a paused row's sweep frees the driver.lock so a single resume wins"
    );
}

#[test]
fn an_autopilot_to_standard_flip_releases_the_driver_lock() {
    // `m` flips config.autonomy to Standard while the row stays ENABLED; pmd no longer drives
    // it, so it must drop the driver.lock it held on autopilot — else Enter-to-drive by hand
    // would race a stale lock, the same defect as the pause case.
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "bot", Tier::Autopilot);
    crate::job::save(
        &ProjectPaths::for_session(dir.path(), "bot"),
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    let lock = ProjectPaths::for_session(dir.path(), "bot")
        .daemon_dir()
        .join("driver.lock");
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    daemon.sweep(
        &reg(vec![agent_loop_entry("bot", dir.path())]),
        &driver,
        &clock,
        &notif,
    );
    assert!(
        crate::lease::try_acquire(&lock).unwrap().is_none(),
        "precondition: pmd holds the driver.lock while on autopilot"
    );

    // Flip to Standard through the same per-session config.json `m` writes; row stays enabled.
    seed_agent_loop_config(dir.path(), "bot", Tier::Standard);
    daemon.sweep(
        &reg(vec![agent_loop_entry("bot", dir.path())]),
        &driver,
        &clock,
        &notif,
    );
    assert!(
        lock_is_free(&lock),
        "an autopilot→Standard flip frees the driver.lock for pmtui to drive by hand"
    );
}

#[test]
fn agent_loop_escalation_notifies_once_across_sweeps() {
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "bot", Tier::Autopilot);
    // A Blocked ledger with one open stop (as a blocked wake would leave).
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    l.open_stops = vec![OpenStop {
        id: "stop-1".into(),
        kind: StopKind::Ambiguity,
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
    crate::job::save(&ProjectPaths::for_session(dir.path(), "bot"), &l).unwrap();
    let p = agent_loop_entry("bot", dir.path());

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    // JobScheduler re-emits Escalated every parked tick; the daemon must notify
    // exactly once (same notify-once dedup as the native path).
    daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);
    assert_eq!(notif.count(), 1, "a still-parked escalation notifies once");
    assert_eq!(driver.spawn_count(), 0, "a Blocked session is not driven");
}

#[test]
fn newly_detected_stuck_notifies_once_when_later_reemitted_as_escalated() {
    let dir = tempfile::tempdir().unwrap();
    let id = "wedged";
    seed_agent_loop_config(dir.path(), id, Tier::Autopilot);
    let paths = ProjectPaths::for_session(dir.path(), id);
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    ledger.conversation_id = Some("conversation".into());
    ledger.run = JobRun::Monitoring { until: 1000 };
    crate::job::save(&paths, &ledger).unwrap();

    let session = crate::tmux::session_name(id, dir.path());
    let driver = FakeDriver::new();
    driver.set_alive(&session, true);
    driver.set_tail(&session, "Working... (esc to interrupt)");
    let clock = FakeClock::new(1000);
    let notifier = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    let registry = reg(vec![agent_loop_entry(id, dir.path())]);

    daemon.sweep(&registry, &driver, &clock, &notifier);
    assert_eq!(
        notifier.count(),
        0,
        "the first busy observation only starts the stall window"
    );

    clock.set(1000 + 60 * 60);
    daemon.sweep(&registry, &driver, &clock, &notifier);
    assert_eq!(notifier.count(), 1, "a continuous stall is surfaced once");
    {
        let seen = notifier.seen.lock().unwrap();
        assert!(
            seen[0].body.contains("busy with no progress"),
            "the notification should explain the detected stall: {}",
            seen[0].body
        );
    }

    let parked = crate::job::load(&paths).unwrap().unwrap();
    assert!(
        matches!(parked.run, JobRun::Blocked { .. }),
        "the synthesized stuck stop parks the session for the human"
    );
    assert_eq!(parked.open_stops.len(), 1);

    daemon.sweep(&registry, &driver, &clock, &notifier);
    assert_eq!(
        notifier.count(),
        1,
        "the blocked tick re-emits the stop id, but the daemon deduplicates it"
    );
}

#[test]
fn agent_loop_escalation_body_carries_the_question_and_options() {
    // The notification is the message a human actually receives, so it must say
    // WHAT is being decided. Previously `open_stops_for_display` hard-coded an
    // empty question and `for_stops` fell back to the bare kind name.
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "bot", Tier::Autopilot);
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.run = JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    l.open_stops = vec![OpenStop {
        id: "stop-1".into(),
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
    }];
    crate::job::save(&ProjectPaths::for_session(dir.path(), "bot"), &l).unwrap();

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    daemon.sweep(
        &reg(vec![agent_loop_entry("bot", dir.path())]),
        &driver,
        &clock,
        &notif,
    );
    let seen = notif.seen.lock().unwrap();
    let body = &seen.first().expect("one escalation").body;
    assert!(
        body.contains("Confirm and close, or keep adding?"),
        "question missing from the notification body: {body}"
    );
    assert!(
        body.contains("1) close") && body.contains("2) keep going"),
        "numbered options missing from the notification body: {body}"
    );
}

#[test]
fn two_agent_loop_sessions_in_one_folder_have_independent_leases() {
    // Two sessions share ONE folder. A foreign lease on session `a` must NOT
    // block session `b` — the lease is per-session (regression for the
    // two-drivers-in-one-tree hole; a root-level lease would block both).
    let dir = tempfile::tempdir().unwrap();
    for id in ["a", "b"] {
        seed_agent_loop_config(dir.path(), id, Tier::Autopilot);
        crate::job::save(
            &ProjectPaths::for_session(dir.path(), id),
            &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
        )
        .unwrap();
    }
    // Hold session `a`'s per-session driver lease from "another process".
    let a_lock = ProjectPaths::for_session(dir.path(), "a")
        .daemon_dir()
        .join("driver.lock");
    let held = crate::lease::try_acquire(&a_lock).unwrap().unwrap();

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    daemon.sweep(
        &reg(vec![
            agent_loop_entry("a", dir.path()),
            agent_loop_entry("b", dir.path()),
        ]),
        &driver,
        &clock,
        &notif,
    );
    assert_eq!(
        driver.launched().len(),
        1,
        "only `b` drives (launches its loop session); `a` is held by a foreign per-session lease"
    );
    drop(held);
}

#[test]
fn agent_loop_adopts_registry_seed_on_first_wake() {
    // Chattable-on-create S1 wiring: a registry `conversation_id` seed reaches
    // the live JobScheduler (via build + the per-sweep set_registry_seed), so the
    // first wake ADOPTS (resumes) the human-created conversation, not a mint.
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "bot", Tier::Autopilot);
    crate::job::save(
        &ProjectPaths::for_session(dir.path(), "bot"),
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    let mut p = agent_loop_entry("bot", dir.path());
    p.conversation_id = Some("seed-from-registry".into());

    let driver = FakeDriver::new();
    // No live chat session owns the seed, so the poll adopts on this first wake.
    let chat = crate::tmux::session_name("bot", dir.path());
    assert!(
        !driver.is_alive(&chat).unwrap(),
        "precondition: no live chat ⇒ adopt fires"
    );
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);

    // The ledger persisted the adopted seed (the daemon stays the sole writer), and the
    // persistent session launched ON THAT id — a fresh mint would have used a random uuid.
    // Whether the seed is RESUMED (a real chat conversation) or CREATED (a create→autopilot
    // ghost) is decided by the deterministic on-disk existence probe and covered by the
    // `seed_{with,without}_a_transcript_*` unit tests; asserting either here would make this
    // test depend on the machine's real ~/.claude, so it pins only the S1 wiring: the seed
    // reaches the scheduler and drives the launch id.
    let l = crate::job::load(&ProjectPaths::for_session(dir.path(), "bot"))
        .unwrap()
        .unwrap();
    assert_eq!(l.conversation_id.as_deref(), Some("seed-from-registry"));
    let sess = crate::tmux::session_name("bot", dir.path());
    let joined = driver
        .launched()
        .into_iter()
        .find(|(s, _)| s == &sess)
        .expect("the loop session was launched")
        .1
        .join(" ");
    assert!(
        joined.contains("seed-from-registry"),
        "the adopted seed id drives the launch (not a fresh mint): {joined}"
    );
}

#[test]
fn agent_loop_defers_adopt_while_chat_session_alive() {
    // A live (detached) chat REPL owns the seed conversation: the poll must DEFER —
    // no spawn, no adopt — until the chat session ends, so the human's REPL and a
    // poll wake never both drive the seeded conversation id.
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), "bot", Tier::Autopilot);
    crate::job::save(
        &ProjectPaths::for_session(dir.path(), "bot"),
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    let mut p = agent_loop_entry("bot", dir.path());
    p.conversation_id = Some("seed-from-registry".into());

    let driver = FakeDriver::new();
    // The human's chat session for this agent-loop row is alive on the shared socket.
    driver.set_alive(&crate::tmux::session_name("bot", dir.path()), true);
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    daemon.sweep(&reg(vec![p]), &driver, &clock, &notif);

    assert_eq!(
        driver.spawn_count(),
        0,
        "no wake spawns while a live chat owns the seed"
    );
    let l = crate::job::load(&ProjectPaths::for_session(dir.path(), "bot"))
        .unwrap()
        .unwrap();
    assert!(
        l.conversation_id.is_none(),
        "no adopt (ledger seed stays None) while a live chat owns the conversation"
    );
}
