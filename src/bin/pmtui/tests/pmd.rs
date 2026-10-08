//! Observing the daemon: the cached liveness probe that never creates the lock file, the
//! down streak that counts only fresh samples, and the waits that keep waiting through a
//! slow boot rather than trusting an unknown.

use super::*;

fn wait_for_spawned_children(app: &App) {
    for _ in 0..100 {
        app.reap_spawned_pmd();
        if app.spawned_pmd.borrow().is_empty() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("spawned daemon stub did not exit");
}

#[test]
fn daemon_ensure_reports_liveness_and_keeps_the_shipped_status_copy() {
    // `is_up` is the arm's precondition; `Display` is what the three print-only
    // callers (`cycle_tier`, `submit_create`, `ensure_daemon_for_enabled_autopilot`)
    // show — pinned byte-for-byte so typing the outcome did not silently reword them.
    assert!(DaemonEnsure::Started.is_up());
    assert!(DaemonEnsure::AlreadyRunning.is_up());
    assert!(!DaemonEnsure::Failed("boom".into()).is_up());
    assert_eq!(DaemonEnsure::Started.to_string(), "daemon started");
    assert_eq!(
        DaemonEnsure::AlreadyRunning.to_string(),
        "daemon already running"
    );
    assert_eq!(
        DaemonEnsure::Failed("daemon start failed: nope".into()).to_string(),
        "daemon start failed: nope"
    );
}

#[test]
fn spawn_daemon_from_executable_records_arguments_and_logs() {
    let dir = tempfile::tempdir().unwrap();
    let registry_dir = dir.path().join("state");
    std::fs::create_dir(&registry_dir).unwrap();
    let registry = registry_dir.join("registry.json");
    let app = App::new(registry.clone(), "pm-spawn-success".into());

    assert_eq!(
        app.spawn_daemon_from(Path::new("/bin/echo")),
        DaemonEnsure::Started,
        "an executable sibling starts successfully"
    );
    wait_for_spawned_children(&app);

    assert_eq!(
        std::fs::read_to_string(registry_dir.join("pmd.log")).unwrap(),
        format!(
            "--socket pm-spawn-success --registry {}\n",
            registry.display()
        )
    );
    assert!(
        registry_dir.join("pmd.log").exists(),
        "spawn creates the daemon log beside the registry"
    );
}

#[test]
fn spawn_daemon_from_reports_missing_and_unexecutable_programs() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(dir.path().join("registry.json"), "pm-spawn-errors".into());
    let missing = dir.path().join("missing-pmd");

    assert_eq!(
        app.spawn_daemon_from(&missing),
        DaemonEnsure::Failed(format!("pmd binary not found at {}", missing.display()))
    );

    let unexecutable = dir.path().join("not-a-program");
    std::fs::create_dir(&unexecutable).unwrap();
    let DaemonEnsure::Failed(reason) = app.spawn_daemon_from(&unexecutable) else {
        panic!("a directory cannot be started as a daemon");
    };
    assert!(reason.starts_with("daemon start failed:"), "{reason}");
    assert!(app.spawned_pmd.borrow().is_empty());
}

#[test]
fn spawn_daemon_discards_output_when_the_log_cannot_be_opened() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_directory = dir.path().join("not-a-directory");
    std::fs::write(&not_a_directory, "occupied").unwrap();
    let app = App::new(
        not_a_directory.join("registry.json"),
        "pm-spawn-no-log".into(),
    );

    assert_eq!(
        app.spawn_daemon_from(Path::new("/bin/echo")),
        DaemonEnsure::Started
    );
    wait_for_spawned_children(&app);
    assert!(
        !not_a_directory.join("pmd.log").exists(),
        "a log failure falls back to null output without blocking the daemon"
    );
}

#[test]
fn daemon_path_is_the_pmd_sibling_of_the_running_executable() {
    assert_eq!(
        App::pmd_path_for_executable(Path::new("/tmp/agent-manager/pmtui")),
        Some(PathBuf::from("/tmp/agent-manager/pmd"))
    );
    assert_eq!(App::pmd_path_for_executable(Path::new("/")), None);
}

#[test]
fn spawn_daemon_reports_the_missing_real_sibling_without_running_libtest() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(dir.path().join("registry.json"), "pm-real-sibling".into());

    let DaemonEnsure::Failed(reason) = app.spawn_daemon() else {
        panic!("the test executable must not have a real pmd sibling");
    };
    assert!(
        reason.starts_with("pmd binary not found at "),
        "the test must stop before trying to execute its own libtest binary: {reason}"
    );
}

