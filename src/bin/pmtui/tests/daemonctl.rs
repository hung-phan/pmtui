//! Spawning and stopping `pmd`: the creates and startups that ensure a daemon, `r`'s
//! confirm that names what it costs, and the pid a stop refuses to signal because it
//! cannot prove it owns the lock.

use super::*;

#[test]
fn r_on_a_stale_row_reports_gone_and_drops_it() {
    // A row on screen but gone from the registry: refuse AND drop the row, so the key
    // cannot take the same dead branch forever (the whole `a_stale_row_is_refreshed...`
    // family agrees on this).
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    Registry::default().save(&reg_path).unwrap();
    let mut app = app_with(vec![agent_loop_view("ghost")], UiMode::Normal);
    app.registry_path = reg_path;
    app.begin_restart();
    assert!(matches!(app.mode, UiMode::Normal), "{:?}", app.mode);
    assert!(
        app.status.contains("ghost") && app.status.contains("gone from the list"),
        "{}",
        app.status
    );
    assert!(app.projects.is_empty(), "the stale row survived `r`");
}

#[test]
fn r_always_confirms_and_the_confirm_names_what_it_costs() {
    // A restart throws away the turn in flight and `r` sits one key from `d`, so there
    // is no "idle row restarts instantly" shortcut the way `d` has one. The overlay has
    // to say BOTH halves — what is lost (the reply in flight) and what is kept (the
    // conversation) — because "restart" alone tells you neither.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;
    handle_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    let UiMode::Confirming { id, what, .. } = &app.mode else {
        panic!("`r` must confirm, got {:?}", app.mode);
    };
    assert_eq!(id, "bot");
    assert_eq!(*what, Confirmable::Restart);

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    for line in [
        "Confirm restart",
        "Restart bot's agent?",
        "Stops pmd",
        "same conversation",
        // On ONE row: at this width the frame gives the body 60 columns, and the first
        // draft of this line wrapped mid-phrase.
        "a reply in flight is lost",
        "y restart",
    ] {
        assert!(screen.contains(line), "confirm missing {line:?}: {screen}");
    }
    // The BAR, not just the overlay: one confirm serves two destructive actions, and a
    // bar reading `y Delete` under a restart prompt is the worst lie this screen can
    // tell. It drew exactly that for one render — which is why this is asserted.
    assert!(
        screen.contains("y  Restart") && !screen.contains("Delete"),
        "the keybar names the wrong action: {screen}"
    );

    // Any key that is not y/Y cancels — including Enter, which is `attach` next door.
    for code in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('n')] {
        let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
        app.mode = UiMode::Confirming {
            id: "bot".into(),
            session: "pmloop-bot".into(),
            what: Confirmable::Restart,
        };
        handle_key(&mut app, code, KeyModifiers::NONE);
        assert!(
            matches!(app.mode, UiMode::Normal),
            "{code:?}: {:?}",
            app.mode
        );
        assert!(
            app.status.contains("restart cancelled"),
            "{code:?} must say it cancelled a RESTART (not a removal): {}",
            app.status
        );
    }
}

#[test]
fn stop_daemon_never_signals_a_pid_it_cannot_prove_owns_the_lock() {
    // THE safety property of the restart key. `stop_daemon` sends a real SIGTERM, so
    // every path that reaches `kill` must first prove a live daemon holds the lock.
    // The flock is that proof; the pid in the file never is.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    Registry::default().save(&reg_path).unwrap();
    let mut app = App::new(reg_path.clone(), "pm-test-stop".into());
    let lock = lease::daemon_lock_path(&reg_path, "pm-test-stop");

    // (1) Lock FREE — no daemon is running, whatever the file says. A pid left behind by
    // a crashed daemon may belong to something else entirely by now, so this must be a
    // no-op and must not consult the pid at all. Our OWN pid is the fixture precisely
    // because signalling it would kill this test process: if the free-lock check ever
    // regresses, this test dies rather than passing quietly.
    std::fs::write(&lock, std::process::id().to_string()).unwrap();
    assert!(
        !app.stop_daemon(),
        "a free lock must report NOTHING was stopped"
    );

    // (2) Lock HELD but the pid is unreadable — the flock says something is there and we
    // still have no one to signal. Refuse rather than guess. `stop_daemon` above opened+released
    // this same lock, so a single-shot re-acquire is flaky under a sibling test's fork→exec (the
    // dropped fd reads as held for the CLOEXEC-at-exec window); `acquire_free_lease` retries.
    let held = acquire_free_lease(&lock).expect("lock was free");
    std::fs::write(&lock, "not-a-pid").unwrap();
    assert!(
        !app.stop_daemon(),
        "an unparseable pid must not be signalled"
    );
    drop(held);

    // And a restart on a row that is not in the registry stops and starts nothing, and
    // says so, rather than reporting a restart it did not perform.
    app.projects = vec![agent_loop_view("bot")];
    app.restart_agent("bot");
    assert!(
        app.status.contains("bot") && app.status.contains("gone from the list"),
        "a row absent from the registry restarts nothing and says so: {}",
        app.status
    );
}

