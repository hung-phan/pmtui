//! Every terminal the dashboard launches with `PMTUI_BIN` ships the pmtui-spawn skill into its
//! project root first: the canonical `.agents/skills/pmtui-spawn/SKILL.md` for both engines, plus
//! Claude's exact `.claude/skills/pmtui-spawn` alias. A launch that names no pmtui ships nothing,
//! and a failed install never blocks the launch.

use super::*;

use agent_manager::skills::{CLAUDE_SPAWN_SKILL_LINK_TARGET, SPAWN_SKILL_MD};

const PMTUI: &str = "/opt/am/pmtui";

/// Hands the terminal to nothing: these tests pin what lands on disk around the launch, not the
/// suspend/restore sequence `tests/session.rs` owns.
struct NoHandoff;

impl TerminalHandoff for NoHandoff {
    fn suspend(&mut self) -> Result<()> {
        Ok(())
    }

    fn restore(&mut self) -> Result<()> {
        Ok(())
    }
}

fn project_dir(dir: &Path) -> PathBuf {
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    project
}

/// Assert the spawn skill is installed in `root` exactly as `engine` discovers it.
fn assert_spawn_skill(root: &Path, engine: Engine) {
    let paths = ProjectPaths::new(root);
    assert_eq!(
        std::fs::read_to_string(paths.canonical_spawn_skill_file())
            .ok()
            .as_deref(),
        Some(SPAWN_SKILL_MD),
        "{engine:?}: canonical spawn skill"
    );
    match engine {
        Engine::Claude => {
            assert_eq!(
                std::fs::read_link(paths.claude_spawn_skill_dir()).ok(),
                Some(PathBuf::from(CLAUDE_SPAWN_SKILL_LINK_TARGET)),
                "Claude reaches it through its exact per-skill alias"
            );
            assert_eq!(
                std::fs::read_to_string(paths.claude_spawn_skill_dir().join("SKILL.md")).unwrap(),
                SPAWN_SKILL_MD
            );
        }
        Engine::Codex => assert!(
            !paths.claude_root_dir().exists(),
            "Codex discovers .agents/skills natively; no Claude directory appears"
        ),
    }
    assert!(
        !root.join("AGENTS.md").exists(),
        "the installer never creates the user's AGENTS.md"
    );
}

fn assert_no_spawn_skill(root: &Path) {
    let paths = ProjectPaths::new(root);
    assert!(!paths.canonical_spawn_skill_file().exists());
    assert!(std::fs::symlink_metadata(paths.claude_spawn_skill_dir()).is_err());
}

#[test]
fn standard_create_installs_the_spawn_skill_and_claude_link() {
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let project = project_dir(dir.path());
        let mut app = creating_loop_app(&reg_path, &project, engine, Tier::Standard, "goal", 300);
        let pane = FakePane::default();
        app.agent_tmux = Box::new(pane.clone());
        app.pmtui_bin = Some(PathBuf::from(PMTUI));

        app.submit_create();

        assert_eq!(pane.launches().len(), 1, "{engine:?}: {}", app.status);
        assert_spawn_skill(&project, engine);
    }
}

#[test]
fn no_pmtui_bin_means_no_spawn_skill() {
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let project = project_dir(dir.path());
        let mut app = creating_loop_app(&reg_path, &project, engine, Tier::Standard, "goal", 300);
        let pane = FakePane::default();
        app.agent_tmux = Box::new(pane.clone());
        assert!(app.pmtui_bin.is_none());

        app.submit_create();

        assert_eq!(pane.launches().len(), 1, "{engine:?}: {}", app.status);
        assert_no_spawn_skill(&project);
    }
}

#[test]
fn resume_and_restart_ship_the_skill_before_the_terminal_starts() {
    // `start_undriven_session` is the launch behind Enter-after-pause and `r`. The install runs
    // before the launch, so even a launch tmux refuses leaves the skill for the retry.
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let pane = FakePane::default();
        let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
        app.pmtui_bin = Some(PathBuf::from(PMTUI));
        let session = session_name("bot", dir.path());
        pane.arm_launch_error(
            &session,
            tmux::LaunchError::NotOnPath(engine.label().into()),
        );

        let outcome = app.start_undriven_session(
            "bot",
            dir.path(),
            engine,
            Some("conv-abc".into()),
            None,
            None,
        );

        assert!(
            matches!(outcome, StandardStart::NotStarted(_)),
            "{engine:?}: the armed launch error must reach the caller"
        );
        assert_spawn_skill(dir.path(), engine);
    }
}

