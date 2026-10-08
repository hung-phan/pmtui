//! Closing and pausing a row: `d`'s confirm-or-remove, the resume marker a removal
//! clears, the session state a close keeps behind, and what `p`, `m` and Enter do to a
//! paused row.

use super::*;

#[test]
fn confirming_removes_on_y_but_cancels_on_enter_or_other_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "sess");
    let confirming = |reg_path: &Path, root: &Path| {
        let mut app = app_with(
            vec![agent_loop_view("sess")],
            UiMode::Confirming {
                id: "sess".into(),
                session: session_name("sess", root),
                what: Confirmable::Remove,
            },
        );
        app.registry_path = reg_path.to_path_buf();
        app
    };

    // Enter must CANCEL — the overlay promises "any other key cancels", and
    // Enter=attach in Normal mode, so treating it as confirm is a footgun.
    let mut app = confirming(&reg_path, &root);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    assert!(app.status.contains("cancelled"), "{}", app.status);
    assert_eq!(
        Registry::load(&reg_path).unwrap().projects.len(),
        1,
        "Enter must not remove"
    );

    // Any other non-y key (e.g. 'n') also cancels.
    let mut app = confirming(&reg_path, &root);
    handle_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(Registry::load(&reg_path).unwrap().projects.len(), 1);

    // Only an explicit 'y' confirms the removal.
    let mut app = confirming(&reg_path, &root);
    handle_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    assert!(
        Registry::load(&reg_path).unwrap().projects.is_empty(),
        "y confirmed the removal"
    );
}

#[test]
fn begin_delete_reports_when_entry_is_gone_on_disk() {
    // The in-memory view can outlive the on-disk entry (another pmtui/pmd or a
    // hand-edit removed it). `d` should report, not silently no-op.
    let mut app = app_with(vec![agent_loop_view("ghost")], UiMode::Normal);
    app.registry_path = PathBuf::from("/nonexistent/registry.json");
    app.begin_delete();
    assert!(matches!(app.mode, UiMode::Normal));
    assert!(app.status.contains("gone from the list"), "{}", app.status);
}

// --- S5b: closing an agent-loop session (the only "done") -------------------

#[test]
fn pause_stops_the_driving_and_turns_autopilot_off() {
    // *"Pause can stop the current session or autopilot, then kill claude/codex so it is not
    // running."* and then, correcting the first cut: *"when i press pause, i want to stop
    // autopilot too, only when i press m again, then it start again."*
    //
    // BOTH switches, doing different jobs. `enabled:false` is the pause (pmd's sweep returns
    // there before anything else, and `Enter` lifts it); `autonomy: standard` is autopilot off,
    // so nothing restarts the driving until `m`. The pane kill is best-effort on the test socket
    // (a no-op here) and is asserted against real tmux instead.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path.clone();

    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);

    assert!(!enabled_on_disk(&reg_path, "bot"), "{}", app.status);
    // THE DIAL IS OFF, on disk, in the file both pmd and `m` read. This REVERSES the first
    // version of this test, which asserted the tier survived so that `Enter` could restore
    // autopilot: the user's model is that a pause is a stop you can walk away from, and a stop
    // that restarts itself when you press the key that brings the session back is not one.
    assert_eq!(tier_on_disk(&root, "bot"), Tier::Standard);
    // The status must name the undo, because the row's "paused" label does not say what key
    // lifts it — and it must say autopilot is off, which is the part that surprises otherwise.
    assert!(
        app.status.contains("paused")
            && app.status.contains("autopilot off")
            && app.status.contains("Enter"),
        "the status must name what stopped and how to come back: {}",
        app.status
    );
}