#[test]
fn submit_create_ensures_the_daemon_only_on_autopilot() {
    // THE create-time half of "autopilot at create must behave like autopilot via
    // `m`": pmd is the only thing that runs an agent-loop heartbeat, and
    // `submit_create` used not to ensure one — so a session created with Autopilot
    // already selected sat there doing nothing until pmtui was restarted, while the
    // same session flipped on with `m` ran immediately.
    //
    // The TIER is now half the gate, which REVERSES m12's call here (it gated on the
    // mode alone, on the then-true grounds that a Standard agent-loop row was equally
    // undriven without a daemon). Since m15 a Standard row is undriven BY DESIGN
    // (`daemon::pmd_drives_row`), so a daemon started for one would drive every OTHER
    // project while doing nothing for this row — and would contradict the status that
    // just told the human they drive this session. Both directions are asserted, so a
    // future "simplification" back to a mode-only gate fails here.
    for tier in [Tier::Standard, Tier::Autopilot] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let proj = std::fs::canonicalize({
            let p = dir.path().join("bots");
            std::fs::create_dir_all(&p).unwrap();
            p
        })
        .unwrap();

        // On AUTOPILOT: pre-acquire the singleton lock (matching `creating_loop_app`'s
        // "pm-test" socket) and hold it across the submit, so the ensure that DOES run
        // takes `ensure_daemon`'s "already running" no-spawn branch and never spawns a
        // real pmd. On STANDARD: deliberately do NOT pre-acquire, so the lock FILE's
        // absence afterwards is the (deterministic) proof that no ensure ran at all —
        // `lease::try_acquire` opens it `create(true)`, which is the same observable
        // the sibling refusal tests use. Acquirability would NOT be a sound observable
        // here: this is a multi-threaded test binary that forks children (tmux probes,
        // `spawn_daemon`), and a child can inherit a lock fd, so a "the lock is free"
        // probe is racy by construction.
        let lock = lease::daemon_lock_path(&reg_path, "pm-test");
        let held = (tier == Tier::Autopilot).then(|| {
            lease::try_acquire(&lock)
                .unwrap()
                .expect("daemon lock is free to pre-acquire")
        });

        let mut app = creating_loop_app(
            &reg_path,
            &proj,
            Engine::Claude,
            tier,
            "keep the build green",
            job_engine::DEFAULT_CADENCE_S,
        );
        app.submit_create();

        assert!(
            app.status.contains("created"),
            "{tier:?}: the create still reports itself: {}",
            app.status
        );
        if tier == Tier::Autopilot {
            assert!(
                app.status.contains("already running"),
                "an autopilot create must fold in `ensure_daemon`'s outcome: {}",
                app.status
            );
            // The daemon clause must come FIRST. `keybar_line` caps a transient status
            // at a third of the bar and truncates the TAIL, so at ordinary widths a
            // trailing daemon clause is the part that disappears — and it is the only
            // part the human cannot read off the row itself. Ordering is the fix.
            assert!(
                app.status.find("already running") < app.status.find("created"),
                "the daemon outcome must precede the create echo so truncation cannot \
                 eat it: {}",
                app.status
            );
        } else {
            assert!(
                !app.status.contains("daemon") && !app.status.contains("already running"),
                "a Standard create must NOT ensure or claim a daemon — nothing drives \
                 that row by design: {}",
                app.status
            );
            assert!(
                app.status.contains("you drive it"),
                "and it must say who does drive it: {}",
                app.status
            );
            // The observable half: the singleton lock file was never even created, so
            // `ensure_daemon` did not run — no probe, no spawn.
            assert!(
                !lock.exists(),
                "a Standard create must not even probe the daemon singleton lock ({})",
                lock.display()
            );
        }
        drop(held);
    }
}