#[test]
fn spawn_daemon_reports_executable_lookup_and_parent_errors() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(dir.path().join("registry.json"), "pm-exe-errors".into());

    assert_eq!(
        app.spawn_daemon_for_executable(Err(std::io::Error::other("lookup failed"))),
        DaemonEnsure::Failed("could not locate pmtui exe".into())
    );
    assert_eq!(
        app.spawn_daemon_for_executable(Ok(PathBuf::from("/"))),
        DaemonEnsure::Failed("could not locate pmd".into())
    );
}

#[test]
fn ensure_daemon_covers_free_owned_and_broken_socket_ownership() {
    let dir = tempfile::tempdir().unwrap();

    let free_app = App::new(dir.path().join("free.json"), "pm-owner-free".into());
    let free_owner = dir.path().join("free-owner.lock");
    assert_eq!(
        free_app.ensure_daemon_with(&free_owner, || DaemonEnsure::Started),
        DaemonEnsure::Started
    );

    let held_app = App::new(dir.path().join("held.json"), "pm-owner-held".into());
    let held_owner = dir.path().join("held-owner.lock");
    let _held = acquire_free_lease(&held_owner).expect("the owner lock is free");
    assert_eq!(
        held_app.ensure_daemon_with(&held_owner, || {
            panic!("an owned socket must not spawn another daemon")
        }),
        DaemonEnsure::Failed("socket pm-owner-held belongs to another registry".into())
    );

    let broken_app = App::new(dir.path().join("broken.json"), "pm-owner-broken".into());
    let not_a_directory = dir.path().join("not-a-directory");
    std::fs::write(&not_a_directory, "occupied").unwrap();
    let broken_owner = not_a_directory.join("owner.lock");
    let DaemonEnsure::Failed(reason) = broken_app.ensure_daemon_with(&broken_owner, || {
        panic!("a failed ownership check must not spawn")
    }) else {
        panic!("an unusable owner-lock path must fail");
    };
    assert!(
        reason.starts_with("socket ownership check failed:"),
        "{reason}"
    );
}

#[test]
fn reaping_spawned_daemons_drops_exited_children_and_keeps_live_ones() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(dir.path().join("registry.json"), "pm-test".into());
    let mut exited = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    exited.wait().unwrap();
    let mut live = std::process::Command::new("sh")
        .args(["-c", "read -r _"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let live_pid = live.id();
    let live_stdin = live.stdin.take().unwrap();
    app.spawned_pmd.borrow_mut().extend([exited, live]);

    app.reap_spawned_pmd();

    let retained_pids: Vec<u32> = app
        .spawned_pmd
        .borrow()
        .iter()
        .map(std::process::Child::id)
        .collect();
    drop(live_stdin);
    for mut child in std::mem::take(&mut *app.spawned_pmd.borrow_mut()) {
        child.wait().unwrap();
    }
    assert_eq!(
        retained_pids,
        [live_pid],
        "only the still-running child remains"
    );
}

#[test]
fn reaping_spawned_daemons_is_inert_while_the_child_list_is_borrowed() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(dir.path().join("registry.json"), "pm-borrowed".into());
    let borrowed = app.spawned_pmd.borrow();

    app.reap_spawned_pmd();

    assert!(borrowed.is_empty());
}

#[test]
fn ensure_daemon_reports_daemon_lock_probe_errors() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_directory = dir.path().join("not-a-directory");
    std::fs::write(&not_a_directory, "occupied").unwrap();
    let app = App::new(not_a_directory.join("registry.json"), "pm-test".into());

    let result = app.ensure_daemon();

    let DaemonEnsure::Failed(reason) = result else {
        panic!("an unusable registry directory must fail the daemon probe");
    };
    assert!(reason.starts_with("daemon check failed:"), "{reason}");
    assert!(app.spawned_pmd.borrow().is_empty());
}

#[test]
fn stop_daemon_waits_for_lock_release_and_reaps_exited_children() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let app = App::new(registry.clone(), "pm-stop-test".into());
    let mut exited = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    exited.wait().unwrap();
    app.spawned_pmd.borrow_mut().push(exited);
    let lock = lease::daemon_lock_path(&registry, "pm-stop-test");
    let held = acquire_free_lease(&lock).expect("the daemon lock is free to pre-acquire");
    let stop = lease::daemon_stop_path(&registry, "pm-stop-test");
    let stop_for_owner = stop.clone();
    let owner = std::thread::spawn(move || {
        for _ in 0..400 {
            if stop_for_owner.exists() {
                drop(held);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the stop request was not written");
    });

    let stopped = app.stop_daemon();
    owner.join().unwrap();

    assert!(stopped, "lock release must confirm the daemon stopped");
    assert_eq!(
        std::fs::read_to_string(stop).unwrap(),
        format!("{}\n", std::process::id())
    );
    assert!(
        app.spawned_pmd.borrow().is_empty(),
        "successful stop reaps children that already exited"
    );
}

#[test]
fn stop_daemon_reports_a_stop_request_write_failure() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let app = App::new(registry.clone(), "pm-stop-write-error".into());
    let lock = lease::daemon_lock_path(&registry, "pm-stop-write-error");
    let _held = acquire_free_lease(&lock).expect("the daemon lock is free to pre-acquire");
    let stop = lease::daemon_stop_path(&registry, "pm-stop-write-error");
    std::fs::create_dir_all(&stop).unwrap();

    assert!(
        !app.stop_daemon(),
        "a directory at the stop-request path must be reported as a write failure"
    );
}

