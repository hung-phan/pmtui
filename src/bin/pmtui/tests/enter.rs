//! Where Enter goes: watch a live worker, attach the persistent loop session, chat a
//! parked conversation, or arm for the daemon — and the fallbacks between them when the
//! loop session is gone or the lease is held.

use super::*;

#[test]
fn seeding_a_conversation_refuses_a_row_removed_from_the_registry() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    Registry::default().save(&reg_path).unwrap();
    let mut app = app_with(vec![agent_loop_view("ghost")], UiMode::Normal);
    app.registry_path = reg_path;

    let err = app
        .seed_registry_conversation_id("ghost", "conversation")
        .expect_err("a removed row must not be recreated by a seed write");

    assert!(err.to_string().contains("ghost is gone from the list"));
}

#[test]
fn enter_on_agent_loop_with_live_worker_requests_watch() {
    // The tmux liveness probe (`is_alive`) needs a real server, so — per the
    // slice's testability note — this asserts the PURE pane-selection step that
    // gates the watch request: a `Running` driver.json yields the daemon-owned
    // `pmj-…` pane, which `request_attach` then attaches to iff that pane is
    // alive. Reads driver.json through the same path the handler uses.
    let dir = tempfile::tempdir().unwrap();
    let (_reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let pane = "pmj-bot-abc123-3";

    write_driver_json(&sp, pane, ExitReason::Running);
    let running = state::read_json_opt::<DriverState>(&sp.driver()).unwrap();
    assert_eq!(
        watch_pane(running.as_ref()),
        Some(pane.to_string()),
        "a live (Running) worker's pane is selected to watch"
    );

    // A finished wake (or none at all) is not watchable — the fall-through to a
    // helpful status, never a watch request.
    write_driver_json(&sp, pane, ExitReason::Clean);
    let finished = state::read_json_opt::<DriverState>(&sp.driver()).unwrap();
    assert_eq!(
        watch_pane(finished.as_ref()),
        None,
        "a finished wake is not watchable"
    );
    assert_eq!(watch_pane(None), None, "no driver.json ⇒ nothing to watch");

    write_driver_json(&sp, pane, ExitReason::Running);
    let live = FakePane::live(pane, "working");
    let mut app = pane_app(&_reg_path, live);
    app.request_attach();
    assert!(
        matches!(app.mode, UiMode::WakeView { ref id, .. } if id == "bot"),
        "a recorded and live worker pane opens the wake view"
    );
}

#[test]
fn enter_on_agent_loop_without_live_worker_or_conversation_arms_when_daemon_drives() {
    // No driver.json (no wake has run) AND a fresh ledger (no conversation id
    // yet), with pmd DRIVING (the per-session driver.lock is held): Enter must
    // NOT watch, interactive-attach, chat, or create — it ARMS the auto-open and
    // shows the honest "creating on the first wake" copy. (S2 replaced the old
    // static "waiting for first wake" message: a never-woken claude session now
    // either creates-and-chats when the lease is free, or arms when pmd holds
    // it — see the sibling free-lease/held-lease tests.) Because `watch_pane`
    // returns None the tmux liveness probe is never reached, so this exercises
    // the full `request_attach` path deterministically.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let lock = sp.daemon_dir().join("driver.lock");
    let _held = lease::try_acquire(&lock)
        .unwrap()
        .expect("pmd holds the lease as it drives this session");

    let mut app = loop_app(&reg_path);
    assert_eq!(
        app.selected_view().map(|v| v.mode),
        Some(Mode::AgentLoop),
        "the refreshed dashboard has the agent-loop row selected"
    );

    app.request_attach();

    assert!(
        !matches!(app.mode, UiMode::WakeView { .. }),
        "no live wake ⇒ not a watch/follow"
    );
    assert!(
        app.pending_chat.is_none(),
        "no conversation id yet ⇒ no chat request"
    );
    assert!(
        app.pending_create_chat.is_none(),
        "pmd is driving ⇒ pmtui does not create"
    );
    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "a never-woken session under a driving pmd arms the auto-open"
    );
    assert!(
        app.status.contains("creating on the first wake"),
        "honest waiting copy: {}",
        app.status
    );
}

