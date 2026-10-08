//! Building a scheduler and recovering one: what a `pmd` restart must rebuild from
//! disk, what it must never kill, the benign no-session states, and `abort`.

use super::*;

// --- crash recovery ------------------------------------------------------

#[test]
fn restore_terminates_stray_pmj_worker_and_relaunches_resuming_the_same_id() {
    let fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let root = fx.sched.work_dir.clone();
    let stray = tmux::job_session_name(SESSION_ID, &root, 3);
    fx.driver.set_alive(&stray, true); // the old ephemeral worker is still "alive"
    // Write an old ephemeral ledger handle (pre-upgrade): Running on a pmj-<seq>.
    let mut l = ledger(&fx);
    l.conversation_id = Some("convo-keep".into());
    l.run = JobRun::Running {
        seq: 3,
        session: stray.clone(),
        deadline: 9999,
    };
    job::save(&fx.paths, &l).unwrap();
    // Rebuild from disk (simulates a daemon restart after the upgrade).
    let mut revived = JobScheduler::new(SESSION_ID, root.clone(), SESSION_ID, Engine::Claude, None);
    assert!(
        matches!(revived.run, JobRun::Running { .. }),
        "the old ephemeral handle is restored as Running"
    );
    let sess = tmux::session_name(SESSION_ID, &root);
    // First tick: terminate the stray worker, relaunch the persistent session.
    assert_eq!(
        revived.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + LAUNCH_GRACE_S
        }
    );
    assert!(
        !fx.driver.is_alive(&stray).unwrap(),
        "the stray pmj-<seq> worker is terminated"
    );
    assert_eq!(fx.driver.launched().len(), 1);
    assert_eq!(fx.driver.launched()[0].0, sess);
    assert_eq!(
        launched_flag(&fx, &sess, "--resume").as_deref(),
        Some("convo-keep"),
        "relaunched RESUMING the SAME conversation id (never re-created)"
    );
    assert_eq!(
        job::load(&fx.paths)
            .unwrap()
            .unwrap()
            .conversation_id
            .as_deref(),
        Some("convo-keep"),
        "conversation_id is never lost"
    );
}

#[test]
fn a_pmd_restart_never_terminates_the_live_persistent_session() {
    // m23 bug 2, and it fired on EVERY restart. `ensure_session` records the persistent
    // session's own handle with `write_driver_running(0, &self.loop_session(), …)` and
    // never marks it ended while it lives, so a healthy session's `driver.json` reads
    // `Running{pane: "pmloop-…"}` — indistinguishable, before the pane check, from a
    // stray ephemeral worker. `restore_from_disk` therefore restored `Running`, and the
    // first tick's `Running` arm `terminate`d that pane: restarting the daemon killed the
    // user's running agent and threw away its in-flight turn.
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let root = fx.sched.work_dir.clone();
    let sess = loop_session(&fx);
    // Tick once so the session is really launched and `driver.json` holds ITS handle —
    // the premise of the bug, asserted rather than assumed.
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + LAUNCH_GRACE_S
        }
    );
    let d = state::read_json_opt::<DriverState>(&fx.paths.driver())
        .unwrap()
        .expect("the launch recorded a driver handle");
    assert_eq!(d.exit_reason, ExitReason::Running);
    assert_eq!(
        d.pane, sess,
        "premise: the recorded pane IS the loop session"
    );

    // Restart pmd over the same paths.
    let mut revived = JobScheduler::new(SESSION_ID, root, SESSION_ID, Engine::Claude, None);
    assert!(
        !matches!(revived.run, JobRun::Running { .. }),
        "the persistent session's OWN handle must not restore as a stray worker: {:?}",
        revived.run
    );
    let _ = revived.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        fx.driver.launched().len(),
        1,
        "a second launch is the observable proof the live agent was killed and re-driven"
    );
    assert!(
        fx.driver.is_alive(&sess).unwrap(),
        "the live agent must survive a daemon restart"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "nothing is typed at a session that was mid-turn"
    );
}