#[test]
fn startup_autopilot_ensure_handles_no_match_and_one_match() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = loop_app(&reg_path);
    app.status = "unchanged".into();

    app.ensure_daemon_for_enabled_autopilot();

    assert_eq!(
        app.status, "unchanged",
        "a Standard-only registry is a silent no-op"
    );
    assert!(
        !lease::daemon_lock_path(&reg_path, &app.socket).exists(),
        "the no-op must not probe through the creating lock API"
    );

    state::write_json_atomic(
        &ProjectPaths::for_session(&root, "bot").config(),
        &config_at(Tier::Autopilot),
    )
    .unwrap();
    let _held = lease::try_acquire(&lease::daemon_lock_path(&reg_path, "pm-test"))
        .unwrap()
        .expect("the daemon lock is free to pre-acquire");

    app.ensure_daemon_for_enabled_autopilot();

    assert_eq!(
        app.status, "bot is on Autopilot; daemon already running",
        "one matching session is named directly"
    );
}

#[test]
fn startup_status_counts_multiple_enabled_autopilot_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, first_root) = reg_with_agent_loop(dir.path(), "first");
    let second_root = dir.path().join("second");
    let mut registry = Registry::load(&reg_path).unwrap();
    registry.projects.push(ProjectEntry {
        id: "second".into(),
        display_name: None,
        root: second_root.clone(),
        enabled: true,
        mode: Mode::AgentLoop,
        engine: Some(Engine::Claude),
        worker_model: None,
        initial_prompt: None,
        task_title: None,
        forked_from: None,
        spawned_by: None,
        launch: None,
        conversation_id: None,
        cadence_s: Some(300),
    });
    registry.save(&reg_path).unwrap();
    for (id, root) in [("first", &first_root), ("second", &second_root)] {
        state::write_json_atomic(
            &ProjectPaths::for_session(root, id).config(),
            &config_at(Tier::Autopilot),
        )
        .unwrap();
    }
    let _held = lease::try_acquire(&lease::daemon_lock_path(&reg_path, "pm-test"))
        .unwrap()
        .expect("the daemon lock is free to pre-acquire");
    let mut app = loop_app(&reg_path);
    app.status.clear();

    app.ensure_daemon_for_enabled_autopilot();

    assert_eq!(
        app.status, "2 sessions on Autopilot; daemon already running",
        "the startup status must summarize multiple driven sessions"
    );
    assert!(app.spawned_pmd.borrow().is_empty());
}

#[test]
fn stop_daemon_reports_absent_broken_and_unresponsive_daemons() {
    let dir = tempfile::tempdir().unwrap();

    let absent = App::new(dir.path().join("absent.json"), "pm-stop-absent".into());
    assert!(
        !absent.stop_daemon(),
        "a free singleton lock means there is no daemon to stop"
    );

    let not_a_directory = dir.path().join("not-a-directory");
    std::fs::write(&not_a_directory, "occupied").unwrap();
    let broken = App::new(
        not_a_directory.join("registry.json"),
        "pm-stop-broken".into(),
    );
    assert!(
        !broken.stop_daemon(),
        "an unreadable singleton lock cannot identify a daemon"
    );

    let registry = dir.path().join("unresponsive.json");
    let unresponsive = App::new(registry.clone(), "pm-stop-timeout".into());
    let lock = lease::daemon_lock_path(&registry, "pm-stop-timeout");
    let _held = acquire_free_lease(&lock).expect("the daemon lock is free to pre-acquire");
    assert!(
        !unresponsive.stop_daemon_with_budget(1, std::time::Duration::ZERO),
        "a daemon that keeps its lock past the budget is not reported stopped"
    );
    assert!(
        lease::daemon_stop_path(&registry, "pm-stop-timeout").exists(),
        "the unresponsive daemon still receives a stop request"
    );
}

