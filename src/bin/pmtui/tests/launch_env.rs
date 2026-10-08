//! Every terminal the dashboard launches tells the agent inside it which managed session it
//! is in: the stable id, that session's state directory, and this pmtui (`ManagedEnv`). A
//! typed launch failure still reaches the human in the driver's own words.

use super::*;

const PMTUI: &str = "/opt/am/pmtui";

fn identity(id: &str, root: &Path, bin: Option<&str>) -> tmux::ManagedEnv {
    tmux::ManagedEnv {
        session_id: id.into(),
        state_dir: ProjectPaths::for_session(root, id).state_dir(),
        pmtui_bin: bin.map(PathBuf::from),
    }
}

#[test]
fn managed_env_names_the_session_its_state_dir_and_this_pmtui() {
    let mut app = app_with(vec![], UiMode::Normal);
    let root = Path::new("/work/service");
    assert_eq!(app.managed_env("bot", root), identity("bot", root, None));
    app.pmtui_bin = Some(PathBuf::from(PMTUI));
    assert_eq!(
        app.managed_env("bot", root),
        identity("bot", root, Some(PMTUI))
    );
    let state_dir = app.managed_env("bot", root).state_dir;
    assert_eq!(
        state_dir.parent(),
        Some(root.join(".project-state/sessions").as_path()),
        "the state dir is the session's own subtree, never the shared project root"
    );
    assert!(
        state_dir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("bot-")),
        "{state_dir:?}"
    );
}

#[test]
fn a_standard_create_launches_its_terminal_with_the_new_sessions_identity() {
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut app = creating_loop_app(&reg_path, &project, engine, Tier::Standard, "goal", 300);
        let pane = FakePane::default();
        app.agent_tmux = Box::new(pane.clone());
        app.pmtui_bin = Some(PathBuf::from(PMTUI));

        app.submit_create();

        let created = Registry::load(&reg_path)
            .unwrap()
            .projects
            .into_iter()
            .find(|entry| entry.root == project)
            .expect("new session registered");
        assert_eq!(
            pane.launched_env(),
            vec![(
                session_name(&created.id, &created.root),
                identity(&created.id, &created.root, Some(PMTUI)),
            )],
            "{engine:?}"
        );
    }
}

#[test]
fn a_launch_refused_before_tmux_reaches_the_human_in_the_drivers_words() {
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let pane = FakePane::default();
        let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
        let session = session_name("bot", dir.path());
        let bin = engine.label().to_string();
        pane.arm_launch_error(&session, tmux::LaunchError::NotOnPath(bin.clone()));

        let outcome = app.start_undriven_session("bot", dir.path(), engine, None, None, None);

        let StandardStart::NotStarted(why) = outcome else {
            panic!("{engine:?}: a refused launch must not report a running agent");
        };
        assert_eq!(
            why,
            format!("{bin:?} was not found on PATH — is it installed?")
        );
        assert_eq!(
            pane.launched_env(),
            vec![(session, identity("bot", dir.path(), None))],
            "{engine:?}"
        );
    }
}

#[test]
fn enter_queues_every_chat_launch_with_the_sessions_identity() {
    // First Enter on a Standard row whose immediate launch failed: Claude creates its
    // conversation (`CreateChatReq`), Codex opens a fresh chat (`ChatReq`).
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut app =
            creating_loop_app(&reg_path, &project, engine, Tier::Standard, "message", 300);
        app.agent_tmux = Box::new(FakePane::default().with(|inner| inner.fail_launch = true));
        app.pmtui_bin = Some(PathBuf::from(PMTUI));
        app.submit_create();
        app.agent_tmux = Box::new(FakePane::default());
        wait_until_free(
            &ProjectPaths::for_session(&project, "project")
                .daemon_dir()
                .join("driver.lock"),
        );

        handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

        let env = match engine {
            Engine::Claude => app.pending_create_chat.as_ref().map(|req| req.env.clone()),
            Engine::Codex => app.pending_chat.as_ref().map(|req| req.env.clone()),
        };
        assert_eq!(
            env,
            Some(identity("project", &project, Some(PMTUI))),
            "{engine:?}: {}",
            app.status
        );
    }

    // Enter on a parked row with a known conversation resumes it (`ChatReq`).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.conversation_id = Some("conv-xyz".into());
    ledger.run = job::JobRun::Monitoring { until: 9999 };
    job::save(&paths, &ledger).unwrap();
    let mut app = loop_app(&reg_path);
    app.pmtui_bin = Some(PathBuf::from(PMTUI));

    app.request_attach();

    assert_eq!(
        app.pending_chat.as_ref().map(|req| req.env.clone()),
        Some(identity("bot", &root, Some(PMTUI)))
    );
}
