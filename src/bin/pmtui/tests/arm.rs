//! Arming an Enter that cannot open yet: the budget a bounded arm spends, the presses and
//! refreshes that disarm it, and the attach-or-chat route it resolves to once the session
//! becomes reachable.

use super::*;

#[test]
fn first_enter_route_decides_on_the_tier_not_the_lease() {
    // The fix in one assertion: the never-woken Enter route is a function of the
    // AUTONOMY DIAL. Autopilot must never reach the `driver.lock` probe (whose
    // free-lease arm is `CreateAndChat` — the second writer that deadlocks pmd),
    // and Standard must still get exactly that lease-keyed routing.
    assert_eq!(
        first_enter_route(Tier::Autopilot),
        FirstEnterRoute::EnsureDaemonThenArm
    );
    assert_eq!(
        first_enter_route(Tier::Standard),
        FirstEnterRoute::LeaseKeyed
    );
}

#[test]
fn autopilot_first_enter_arms_only_when_a_daemon_is_up() {
    // The ensure is a PRECONDITION of the arm: with a daemon up (either way it got
    // there) we arm and promise "you'll drop in"; with no daemon we REFUSE and name
    // `m` — arming there would park the human behind an encouraging status that
    // nothing can ever make true, which is the complaint this slice fixes.
    for up in [DaemonEnsure::Started, DaemonEnsure::AlreadyRunning] {
        match autopilot_first_enter("bot", &up) {
            AutopilotFirstEnter::Arm(s) => {
                assert!(s.contains("autopilot"), "{s}");
                assert!(
                    s.contains("you'll drop in"),
                    "the arm promises a drop-in, not a guaranteed landing: {s}"
                );
            }
            other => panic!("{up:?} must arm, got {other:?}"),
        }
    }
    let down = DaemonEnsure::Failed("pmd binary not found at /nope/pmd".into());
    match autopilot_first_enter("bot", &down) {
        AutopilotFirstEnter::Refuse(s) => {
            assert!(
                s.contains("autopilot needs pmd"),
                "the refusal front-loads the blocker (the keybar truncates the TAIL): {s}"
            );
            assert!(
                s.contains("press m for Standard"),
                "the refusal names the escape hatch: {s}"
            );
            assert!(s.contains("/nope/pmd"), "and stays honest about WHY: {s}");
        }
        other => panic!("a missing pmd must refuse, got {other:?}"),
    }
}

#[test]
fn armed_route_is_attach_or_stay_on_autopilot_and_attach_or_chat_on_standard() {
    // The invariant, as a matrix. A live `pmloop-` always attaches (either tier).
    // With NO live loop, Autopilot STAYS ARMED — the old `pending_chat` fallback here
    // is what re-created the second writer ~500ms after the Enter had carefully
    // avoided it — while Standard keeps chatting (its pmd-down fallback).
    // `Blocked`/`Stay` are tier-independent.
    let open = || ArmedOpen::Open("conv-1".into());
    for tier in [Tier::Autopilot, Tier::Standard] {
        assert_eq!(
            armed_route(tier, open(), true),
            ArmedRoute::AttachLoop,
            "{tier:?}: a live pmloop- is attached, never chatted"
        );
        assert_eq!(
            armed_route(tier, ArmedOpen::Stay, true),
            ArmedRoute::AttachLoop,
            "{tier:?}: a live agent session outranks the ledger transition, exactly as \
             request_attach's loop_alive pre-empt does"
        );
        assert_eq!(
            armed_route(tier, ArmedOpen::Blocked, true),
            ArmedRoute::Blocked,
            "{tier:?}: but a parked-blocked session is never auto-opened — `a` answers it"
        );
        assert_eq!(armed_route(tier, ArmedOpen::Stay, false), ArmedRoute::Stay);
    }
    // Autopilot pays one `has-session` per armed tick so a live agent can win over a
    // `Running` ledger; Standard's probe condition is unchanged (Open only).
    assert!(armed_probes_loop(Tier::Autopilot, &ArmedOpen::Stay));
    assert!(armed_probes_loop(Tier::Autopilot, &ArmedOpen::Blocked));
    assert!(!armed_probes_loop(Tier::Standard, &ArmedOpen::Stay));
    assert!(!armed_probes_loop(Tier::Standard, &ArmedOpen::Blocked));
    assert!(armed_probes_loop(Tier::Standard, &open()));
    assert_eq!(
        armed_route(Tier::Autopilot, open(), false),
        ArmedRoute::Stay,
        "autopilot is attach-or-stay-armed: it must NOT launch a pmchat- on the id pmd just minted"
    );
    assert_eq!(
        armed_route(Tier::Standard, open(), false),
        ArmedRoute::Chat("conv-1".into()),
        "standard keeps its pmd-down chat fallback verbatim"
    );
}