#[test]
fn a_ctrl_chord_in_normal_mode_does_not_fire_a_bare_letter_binding() {
    // In raw mode crossterm delivers Ctrl+P as Char('p')+CONTROL. Ctrl+P must NOT pause (which
    // kills the agent, no confirm), and a reflex Ctrl+C should quit — not open the cadence editor
    // (the plain `c` binding). Plain letters still fire.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path.clone();

    // Ctrl+P is swallowed: the row stays enabled (not paused) and the mode stays Normal.
    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert!(
        enabled_on_disk(&reg_path, "bot"),
        "Ctrl+P must not pause the row"
    );
    assert!(matches!(app.mode, UiMode::Normal));
    assert!(!app.should_quit);

    // Plain 'p' still pauses — the binding works without a modifier.
    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(!enabled_on_disk(&reg_path, "bot"), "plain p still pauses");

    // Ctrl+C quits rather than opening the cadence editor.
    let mut app2 = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app2.registry_path = reg_path.clone();
    handle_key(&mut app2, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(app2.should_quit, "Ctrl+C quits");
    assert!(
        matches!(app2.mode, UiMode::Normal),
        "Ctrl+C must not open the cadence editor"
    );
}

#[test]
fn m_on_a_paused_row_starts_it_again() {
    // *"only when i press m again, then it start again."* `m` has to be sufficient BY ITSELF,
    // which means it must lift the pause as well as turning the dial: pmd's sweep returns on a
    // disabled row before it ever reads the tier, so writing Autopilot alone would report
    // "→ Autopilot" over a session pmd still skips — the same shape of lie as the pre-m15 chip
    // that said "Autopilot off" and stopped nothing.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path.clone();
    // Hold the daemon singleton for this app's socket so the `ensure_daemon` the Autopilot arm
    // runs takes its "already running" branch instead of spawning a real pmd.
    let _held = hold_current_daemon(&reg_path, "pm-test");

    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(!enabled_on_disk(&reg_path, "bot"), "precondition: paused");
    assert_eq!(tier_on_disk(&root, "bot"), Tier::Standard, "precondition");

    // The view still carries the PRE-pause tier until a refresh; `cycle_tier` reads the dial
    // from disk, so this press flips Standard → Autopilot however the row is painted. It asks
    // for the goal on the way (the seeded brief means an empty save keeps it).
    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    assert!(
        matches!(
            app.mode,
            UiMode::EditingGoal {
                then_autopilot: true,
                ..
            }
        ),
        "{:?}",
        app.mode
    );
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        tier_on_disk(&root, "bot"),
        Tier::Autopilot,
        "{}",
        app.status
    );
    assert!(
        enabled_on_disk(&reg_path, "bot"),
        "`m` must lift the pause too, or pmd skips the row it just promised to drive: {}",
        app.status
    );
    // And it must SAY it did both, so the human does not go looking for a separate resume.
    assert!(
        app.status.contains("unpaused"),
        "one key did two things and the status must name both: {}",
        app.status
    );
    assert!(
        app.spawned_pmd.borrow().is_empty(),
        "this test must not spawn a daemon"
    );
}

#[test]
fn enter_on_a_paused_row_resumes_it_without_restarting_autopilot() {
    // *"When i press enter on pause session, it will resume for me."* — and, per the later
    // ruling, resume means the SESSION is back, not that the driving is back. Enter is otherwise
    // "attach"; on a paused row the way in is to bring it back first.
    for tier in [Tier::Standard, Tier::Autopilot] {
        let dir = tempfile::tempdir().unwrap();
        let (reg_path, root) = reg_with_tier(dir.path(), "bot", tier);
        let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
        app.registry_path = reg_path.clone();

        handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
        assert!(!enabled_on_disk(&reg_path, "bot"), "precondition: paused");

        handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

        assert!(
            enabled_on_disk(&reg_path, "bot"),
            "{tier:?}: Enter must lift the pause: {}",
            app.status
        );
        assert!(app.status.contains("resumed"), "{tier:?}: {}", app.status);
        // THE DIAL STAYS OFF, from BOTH starting tiers — a row paused from Autopilot comes back
        // as Standard, and pause left an already-Standard row alone. This is the assertion that
        // encodes *"only when i press m again, then it start again"*.
        assert_eq!(
            tier_on_disk(&root, "bot"),
            Tier::Standard,
            "{tier:?}: resume must not restart autopilot"
        );
        // …so the status has to name the key that does, or a resumed session sitting still is
        // indistinguishable from a broken one.
        assert!(
            // The whole phrase, not a bare 'm' — "resumed" contains one.
            app.status.contains("m starts autopilot"),
            "{tier:?}: resume must name what starts the driving: {}",
            app.status
        );
        // Resume must NOT attach: there is nothing to attach to (pause killed the pane, and
        // nothing relaunches it while the dial is off). A pending attach would drop the human
        // into a dead pane.
        assert!(app.pending_attach_loop.is_none() && app.pending_chat.is_none());
        // NO DAEMON, from either starting tier: the resumed row is Standard, and starting one
        // "for" it would drive every OTHER enabled project while doing nothing here.
        assert!(
            app.spawned_pmd.borrow().is_empty(),
            "{tier:?}: this test must not spawn a daemon"
        );
    }
}

