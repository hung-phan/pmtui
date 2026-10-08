//! Building the command that attaches or chats: the guard on a dash-leading prompt, the
//! resume flags each engine gets and the headless ones it must not, and the conversation
//! id `pmtui` resumes rather than re-mints.

use super::*;
use std::cell::Cell;
use std::os::unix::process::ExitStatusExt;

#[test]
fn build_chat_claude_resumes_interactively_without_headless_flags() {
    // Interactive resume: `env -u CLAUDECODE` (drop the inherited Claude Code
    // marker, like the poll worker/spike) then `claude --resume <id>` — NO `-p`.
    let argv = build_chat(Engine::Claude, "conv-abc123", None, test_turn_signal());
    assert_eq!(&argv[..4], ["env", "-u", "CLAUDECODE", "claude"]);
    assert!(argv.iter().any(|arg| arg == "--settings"), "{argv:?}");
    assert_eq!(&argv[argv.len() - 2..], ["--resume", "conv-abc123"]);
    assert!(
        !argv.iter().any(|a| a == "-p"),
        "interactive chat is not headless: no -p"
    );
}

#[test]
fn build_chat_codex_resumes_interactively_without_exec() {
    // Interactive resume: `codex resume <id>` — NO `exec` (that is the headless
    // poll path).
    let argv = build_chat(Engine::Codex, "sess-xyz", None, test_turn_signal());
    assert_eq!(argv[0], "codex");
    assert!(argv.iter().any(|arg| arg == "-c"), "{argv:?}");
    assert_eq!(&argv[argv.len() - 2..], ["resume", "sess-xyz"]);
    assert!(
        !argv.iter().any(|a| a == "exec"),
        "interactive chat is not headless: no exec"
    );
}

#[test]
fn build_chat_inserts_model_when_some_omits_when_none() {
    // Present-iff-Some AND position. claude: `--model <m>` AFTER `claude`, BEFORE `--resume`.
    let argv = build_chat(
        Engine::Claude,
        "conv-1",
        Some("global.anthropic.claude-opus-5"),
        test_turn_signal(),
    );
    assert!(
        argv.windows(2)
            .any(|w| w == ["--model", "global.anthropic.claude-opus-5"]),
        "{argv:?}"
    );
    let model_at = argv.iter().position(|a| a == "--model").unwrap();
    let claude_at = argv.iter().position(|a| a == "claude").unwrap();
    let resume_at = argv.iter().position(|a| a == "--resume").unwrap();
    assert!(
        claude_at < model_at && model_at < resume_at,
        "--model after claude, before --resume: {argv:?}"
    );
    // codex: `-m <m>` AFTER `codex`, BEFORE `resume`.
    let cx = build_chat(
        Engine::Codex,
        "sess-1",
        Some("openai.gpt-5.6-sol"),
        test_turn_signal(),
    );
    assert!(
        cx.windows(2).any(|w| w == ["-m", "openai.gpt-5.6-sol"]),
        "{cx:?}"
    );
    let m_at = cx.iter().position(|a| a == "-m").unwrap();
    let codex_at = cx.iter().position(|a| a == "codex").unwrap();
    let cx_resume_at = cx.iter().position(|a| a == "resume").unwrap();
    assert!(
        codex_at < m_at && m_at < cx_resume_at,
        "-m after codex, before resume: {cx:?}"
    );
    // None ⇒ no flag on either engine.
    assert!(
        !build_chat(Engine::Claude, "conv-1", None, test_turn_signal())
            .iter()
            .any(|a| a == "--model")
    );
    assert!(
        !build_chat(Engine::Codex, "sess-1", None, test_turn_signal())
            .iter()
            .any(|a| a == "-m")
    );
}

#[test]
fn build_chat_create_inserts_model_when_some_omits_when_none() {
    // claude create: `--model <m>` AFTER `claude`, BEFORE `--session-id`.
    let argv = build_chat_create(
        Engine::Claude,
        "new-uuid-1",
        Some("global.anthropic.claude-opus-5"),
        test_turn_signal(),
    );
    assert!(
        argv.windows(2)
            .any(|w| w == ["--model", "global.anthropic.claude-opus-5"]),
        "{argv:?}"
    );
    let model_at = argv.iter().position(|a| a == "--model").unwrap();
    let claude_at = argv.iter().position(|a| a == "claude").unwrap();
    let sid_at = argv.iter().position(|a| a == "--session-id").unwrap();
    assert!(
        claude_at < model_at && model_at < sid_at,
        "--model after claude, before --session-id: {argv:?}"
    );
    assert!(
        !build_chat_create(Engine::Claude, "new-uuid-1", None, test_turn_signal())
            .iter()
            .any(|a| a == "--model")
    );
}