#[test]
fn a_pmd_restart_still_terminates_a_legacy_pmj_handle_in_driver_json() {
    // The other half of the pane check: the terminate-and-re-drive path is REAL (a
    // pre-upgrade `driver.json` can name an ephemeral `pmj-<seq>` worker that is still
    // editing files) and must keep working, so the fix is a pane comparison and not the
    // removal of the `Running` restore.
    let fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let root = fx.sched.work_dir.clone();
    let stray = tmux::job_session_name(SESSION_ID, &root, 7);
    fx.driver.set_alive(&stray, true);
    state::write_driver(
        &fx.paths,
        &DriverState {
            step_id: 7,
            pane: stray.clone(),
            spawned_at: START,
            deadline: START + 100,
            ended_at: None,
            exit_code: None,
            exit_reason: ExitReason::Running,
            consecutive_failures: 0,
            observed_at: START,
        },
    )
    .unwrap();
    let mut revived = JobScheduler::new(SESSION_ID, root.clone(), SESSION_ID, Engine::Claude, None);
    assert!(
        matches!(revived.run, JobRun::Running { .. }),
        "a pmj-<seq> handle still restores as Running: {:?}",
        revived.run
    );
    let _ = revived.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        !fx.driver.is_alive(&stray).unwrap(),
        "the stray ephemeral worker must still be terminated so it cannot keep editing"
    );
    assert_eq!(
        fx.driver.launched().len(),
        1,
        "and the persistent session is re-driven in its place"
    );
    assert_eq!(
        fx.driver.launched()[0].0,
        tmux::session_name(SESSION_ID, &root)
    );
}

#[test]
fn restore_resumes_blocked_without_renotifying_or_launching() {
    let fx = blocked_fx();
    let root = fx.sched.work_dir.clone();
    // Rebuild from disk: the Blocked park is restored verbatim.
    let mut revived = JobScheduler::new(SESSION_ID, root, SESSION_ID, Engine::Claude, None);
    assert!(matches!(revived.run, JobRun::Blocked { .. }));
    assert!(matches!(
        revived.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert!(
        fx.driver.launched().is_empty(),
        "no launch while parked Blocked"
    );
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn no_ledger_is_waiting_for_intake() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), SESSION_ID);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    state::write_json_atomic(
        &paths.config(),
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
    let mut sched = JobScheduler::new(
        SESSION_ID,
        dir.path().to_path_buf(),
        SESSION_ID,
        Engine::Claude,
        None,
    );
    let driver = FakeDriver::new();
    let clock = FakeClock::new(START);
    assert_eq!(
        sched.tick(&driver, &clock).unwrap(),
        JobTick::Monitoring {
            until: START + LAUNCH_GRACE_S
        }
    );
    assert_eq!(
        driver.launched().len(),
        1,
        "pmd creates the missing ledger and terminal"
    );
}

#[test]
fn missing_config_is_waiting_for_intake_not_an_error() {
    // Bug A (create-time race): pmtui registers the session and writes the ledger,
    // but pmd may tick BEFORE the per-session config.json write lands. A missing
    // config must be the benign idle tick (mirroring the missing-ledger case) — NOT
    // an Err, which the daemon's poison counter would strike toward a spurious
    // "corrupt state?" pause of a brand-new session.
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), SESSION_ID);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    // A ledger exists (state.json) but NO config.json (the create-race window).
    job::save(
        &paths,
        &AgentLoopState::fresh(Engine::Claude, Some(300), START),
    )
    .unwrap();
    assert!(
        !paths.config().try_exists().unwrap(),
        "precondition: config.json is absent"
    );
    let mut sched = JobScheduler::new(
        SESSION_ID,
        dir.path().to_path_buf(),
        SESSION_ID,
        Engine::Claude,
        None,
    );
    let driver = FakeDriver::new();
    let clock = FakeClock::new(START);
    assert_eq!(
        sched.tick(&driver, &clock).unwrap(),
        JobTick::WaitingForIntake,
        "a missing config is a benign idle tick, not an error"
    );
    // No drive / nudge, and the ledger `run` is untouched.
    assert!(driver.launched().is_empty(), "must not launch a session");
    assert!(driver.sent_keys().is_empty(), "must not nudge");
    assert_eq!(
        job::load(&paths).unwrap().unwrap().run,
        JobRun::Idle,
        "the ledger run is not mutated"
    );
}