#[test]
fn standard_codex_first_enter_opens_a_fresh_chat_without_seeding() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut registry = Registry::load(&reg_path).unwrap();
    registry.projects[0].engine = Some(Engine::Codex);
    registry.save(&reg_path).unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut app = loop_app(&reg_path);

    app.request_attach();

    let request = app
        .pending_chat
        .as_ref()
        .expect("Standard Codex must open a fresh human-driven chat");
    assert_eq!(
        request.argv,
        build_codex_fresh_chat(None, &paths.turn_signal())
    );
    assert_eq!(request.session, session_name("bot", &root));
    assert!(app.pending_create_chat.is_none());
    assert!(
        Registry::load(&reg_path).unwrap().projects[0]
            .conversation_id
            .is_none(),
        "Codex chooses its own conversation id"
    );
}

#[test]
fn first_enter_reports_an_unusable_driver_lock_path() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let paths = ProjectPaths::for_session(&root, "bot");
    let daemon_dir = paths.daemon_dir();
    let _ = std::fs::remove_dir_all(&daemon_dir);
    std::fs::write(&daemon_dir, "not a directory").unwrap();
    let mut app = loop_app(&reg_path);

    app.request_attach();

    assert!(
        app.status.contains("something else may be starting it")
            && app.status.contains("try again"),
        "{}",
        app.status
    );
    assert!(app.pending_chat.is_none());
    assert!(app.pending_create_chat.is_none());
    assert!(app.pending_first_chat.is_none());
}

#[test]
fn enter_on_parked_agent_loop_with_conversation_id_requests_chat() {
    // A parked agent-loop session (no live wake) that has already captured a
    // conversation id offers the on-demand chat: Enter queues a `pending_chat`
    // resuming that id interactively — not a watch, and never an interactive
    // attach. The argv is exactly `build_chat`'s (interactive resume, no `-p`).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    // Give it a conversation id and park it Monitoring (the first wake has run).
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-xyz".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.request_attach();

    assert!(
        !matches!(app.mode, UiMode::WakeView { .. }),
        "no live wake ⇒ not a watch/follow"
    );
    let req = app
        .pending_chat
        .as_ref()
        .expect("a parked session with a conversation id chats");
    assert_eq!(
        req.argv,
        build_chat(Engine::Claude, "conv-xyz", None, &sp.turn_signal()),
        "resumes the captured conversation interactively"
    );
    assert_eq!(req.label, "bot", "the friendly label is the session id");
    assert_eq!(
        req.root, root,
        "the REPL cwd is the session's PROJECT ROOT, not pmtui's cwd or the state dir"
    );
    assert!(
        app.status.contains("chatting with") && app.status.contains("bot"),
        "status names the chat + conversation: {}",
        app.status
    );
}

#[test]
fn enter_on_parked_agent_loop_with_a_ghost_conversation_id_creates_it_rather_than_resuming() {
    // Same as above, but the captured conversation id is a GHOST — no transcript on disk.
    // `--resume <ghost>` opens claude into a dead/empty session (the dream-poster bug), so a
    // FRESH chat launch must CREATE the id with `--session-id` instead, mirroring the daemon's
    // seed-probe. This is the non-paused Enter sibling of the paused-resume fix.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("ghost-conv".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    // projects/ present but no transcript for `ghost-conv` under this root's slug ⇒ Some(false).
    let claude_home = dir.path().join("claude-home");
    std::fs::create_dir_all(claude_home.join("projects")).unwrap();

    let mut app = loop_app(&reg_path);
    app.claude_home = Some(claude_home);
    app.request_attach();

    let req = app
        .pending_chat
        .as_ref()
        .expect("a parked session with a conversation id chats");
    assert_eq!(
        req.argv,
        build_chat_create(Engine::Claude, "ghost-conv", None, &sp.turn_signal()),
        "a ghost id is CREATED with --session-id, not --resume'd into a dead session"
    );
    assert!(
        !req.argv.contains(&"--resume".to_string()),
        "must not --resume a transcript that does not exist: {:?}",
        req.argv
    );
}