#[test]
fn build_chat_and_create_omit_model_when_empty_or_whitespace() {
    // A blank model is "no model set": `Some("")`/`Some("  ")` emits NO flag on either engine,
    // for both the resume (build_chat) and create (build_chat_create) builders — same as None.
    for m in ["", "  "] {
        assert!(
            !build_chat(Engine::Claude, "conv-1", Some(m), test_turn_signal())
                .iter()
                .any(|a| a == "--model"),
            "build_chat claude m={m:?}"
        );
        assert!(
            !build_chat(Engine::Codex, "sess-1", Some(m), test_turn_signal())
                .iter()
                .any(|a| a == "-m"),
            "build_chat codex m={m:?}"
        );
        assert!(
            !build_chat_create(Engine::Claude, "new-uuid-1", Some(m), test_turn_signal(),)
                .iter()
                .any(|a| a == "--model"),
            "build_chat_create claude m={m:?}"
        );
    }
}

#[test]
fn agent_loop_enter_watches_a_live_wake_pane() {
    // A live wake pane wins regardless of the conversation id / run state — the
    // existing read-only watch of the autonomous work in flight.
    assert_eq!(
        agent_loop_enter(Some("pmj-bot-3"), Some("conv-1"), true),
        EnterAction::Watch("pmj-bot-3".to_string())
    );
}

#[test]
fn agent_loop_enter_chats_when_parked_with_a_conversation_id() {
    // No live pane + a captured conversation id + NOT running ⇒ chat on that id.
    assert_eq!(
        agent_loop_enter(None, Some("conv-1"), false),
        EnterAction::Chat("conv-1".to_string())
    );
}

#[test]
fn agent_loop_enter_waits_for_first_wake_without_a_conversation_id() {
    // No live pane + no conversation id yet ⇒ waiting for the first wake (the id
    // is minted/captured on the first heartbeat).
    assert_eq!(
        agent_loop_enter(None, None, false),
        EnterAction::WaitingFirstWake
    );
}

#[test]
fn agent_loop_enter_reports_no_wake_when_running_without_a_live_pane() {
    // A wake is `Running` but its pane wasn't found alive (a rare race): neither
    // watch nor chat — fall through to the no-wake cadence hint.
    assert_eq!(
        agent_loop_enter(None, Some("conv-1"), true),
        EnterAction::NoWake
    );
}

#[test]
fn reserved_id_disambiguates_registry_entries() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = Registry::default();
    reg.projects.push(ProjectEntry {
        id: "proj".into(),
        display_name: None,
        root: PathBuf::from("/a"),
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
        cadence_s: None,
    });
    assert_eq!(
        reserve_unique_id(&reg, dir.path(), "proj").unwrap(),
        "proj-2"
    );
    assert_eq!(
        reserve_unique_id(&reg, dir.path(), "other").unwrap(),
        "other"
    );
}

#[test]
fn build_chat_create_claude_uses_session_id_not_resume_or_headless() {
    // The CREATE launch pins a caller-chosen id with `--session-id` and is
    // interactive: `env -u CLAUDECODE claude --session-id <id>` — NO `-p` (that
    // is headless) and NO `--resume` (that is the RESUME path, build_chat).
    let argv = build_chat_create(Engine::Claude, "new-uuid-1", None, test_turn_signal());
    assert_eq!(&argv[..4], ["env", "-u", "CLAUDECODE", "claude"]);
    assert!(argv.iter().any(|arg| arg == "--settings"), "{argv:?}");
    assert_eq!(&argv[argv.len() - 2..], ["--session-id", "new-uuid-1"]);
    assert!(
        !argv.iter().any(|a| a == "-p"),
        "create is interactive: no -p"
    );
    assert!(
        !argv.iter().any(|a| a == "--resume"),
        "create is not a resume"
    );
}