#[test]
fn present_but_malformed_config_still_errors() {
    // The missing-vs-malformed distinction is the whole point of the fix: an ABSENT
    // config is "not provisioned yet" (benign), but a config that EXISTS and is
    // unparseable is genuine corruption (pmtui writes it atomically tmp+rename, so a
    // present config is always complete) → keep erroring so the daemon still poisons
    // truly corrupt state.
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), SESSION_ID);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    std::fs::write(paths.config(), "{ not json").unwrap();
    job::save(
        &paths,
        &AgentLoopState::fresh(Engine::Claude, Some(300), START),
    )
    .unwrap();
    let mut sched = JobScheduler::new(
        SESSION_ID,
        dir.path().to_path_buf(),
        SESSION_ID,
        Engine::Claude,
        None,
    );
    let driver = FakeDriver::new();
    let clock = FakeClock::new(START);
    assert!(
        sched.tick(&driver, &clock).is_err(),
        "a present-but-malformed config is corrupt state and must error"
    );
}

// --- launch failure recovery ---------------------------------------------

struct LaunchFailDriver;

impl Driver for LaunchFailDriver {
    fn spawn_step(
        &self,
        _session: &str,
        _cwd: &Path,
        _command: &[String],
        _done_signal: &Path,
        _log: &Path,
    ) -> anyhow::Result<tmux::StepHandle> {
        unreachable!("the persistent-session launch path does not spawn a step")
    }

    fn is_alive(&self, _session: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn capture_tail(&self, _session: &str, _lines: usize) -> anyhow::Result<String> {
        Ok(String::new())
    }

    fn terminate(&self, _session: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[test]
fn launch_error_identifies_the_persistent_session_and_preserves_the_ledger() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let session = loop_session(&fx);
    let base = ledger(&fx);

    let error = fx
        .sched
        .ensure_session(&LaunchFailDriver, START, &base)
        .unwrap_err();

    assert!(
        format!("{error:#}").contains(&format!(
            "launch persistent loop session {session} for {SESSION_ID}"
        )),
        "the launch error must identify both the terminal and project: {error:#}"
    );
    assert_eq!(
        ledger(&fx),
        base,
        "a failed launch must not mutate the durable ledger"
    );
}

#[test]
fn ledger_save_failure_terminates_the_just_launched_session() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let session = loop_session(&fx);
    let base = ledger(&fx);
    std::fs::remove_file(fx.paths.pmstate()).unwrap();
    std::fs::create_dir(fx.paths.pmstate()).unwrap();

    let error = fx
        .sched
        .ensure_session(&fx.driver, START, &base)
        .unwrap_err();

    assert!(
        format!("{error:#}").contains(&format!(
            "persist ledger after launching loop session for {SESSION_ID}"
        )),
        "the save error must identify the project: {error:#}"
    );
    assert!(
        !fx.driver.is_alive(&session).unwrap(),
        "a live process without a durable ledger must be terminated"
    );
    assert_eq!(
        fx.sched.run, base.run,
        "the in-memory run state changes only after the durable save"
    );
}

#[test]
fn abort_terminates_the_loop_session() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch → sess alive
    assert!(fx.driver.is_alive(&sess).unwrap());
    fx.sched.abort(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(fx.sched.run, JobRun::Idle));
    assert!(
        !fx.driver.is_alive(&sess).unwrap(),
        "abort terminates the persistent loop session"
    );
}

#[test]
fn abort_terminates_a_legacy_running_handle() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let session = "pmj-legacy".to_string();
    fx.driver.set_alive(&session, true);
    fx.sched.run = JobRun::Running {
        seq: 7,
        session: session.clone(),
        deadline: START + 10,
    };

    fx.sched.abort(&fx.driver, &fx.clock).unwrap();

    assert!(!fx.driver.is_alive(&session).unwrap());
}