#[test]
fn board_pause_control_toggles_between_pause_and_resume() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Board);
    app.registry_path = reg_path.clone();

    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(!enabled_on_disk(&reg_path, "bot"), "{}", app.status);
    assert!(matches!(app.mode, UiMode::Board));

    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(enabled_on_disk(&reg_path, "bot"), "{}", app.status);
    assert!(app.status.contains("resumed"), "{}", app.status);
    assert!(matches!(app.mode, UiMode::Board));
}

#[test]
fn resume_on_autopilot_reenables_the_row_and_ensures_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut registry = Registry::load(&reg_path).unwrap();
    registry.projects[0].enabled = false;
    registry.save(&reg_path).unwrap();
    let mut app = loop_app(&reg_path);
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.resume_session("bot", &root);

    assert!(Registry::load(&reg_path).unwrap().projects[0].enabled);
    assert!(
        app.status.contains("resumed on Autopilot")
            && app.status.contains("daemon already running"),
        "{}",
        app.status
    );
    drop(held);
}

#[test]
fn resume_reports_a_registry_save_failure() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut registry = Registry::load(&reg_path).unwrap();
    registry.projects[0].enabled = false;
    registry.save(&reg_path).unwrap();
    let original = std::fs::metadata(dir.path()).unwrap().permissions();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut app = loop_app(&reg_path);

    app.resume_session("bot", &root);
    std::fs::set_permissions(dir.path(), original).unwrap();

    assert!(app.status.contains("could not resume"), "{}", app.status);
    assert!(!Registry::load(&reg_path).unwrap().projects[0].enabled);
}

#[test]
fn pause_refuses_what_it_cannot_pause() {
    // Empty list: the recurring bug in this UI is a key that looks broken.
    let mut app = app_with(vec![], UiMode::Normal);
    app.status = String::new();
    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(app.status.contains("nothing is selected"), "{}", app.status);

    // ALREADY PAUSED: say which key lifts it rather than re-killing a dead pane.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;
    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(
        app.status.contains("already paused") && app.status.contains("Enter"),
        "{}",
        app.status
    );
}

#[test]
fn lifecycle_helpers_report_corrupt_or_missing_registry_rows() {
    let dir = tempfile::tempdir().unwrap();
    let corrupt = dir.path().join("registry.json");
    std::fs::write(&corrupt, "{not-json").unwrap();
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = corrupt.clone();

    let err = app
        .set_row_enabled("bot", false)
        .expect_err("a corrupt registry must not be overwritten");
    assert!(err.contains("parse"), "{err}");
    assert_eq!(std::fs::read_to_string(&corrupt).unwrap(), "{not-json");

    let valid = dir.path().join("valid.json");
    Registry::default().save(&valid).unwrap();
    app.registry_path = valid;
    assert_eq!(
        app.set_row_enabled("ghost", true),
        Err("ghost is gone from the list".into())
    );

    app.resume_session("ghost", dir.path());
    assert!(app.status.contains("ghost is gone from the list"));
}