#[test]
fn build_codex_fresh_chat_is_a_plain_interactive_codex_with_optional_flags() {
    // Bare: no id (`resume`/`--session-id`), no unattended approval flags — codex mints
    // the conversation and a human answers approvals (mirrors build_chat's codex posture).
    let bare = build_codex_fresh_chat(None, test_turn_signal());
    assert_eq!(bare[0], "codex");
    assert!(bare.iter().any(|arg| arg == "-c"), "{bare:?}");
    // A model inserts `-m <m>` after `codex` (GLOBAL option, before any subcommand).
    assert_eq!(
        &build_codex_fresh_chat(Some("gpt-x"), test_turn_signal())[..3],
        ["codex", "-m", "gpt-x"]
    );
    // A blank/whitespace model is "no model set": no `-m` flag (same rule as build_chat).
    for m in ["", "  "] {
        assert!(
            !build_codex_fresh_chat(Some(m), test_turn_signal())
                .iter()
                .any(|a| a == "-m"),
            "blank model m={m:?} emits no -m"
        );
    }
}

#[test]
fn effective_id_prefers_ledger_then_registry_seed() {
    // The ledger's id wins (the daemon's authority); the registry seed is only
    // the fallback; both absent ⇒ None.
    assert_eq!(
        effective_id(Some("ledger"), Some("seed")),
        Some("ledger".to_string())
    );
    assert_eq!(effective_id(None, Some("seed")), Some("seed".to_string()));
    assert_eq!(
        effective_id(Some("ledger"), None),
        Some("ledger".to_string())
    );
    assert_eq!(effective_id(None, None), None);
}

#[test]
fn second_enter_on_seed_only_session_resumes_not_re_mints() {
    // After a create-and-chat seeded the registry (the ledger's id is still
    // None), a second Enter must Chat the SEEDED id via the resume path — never
    // mint a second conversation. effective_id feeds agent_loop_enter, so a
    // seed-only state is Chat(seed), not WaitingFirstWake.
    let eff = effective_id(None, Some("seed-42"));
    assert_eq!(
        agent_loop_enter(None, eff.as_deref(), false),
        EnterAction::Chat("seed-42".to_string())
    );
}

#[test]
#[should_panic(expected = "codex conversations are never created by pmtui")]
fn build_chat_create_rejects_codex_without_a_caller_chosen_id() {
    let _ = build_chat_create(Engine::Codex, "unused", None, test_turn_signal());
}

#[derive(Default)]
struct RecordingHandoff {
    events: Vec<&'static str>,
    event_log: Option<PathBuf>,
    fail_suspend: bool,
    fail_restore: bool,
}

impl RecordingHandoff {
    fn with_log(event_log: PathBuf) -> Self {
        Self {
            event_log: Some(event_log),
            ..Self::default()
        }
    }

    fn failing_suspend() -> Self {
        Self {
            fail_suspend: true,
            ..Self::default()
        }
    }

    fn failing_restore() -> Self {
        Self {
            fail_restore: true,
            ..Self::default()
        }
    }

    fn record(&mut self, event: &'static str) -> Result<()> {
        use std::io::Write;

        self.events.push(event);
        if let Some(path) = &self.event_log {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            writeln!(file, "{event}")?;
        }
        Ok(())
    }
}

impl TerminalHandoff for RecordingHandoff {
    fn suspend(&mut self) -> Result<()> {
        self.record("suspend")?;
        if self.fail_suspend {
            anyhow::bail!("test suspend failure");
        }
        Ok(())
    }

    fn restore(&mut self) -> Result<()> {
        self.record("restore")?;
        if self.fail_restore {
            anyhow::bail!("test restore failure");
        }
        Ok(())
    }
}

fn exit_status(code: i32) -> std::process::ExitStatus {
    std::process::ExitStatus::from_raw(code << 8)
}

#[test]
fn chat_launch_error_clears_attach_marker_without_terminal_handoff() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    let socket = format!("pmtui-session-chat-error-{}", std::process::id());
    let req = ChatReq {
        session_paths: paths.clone(),
        root: dir.path().to_path_buf(),
        argv: vec!["pmtui-command-that-does-not-exist".into()],
        label: "bot".into(),
        socket,
        session: "pmtui-session-chat-error-target".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    };

    let pane = FakePane::default().with(|inner| inner.fail_launch = true);
    let mut handoff = RecordingHandoff::default();
    let started = Cell::new(false);
    let error = chat_with_handoff(&mut handoff, &pane, &req, || started.set(true)).unwrap_err();

    assert!(
        error.to_string().contains("new-session failed"),
        "{error:#}"
    );
    assert!(handoff.events.is_empty());
    assert!(!started.get(), "a failed launch must remain retryable");
    assert!(
        !paths.chat_lock().exists(),
        "the guard must clear attach intent on an early launch error"
    );
}