#[test]
fn enter_resume_and_ghost_create_carry_the_entrys_worker_model() {
    // A Standard entry whose `worker_model` is set must launch its human-driven chat WITH
    // `--model` — the resume route AND the ghost-create route both read it from the entry.
    for ghost in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
        // Set the per-session worker model on the registry entry.
        let mut reg = Registry::load(&reg_path).unwrap();
        reg.projects[0].worker_model = Some("global.anthropic.claude-opus-5".into());
        reg.save(&reg_path).unwrap();

        let sp = ProjectPaths::for_session(&root, "bot");
        let mut l = job::load(&sp).unwrap().unwrap();
        l.conversation_id = Some("conv-xyz".into());
        l.run = job::JobRun::Monitoring { until: 9999 };
        job::save(&sp, &l).unwrap();

        let mut app = loop_app(&reg_path);
        if ghost {
            // projects/ present but no transcript ⇒ Some(false) ⇒ the create route.
            let claude_home = dir.path().join("claude-home");
            std::fs::create_dir_all(claude_home.join("projects")).unwrap();
            app.claude_home = Some(claude_home);
        }
        app.request_attach();

        let req = app
            .pending_chat
            .as_ref()
            .expect("a parked session with a conversation id chats");
        assert!(
            req.argv
                .windows(2)
                .any(|w| w == ["--model", "global.anthropic.claude-opus-5"]),
            "ghost={ghost}: the human-driven launch must carry the entry's worker_model: {:?}",
            req.argv
        );
    }
}