#[test]
fn pause_reports_when_autopilot_state_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let config = ProjectPaths::for_session(&root, "bot").config();
    std::fs::write(&config, "{not-json").unwrap();
    let pane = FakePane::default();
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();

    app.pause_session();

    assert!(
        !enabled_on_disk(&reg_path, "bot"),
        "the row is still paused"
    );
    assert_eq!(pane.terminated().len(), 1, "the agent is still stopped");
    assert!(
        app.status.contains("autopilot could not be turned off"),
        "{}",
        app.status
    );
}

#[test]
fn pause_reports_when_the_agent_pane_cannot_be_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let pane = FakePane::default().with(|inner| inner.fail_terminate = true);
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();

    app.pause_session();

    assert!(
        !enabled_on_disk(&reg_path, "bot"),
        "the row is still paused"
    );
    assert_eq!(pane.terminated().len(), 1);
    assert!(
        app.status.contains("pane may still be up"),
        "{}",
        app.status
    );
}

#[test]
fn codex_restart_refuses_unreadable_liveness_and_dead_pane_probes() {
    for fail_dead_probe in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
        let mut registry = Registry::load(&reg_path).unwrap();
        registry.projects[0].engine = Some(Engine::Codex);
        registry.save(&reg_path).unwrap();
        let session = session_name("bot", &root);
        let pane = FakePane::live(&session, "").with(|inner| {
            inner.fail_alive = !fail_dead_probe;
            inner.fail_pane_dead = fail_dead_probe;
        });
        let mut app = app_with_driver(
            vec![agent_loop_view("bot")],
            UiMode::Normal,
            Box::new(pane.clone()),
        );
        app.registry_path = reg_path;

        app.restart_agent("bot");

        assert!(pane.terminated().is_empty());
        assert!(
            app.status.contains("could not determine") && app.status.contains("left unchanged"),
            "{}",
            app.status
        );
    }
}

#[test]
fn standard_restart_reports_a_terminate_failure_without_claiming_success() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session = session_name("bot", &root);
    let pane = FakePane::live(&session, "").with(|inner| inner.fail_terminate = true);
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path;

    app.restart_agent("bot");

    assert_eq!(pane.terminated(), [session]);
    assert!(
        app.status.contains("could not stop the agent")
            && app.status.contains("kill-session failed"),
        "{}",
        app.status
    );
}

#[test]
fn removing_an_already_missing_row_is_honest_and_safe() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    Registry::default().save(&reg_path).unwrap();
    let pane = FakePane::default();
    let mut app = app_with_driver(
        vec![agent_loop_view("ghost")],
        UiMode::Confirming {
            id: "ghost".into(),
            session: "pm-ghost".into(),
            what: Confirmable::Remove,
        },
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path;

    app.remove_project("ghost", "pm-ghost");

    assert_eq!(pane.terminated(), vec!["pm-ghost"]);
    assert!(matches!(app.mode, UiMode::Normal));
    assert!(app.status.contains("ghost is gone from the list"));
}

#[test]
fn begin_delete_on_agent_loop_enters_confirming() {
    // Closing an agent-loop session is the ONLY "done", so `d` must route
    // through the confirm gate — not the native "autonomous" refusal.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path.clone();

    app.begin_delete();

    assert!(
        matches!(app.mode, UiMode::Confirming { .. }),
        "an agent-loop close asks for confirmation ({})",
        app.status
    );
    assert_eq!(
        Registry::load(&reg_path).unwrap().projects.len(),
        1,
        "nothing removed before confirming"
    );
}