#[test]
fn status_bar_reports_the_daemon_as_up_down_or_unknown() {
    // Nothing on this screen used to answer "is a daemon running?" — `app.socket` is
    // a tmux SOCKET NAME and reads identically either way. Three states, because a
    // failed probe must never be reported as `up`.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");

    // DOWN: no lock file at all (no pmd has ever run here).
    let mut app = loop_app(&reg_path);
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render down");
    assert!(
        screen_text(&terminal).contains("pmd DOWN"),
        "no daemon must read DOWN: {}",
        screen_text(&terminal)
    );

    // UP: hold the singleton flock the way a real pmd does (flock is per
    // open-file-description, so an in-process holder models another process).
    let _held = lease::try_acquire(&lease::daemon_lock_path(&reg_path, &app.socket))
        .unwrap()
        .expect("the daemon lock is free to pre-acquire");
    app.restart_daemon_watch(); // don't read the cached DOWN sample
    terminal.draw(|f| render(f, &app)).expect("render up");
    let up = screen_text(&terminal);
    assert!(up.contains("pmd up"), "a held lock must read up: {up}");
    assert!(!up.contains("pmd DOWN"), "and not both: {up}");

    // UNKNOWN: a probe FAULT (here `<regular-file>/pmd-*.lock` ⇒ ENOTDIR) is neither
    // up nor down, and must never claim up.
    let broken = dir.path().join("not-a-dir");
    std::fs::write(&broken, "{}").unwrap();
    app.registry_path = broken.join("registry.json");
    app.restart_daemon_watch();
    terminal.draw(|f| render(f, &app)).expect("render unknown");
    let unknown = screen_text(&terminal);
    assert!(
        unknown.contains("pmd ?"),
        "a failed probe must read as unknown: {unknown}"
    );
    assert!(
        !unknown.contains("pmd up"),
        "a failed probe must never claim up: {unknown}"
    );
}

#[test]
fn daemon_liveness_is_cached_and_never_creates_the_lock_file() {
    // The render loop redraws ~2x/second, so this MUST NOT be a per-frame `open()`
    // (the same reason `refresh` limits its tmux probes to idle ticks). And the probe
    // must not CREATE the lock file: several tests here use its existence as proof
    // that an `ensure_daemon` ran, so a creating probe would silently void them.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let app = loop_app(&reg_path);
    let lock = lease::daemon_lock_path(&reg_path, &app.socket);

    assert_eq!(app.daemon_live(), DaemonLive::Down);
    assert!(
        !lock.exists(),
        "the liveness probe must not create the daemon singleton lock file"
    );

    // A holder appears, but within the TTL the CACHED answer is reused — proving the
    // probe is not re-run per call.
    let held = lease::try_acquire(&lock).unwrap().expect("free");
    assert_eq!(
        app.daemon_live(),
        DaemonLive::Down,
        "inside the TTL the cached sample is reused"
    );
    // Age the cache past the TTL (the same effect as the next idle tick a few
    // seconds later) and the fresh probe sees the holder.
    expire_daemon_cache(&app);
    assert_eq!(
        app.daemon_live(),
        DaemonLive::Up,
        "past the TTL it re-probes and sees the holder"
    );
    drop(held);
}

#[test]
fn daemon_down_streak_counts_only_fresh_samples() {
    // The streak is the TIME base for `armed_wait_decision`: it may advance only when
    // a probe actually runs (once per TTL), never per frame, or a 500ms render loop
    // would "confirm" a 9s outage in 2 seconds.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let app = loop_app(&reg_path);

    for _ in 0..10 {
        assert_eq!(app.daemon_live(), DaemonLive::Down);
    }
    assert_eq!(
        app.daemon_down_streak.get(),
        1,
        "ten cache hits are ONE observation"
    );

    // Expire the cache twice ⇒ two more fresh samples.
    for _ in 0..2 {
        expire_daemon_cache(&app);
        app.daemon_live();
    }
    assert_eq!(app.daemon_down_streak.get(), 3);

    // An `Up` sample BREAKS the streak (a late-booting daemon is not an outage).
    let _held = lease::try_acquire(&lease::daemon_lock_path(&reg_path, &app.socket))
        .unwrap()
        .expect("free");
    app.restart_daemon_watch();
    assert_eq!(app.daemon_live(), DaemonLive::Up);
    assert_eq!(app.daemon_down_streak.get(), 0);
}