#[test]
fn request_attach_on_autopilot_arms_and_never_creates_a_chat() {
    // THE BUG. A freshly created Autopilot row (no id, no session, FREE per-session
    // `driver.lock`) used to win that lease and CREATE-and-chat a second claude, whose
    // live `pmchat-` then made `JobScheduler::drive`'s gate 1 defer forever — so the
    // agent the human asked for was never launched. Enter must instead ARM and leave
    // the whole conversation to pmd.
    //
    // The daemon singleton flock is held by the TEST, so `ensure_daemon` reports
    // AlreadyRunning without spawning anything.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    let _daemon = lease::try_acquire(&lease::daemon_lock_path(&reg_path, &app.socket))
        .unwrap()
        .expect("the test stands in for a running pmd");

    app.request_attach();

    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "autopilot arms the auto-attach: {}",
        app.status
    );
    assert_eq!(
        app.armed_drains_left,
        Some(AUTOPILOT_ARM_DRAINS),
        "the claude arm is bounded, so a pmd that dies during boot is reported"
    );
    assert!(
        app.pending_create_chat.is_none() && app.pending_chat.is_none(),
        "no fresh pmchat- launch site is reachable from the autopilot Enter"
    );
    // pmtui NEVER writes state.json, and on this path it must not write the REGISTRY
    // either — no mint, no seed. pmd's `ensure_session` owns the conversation id.
    assert!(
        Registry::load(&reg_path)
            .unwrap()
            .projects
            .iter()
            .find(|p| p.id == "bot")
            .unwrap()
            .conversation_id
            .is_none(),
        "the autopilot arm must not mint or seed a conversation id"
    );
    assert!(
        !sp.chat_lock().exists(),
        "no chat marker — nothing to deadlock the daemon's defer gate with"
    );
    // The strongest proof the lease probe was BYPASSED rather than won-and-released:
    // `lease::try_acquire` creates its lock file, so an untouched path means we never
    // probed. pmd can still take it.
    assert!(
        !sp.daemon_dir().join("driver.lock").exists(),
        "the autopilot branch must not consult the per-session driver.lock at all"
    );
    assert!(app.status.contains("autopilot"), "{}", app.status);
}

#[test]
fn request_attach_on_autopilot_refuses_honestly_with_no_pmd() {
    // Same fixture, but nothing is holding the daemon flock and the test binary has no
    // `pmd` sibling (it lives in `target/debug/deps/`), so the ensure genuinely fails.
    // Do NOT arm behind a hopeful status: refuse, and name the key that lets the human
    // chat it themselves. A second Enter must repeat the refusal, not silently no-op.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot);
    let mut app = loop_app(&reg_path);

    for attempt in 1..=2 {
        // The previous attempt's ensure probed the daemon flock; a sibling test's fork can hold
        // that fd until it execs, which the single-shot probe would read as a running pmd.
        wait_until_free(&lease::daemon_lock_path(&reg_path, &app.socket));
        app.status.clear();
        app.request_attach();
        assert!(
            app.pending_first_chat.is_none(),
            "attempt {attempt}: an arm with no daemon would never resolve"
        );
        assert!(
            app.pending_create_chat.is_none() && app.pending_chat.is_none(),
            "attempt {attempt}: a failed ensure still must not create a second writer"
        );
        assert!(
            app.status.contains("autopilot needs pmd")
                && app.status.contains("press m for Standard"),
            "attempt {attempt}: {}",
            app.status
        );
    }
}

#[test]
fn armed_drain_on_autopilot_waits_for_the_agent_instead_of_chatting() {
    // C1, second half: pmd has pinned the id and parked `Monitoring`, but its
    // `pmloop-` is not alive for us (no tmux server on this socket). The old drain
    // fresh-launched a `pmchat-` on that brand-new id — re-creating the very state the
    // Enter avoided. Autopilot must STAY ARMED and keep waiting.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot);
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-pmd-minted".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    app.drain_armed_first_chat();

    assert!(
        app.pending_chat.is_none(),
        "autopilot never opens a pmchat- on the conversation pmd owns"
    );
    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "it stays armed until the agent's own session comes up"
    );
    assert!(
        app.pending_attach_loop.is_none(),
        "nothing live to attach yet"
    );
}