#[test]
fn chat_refuses_a_running_wake_and_cleans_attach_marker() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    let socket = format!("pmtui-session-running-wake-{}", std::process::id());
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1_000);
    ledger.run = job::JobRun::Running {
        seq: 7,
        session: "pmj-bot-7".into(),
        deadline: 2_000,
    };
    job::save(&paths, &ledger).unwrap();
    let req = ChatReq {
        session_paths: paths.clone(),
        root: dir.path().to_path_buf(),
        argv: vec!["sleep".into(), "30".into()],
        label: "bot".into(),
        socket,
        session: "pmtui-session-running-wake-target".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    };

    let pane = FakePane::default();
    let mut handoff = RecordingHandoff::default();
    let error = chat_with_handoff(&mut handoff, &pane, &req, || {}).unwrap_err();

    assert!(
        error.to_string().contains("a wake just started"),
        "{error:#}"
    );
    assert!(handoff.events.is_empty());
    assert!(
        !paths.chat_lock().exists(),
        "the guard must clear attach intent when a durable wake wins the race"
    );
}

#[test]
fn chat_restores_the_terminal_when_foreground_attach_fails() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    let req = ChatReq {
        session_paths: paths.clone(),
        root: dir.path().to_path_buf(),
        argv: vec!["agent".into()],
        label: "bot".into(),
        socket: "pm-test".into(),
        session: "pm-bot".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    };
    let pane = FakePane::default().with(|inner| inner.fail_attach = true);
    let mut handoff = RecordingHandoff::default();
    let started = Cell::new(false);

    let error = chat_with_handoff(&mut handoff, &pane, &req, || started.set(true)).unwrap_err();

    assert!(error.to_string().contains("attach-session failed"));
    assert!(
        started.get(),
        "attach failure occurs after retry eligibility is consumed"
    );
    assert_eq!(handoff.events, ["suspend", "restore"]);
    assert!(!paths.chat_lock().exists());
}

#[test]
fn chat_create_and_attach_run_through_the_injected_driver_and_handoff() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    let session = "pm-session";
    let chat_env = tmux::ManagedEnv {
        session_id: "bot".into(),
        state_dir: paths.state_dir(),
        pmtui_bin: Some(PathBuf::from("/opt/am/pmtui")),
    };
    let chat_req = ChatReq {
        session_paths: paths.clone(),
        root: dir.path().to_path_buf(),
        argv: vec!["agent".into()],
        label: "bot".into(),
        socket: "pm-test".into(),
        session: session.into(),
        engine: Engine::Claude,
        env: chat_env.clone(),
    };
    let chat_pane = FakePane::default();
    let mut chat_handoff = RecordingHandoff::default();

    chat_with_handoff(&mut chat_handoff, &chat_pane, &chat_req, || {}).unwrap();

    assert_eq!(chat_handoff.events, ["suspend", "restore"]);
    assert_eq!(chat_pane.launches().len(), 1);
    assert_eq!(
        chat_pane.launched_env(),
        [(session.to_string(), chat_env)],
        "the request's identity reaches the terminal it launches"
    );
    assert_eq!(chat_pane.foreground_attaches(), [session]);
    assert!(!paths.chat_lock().exists());

    let create_paths = ProjectPaths::for_session(dir.path(), "created");
    let lease = acquire_free_lease(&create_paths.daemon_dir().join("driver.lock"))
        .expect("create lease should be free");
    let create_env = tmux::ManagedEnv {
        session_id: "created".into(),
        state_dir: create_paths.state_dir(),
        pmtui_bin: None,
    };
    let create_req = CreateChatReq {
        session_paths: create_paths.clone(),
        root: dir.path().to_path_buf(),
        argv: vec!["agent".into()],
        label: "created".into(),
        lease,
        socket: "pm-test".into(),
        session: "pm-created".into(),
        engine: Engine::Claude,
        env: create_env.clone(),
    };
    let create_pane = FakePane::default();
    let mut create_handoff = RecordingHandoff::default();

    create_chat_with_handoff(&mut create_handoff, &create_pane, create_req, || {}).unwrap();

    assert_eq!(create_handoff.events, ["suspend", "restore"]);
    assert_eq!(
        create_pane.launched_env(),
        [("pm-created".to_string(), create_env)]
    );
    assert_eq!(create_pane.foreground_attaches(), ["pm-created"]);
    assert!(!create_paths.chat_lock().exists());

    let attach_pane = FakePane::live("pm-attached", "");
    let mut attach_handoff = RecordingHandoff::default();
    attach_loop_with_handoff(
        &mut attach_handoff,
        &attach_pane,
        &paths,
        "pm-test",
        "pm-attached",
    )
    .unwrap();
    assert_eq!(attach_handoff.events, ["suspend", "restore"]);
    assert_eq!(attach_pane.foreground_attaches(), ["pm-attached"]);
    assert!(!paths.chat_lock().exists());
}