#[test]
fn startup_ensures_the_daemon_when_an_enabled_session_is_already_on_autopilot() {
    // THE REAL FIX for "autopilot is on but pmd is dead". No keypress can do it (the
    // tier flip is a pure 2-value toggle, so `m` on an Autopilot row turns it OFF),
    // so pmtui ensures the daemon once at startup instead.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // seeds Standard
    let cfg_path = ProjectPaths::for_session(&root, "bot").config();
    let mut c = state::read_json::<Config>(&cfg_path).unwrap();
    c.autonomy = Tier::Autopilot;
    state::write_json_atomic(&cfg_path, &c).unwrap();

    // Pre-acquire the singleton lock (matching `loop_app`'s "pm-test" socket) and
    // hold it across the call, so the ensure takes `ensure_daemon`'s "already
    // running" no-spawn branch and this test never spawns a real pmd.
    let _held = lease::try_acquire(&lease::daemon_lock_path(&reg_path, "pm-test"))
        .unwrap()
        .expect("daemon lock is free to pre-acquire");

    let mut app = loop_app(&reg_path);
    app.status.clear();
    app.ensure_daemon_for_enabled_autopilot();

    assert!(
        app.status.contains("bot is on Autopilot") && app.status.contains("already running"),
        "startup folds the ensure outcome into the initial status: {}",
        app.status
    );
    assert_eq!(
        state::read_json::<Config>(&cfg_path).unwrap().autonomy,
        Tier::Autopilot,
        "and it only READS the dial — startup never flips a tier"
    );
}

#[test]
fn startup_does_not_ensure_the_daemon_when_nothing_is_on_autopilot() {
    // Silent no-op: a Standard session and a DISABLED Autopilot session must both
    // leave the daemon alone. Same lock-file observable as the off-direction test —
    // with no `ensure_daemon` call the singleton lock is never created.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // enabled, Standard

    // A DISABLED agent-loop session sitting on Autopilot.
    let paused_root = dir.path().join("paused");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects.push(ProjectEntry {
        id: "paused".into(),
        display_name: None,
        root: paused_root.clone(),
        enabled: false,
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
    reg.save(&reg_path).unwrap();
    let autopilot = config_at(Tier::Autopilot);
    state::write_json_atomic(
        &ProjectPaths::for_session(&paused_root, "paused").config(),
        &autopilot,
    )
    .unwrap();
    assert_eq!(
        state::read_json::<Config>(&ProjectPaths::for_session(&root, "bot").config())
            .unwrap()
            .autonomy,
        Tier::Standard,
        "the only enabled driven session is on Standard"
    );

    let mut app = loop_app(&reg_path);
    app.status.clear();
    app.ensure_daemon_for_enabled_autopilot();

    assert!(
        !lease::daemon_lock_path(&reg_path, "pm-test").exists(),
        "nothing on Autopilot => the daemon singleton lock is never even probed"
    );
    assert!(
        app.status.is_empty(),
        "and it stays silent rather than reporting a daemon: {}",
        app.status
    );
}

#[test]
fn restarting_a_standard_row_leaves_the_shared_daemon_alone() {
    // THE HIGH-severity fix: `r` on a Standard (human-driven) agent-loop row must NOT bounce
    // the shared pmd. `stop_daemon` SIGTERMs it and pmd reaps EVERY `pmloop-*` on death
    // (reap.rs), so cycling the daemon to restart ONE human-driven row would kill unrelated
    // Autopilot agents mid-turn. A Standard restart tears down only its own panes and
    // relaunches for the human, leaving the daemon (and everyone it drives) untouched.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot"); // seeds Standard
    let mut app = loop_app(&reg_path);
    // Fake the relaunch driver so `start_after_lifecycle_key` never spawns a real agent;
    // restart_agent's OWN teardown is idempotent on a session that was never launched.
    app.agent_tmux = Box::new(FakePane::default());
    app.status.clear();

    app.restart_agent("bot");

    // The daemon singleton lock is created only by `stop_daemon`/`ensure_daemon` (both
    // `try_acquire` it `create(true)`). Neither runs on a Standard row, so the lock file
    // never appears — the same deterministic observable the create-time gate test uses.
    assert!(
        !lease::daemon_lock_path(&reg_path, "pm-test").exists(),
        "a Standard restart must never probe or signal the shared daemon: {}",
        app.status
    );
    // And it SAYS so, rather than claiming a pmd cycle it did not perform.
    assert!(
        app.status.contains("pmd left running"),
        "a Standard restart leaves pmd running: {}",
        app.status
    );
    assert!(
        !app.status.contains("pmd stopped") && !app.status.contains("was not running"),
        "a Standard restart must not report a daemon cycle: {}",
        app.status
    );
}