#[test]
fn armed_drain_on_autopilot_attaches_a_live_agent_despite_a_running_ledger() {
    // Wiring check for `armed_probes_loop`: on Autopilot the LIVE agent session outranks
    // the ledger transition (same rule as `request_attach`'s `loop_alive` pre-empt).
    // Without the always-probe an armed row whose ledger read `Running` would sit `Stay`
    // in front of a perfectly live agent and then time out on its bound.
    if !tmux_available() {
        eprintln!("skipping autopilot live-attach drain test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    std::fs::create_dir_all(&root).unwrap();
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot);
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-live".into());
    l.run = job::JobRun::Running {
        seq: 0,
        session: "pmj-bot-0".into(),
        deadline: 9999,
    };
    job::save(&sp, &l).unwrap();

    let socket = TmuxSocket::new("pmtui-aprun");
    let driver = TmuxDriver::with_socket(socket.name());
    let loop_session = session_name("bot", &root);
    driver
        .launch_interactive(
            &loop_session,
            &root,
            &["sh".to_string()],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();
    app.pending_first_chat = Some("bot".into());
    app.armed_drains_left = Some(1); // would disarm on this very tick if it stayed
    app.drain_armed_first_chat();
    let attached = app.pending_attach_loop.clone();
    let chatted = app.pending_chat.is_some();
    let armed = app.pending_first_chat.clone();

    let _ = driver.terminate(&loop_session);
    let _ = std::process::Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .output();

    assert_eq!(
        attached.as_deref(),
        Some(loop_session.as_str()),
        "a live agent session is attached even while the ledger says Running"
    );
    assert!(!chatted, "and never chatted");
    assert!(armed.is_none(), "resolved ⇒ disarmed");
}

#[test]
fn a_bounded_arm_spends_its_budget_then_disarms_honestly() {
    // A naive arm would sit `Stay` forever behind an encouraging status if the pmd we
    // ensured died during boot. Spend the budget one drain at a time, then disarm and
    // say so — NOT a re-ensure loop.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot); // Idle ledger, no conversation id ⇒ Stay

    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    app.armed_drains_left = Some(2);

    app.drain_armed_first_chat();
    assert_eq!(app.armed_drains_left, Some(1), "one drain spent");
    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "still armed"
    );

    app.drain_armed_first_chat();
    assert!(app.pending_first_chat.is_none(), "budget spent ⇒ disarmed");
    assert!(
        app.armed_drains_left.is_none(),
        "the bound is cleared with the arm, so it cannot bound the NEXT one"
    );
    assert!(
        app.status.contains("could not start the agent") && app.status.contains("Enter"),
        "the honest status says what happened and what to press: {}",
        app.status
    );
}

#[test]
fn an_unbounded_arm_never_times_out() {
    // Every arm that predates this slice (Standard's lease-keyed `Arm`, and codex —
    // whose id is captured only after a wake COMPLETES) is UNBOUNDED, so no timeout
    // can regress them. Drain repeatedly on a `Stay` ledger: still armed.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot"); // Standard, no cid
    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    assert!(app.armed_drains_left.is_none(), "unbounded by default");
    for _ in 0..(AUTOPILOT_ARM_DRAINS + 5) {
        app.drain_armed_first_chat();
    }
    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "an unbounded arm waits as long as it takes"
    );
}

// ── the `pmd up` / `pmd DOWN` indicator (and the disarm that reads it) ──────────

#[test]
fn arming_an_autopilot_enter_restarts_the_liveness_window() {
    // Wiring: the arm must zero the streak, or a pmtui that had been sitting in front
    // of a dead daemon would disarm on the very next drain — in front of the pmd it
    // just ensured.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    // A daemon must be up for the Enter to ARM at all (`autopilot_first_enter`), and
    // holding the lock here also keeps `ensure_daemon` off its spawn branch.
    let _held = lease::try_acquire(&lease::daemon_lock_path(&reg_path, &app.socket))
        .unwrap()
        .expect("free");
    app.daemon_down_streak.set(DAEMON_DOWN_DISARM_SAMPLES + 5);

    app.request_attach(); // fresh Autopilot row, no cid ⇒ ensure + arm

    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "the Enter should have armed: {}",
        app.status
    );
    assert_eq!(
        app.daemon_down_streak.get(),
        0,
        "arming restarts the DOWN observation window"
    );
    assert!(
        app.daemon_live.get().is_none(),
        "and invalidates the pre-ensure sample"
    );
}