#[test]
fn attach_surfaces_input_lock_contention_and_lock_io_errors_before_handoff() {
    let held_dir = tempfile::tempdir().unwrap();
    let held_paths = ProjectPaths::for_session(held_dir.path(), "held");
    let _held = lease::try_acquire(&held_paths.input_lock())
        .unwrap()
        .expect("input lock should be free");
    let mut held_handoff = RecordingHandoff::default();
    let error = attach_loop_with_handoff(
        &mut held_handoff,
        &FakePane::default(),
        &held_paths,
        "test-socket",
        "pm-held",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("agent input is busy"),
        "{error:#}"
    );
    assert!(held_handoff.events.is_empty());

    let broken_dir = tempfile::tempdir().unwrap();
    let broken_paths = ProjectPaths::for_session(broken_dir.path(), "broken");
    std::fs::create_dir_all(broken_paths.input_lock()).unwrap();
    let mut broken_handoff = RecordingHandoff::default();
    let error = attach_loop_with_handoff(
        &mut broken_handoff,
        &FakePane::default(),
        &broken_paths,
        "test-socket",
        "pm-broken",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("Is a directory")
            || error.to_string().contains("is a directory"),
        "{error:#}"
    );
    assert!(broken_handoff.events.is_empty());
}

#[test]
fn create_chat_preserves_launch_and_terminal_restore_failures() {
    let launch_dir = tempfile::tempdir().unwrap();
    let launch_paths = ProjectPaths::for_session(launch_dir.path(), "launch-fails");
    let launch_req = CreateChatReq {
        session_paths: launch_paths.clone(),
        root: launch_dir.path().to_path_buf(),
        argv: vec!["agent".into()],
        label: "launch-fails".into(),
        lease: acquire_free_lease(&launch_paths.daemon_dir().join("driver.lock"))
            .expect("driver lock"),
        socket: "test-socket".into(),
        session: "pm-launch-fails".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    };
    let failing_driver = FakePane::default().with(|inner| inner.fail_launch = true);
    let mut launch_handoff = RecordingHandoff::default();
    let error = create_chat_with_handoff(&mut launch_handoff, &failing_driver, launch_req, || {})
        .unwrap_err();
    assert!(
        error.to_string().contains("new-session failed"),
        "{error:#}"
    );
    assert!(launch_handoff.events.is_empty());
    assert!(!launch_paths.chat_lock().exists());

    let restore_dir = tempfile::tempdir().unwrap();
    let restore_paths = ProjectPaths::for_session(restore_dir.path(), "restore-fails");
    let restore_req = CreateChatReq {
        session_paths: restore_paths.clone(),
        root: restore_dir.path().to_path_buf(),
        argv: vec!["agent".into()],
        label: "restore-fails".into(),
        lease: acquire_free_lease(&restore_paths.daemon_dir().join("driver.lock"))
            .expect("driver lock"),
        socket: "test-socket".into(),
        session: "pm-restore-fails".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    };
    let mut restore_handoff = RecordingHandoff::failing_restore();
    let error = create_chat_with_handoff(
        &mut restore_handoff,
        &FakePane::default(),
        restore_req,
        || {},
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("test restore failure"),
        "{error:#}"
    );
    assert_eq!(restore_handoff.events, ["suspend", "restore"]);
    assert!(!restore_paths.chat_lock().exists());
}