#[test]
fn confirming_close_of_agent_loop_removes_row_but_keeps_session_state() {
    // Confirming the close drops the registry row (the only "done") but MUST
    // retain the per-session `sessions/<id>/` state (ledger/config/brief) per
    // the design — the human closes the session, not the on-disk history.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let session_paths = ProjectPaths::for_session(&root, "bot");
    assert!(
        session_paths.pmstate().exists(),
        "precondition: ledger seeded"
    );
    assert!(
        session_paths.config().exists(),
        "precondition: config seeded"
    );
    assert!(session_paths.brief().exists(), "precondition: brief seeded");

    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path.clone();
    app.begin_delete();
    assert!(matches!(app.mode, UiMode::Confirming { .. }));
    // 'y' confirms; the carried session is the surviving `pmchat-…` (SURVIVE),
    // terminated best-effort on the test socket (a harmless no-op here).
    handle_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);

    assert!(matches!(app.mode, UiMode::Normal));
    assert!(
        Registry::load(&reg_path).unwrap().projects.is_empty(),
        "the registry row is dropped on confirm"
    );
    assert!(
        session_paths.pmstate().exists(),
        "per-session ledger is retained on close"
    );
    assert!(
        session_paths.config().exists(),
        "per-session config is retained on close"
    );
    assert!(
        session_paths.brief().exists(),
        "per-session brief is retained on close"
    );
}

#[test]
fn begin_delete_on_agent_loop_names_chat_session() {
    // SURVIVE: the chat REPL outlives a detach, so `d`'s confirm must carry the
    // deterministic pmchat- name so confirm→remove_project terminates the surviving
    // session (the OLD model left it empty because the chat was killed on return).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;

    app.begin_delete();

    let UiMode::Confirming { id, session, .. } = &app.mode else {
        panic!("expected Confirming, got status {}", app.status);
    };
    assert_eq!(id, "bot");
    assert_eq!(*session, session_name("bot", &root));
}

// --- I1 (S6 r1): the one-tree guard rejects legacy interactive too -----------

/// Hold the daemon singleton flock for the duration of a test, so `ensure_daemon` sees a live daemon
/// and never spawns a real `pmd` — and `daemon_live()` reports `Up`.
fn hold_daemon_lock(reg_path: &Path, socket: &str) -> lease::ProjectLease {
    lease::try_acquire(&lease::daemon_lock_path(reg_path, socket))
        .unwrap()
        .expect("the daemon lock must be free in a scratch registry")
}

#[test]
fn r_on_a_standard_row_starts_the_agent_again_because_pmd_never_will() {
    // User: *"when i press r to restart, or enter after press p to pause. The main panel doesn't render
    // the session immediate … it should start the session and render it immediately"*.
    //
    // `r` ended by cycling the DAEMON and returning, which is the whole fix on an autopilot row — pmd
    // relaunches the agent within a sweep. On a STANDARD row `pmd_drives_row` is false BY DESIGN
    // (m15), so nothing was ever coming: the key killed the agent and left an empty pane.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    // A conversation the human has been working in — the thing a restart must RESUME, not replace.
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.conversation_id = Some("conv-keep".into());
    job::save(&sp, &l).unwrap();

    let pane = FakePane::default(); // nothing alive: `r` has just killed it
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.restart_agent("bot");

    let launches = pane.launches();
    assert_eq!(
        launches.len(),
        1,
        "`r` must bring the session back up itself: {launches:?}"
    );
    assert_eq!(
        launches[0].0,
        session_name("bot", &root),
        "…in the session name Enter re-attaches"
    );
    // RESUMED, not re-created: `--session-id` would start a NEW conversation and silently abandon the
    // one the human has been working in.
    assert!(
        launches[0].2.contains(&"--resume".to_string())
            && launches[0].2.contains(&"conv-keep".to_string()),
        "the restart must resume the existing conversation: {:?}",
        launches[0].2
    );
    let settings_at = launches[0]
        .2
        .iter()
        .position(|arg| arg == "--settings")
        .expect("a Standard pane must keep the turn hook needed after an Autopilot flip");
    assert!(
        launches[0].2[settings_at + 1].contains(sp.turn_signal().to_str().unwrap()),
        "turn hook must target this session: {:?}",
        launches[0].2
    );
    assert!(
        app.status.contains("started it"),
        "and say that it started: {}",
        app.status
    );
    drop(held);
}