#[test]
fn armed_wait_decision_needs_a_stable_down_and_never_trusts_unknown() {
    // PURE: the whole Task-5 rule in one function. A single DOWN sample is a booting
    // daemon (`ensure_daemon` returns the instant `spawn()` succeeds — nothing
    // confirms the child took the flock yet), and an `Unknown` probe is not an outage.
    assert_eq!(
        armed_wait_decision(DaemonLive::Down, 1),
        ArmedWait::KeepWaiting,
        "one sample could be a boot in progress"
    );
    assert_eq!(
        armed_wait_decision(DaemonLive::Down, DAEMON_DOWN_DISARM_SAMPLES - 1),
        ArmedWait::KeepWaiting
    );
    assert_eq!(
        armed_wait_decision(DaemonLive::Down, DAEMON_DOWN_DISARM_SAMPLES),
        ArmedWait::DisarmDaemonDown,
        "an unbroken run of DOWN samples is believable"
    );
    for streak in [0, 1, DAEMON_DOWN_DISARM_SAMPLES, u32::MAX] {
        assert_eq!(
            armed_wait_decision(DaemonLive::Up, streak),
            ArmedWait::KeepWaiting
        );
        assert_eq!(
            armed_wait_decision(DaemonLive::Unknown, streak),
            ArmedWait::KeepWaiting,
            "a failed probe must never disarm"
        );
    }
}

#[test]
fn armed_drain_disarms_for_both_engines_when_pmd_is_observed_down() {
    // The hole the drain bound alone cannot close: `DaemonEnsure::Started` is
    // optimistic, so a pmd that dies during boot leaves a CODEX arm (unbounded on
    // purpose) waiting forever behind "autopilot is starting the agent". A stable
    // DOWN observation disarms it — and says pmd, not something vague.
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
        let sp = ProjectPaths::for_session(&root, "bot");
        set_tier(&sp, Tier::Autopilot); // Idle ledger, no cid ⇒ the drain would Stay
        let mut app = loop_app(&reg_path);
        app.pending_first_chat = Some("bot".into());
        app.armed_drains_left = match engine {
            Engine::Claude => Some(AUTOPILOT_ARM_DRAINS),
            Engine::Codex => None, // the unbounded arm — the one that used to hang
        };
        // Pre-seed a streak one short of the threshold, then let the drain take the
        // sample that reaches it (no lock file exists ⇒ DOWN).
        app.daemon_down_streak.set(DAEMON_DOWN_DISARM_SAMPLES - 1);

        app.drain_armed_first_chat();

        assert!(
            app.pending_first_chat.is_none(),
            "{engine:?}: a stable pmd-DOWN must disarm the auto-open"
        );
        assert!(
            app.armed_drains_left.is_none(),
            "{engine:?}: the bound is cleared with the arm"
        );
        assert!(
            app.status.starts_with("pmd is not running"),
            "{engine:?}: the verdict is front-loaded (keybar truncates the tail): {}",
            app.status
        );
        assert!(
            app.pending_chat.is_none() && app.pending_attach_loop.is_none(),
            "{engine:?}: giving up must not launch or attach anything"
        );
    }
}

#[test]
fn armed_drain_keeps_waiting_through_a_slow_daemon_boot() {
    // The boot race, from the other side: a freshly-armed row has a ZERO streak (the
    // arm calls `restart_daemon_watch`), so several drains inside one boot window
    // must NOT disarm — otherwise this "honest" give-up becomes its own false alarm
    // on a loaded machine.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    app.armed_drains_left = Some(AUTOPILOT_ARM_DRAINS);
    app.restart_daemon_watch();

    // Many idle ticks, all inside ONE probe TTL (that is what the cache buys).
    for _ in 0..20 {
        app.drain_armed_first_chat();
    }
    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "20 drains inside one TTL must not disarm a booting daemon: {}",
        app.status
    );
    assert_eq!(app.daemon_down_streak.get(), 1, "one observation, not 20");
    assert!(
        !app.status.starts_with("pmd is not running"),
        "no premature verdict: {}",
        app.status
    );
}

#[test]
fn armed_drain_never_disarms_while_a_daemon_holds_the_lock() {
    // A live pmd must be able to take as long as it likes: with the singleton flock
    // held, the drain stays armed no matter how stale the streak was.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    let _held = lease::try_acquire(&lease::daemon_lock_path(&reg_path, &app.socket))
        .unwrap()
        .expect("free");
    app.pending_first_chat = Some("bot".into());
    app.armed_drains_left = None; // codex: unbounded
    app.daemon_down_streak.set(u32::MAX); // stale history, before the ensure

    app.drain_armed_first_chat();

    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "a held lock means a daemon IS up: stay armed ({})",
        app.status
    );
    assert_eq!(
        app.daemon_down_streak.get(),
        0,
        "an Up sample resets the streak"
    );
}