#[test]
fn production_editor_wrappers_spawn_the_selected_editor() {
    assert_eq!(
        editor_command_from(Some("visual".into()), Some("editor".into())),
        "visual"
    );
    assert_eq!(editor_command_from(None, Some("editor".into())), "editor");
    assert_eq!(editor_command_from(None, None), "vi");

    let mut send_handoff = RecordingHandoff::default();
    assert_eq!(
        edit_send_message(&mut send_handoff, "draft", "/bin/true")
            .unwrap()
            .as_deref(),
        Some("draft")
    );
    assert_eq!(send_handoff.events, ["suspend", "restore"]);

    let mut directive_handoff = RecordingHandoff::default();
    assert_eq!(
        edit_directive(&mut directive_handoff, "keep this", "/bin/true")
            .unwrap()
            .as_deref(),
        Some("keep this")
    );
    assert_eq!(directive_handoff.events, ["suspend", "restore"]);

    let mut brief_handoff = RecordingHandoff::default();
    assert_eq!(
        edit_brief(&mut brief_handoff, "ship it", "/bin/true")
            .unwrap()
            .as_deref(),
        Some("ship it")
    );
    assert_eq!(brief_handoff.events, ["suspend", "restore"]);
}

#[test]
fn attach_marker_write_failures_stop_before_terminal_handoff() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();
    std::fs::create_dir(paths.chat_lock()).unwrap();
    let pane = FakePane::live("pm-bot", "");

    let mut attach_handoff = RecordingHandoff::default();
    let attach_error =
        attach_loop_with_handoff(&mut attach_handoff, &pane, &paths, "pm-test", "pm-bot")
            .unwrap_err();
    assert!(format!("{attach_error:#}").contains("chat.json"));
    assert!(attach_handoff.events.is_empty());

    let req = ChatReq {
        session_paths: paths.clone(),
        root: dir.path().to_path_buf(),
        argv: vec!["agent".into()],
        label: "bot".into(),
        socket: "pm-test".into(),
        session: "pm-bot".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    };
    let mut chat_handoff = RecordingHandoff::default();
    let chat_error = chat_with_handoff(&mut chat_handoff, &pane, &req, || {}).unwrap_err();
    assert!(format!("{chat_error:#}").contains("chat.json"));
    assert!(chat_handoff.events.is_empty());

    let create_paths = ProjectPaths::for_session(dir.path(), "created");
    std::fs::create_dir_all(create_paths.daemon_dir()).unwrap();
    std::fs::create_dir(create_paths.chat_lock()).unwrap();
    let create_req = CreateChatReq {
        session_paths: create_paths.clone(),
        root: dir.path().to_path_buf(),
        argv: vec!["agent".into()],
        label: "created".into(),
        lease: acquire_free_lease(&create_paths.daemon_dir().join("driver.lock"))
            .expect("create lease should be free"),
        socket: "pm-test".into(),
        session: "pm-created".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    };
    let mut create_handoff = RecordingHandoff::default();
    let create_error =
        create_chat_with_handoff(&mut create_handoff, &pane, create_req, || {}).unwrap_err();
    assert!(format!("{create_error:#}").contains("chat.json"));
    assert!(create_handoff.events.is_empty());
}