#[test]
fn restarting_a_standard_session_never_replays_its_initial_message() {
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_agent_loop(dir.path(), "bot");
    Registry::update(&registry, |reg| {
        reg.projects[0].initial_prompt = Some("do not replay me".into());
    })
    .unwrap();
    let pane = FakePane::default();
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = registry;
    app.initial_message_retries
        .insert("bot".into(), "do not replay me".into());

    let result = app.start_after_lifecycle_key("bot", &root, Engine::Claude, Mode::AgentLoop, None);

    assert!(result.is_some());
    let launches = pane.launches();
    assert_eq!(launches.len(), 1);
    assert!(!launches[0].2.iter().any(|arg| arg == "do not replay me"));
    assert!(!app.initial_message_retries.contains_key("bot"));
}

#[test]
fn r_on_standard_codex_uses_the_recorded_id_when_the_live_probe_matches() {
    for model in [None, Some("openai.gpt-5.5")] {
        let dir = tempfile::tempdir().unwrap();
        let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
        let mut reg = Registry::load(&reg_path).unwrap();
        reg.projects[0].engine = Some(Engine::Codex);
        reg.projects[0].worker_model = model.map(str::to_string);
        reg.save(&reg_path).unwrap();
        let paths = ProjectPaths::for_session(&root, "bot");
        let mut ledger = AgentLoopState::fresh(Engine::Codex, Some(300), 1000);
        ledger.conversation_id = Some("11111111-2222-3333-4444-555555555555".into());
        job::save(&paths, &ledger).unwrap();

        let session = session_name("bot", &root);
        let pane = FakePane::with_codex_session(&session, "11111111-2222-3333-4444-555555555555");
        let mut app = app_with_driver(
            vec![agent_loop_view("bot")],
            UiMode::Normal,
            Box::new(pane.clone()),
        );
        app.registry_path = reg_path.clone();
        let held = hold_daemon_lock(&reg_path, &app.socket);

        app.restart_agent("bot");

        let expected = build_chat(
            Engine::Codex,
            "11111111-2222-3333-4444-555555555555",
            model,
            &paths.turn_signal(),
        );
        assert_eq!(pane.launches()[0].2, expected, "{model:?}");
        assert_eq!(pane.terminated(), vec![session]);
        drop(held);
    }
}

#[test]
fn r_on_standard_codex_captures_and_persists_the_live_exact_id_before_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].engine = Some(Engine::Codex);
    reg.projects[0].conversation_id = Some("ffffffff-eeee-4ddd-8ccc-bbbbbbbbbbbb".into());
    reg.save(&reg_path).unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    job::save(
        &paths,
        &AgentLoopState::fresh(Engine::Codex, Some(300), 1000),
    )
    .unwrap();

    let session = session_name("bot", &root);
    let captured = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let pane = FakePane::with_codex_session(&session, captured);
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.restart_agent("bot");

    assert_eq!(
        pane.launches()[0].2,
        build_chat(Engine::Codex, captured, None, &paths.turn_signal())
    );
    assert_eq!(pane.terminated(), vec![session]);
    let saved = Registry::load(&reg_path).unwrap();
    assert_eq!(
        saved.projects[0].conversation_id.as_deref(),
        Some(captured),
        "pmtui must replace a stale registry seed before killing its only live source"
    );
    drop(held);
}

#[test]
fn r_on_standard_codex_refuses_to_kill_when_the_live_id_cannot_be_proven() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].engine = Some(Engine::Codex);
    reg.save(&reg_path).unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    job::save(
        &paths,
        &AgentLoopState::fresh(Engine::Codex, Some(300), 1000),
    )
    .unwrap();

    let session = session_name("bot", &root);
    let pane = FakePane::codex_probe_fails(&session);
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.restart_agent("bot");

    assert!(
        pane.terminated().is_empty(),
        "an unresumable Codex pane must be left alive"
    );
    assert!(pane.launches().is_empty());
    assert!(
        app.status.contains("could not identify") && app.status.contains("left unchanged"),
        "{}",
        app.status
    );
    drop(held);
}