#[test]
fn a_failed_install_never_blocks_a_standard_launch() {
    let dir = tempfile::tempdir().unwrap();
    let pane = FakePane::default();
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
    app.pmtui_bin = Some(PathBuf::from(PMTUI));
    // A regular file where the project's `.agents` directory belongs.
    std::fs::write(dir.path().join(".agents"), "keep").unwrap();

    let outcome = app.start_undriven_session("bot", dir.path(), Engine::Codex, None, None, None);

    assert!(matches!(outcome, StandardStart::Running), "{}", app.status);
    assert_eq!(pane.launches().len(), 1);
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".agents")).unwrap(),
        "keep",
        "a conflicting user path is never replaced"
    );
}

#[test]
fn chat_launches_ship_the_skill_before_the_terminal_starts() {
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::for_session(dir.path(), "bot");
        let req = ChatReq {
            session_paths: paths.clone(),
            root: dir.path().to_path_buf(),
            argv: vec!["agent".into()],
            label: "bot".into(),
            socket: "pm-test".into(),
            session: "pm-bot".into(),
            engine,
            env: tmux::ManagedEnv {
                session_id: "bot".into(),
                state_dir: paths.state_dir(),
                pmtui_bin: Some(PathBuf::from(PMTUI)),
            },
        };
        // The launch fails, so the skill on disk can only have come from before it.
        let pane = FakePane::default().with(|inner| inner.fail_launch = true);

        chat_with_handoff(&mut NoHandoff, &pane, &req, || {}).unwrap_err();

        assert_spawn_skill(dir.path(), engine);
    }

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "created");
    let req = CreateChatReq {
        session_paths: paths.clone(),
        root: dir.path().to_path_buf(),
        argv: vec!["agent".into()],
        label: "created".into(),
        lease: acquire_free_lease(&paths.daemon_dir().join("driver.lock")).expect("driver lock"),
        socket: "pm-test".into(),
        session: "pm-created".into(),
        engine: Engine::Claude,
        env: tmux::ManagedEnv {
            session_id: "created".into(),
            state_dir: paths.state_dir(),
            pmtui_bin: Some(PathBuf::from(PMTUI)),
        },
    };
    let pane = FakePane::default().with(|inner| inner.fail_launch = true);

    create_chat_with_handoff(&mut NoHandoff, &pane, req, || {}).unwrap_err();

    assert_spawn_skill(dir.path(), Engine::Claude);
}

#[test]
fn a_chat_launch_that_names_no_pmtui_ships_no_skill() {
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

    chat_with_handoff(&mut NoHandoff, &FakePane::default(), &req, || {}).unwrap();

    assert_no_spawn_skill(dir.path());
}

#[test]
fn enter_queues_each_chat_launch_with_its_rows_engine() {
    // First Enter on a Standard row whose immediate launch failed: Claude creates its
    // conversation (`CreateChatReq`), Codex opens a fresh chat (`ChatReq`). Each request carries
    // the row's engine so the launch installs the skill in that engine's discovery form.
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let project = project_dir(dir.path());
        let mut app =
            creating_loop_app(&reg_path, &project, engine, Tier::Standard, "message", 300);
        app.agent_tmux = Box::new(FakePane::default().with(|inner| inner.fail_launch = true));
        app.submit_create();
        app.agent_tmux = Box::new(FakePane::default());
        wait_until_free(
            &ProjectPaths::for_session(&project, "project")
                .daemon_dir()
                .join("driver.lock"),
        );

        handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

        let queued = match engine {
            Engine::Claude => app.pending_create_chat.as_ref().map(|req| req.engine),
            Engine::Codex => app.pending_chat.as_ref().map(|req| req.engine),
        };
        assert_eq!(queued, Some(engine), "{engine:?}: {}", app.status);
    }

    // Enter on a parked Codex row with a known conversation resumes it (`ChatReq`).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    Registry::update(&reg_path, |registry| {
        registry.projects[0].engine = Some(Engine::Codex);
    })
    .unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.engine = Engine::Codex;
    ledger.conversation_id = Some("conv-xyz".into());
    ledger.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&paths, &ledger).unwrap();
    let mut app = loop_app(&reg_path);

    app.request_attach();

    assert_eq!(
        app.pending_chat.as_ref().map(|req| req.engine),
        Some(Engine::Codex),
        "{}",
        app.status
    );
}