#[test]
fn terminal_request_drain_handles_every_request_type() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let paths = ProjectPaths::for_session(&root, "bot");
    let pane = FakePane::default();
    let mut app = pane_app(&reg_path, pane.clone());

    app.pending_chat = Some(ChatReq {
        session_paths: paths.clone(),
        root: root.clone(),
        argv: vec!["agent".into()],
        label: "bot".into(),
        socket: app.socket.clone(),
        session: "pm-chat".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    });
    let create_paths = ProjectPaths::for_session(&root, "created");
    app.pending_create_chat = Some(CreateChatReq {
        session_paths: create_paths.clone(),
        root: root.clone(),
        argv: vec!["agent".into()],
        label: "created".into(),
        lease: acquire_free_lease(&create_paths.daemon_dir().join("driver.lock"))
            .expect("create lease should be free"),
        socket: app.socket.clone(),
        session: "pm-created".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv::default(),
    });
    app.pending_attach_loop = Some("pm-chat".into());
    app.pending_brief_edit = Some(BriefEdit {
        goal: "goal".into(),
        target: BriefEditTarget::CreateForm,
    });
    app.pending_directive_edit = Some(DirectiveEditReq {
        id: "bot".into(),
        directive: paths.directive(),
        current: "keep this".into(),
    });
    app.pending_send = Some(SendReq {
        target: SendTarget {
            id: "bot".into(),
            root,
            session: "pm-chat".into(),
            agent_loop: true,
            driven: false,
            in_chat: true,
        },
        seed: "message".into(),
        cursor: (0, 7),
    });

    let mut handoff = RecordingHandoff::default();
    drain_terminal_requests(&mut app, &mut handoff, "/bin/true");

    assert_eq!(
        handoff.events,
        [
            "suspend", "restore", "suspend", "restore", "suspend", "restore", "suspend", "restore",
            "suspend", "restore", "suspend", "restore",
        ]
    );
    assert_eq!(pane.launches().len(), 2);
    assert_eq!(
        pane.foreground_attaches(),
        ["pm-chat", "pm-created", "pm-chat"]
    );
    assert!(app.pending_chat.is_none());
    assert!(app.pending_create_chat.is_none());
    assert!(app.pending_attach_loop.is_none());
    assert!(app.pending_brief_edit.is_none());
    assert!(app.pending_directive_edit.is_none());
    assert!(app.pending_send.is_none());
}

#[test]
fn editor_temp_names_are_kind_specific_and_unique() {
    let send = editor_temp("send");
    let brief = editor_temp("brief");
    let send_name = send.file_name().unwrap().to_string_lossy();
    let brief_name = brief.file_name().unwrap().to_string_lossy();

    assert!(send_name.starts_with(&format!("pmtui-send-{}-", std::process::id())));
    assert!(send_name.ends_with(".md"));
    assert!(brief_name.starts_with(&format!("pmtui-brief-{}-", std::process::id())));
    assert_ne!(send, brief);
}

#[test]
fn message_editor_runs_between_terminal_handoffs_and_removes_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("send.md");
    let seed_copy = dir.path().join("seed.txt");
    let events = dir.path().join("events.txt");
    let mut handoff = RecordingHandoff::with_log(events.clone());

    let edited = edit_send_message_with_handoff(
        &mut handoff,
        "draft with trailing space   ",
        &tmp,
        "test editor",
        Box::new(|path| {
            std::fs::copy(path, &seed_copy)?;
            let mut event_log = std::fs::OpenOptions::new().append(true).open(&events)?;
            writeln!(event_log, "editor")?;
            std::fs::write(path, b"# heading\nsecond\x1bline\n")?;
            Ok(exit_status(0))
        }),
    )
    .unwrap();

    assert_eq!(edited.as_deref(), Some("# heading\nsecond line"));
    assert_eq!(
        std::fs::read_to_string(seed_copy).unwrap(),
        "draft with trailing space\n"
    );
    assert_eq!(
        std::fs::read_to_string(events).unwrap(),
        "suspend\neditor\nrestore\n"
    );
    assert!(!tmp.exists(), "successful editor temp must be removed");
}

#[test]
fn directive_and_brief_editors_preserve_text_and_treat_blank_as_keep() {
    let dir = tempfile::tempdir().unwrap();
    let directive_tmp = dir.path().join("directive.md");
    let mut directive_handoff = RecordingHandoff::default();
    let directive = edit_directive_with_handoff(
        &mut directive_handoff,
        "old directive",
        &directive_tmp,
        "test editor",
        Box::new(|path| {
            std::fs::write(path, "# policy\nkeep this\n")?;
            Ok(exit_status(0))
        }),
    )
    .unwrap();
    assert_eq!(directive.as_deref(), Some("# policy\nkeep this"));
    assert_eq!(directive_handoff.events, ["suspend", "restore"]);
    assert!(!directive_tmp.exists());

    let brief_tmp = dir.path().join("brief.md");
    let mut brief_handoff = RecordingHandoff::default();
    let brief = edit_brief_with_handoff(
        &mut brief_handoff,
        "old goal",
        &brief_tmp,
        "test editor",
        Box::new(|path| {
            std::fs::write(path, "  \n\t\n")?;
            Ok(exit_status(0))
        }),
    )
    .unwrap();
    assert_eq!(brief, None, "an empty save keeps the existing brief");
    assert_eq!(brief_handoff.events, ["suspend", "restore"]);
    assert!(!brief_tmp.exists());
}