#[test]
fn autopilot_codex_restart_refuses_when_live_id_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].engine = Some(Engine::Codex);
    reg.save(&reg_path).unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut config: Config = state::read_json(&paths.config()).unwrap();
    config.autonomy = Tier::Autopilot;
    state::write_json_atomic(&paths.config(), &config).unwrap();

    let session = session_name("bot", &root);
    let pane = FakePane::live(&session, "");
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path;

    app.restart_agent("bot");

    assert!(pane.terminated().is_empty());
    assert!(pane.launches().is_empty());
    assert!(
        app.status
            .contains("could not identify the live Codex conversation")
            && app.status.contains("left unchanged"),
        "{}",
        app.status
    );
}

#[test]
fn r_on_standard_codex_refuses_a_live_id_that_disagrees_with_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].engine = Some(Engine::Codex);
    reg.save(&reg_path).unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut ledger = AgentLoopState::fresh(Engine::Codex, Some(300), 1000);
    ledger.conversation_id = Some("11111111-2222-4333-8444-555555555555".into());
    job::save(&paths, &ledger).unwrap();

    let session = session_name("bot", &root);
    let pane = FakePane::with_codex_session(&session, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.restart_agent("bot");

    assert!(pane.terminated().is_empty());
    assert!(pane.launches().is_empty());
    assert!(
        app.status.contains("does not match ledger id") && app.status.contains("left unchanged"),
        "{}",
        app.status
    );
    drop(held);
}

#[test]
fn r_on_a_dead_standard_codex_row_without_an_id_starts_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].engine = Some(Engine::Codex);
    reg.save(&reg_path).unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    job::save(
        &paths,
        &AgentLoopState::fresh(Engine::Codex, Some(300), 1000),
    )
    .unwrap();

    let pane = FakePane::default();
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.restart_agent("bot");

    assert_eq!(
        pane.launches()[0].2,
        build_codex_fresh_chat(None, &paths.turn_signal()),
        "no live conversation exists to preserve"
    );
    assert!(app.status.contains("restarted"), "{}", app.status);
    drop(held);
}

#[test]
fn r_on_a_codex_remain_on_exit_corpse_starts_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].engine = Some(Engine::Codex);
    reg.save(&reg_path).unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    job::save(
        &paths,
        &AgentLoopState::fresh(Engine::Codex, Some(300), 1000),
    )
    .unwrap();

    let session = session_name("bot", &root);
    let pane = FakePane::live(&session, "").with(|inner| inner.pane_dead = true);
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.restart_agent("bot");

    assert_eq!(
        pane.launches()[0].2,
        build_codex_fresh_chat(None, &paths.turn_signal())
    );
    assert_eq!(pane.terminated(), vec![session]);
    drop(held);
}

#[test]
fn r_on_an_autopilot_row_leaves_the_launch_to_pmd() {
    // The complement, and the reason the helper checks the dial rather than always launching: on a
    // driven row pmd relaunches the agent itself (~0.5s, measured with real tmux), and a second
    // launcher racing it on ONE conversation is the fork this codebase spends its locks preventing.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot);
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.conversation_id = Some("conv-keep".into());
    job::save(&sp, &l).unwrap();

    let pane = FakePane::default();
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();

    app.restart_agent("bot");

    assert!(
        pane.launches().is_empty(),
        "pmtui must not race pmd for the launch on a driven row: {:?}",
        pane.launches()
    );
    assert!(
        !app.status.contains("started it"),
        "and must not claim a start it did not perform: {}",
        app.status
    );
}