#[test]
fn armed_first_chat_opens_when_the_session_becomes_chattable() {
    // An armed session that gains a conversation id and parks Monitoring is
    // cleanly chattable ⇒ the idle-tick drain drops the human into the resume
    // chat and disarms.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-live".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    app.drain_armed_first_chat();

    assert!(app.pending_first_chat.is_none(), "disarmed once opened");
    let req = app
        .pending_chat
        .as_ref()
        .expect("armed auto-open queued the resume chat");
    assert_eq!(
        req.argv,
        build_chat(Engine::Claude, "conv-live", None, &sp.turn_signal())
    );
    assert_eq!(req.label, "bot");
    assert_eq!(
        req.root, root,
        "the armed auto-open REPL cwd is the session's PROJECT ROOT, not pmtui's cwd"
    );
}

#[test]
fn armed_first_chat_carries_the_entrys_worker_model() {
    // The armed pmd-down chat fallback must launch with the entry's `worker_model` as `--model`.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].worker_model = Some("global.anthropic.claude-opus-5".into());
    reg.save(&reg_path).unwrap();
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-live".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    app.drain_armed_first_chat();

    let req = app
        .pending_chat
        .as_ref()
        .expect("armed auto-open queued the resume chat");
    assert!(
        req.argv
            .windows(2)
            .any(|w| w == ["--model", "global.anthropic.claude-opus-5"]),
        "the armed chat launch must carry the entry's worker_model: {:?}",
        req.argv
    );
}

#[test]
fn armed_first_chat_stays_armed_while_a_wake_is_running() {
    // A running wake means a second resume of the id would collide — stay armed
    // and keep waiting; do not open a chat.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-live".into());
    l.run = job::JobRun::Running {
        seq: 0,
        session: "pmj-bot-0".into(),
        deadline: 9999,
    };
    job::save(&sp, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    app.drain_armed_first_chat();

    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "a running wake keeps the arm"
    );
    assert!(app.pending_chat.is_none(), "no chat while the wake runs");
}

#[test]
fn armed_first_chat_routes_a_blocked_session_to_the_answer_prompt() {
    // If the armed session parks Blocked, disarm and point at `a` — never
    // auto-open a REPL on a parked-blocked session.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    l.open_stops = vec![open_stop("stop-1", pmstate::StopKind::Ambiguity)];
    job::save(&sp, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    app.drain_armed_first_chat();

    assert!(app.pending_first_chat.is_none(), "disarmed on Blocked");
    assert!(
        app.pending_chat.is_none(),
        "never auto-opens a REPL on a blocked session"
    );
    assert!(
        app.status.contains("answer") && app.status.contains(" a "),
        "the status points the human at a: {}",
        app.status
    );
}

#[test]
fn move_sel_disarms_a_pending_first_chat() {
    // Navigating away cancels an armed auto-open — the human changed focus.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    app.move_sel(1);
    assert!(
        app.pending_first_chat.is_none(),
        "navigating disarms the auto-open"
    );
}

#[test]
fn a_fresh_enter_supersedes_a_stale_arm() {
    // Re-entrancy guard: a pre-existing arm must NOT survive a fresh resolving
    // Enter, or a stale arm would re-fire and yank the human back into a REPL
    // they just left. Here the lease is free ⇒ Enter create-and-chats; the arm
    // set earlier must be cleared so no redundant auto-open queues afterward.
    // (Fails before the top-of-block `self.pending_first_chat = None`.)
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot"); // claude, Idle, no cid
    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into()); // a stale arm from an earlier Enter

    app.request_attach();

    assert!(
        app.pending_first_chat.is_none(),
        "a resolving Enter clears the stale arm (no redundant re-open)"
    );
    assert!(
        app.pending_create_chat.is_some(),
        "the free lease resolved to create-and-chat"
    );
}

#[test]
fn refresh_does_not_auto_open_an_armed_first_chat() {
    // The auto-open must fire ONLY from the run loop's idle-tick branch (gated on
    // Normal mode), NOT as a side effect of `refresh()` — otherwise an action
    // refresh (cycle_tier/submit_answer/submit_create) or a modal
    // would silently yank the human into a REPL. Even with a chattable ledger AND
    // an arm, `refresh()` alone must NOT populate `pending_chat`; an explicit
    // `drain_armed_first_chat` (what the idle tick calls) still opens it.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-live".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    let mut app = loop_app(&reg_path); // loop_app already refreshed once
    app.pending_first_chat = Some("bot".into());
    app.refresh(); // an action-style refresh must NOT auto-open
    assert!(
        app.pending_chat.is_none(),
        "refresh() must not auto-open — that is the idle-tick's job"
    );
    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "the arm survives a plain refresh (only the idle drain consumes it)"
    );

    // The idle tick's explicit drain (Normal mode) still opens it.
    app.drain_armed_first_chat();
    assert!(
        app.pending_chat.is_some(),
        "the explicit idle-tick drain opens the chat"
    );
    assert!(app.pending_first_chat.is_none(), "and disarms");
}