#[test]
fn editor_exit_and_spawn_failures_restore_terminal_and_remove_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let failed_tmp = dir.path().join("failed.md");
    let mut failed_handoff = RecordingHandoff::default();
    let error = edit_brief_with_handoff(
        &mut failed_handoff,
        "goal",
        &failed_tmp,
        "failed editor",
        Box::new(|_| Ok(exit_status(17))),
    )
    .unwrap_err();
    assert!(error.to_string().contains("exit status: 17"), "{error:#}");
    assert_eq!(failed_handoff.events, ["suspend", "restore"]);
    assert!(!failed_tmp.exists());

    let missing_tmp = dir.path().join("missing.md");
    let missing_editor = dir.path().join("missing-editor");
    let mut missing_handoff = RecordingHandoff::default();
    let error = edit_directive_with_handoff(
        &mut missing_handoff,
        "directive",
        &missing_tmp,
        missing_editor.to_str().unwrap(),
        Box::new(|_| Err(std::io::Error::from(std::io::ErrorKind::NotFound).into())),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("could not launch editor"),
        "{error:#}"
    );
    assert_eq!(missing_handoff.events, ["suspend", "restore"]);
    assert!(!missing_tmp.exists());
}

#[test]
fn editor_handoff_failures_remove_temp_file_and_preserve_terminal_error() {
    let dir = tempfile::tempdir().unwrap();
    let editor_ran = Cell::new(false);

    let suspend_tmp = dir.path().join("suspend.md");
    let mut suspend_handoff = RecordingHandoff::failing_suspend();
    let error = edit_send_message_with_handoff(
        &mut suspend_handoff,
        "draft",
        &suspend_tmp,
        "test editor",
        Box::new(|path| {
            editor_ran.set(true);
            std::fs::write(path, "edited\n")?;
            Ok(exit_status(0))
        }),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("test suspend failure"),
        "{error:#}"
    );
    assert_eq!(suspend_handoff.events, ["suspend"]);
    assert!(!editor_ran.get(), "editor must not run after suspend fails");
    assert!(
        !suspend_tmp.exists(),
        "suspend failure must remove temp file"
    );

    let restore_tmp = dir.path().join("restore.md");
    let mut restore_handoff = RecordingHandoff::failing_restore();
    let error = edit_brief_with_handoff(
        &mut restore_handoff,
        "goal",
        &restore_tmp,
        "test editor",
        Box::new(|path| {
            editor_ran.set(true);
            std::fs::write(path, "edited\n")?;
            Ok(exit_status(0))
        }),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("test restore failure"),
        "{error:#}"
    );
    assert_eq!(restore_handoff.events, ["suspend", "restore"]);
    assert!(editor_ran.get(), "editor must run before restore fails");
    assert!(
        !restore_tmp.exists(),
        "restore failure must remove temp file"
    );
}

#[test]
fn editor_read_failure_removes_invalid_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("invalid-utf8.md");
    let mut handoff = RecordingHandoff::default();

    let error = edit_directive_with_handoff(
        &mut handoff,
        "directive",
        &tmp,
        "test editor",
        Box::new(|path| {
            std::fs::write(path, [0xff])?;
            Ok(exit_status(0))
        }),
    )
    .unwrap_err();

    assert!(
        error.to_string().contains("read directive temp"),
        "{error:#}"
    );
    assert_eq!(handoff.events, ["suspend", "restore"]);
    assert!(!tmp.exists(), "read failure must remove invalid temp file");
}

#[test]
fn editor_temp_write_failure_stops_before_terminal_handoff() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("missing-parent").join("send.md");
    let mut handoff = RecordingHandoff::default();
    let editor_ran = Cell::new(false);

    let error = edit_send_message_with_handoff(
        &mut handoff,
        "draft",
        &tmp,
        "test editor",
        Box::new(|_| {
            editor_ran.set(true);
            Ok(exit_status(0))
        }),
    )
    .unwrap_err();

    assert!(error.to_string().contains("write send temp"), "{error:#}");
    assert!(handoff.events.is_empty());
    assert!(!editor_ran.get());
}