#[test]
fn r_on_autopilot_refuses_to_touch_the_agent_when_pmd_does_not_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let paths = ProjectPaths::for_session(&root, "bot");
    set_tier(&paths, Tier::Autopilot);
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    ledger.conversation_id = Some("conv-keep".into());
    job::save(&paths, &ledger).unwrap();

    let session = session_name("bot", &root);
    let pane = FakePane::live(&session, "");
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.restart_agent("bot");

    assert!(pane.terminated().is_empty());
    assert!(pane.launches().is_empty());
    assert!(
        app.status.contains("could not stop pmd") && app.status.contains("left unchanged"),
        "{}",
        app.status
    );
    drop(held);
}

#[test]
fn enter_after_pause_starts_the_session_rather_than_only_re_enabling_the_row() {
    // `pause_session` KILLS the agent, so re-enabling the row on its own left a session with nothing
    // running in it — and on Standard nothing ever would. User: *"enter after press p to pause … it
    // should start the session and render it immediately"*.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.conversation_id = Some("conv-keep".into());
    job::save(&sp, &l).unwrap();
    // PAUSED, the state `p` leaves behind.
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].enabled = false;
    reg.save(&reg_path).unwrap();

    let pane = FakePane::default();
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    app.refresh();
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.request_attach(); // Enter on a paused row resumes it

    let launches = pane.launches();
    assert_eq!(
        launches.len(),
        1,
        "resuming must actually start the session: {launches:?}"
    );
    assert!(
        launches[0].2.contains(&"conv-keep".to_string()),
        "on the conversation it was paused in: {:?}",
        launches[0].2
    );
    assert!(
        app.status.contains("resumed") && app.status.contains("started it"),
        "and the status must say both happened: {}",
        app.status
    );
    drop(held);
}

#[test]
fn enter_on_a_paused_row_with_a_ghost_conversation_id_creates_it_instead_of_resuming_a_dead_session()
 {
    // The dream-poster bug: a Standard row was paused with a recorded `conversation_id` whose
    // claude transcript is GONE — a ghost id pmd seeded that never became a real conversation
    // on disk. `--resume <ghost>` opens claude into a dead/empty session, so pressing Enter to
    // resume it "did nothing". The daemon's own launch probes claude's `projects/` and CREATES
    // a confidently-absent id instead; the manual pmtui resume must do the same, re-establishing
    // the recorded id as a real conversation with `--session-id` rather than `--resume`.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.conversation_id = Some("ghost-conv".into());
    job::save(&sp, &l).unwrap();
    // PAUSED, the state `p` leaves behind.
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].enabled = false;
    reg.save(&reg_path).unwrap();

    // A claude home whose `projects/` dir EXISTS but holds no transcript for `ghost-conv`
    // under this root's slug ⇒ the probe answers `Some(false)` (confidently absent) ⇒ CREATE.
    let claude_home = dir.path().join("claude-home");
    std::fs::create_dir_all(claude_home.join("projects")).unwrap();

    let pane = FakePane::default();
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    app.claude_home = Some(claude_home);
    app.refresh();
    let held = hold_daemon_lock(&reg_path, &app.socket);

    app.request_attach(); // Enter on a paused row resumes it

    let launches = pane.launches();
    assert_eq!(
        launches.len(),
        1,
        "resuming a paused row must still start the session: {launches:?}"
    );
    // CREATED on the recorded id (re-establishing it), NOT `--resume`d into a dead session.
    assert_eq!(
        launches[0].2,
        build_chat_create(
            Engine::Claude,
            "ghost-conv",
            None,
            &ProjectPaths::for_session(&root, "bot").turn_signal(),
        ),
        "a ghost conversation id must be CREATED with --session-id, not --resume'd: {:?}",
        launches[0].2
    );
    assert!(
        !launches[0].2.contains(&"--resume".to_string()),
        "must not --resume a transcript that does not exist: {:?}",
        launches[0].2
    );
    drop(held);
}