#[test]
fn enter_attaches_the_live_persistent_loop_session_over_any_chat_routing() {
    // Slice 1 correctness guard: when the daemon's persistent `pmloop-` agent
    // session is ALIVE, Enter ATTACHES it (a new `pending_attach_loop`) and does
    // NOT queue any chat/create/interactive-attach — so a second `claude --resume`
    // can never collide with the live loop on the same conversation id. Even a
    // captured conversation id + a Monitoring ledger (the case that would otherwise
    // route to `pending_chat`) must lose to the alive loop session. The tmux
    // liveness probe needs a real server (like the live-delete test), so launch a
    // real `pmloop-…` session on a private socket first.
    if !tmux_available() {
        eprintln!("skipping live-attach test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    std::fs::create_dir_all(&root).unwrap();
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-live".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    let socket = TmuxSocket::new("pmtui-loop");
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
    assert!(
        driver.is_alive(&loop_session).unwrap(),
        "the pmloop- session should be live"
    );

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();
    app.agent_tmux = Box::new(TmuxDriver::with_socket(socket.name()));
    app.request_attach();

    assert_eq!(
        app.pending_attach_loop.as_deref(),
        Some(loop_session.as_str()),
        "an alive pmloop- session is attached, regardless of the ledger run state"
    );
    assert!(
        app.pending_chat.is_none() && app.pending_create_chat.is_none(),
        "no colliding chat/create resume when the live loop is attached"
    );
    assert!(
        !matches!(app.mode, UiMode::WakeView { .. }),
        "attaching the loop is not a wake-follow"
    );
    assert!(
        app.status.contains("attached") && app.status.contains("bot"),
        "status names the attach: {}",
        app.status
    );

    let _ = driver.terminate(&loop_session);
    let _ = std::process::Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .output();
}

#[test]
fn enter_falls_back_to_chat_when_no_live_loop_session() {
    // pmd-down fallback: with NO live `pmloop-` session (the test socket has no
    // server, so `is_alive` is Ok(false)) but a captured conversation id, Enter must
    // still fall through to the EXISTING routing and queue `pending_chat` — safe,
    // because no process is driving this conversation. Proves the alive-loop guard
    // leaves the shipped chat/create/wait fallback fully intact when pmd is down.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-xyz".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.request_attach();

    assert!(
        app.pending_attach_loop.is_none(),
        "no live pmloop- session ⇒ nothing to attach"
    );
    assert!(
        app.pending_chat.is_some(),
        "the pmd-down fallback still offers the on-demand chat"
    );
}

#[test]
fn armed_open_attaches_the_live_persistent_loop_session_instead_of_chatting() {
    // C1 regression: the armed auto-open fires precisely because pmd was DRIVING,
    // so by the time it resolves the daemon has usually launched the persistent
    // `pmloop-` claude on this conversation. `drain_armed_first_chat` must ATTACH
    // that live session, NOT resume the same cid in a second `pmchat-` REPL (which
    // would fork the transcript — two claude on one conversation id). This test
    // FAILS before the C1 fix (the old code queued `pending_chat`) and passes after.
    // Needs a real tmux server for the liveness probe (like the live-attach test).
    if !tmux_available() {
        eprintln!("skipping armed-open live-attach test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    std::fs::create_dir_all(&root).unwrap();
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-live".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    let socket = TmuxSocket::new("pmtui-armed");
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
    assert!(
        driver.is_alive(&loop_session).unwrap(),
        "the pmloop- session should be live"
    );

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();
    app.pending_first_chat = Some("bot".into());
    app.drain_armed_first_chat();

    assert_eq!(
        app.pending_attach_loop.as_deref(),
        Some(loop_session.as_str()),
        "an alive pmloop- session is attached instead of chatted"
    );
    assert!(
        app.pending_chat.is_none(),
        "no second claude --resume is queued on the live conversation"
    );
    assert!(
        app.pending_first_chat.is_none(),
        "the arm is disarmed once it resolves"
    );

    let _ = driver.terminate(&loop_session);
    let _ = std::process::Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .output();
}

#[test]
fn armed_open_falls_back_to_chat_when_no_live_loop_session() {
    // pmd-down fallback for the armed path: with NO live `pmloop-` session, an arm
    // that resolves to a cleanly-chattable ledger still queues the resume
    // `pending_chat` and disarms — the shipped behavior is preserved, and nothing is
    // attached.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut l = job::load(&sp).unwrap().unwrap();
    l.conversation_id = Some("conv-xyz".into());
    l.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&sp, &l).unwrap();

    let mut app = loop_app(&reg_path);
    app.pending_first_chat = Some("bot".into());
    app.drain_armed_first_chat();

    assert!(
        app.pending_attach_loop.is_none(),
        "no live loop ⇒ nothing to attach"
    );
    assert!(
        app.pending_chat.is_some(),
        "armed-open still chats when pmd is down (fallback preserved)"
    );
    assert!(
        app.pending_first_chat.is_none(),
        "the arm is disarmed once it resolves"
    );
}

// --- S2: chattable-on-create (create launch argv, effective_id, lease dance,
// armed auto-open) -----------------------------------------------------------

#[test]
fn first_wake_action_routes_and_copies_per_engine_and_lease() {
    // claude + free lock ⇒ create-and-chat now (no copy — the caller builds the
    // create status once the mint/seed succeeds).
    assert_eq!(
        first_wake_action(Engine::Claude, true, "bot"),
        FirstWakeAction::CreateAndChat
    );
    // claude + held lock ⇒ arm with the "you'll drop in automatically" copy.
    match first_wake_action(Engine::Claude, false, "bot") {
        FirstWakeAction::Arm(s) => {
            assert!(s.contains("creating on the first wake"), "{s}");
            assert!(s.contains("automatically"), "{s}");
        }
        other => panic!("claude+held must ARM, got {other:?}"),
    }
    // codex + held lock ⇒ arm with the codex-after-wake copy.
    match first_wake_action(Engine::Codex, false, "bot") {
        FirstWakeAction::Arm(s) => {
            assert!(
                s.contains("codex") && s.contains("after the first wake"),
                "{s}"
            )
        }
        other => panic!("codex+held must ARM, got {other:?}"),
    }
    // codex + free lock (pmd down) ⇒ NOT an arm and NOT a start-pmd dead-end: open a
    // FRESH codex chat now (codex creates the conversation itself — no id to mint/seed).
    match first_wake_action(Engine::Codex, true, "bot") {
        FirstWakeAction::CreateChatNoSeed(s) => {
            assert!(s.contains("codex chat"), "{s}");
            assert!(s.contains("bot"), "{s}");
        }
        other => panic!("codex+free must be CreateChatNoSeed, got {other:?}"),
    }
}

#[test]
fn armed_open_decision_opens_only_when_cleanly_chattable() {
    // Parked WITH an id ⇒ open (both Monitoring and Idle count).
    assert_eq!(
        armed_open_decision(Some("c1"), &job::JobRun::Monitoring { until: 9 }),
        ArmedOpen::Open("c1".to_string())
    );
    assert_eq!(
        armed_open_decision(Some("c1"), &job::JobRun::Idle),
        ArmedOpen::Open("c1".to_string())
    );
    // A running wake ⇒ stay armed (a second resume of the id would collide).
    assert_eq!(
        armed_open_decision(
            Some("c1"),
            &job::JobRun::Running {
                seq: 0,
                session: "pmj".into(),
                deadline: 9
            }
        ),
        ArmedOpen::Stay
    );
    // Parked but no id yet ⇒ stay armed.
    assert_eq!(
        armed_open_decision(None, &job::JobRun::Monitoring { until: 9 }),
        ArmedOpen::Stay
    );
    // Blocked ⇒ route to the answer prompt regardless of id (a codex
    // capture-failure with no id lands here too, never left "waiting").
    assert_eq!(
        armed_open_decision(
            Some("c1"),
            &job::JobRun::Blocked {
                stop_ids: vec!["s".into()],
                since: 1
            }
        ),
        ArmedOpen::Blocked
    );
    assert_eq!(
        armed_open_decision(
            None,
            &job::JobRun::Blocked {
                stop_ids: vec!["s".into()],
                since: 1
            }
        ),
        ArmedOpen::Blocked
    );
}

#[test]
fn request_attach_creates_and_chats_a_never_woken_claude_when_lease_free() {
    // pmd is NOT driving (the driver.lock is free): Enter on a never-woken claude
    // session MINTS an id, SEEDS registry.conversation_id (so the daemon adopts
    // it), and queues a create-and-chat holding the lease — NOT an arm.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // claude, Idle, no cid
    let mut app = loop_app(&reg_path);

    app.request_attach();

    assert!(
        app.pending_first_chat.is_none(),
        "a free lease creates, it does not arm"
    );
    assert!(!matches!(app.mode, UiMode::WakeView { .. }));
    let req = app
        .pending_create_chat
        .as_ref()
        .expect("a free lease queues a create-and-chat");
    // The registry now carries the minted seed, and the argv creates that id.
    let reg = Registry::load(&reg_path).unwrap();
    let seeded = reg
        .projects
        .iter()
        .find(|p| p.id == "bot")
        .unwrap()
        .conversation_id
        .clone()
        .expect("registry seeded with the minted id");
    assert_eq!(
        req.argv,
        build_chat_create(
            Engine::Claude,
            &seeded,
            None,
            &req.session_paths.turn_signal(),
        )
    );
    assert_eq!(req.label, "bot");
    assert_eq!(
        req.root, root,
        "the create-and-chat REPL cwd is the session's PROJECT ROOT, not pmtui's cwd"
    );
    assert_eq!(
        req.argv.last().unwrap(),
        &seeded,
        "the argv creates exactly the seeded id"
    );
    assert!(app.status.contains("creating"), "{}", app.status);
}

#[test]
fn request_attach_arms_a_never_woken_claude_when_daemon_holds_the_lease() {
    // pmd holds the driver.lock (it is driving): Enter must ARM the auto-open —
    // NOT mint, NOT seed the registry, NOT create.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    let lock = sp.daemon_dir().join("driver.lock");
    let _held = lease::try_acquire(&lock)
        .unwrap()
        .expect("the test holds the lease exactly as pmd would");

    let mut app = loop_app(&reg_path);
    app.request_attach();

    assert_eq!(
        app.pending_first_chat.as_deref(),
        Some("bot"),
        "a held lease arms the auto-open"
    );
    assert!(
        app.pending_create_chat.is_none(),
        "no create while pmd is driving"
    );
    // The registry seed is untouched — arming never mints.
    let reg = Registry::load(&reg_path).unwrap();
    assert!(
        reg.projects
            .iter()
            .find(|p| p.id == "bot")
            .unwrap()
            .conversation_id
            .is_none(),
        "arming must not seed the registry"
    );
    assert!(
        app.status.contains("creating on the first wake"),
        "{}",
        app.status
    );
}

#[test]
fn codex_standard_free_lease_opens_a_fresh_chat_not_a_start_pmd_dead_end() {
    // codex + free lock (pmd down), STANDARD: this used to dead-end ("start pmd for the
    // first codex wake") because codex has no caller-chosen id. It no longer does — the
    // human is present, so pmtui opens a FRESH interactive codex REPL now and lets codex
    // create the conversation itself. The Enter arm (`FirstWakeAction::CreateChatNoSeed`)
    // drops the lease, then queues `pending_chat` with `build_codex_fresh_chat`'s argv.
    //
    // Asserted PURELY (decision + argv), NOT by driving the real `request_attach`: the
    // The pure route and argv are asserted here; the real terminal path is covered by
    // the ignored tmux acceptance suite.
    match first_wake_action(Engine::Codex, true, "cx") {
        FirstWakeAction::CreateChatNoSeed(s) => assert!(s.contains("codex chat"), "{s}"),
        other => panic!("codex+free STANDARD must open a fresh chat, got {other:?}"),
    }
    // The queued argv is a FRESH interactive codex launch: no `resume`, no id — codex
    // mints the conversation. (Trust profile / model insertion are covered in session.rs.)
    let argv = build_codex_fresh_chat(None, test_turn_signal());
    assert_eq!(argv[0], "codex", "{argv:?}");
    assert!(argv.iter().any(|arg| arg == "-c"), "{argv:?}");
    assert!(!argv.iter().any(|a| a == "resume"), "no resume: {argv:?}");
    assert!(
        !argv.iter().any(|a| a == "--session-id"),
        "no caller-chosen id: {argv:?}"
    );
}

#[test]
fn enter_while_pmd_is_starting_the_agent_does_not_open_a_chat_that_pauses_it() {
    // User: *"when i enable autopilot it doesn't run immediately on the tick. Also when i enter the
    // session, autopilot is paused. This is a bug"*. ONE chain, not two reports: the agent takes ~13s
    // to reach its first nudge (LAUNCH_GRACE_S so a booting agent is never typed into, plus one
    // BUSY_RECHECK_S for the second idle confirmation), the human presses Enter to see what is
    // happening, and Enter opened a `pmchat-` REPL on the conversation — which `chat_lock::is_active`
    // then honours with NO age cap, pausing the autopilot the previous keystroke switched on.
    //
    // Enter must not be able to undo `m`.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    set_tier(&sp, Tier::Autopilot);
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.conversation_id = Some("conv-1".into()); // a conversation exists ⇒ the chat branch was reachable
    job::save(&sp, &l).unwrap();

    let pane = FakePane::default(); // no `pmloop-` yet: pmd is between launch and nudge
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Normal,
        Box::new(pane.clone()),
    );
    app.registry_path = reg_path.clone();
    // A LIVE DAEMON is half the gate: with pmd down nothing would ever launch the agent and the chat
    // is the only way in, so the refusal must not fire there.
    let held = lease::try_acquire(&lease::daemon_lock_path(&reg_path, &app.socket))
        .unwrap()
        .expect("free in a scratch registry");

    app.request_attach();

    assert!(
        app.pending_chat.is_none() && app.pending_create_chat.is_none(),
        "Enter must not open a chat that pauses the autopilot just turned on"
    );
    assert!(
        app.status.contains("pmd is starting the agent"),
        "…and must say what IS happening, plus what to do: {}",
        app.status
    );
    assert!(
        pane.launches().is_empty(),
        "nothing may be launched here — the launch is pmd's: {:?}",
        pane.launches()
    );

    // WITH PMD DOWN the same key must still get the human in: nothing else will ever start this agent.
    drop(held);
    // The second `request_attach` re-probes the SINGLETON daemon lock through `daemon_live()`
    // (`probe_daemon_live` → `lease::is_held`); its "pmd is starting the agent" refusal fires iff
    // that probe reads Up. `drop(held)` freed the lock, but a sibling test's fork→exec can leave
    // the just-dropped fd readable as HELD for the CLOEXEC-at-exec window — which would re-fire the
    // refusal and strand `pending_chat` at None. This downstream branch cannot be retried, so wait
    // for the lock to be observably free before pressing Enter again.
    wait_until_free(&lease::daemon_lock_path(&reg_path, &app.socket));
    expire_daemon_cache(&app);
    app.status.clear();
    app.request_attach();
    assert!(
        app.pending_chat.is_some(),
        "with no daemon the chat is the only way in and must still open: {}",
        app.status
    );
}
