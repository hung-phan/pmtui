//! Submitting the create form: what lands on disk for an agent-loop session — the
//! registry entry, config, brief and idle ledger — the folder several sessions may
//! share, and the roots and missing goals a submit refuses.

use super::*;

#[test]
fn project_id_base_uses_the_leaf_or_a_root_fallback() {
    assert_eq!(project_id_base(Path::new("/workplace/project")), "project");
    assert_eq!(project_id_base(Path::new("/")), "session");
}

#[test]
fn create_entrypoints_are_inert_outside_their_modes() {
    let mut app = app_with(vec![], UiMode::Normal);
    let before = app.status.clone();

    app.submit_create();
    app.confirm_create_dir();
    app.cancel_create_dir();

    assert_eq!(app.status, before);
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn submit_create_seeds_agent_loop_at_selected_tier() {
    // The Autonomy dial IS the tier: whatever level the form carries at submit is
    // exactly what gets seeded into the per-session config. Every level seeds the
    // SAME Mode::AgentLoop session (empty coordinator_cmd, brief with the goal, an
    // Idle ledger, no attach) — only config.autonomy differs. Parametrized across
    // both levels to prove the selected dial is what lands.
    for (engine, tier) in [
        (Engine::Claude, Tier::Standard),
        (Engine::Codex, Tier::Autopilot),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let proj = std::fs::canonicalize({
            let p = dir.path().join("proj");
            std::fs::create_dir_all(&p).unwrap();
            p
        })
        .unwrap();

        let mut app = creating_loop_app(
            &reg_path,
            &proj,
            engine,
            tier,
            "keep the build green",
            job_engine::DEFAULT_CADENCE_S,
        );
        app.submit_create();

        let reg = Registry::load(&reg_path).unwrap();
        let e = reg
            .projects
            .iter()
            .find(|p| p.root == proj)
            .expect("loop entry created");
        assert_eq!(e.mode, Mode::AgentLoop);
        assert_eq!(e.engine, Some(engine));
        // THE CADENCE IS TIER-DEPENDENT since m40: Autopilot records the number the form
        // asked for; Standard records NONE, because the form does not ask and a heartbeat that
        // does not run should not carry a rhythm nobody chose. `m`'s prompt fills it in when
        // autopilot turns the heartbeat on.
        match tier {
            Tier::Autopilot => {
                assert_eq!(e.cadence_s, Some(job_engine::DEFAULT_CADENCE_S))
            }
            Tier::Standard => {
                assert_eq!(e.cadence_s, None, "a Standard session records no cadence")
            }
        }
        // A STANDARD claude create now STARTS the agent, so it pins and RECORDS the conversation
        // id right here — that seed is what lets autopilot later resume this conversation instead of
        // minting a second one (see `a_standard_create_starts_the_agent_so_enter_only_attaches`).
        // Every other combination still leaves the id to the first wake: codex has no
        // caller-chosen id, and an Autopilot row is pmd's to launch.
        let starts_now = engine == Engine::Claude && tier == Tier::Standard;
        assert_eq!(
            e.conversation_id.is_some(),
            starts_now,
            "{engine:?}/{tier:?}: conversation_id = {:?}",
            e.conversation_id
        );
        assert_eq!(
            e.initial_prompt.as_deref(),
            (tier == Tier::Standard).then_some("keep the build green")
        );
        assert!(e.task_title.is_none(), "Session UI creation is not a Task");

        let session_paths = ProjectPaths::for_session(&proj, &e.id);
        let cfg: Config = state::read_json(&session_paths.config()).unwrap();
        assert_eq!(
            cfg.autonomy, tier,
            "the selected Autonomy dial is what gets seeded"
        );
        let brief = std::fs::read_to_string(session_paths.brief()).unwrap();
        if tier == Tier::Autopilot {
            assert!(brief.contains("keep the build green"), "{brief}");
        } else {
            assert!(
                brief.is_empty(),
                "Standard Message is not an Autopilot goal: {brief}"
            );
        }
        let control = state::read_control(&session_paths).unwrap();
        assert_eq!(control.human_cadence_s, e.cadence_s);
        assert!(
            job::load(&session_paths).unwrap().is_none(),
            "pmd is the sole ledger writer"
        );

        assert!(matches!(app.mode, UiMode::Normal));
    }
}

#[test]
fn task_board_create_prefills_context_persists_title_and_returns_to_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_agent_loop(dir.path(), "bot");
    Registry::update(&registry, |registry| {
        registry.projects[0].engine = Some(Engine::Codex);
        registry.projects[0].worker_model = Some("gpt-task".into());
    })
    .unwrap();
    let pane = FakePane::default();
    let mut selected = agent_loop_view("bot");
    selected.engine = Some(Engine::Codex);
    let mut app = app_with_driver(vec![selected], UiMode::Board, Box::new(pane));
    app.registry_path = registry.clone();
    app.model_catalog.insert(Engine::Codex, Vec::new());
    app.model_catalog.insert(Engine::Claude, Vec::new());

    app.begin_task_create();

    let UiMode::Creating(form) = &mut app.mode else {
        panic!("Task Board did not open create");
    };
    assert!(form.task_mode);
    assert_eq!(form.dir.as_str(), root.display().to_string());
    assert_eq!(form.engine, Engine::Codex);
    assert_eq!(form.worker_model.as_deref(), Some("gpt-task"));
    form.goal = Field::from("Build the release dashboard\nwith exact CI status");

    app.submit_create();

    let registry = Registry::load(&registry).unwrap();
    let task = registry
        .projects
        .iter()
        .find(|entry| entry.id != "bot")
        .expect("task-backed managed session");
    assert_eq!(
        task.task_title.as_deref(),
        Some("Build the release dashboard")
    );
    assert_eq!(
        task.initial_prompt.as_deref(),
        Some("Build the release dashboard\nwith exact CI status")
    );
    assert!(matches!(app.mode, UiMode::Board));
    assert!(app.board_detail_open);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some(task.id.as_str())
    );
}

#[test]
fn failed_task_create_keeps_the_completed_form_open() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut app = app_with(Vec::new(), UiMode::Board);
    app.registry_path = dir.path().to_path_buf();
    app.model_catalog.insert(Engine::Claude, Vec::new());
    app.begin_task_create();
    let UiMode::Creating(form) = &mut app.mode else {
        panic!("Task create did not open");
    };
    form.dir = Field::from(project.display().to_string());
    form.goal = Field::from("Keep this task draft");

    app.submit_create();

    let UiMode::Creating(form) = &app.mode else {
        panic!("failed Task create discarded its form");
    };
    assert_eq!(form.goal.as_str(), "Keep this task draft");
    assert!(app.return_to_board_after_create);
    assert!(app.status.contains("not overwriting"));
}

#[test]
fn standard_initial_message_is_sent_once_through_the_interactive_engine() {
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let project = std::fs::canonicalize({
            let path = dir.path().join("project");
            std::fs::create_dir_all(&path).unwrap();
            path
        })
        .unwrap();
        let pane = FakePane::default();
        let mut app = creating_loop_app(
            &reg_path,
            &project,
            engine,
            Tier::Standard,
            "inspect the release",
            job_engine::DEFAULT_CADENCE_S,
        );
        app.agent_tmux = Box::new(pane.clone());

        app.submit_create();

        let entry = Registry::load(&reg_path)
            .unwrap()
            .projects
            .into_iter()
            .next()
            .expect("session entry");
        assert_eq!(entry.initial_prompt.as_deref(), Some("inspect the release"));
        let launches = pane.launches();
        assert_eq!(launches.len(), 1);
        let argv = &launches[0].2;
        assert!(
            argv.windows(2)
                .any(|pair| pair[0] == "--" && pair[1] == "inspect the release"),
            "{engine:?} initial Message missing: {argv:?}"
        );
        assert!(
            !argv.iter().any(|arg| arg == "-p" || arg == "exec"),
            "Message must use the normal interactive engine: {argv:?}"
        );
    }
}

#[test]
fn empty_standard_message_preserves_the_existing_launch_shape() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let project = std::fs::canonicalize({
        let path = dir.path().join("project");
        std::fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    let pane = FakePane::default();
    let mut app = creating_loop_app(
        &reg_path,
        &project,
        Engine::Claude,
        Tier::Standard,
        "",
        job_engine::DEFAULT_CADENCE_S,
    );
    app.agent_tmux = Box::new(pane.clone());

    app.submit_create();

    let entry = Registry::load(&reg_path).unwrap().projects.remove(0);
    assert!(entry.initial_prompt.is_none());
    let argv = &pane.launches()[0].2;
    assert_ne!(argv.last().map(String::as_str), Some("--"));
}

fn standard_form_in(reg_path: &Path, project: &Path, engine: Engine, message: &str) -> App {
    creating_loop_app(
        reg_path,
        project,
        engine,
        Tier::Standard,
        message,
        job_engine::DEFAULT_CADENCE_S,
    )
}

fn creating_form(app: &App) -> CreateForm {
    match &app.mode {
        UiMode::Creating(form) => form.clone(),
        other => panic!("not on the create form: {other:?}"),
    }
}

#[test]
fn standard_message_is_bounded_by_the_quoted_launch_command_tmux_accepts() {
    // The Message rides the launch argv into ONE `tmux new-session` command, so the limit is
    // that command's size after shell quoting, not the Message's own byte count.
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let project = std::fs::canonicalize({
            let path = dir.path().join("project");
            std::fs::create_dir_all(&path).unwrap();
            path
        })
        .unwrap();
        let probe = standard_form_in(&reg_path, &project, engine, "x");
        let base = initial_launch_bytes(&creating_form(&probe), &project, &Registry::default())
            .expect("a Standard Message is bounded");
        let fits = "x".repeat(tmux::LAUNCH_COMMAND_MAX_BYTES - base + 1);

        // One byte over: refused on the form, with nothing written or launched.
        let pane = FakePane::default();
        let mut app = standard_form_in(&reg_path, &project, engine, &format!("{fits}x"));
        app.agent_tmux = Box::new(pane.clone());
        app.submit_create();
        assert!(
            app.status.starts_with("initial message too long") && app.status.contains("1 byte"),
            "{engine:?}: {}",
            app.status
        );
        assert!(matches!(app.mode, UiMode::Creating(_)));
        assert!(!reg_path.exists() || Registry::load(&reg_path).unwrap().projects.is_empty());
        assert!(!project.join(state::STATE_DIR).exists());
        assert!(pane.launches().is_empty());

        // Quoting counts: apostrophes cost four bytes each once quoted.
        let quotes = "'".repeat((tmux::LAUNCH_COMMAND_MAX_BYTES - base + 1) / 4 + 1);
        assert!(quotes.len() < tmux::LAUNCH_COMMAND_MAX_BYTES / 3);
        let mut app = standard_form_in(&reg_path, &project, engine, &quotes);
        app.submit_create();
        assert!(
            app.status.starts_with("initial message too long"),
            "{}",
            app.status
        );

        // Exactly at the budget: created, and the real launch is exactly that long.
        let mut app = standard_form_in(&reg_path, &project, engine, &fits);
        app.agent_tmux = Box::new(pane.clone());
        app.submit_create();
        assert_eq!(Registry::load(&reg_path).unwrap().projects.len(), 1);
        let launches = pane.launches();
        assert_eq!(launches.len(), 1, "{engine:?}: {}", app.status);
        assert_eq!(
            tmux::launch_command(&launches[0].2).len(),
            tmux::LAUNCH_COMMAND_MAX_BYTES,
            "{engine:?}: the form's estimate must be the launch itself"
        );
    }
}

#[test]
fn launch_budget_counts_the_id_the_create_will_actually_reserve() {
    // A second session in the same folder gets a suffixed id, and the id is part of the
    // launch command, so the estimate must use it rather than the bare folder name.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let project = std::fs::canonicalize({
        let path = dir.path().join("project");
        std::fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    let mut first = standard_form_in(&reg_path, &project, Engine::Claude, "first");
    first.submit_create();
    let registry = Registry::load(&reg_path).unwrap();
    assert_eq!(registry.projects[0].id, "project");

    let probe = standard_form_in(&reg_path, &project, Engine::Claude, "x");
    let base = initial_launch_bytes(&creating_form(&probe), &project, &registry).unwrap();
    let fits = "x".repeat(tmux::LAUNCH_COMMAND_MAX_BYTES - base + 1);
    let pane = FakePane::default();
    let mut app = standard_form_in(&reg_path, &project, Engine::Claude, &fits);
    app.agent_tmux = Box::new(pane.clone());
    app.submit_create();
    let registry = Registry::load(&reg_path).unwrap();
    assert_eq!(registry.projects[1].id, "project-2");
    assert_eq!(
        tmux::launch_command(&pane.launches()[0].2).len(),
        tmux::LAUNCH_COMMAND_MAX_BYTES
    );
}

#[test]
fn a_message_too_long_for_any_launch_is_refused_before_a_new_folder_is_offered() {
    // The quoted Message alone already exceeds what tmux accepts, so no directory can make it
    // launch: refuse on the form rather than create a folder the session will never use.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let project = dir.path().join("fresh-project");
    let message = "x".repeat(tmux::LAUNCH_COMMAND_MAX_BYTES);
    let mut app = standard_form_in(&reg_path, &project, Engine::Claude, &message);

    app.submit_create();

    assert!(
        app.status.starts_with("initial message too long")
            && app.status.contains("shorten it by at least"),
        "{}",
        app.status
    );
    assert_eq!(creating_form(&app).goal.as_str(), message);
    assert!(!project.exists(), "no folder may be made for this Message");
    assert!(!reg_path.exists() || Registry::load(&reg_path).unwrap().projects.is_empty());
}

#[test]
fn an_unlaunchable_message_returns_to_the_form_after_a_new_folder_is_made() {
    // Short enough to pass the directory-free bound, too long once the launch argv around it
    // (which embeds the session's state paths) is counted in the folder it would run in.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let project = dir.path().join("fresh-project");
    let message = "x".repeat(tmux::LAUNCH_COMMAND_MAX_BYTES - 16);
    let mut app = standard_form_in(&reg_path, &project, Engine::Claude, &message);
    app.submit_create();
    assert!(
        matches!(app.mode, UiMode::ConfirmCreateDir { .. }),
        "{}",
        app.status
    );

    app.confirm_create_dir();

    assert!(
        app.status.starts_with("initial message too long"),
        "{}",
        app.status
    );
    let form = creating_form(&app);
    assert_eq!(form.goal.as_str(), message);
    assert!(!reg_path.exists() || Registry::load(&reg_path).unwrap().projects.is_empty());
}

#[test]
fn only_a_standard_message_is_bounded_by_the_launch_command() {
    let project = Path::new("/tmp/project");
    let mut form = CreateForm::new();
    form.goal = Field::from("   ");
    assert_eq!(
        initial_launch_bytes(&form, project, &Registry::default()),
        None
    );
    form.goal = Field::from("x".repeat(tmux::LAUNCH_COMMAND_MAX_BYTES * 2).as_str());
    form.tier = Tier::Autopilot;
    assert_eq!(
        initial_launch_bytes(&form, project, &Registry::default()),
        None,
        "an Autopilot goal goes to brief.md, not the launch argv"
    );
}

#[test]
fn submit_create_selects_the_new_session_and_resets_its_preview_scroll() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let existing_root = std::fs::canonicalize({
        let path = dir.path().join("existing");
        std::fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    Registry {
        projects: vec![ProjectEntry {
            id: "existing".into(),
            display_name: None,
            root: existing_root,
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
        }],
    }
    .save(&reg_path)
    .unwrap();

    let project = std::fs::canonicalize({
        let path = dir.path().join("new-session");
        std::fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    let mut app = creating_loop_app(
        &reg_path,
        &project,
        Engine::Claude,
        Tier::Standard,
        "",
        job_engine::DEFAULT_CADENCE_S,
    );
    app.projects = vec![view("existing", Posture::Fresh, vec![])];
    app.detail_scroll = 42;
    app.pending_first_chat = Some("existing".into());
    app.armed_drains_left = Some(3);

    app.submit_create();

    let created = Registry::load(&reg_path)
        .unwrap()
        .projects
        .into_iter()
        .find(|entry| entry.root == project)
        .expect("new session registered");
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some(created.id.as_str()),
        "the preview should switch to the session that was just created"
    );
    assert_eq!(
        app.detail_scroll, 0,
        "a new preview should start at the live tail"
    );
    assert!(
        app.pending_first_chat.is_none() && app.armed_drains_left.is_none(),
        "selecting the new row must disarm the previous session's pending open"
    );
}

#[test]
fn standard_create_reports_when_the_agent_cannot_launch() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut app = creating_loop_app(
        &reg_path,
        &project,
        Engine::Claude,
        Tier::Standard,
        "goal",
        300,
    );
    let pane = FakePane::default().with(|inner| inner.fail_launch = true);
    app.agent_tmux = Box::new(pane.clone());

    app.submit_create();

    assert!(
        app.status.contains("not started") && app.status.contains("new-session failed"),
        "{}",
        app.status
    );
    assert_eq!(pane.launches().len(), 1);
    let registry = Registry::load(&reg_path).unwrap();
    assert_eq!(registry.projects.len(), 1, "the session itself was created");
    assert!(
        registry.projects[0].conversation_id.is_none(),
        "a failed launch must not seed a conversation id"
    );
}

#[test]
fn successful_launch_with_seed_failure_is_running_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let pane = FakePane::default();
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));

    let outcome = app.start_undriven_session(
        "bot",
        dir.path(),
        Engine::Claude,
        None,
        None,
        Some("one shot"),
    );

    let StandardStart::RunningWithWarning(warning) = outcome else {
        panic!("a successful launch plus seed failure must remain running");
    };
    assert!(warning.contains("unrecorded"));
    assert_eq!(pane.launches().len(), 1);
}

#[test]
fn standard_start_status_tracks_retry_eligibility_by_launch_truth() {
    let mut app = app_with(vec![], UiMode::Normal);
    app.initial_message_retries
        .insert("bot".into(), "old".into());
    let running = app.standard_start_status(
        "bot",
        "created bot",
        Some("new".into()),
        StandardStart::Running,
    );
    assert!(running.contains("running"));
    assert!(!app.initial_message_retries.contains_key("bot"));

    app.initial_message_retries
        .insert("bot".into(), "old".into());
    let warning = app.standard_start_status(
        "bot",
        "created bot",
        Some("new".into()),
        StandardStart::RunningWithWarning("conversation id unrecorded".into()),
    );
    assert!(warning.contains("conversation id unrecorded"));
    assert!(!app.initial_message_retries.contains_key("bot"));

    let failed = app.standard_start_status(
        "bot",
        "created bot",
        Some("retry me".into()),
        StandardStart::NotStarted("tmux unavailable".into()),
    );
    assert!(failed.contains("not started"));
    assert_eq!(
        app.initial_message_retries.get("bot").map(String::as_str),
        Some("retry me")
    );
}

#[test]
fn lifecycle_start_reports_running_with_warning_as_started() {
    let dir = tempfile::tempdir().unwrap();
    let pane = FakePane::default();
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane));
    app.initial_message_retries
        .insert("bot".into(), "must not replay".into());

    let status = app
        .start_after_lifecycle_key("bot", dir.path(), Engine::Claude, Mode::AgentLoop, None)
        .expect("Standard lifecycle start is handled by pmtui");

    assert!(status.contains("started it"));
    assert!(status.contains("unrecorded"));
    assert!(!status.contains("could not start"));
    assert!(!app.initial_message_retries.contains_key("bot"));
}

#[test]
fn a_paused_codex_row_resumes_the_conversation_the_engine_reported() {
    // The pause→Enter path (and `r`) built its resume id from the ledger and the registry ONLY.
    // Codex has no caller-chosen id, so on a standard codex row BOTH are empty and
    // `start_undriven_session` read that as "open a fresh chat" — the human pressed Enter on
    // their paused session and got an EMPTY one. The engine's own hook file is the third source
    // and the only one codex ever populates.
    const REPORTED: &str = "01a121e0-4863-73c1-aed7-92dd66c2f0c6";
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let paths = ProjectPaths::for_session(root, "bot");
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();
    std::fs::write(paths.codex_conversation_id(), REPORTED).unwrap();
    let pane = FakePane::default();
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));

    let status = app
        .start_after_lifecycle_key("bot", root, Engine::Codex, Mode::AgentLoop, None)
        .expect("Standard lifecycle start is handled by pmtui");

    assert!(status.contains("started it"), "{status}");
    let launches = pane.launches();
    assert_eq!(launches.len(), 1, "one terminal: {launches:?}");
    let argv = &launches[0].2;
    assert!(
        argv.windows(2)
            .any(|w| w == ["resume".to_string(), REPORTED.to_string()]),
        "the paused conversation must be RESUMED by the id codex reported: {argv:?}"
    );
}

/// A failed create has just dropped this session's `driver.lock`, and Enter's fork fence takes it
/// with ONE `try_acquire`. Wait out a sibling test's fork→exec window first (see
/// [`wait_until_free`]) so a transiently inherited fd cannot read as pmd holding the session.
fn wait_for_the_create_lease(project: &Path) {
    wait_until_free(
        &ProjectPaths::for_session(project, "project")
            .daemon_dir()
            .join("driver.lock"),
    );
}

#[test]
fn enter_retry_keeps_the_initial_message_for_the_first_successful_launch() {
    for engine in [Engine::Claude, Engine::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut app = creating_loop_app(
            &reg_path,
            &project,
            engine,
            Tier::Standard,
            "retry this message",
            300,
        );
        app.agent_tmux = Box::new(FakePane::default().with(|inner| inner.fail_launch = true));
        app.submit_create();
        assert!(
            app.status.contains("not started"),
            "{engine:?}: {}",
            app.status
        );

        app.agent_tmux = Box::new(FakePane::default());
        wait_for_the_create_lease(&project);
        handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let argv = match engine {
            Engine::Claude => app
                .pending_create_chat
                .as_ref()
                .map(|request| request.argv.clone()),
            Engine::Codex => app
                .pending_chat
                .as_ref()
                .map(|request| request.argv.clone()),
        }
        .expect("fresh retry queued");
        assert_eq!(
            argv.get(argv.len().saturating_sub(2)).map(String::as_str),
            Some("--"),
            "{engine:?}: retry is not marked as an initial positional"
        );
        assert_eq!(
            argv.last().map(String::as_str),
            Some("retry this message"),
            "{engine:?}: initial Message was lost on retry"
        );
        match engine {
            Engine::Claude => drain_pending_create_chat(&mut app, |_| Ok(true)),
            Engine::Codex => drain_pending_chat(&mut app, |_| Ok(true)),
        }
        assert!(
            !app.initial_message_retries.contains_key("project"),
            "{engine:?}: a successful retry stayed replayable"
        );
    }
}

#[test]
fn successful_fresh_codex_launch_never_replays_initial_message_on_later_enter() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut app = creating_loop_app(
        &reg_path,
        &project,
        Engine::Codex,
        Tier::Standard,
        "one shot message",
        300,
    );
    app.agent_tmux = Box::new(FakePane::default());
    app.submit_create();
    assert!(app.initial_message_retries.is_empty());

    // Simulate that successful fresh Codex terminal ending later. Codex still has no
    // caller-known conversation id, so Enter creates another fresh terminal, but the
    // launch-only Message must not follow it.
    app.agent_tmux = Box::new(FakePane::default());
    wait_for_the_create_lease(&project);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let argv = &app.pending_chat.as_ref().expect("fresh codex retry").argv;
    assert!(!argv.iter().any(|arg| arg == "one shot message"));
    assert!(!argv.iter().any(|arg| arg == "--"));
}

#[test]
fn repeated_failed_claude_retry_remains_the_same_fresh_initial_launch() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut app = creating_loop_app(
        &reg_path,
        &project,
        Engine::Claude,
        Tier::Standard,
        "retry until first launch",
        300,
    );
    app.agent_tmux = Box::new(FakePane::default().with(|inner| inner.fail_launch = true));
    app.submit_create();

    app.agent_tmux = Box::new(FakePane::default());
    wait_for_the_create_lease(&project);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.pending_create_chat.is_some(), "{}", app.status);
    drain_pending_create_chat(&mut app, |_| anyhow::bail!("launch failed again"));
    assert!(app.initial_message_retries.contains_key("project"));

    wait_for_the_create_lease(&project);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let argv = &app.pending_chat.as_ref().expect("second retry queued").argv;
    assert!(argv.iter().any(|arg| arg == "--session-id"));
    assert!(!argv.iter().any(|arg| arg == "--resume"));
    assert_eq!(
        argv.last().map(String::as_str),
        Some("retry until first launch")
    );
}

#[test]
fn create_reports_session_seed_failure_without_registering() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let project = dir.path().join("read-only-project");
    std::fs::create_dir_all(&project).unwrap();
    let original = std::fs::metadata(&project).unwrap().permissions();
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut app = creating_loop_app(
        &reg_path,
        &project,
        Engine::Claude,
        Tier::Standard,
        "goal",
        300,
    );

    app.submit_create();
    std::fs::set_permissions(&project, original).unwrap();

    assert!(
        app.status.contains("could not set up the session"),
        "{}",
        app.status
    );
    assert!(Registry::load(&reg_path).unwrap().projects.is_empty());
}

#[test]
fn create_reports_registry_save_failure_after_seeding() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let registry_dir = dir.path().join("registry");
    std::fs::create_dir_all(&registry_dir).unwrap();
    let reg_path = registry_dir.join("registry.json");
    Registry::default().save(&reg_path).unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let original = std::fs::metadata(&registry_dir).unwrap().permissions();
    std::fs::set_permissions(&registry_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut app = creating_loop_app(
        &reg_path,
        &project,
        Engine::Claude,
        Tier::Standard,
        "goal",
        300,
    );

    app.submit_create();
    std::fs::set_permissions(&registry_dir, original).unwrap();

    assert!(
        app.status.contains("could not save the session list"),
        "{}",
        app.status
    );
    assert!(Registry::load(&reg_path).unwrap().projects.is_empty());
    assert!(
        ProjectPaths::for_session(&project, "project")
            .config()
            .exists(),
        "the failure happens after the session state is seeded"
    );
}

#[test]
fn submit_create_on_a_missing_directory_offers_to_create_it() {
    // A directory that does not exist is no longer a hard rejection: `submit_create` opens the
    // "create this directory?" prompt — so a typo is still caught (nothing is made until the
    // human confirms), but a real new folder is one keystroke away. User: *"if the folder is not
    // defined in the popup to allow user to create the folder too."*
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let missing = dir.path().join("brand/new/tree");
    let mut app = creating_loop_app(
        &reg_path,
        &missing,
        Engine::Claude,
        Tier::Standard,
        "some goal",
        job_engine::DEFAULT_CADENCE_S,
    );
    app.submit_create();
    // The prompt is up; NOTHING is created or registered until the human confirms.
    assert!(
        matches!(app.mode, UiMode::ConfirmCreateDir { .. }),
        "a missing dir opens the create-it prompt, got {:?}",
        app.mode
    );
    assert!(!missing.exists(), "not created until the human confirms");
    assert!(Registry::load(&reg_path).unwrap().projects.is_empty());

    // Confirm → the tree is created and the session registered.
    app.confirm_create_dir();
    assert!(missing.exists(), "confirm creates the directory");
    assert_eq!(
        Registry::load(&reg_path).unwrap().projects.len(),
        1,
        "the session is registered once the directory exists"
    );
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn submit_create_rejects_a_path_whose_parent_is_a_file() {
    // A non-NotFound resolve error (here ENOTDIR: a path component is a FILE) is a real problem,
    // not an offer to create — so it rejects with a status and keeps the form open to fix, rather
    // than prompting to `mkdir` under a file.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let a_file = dir.path().join("iam-a-file");
    std::fs::write(&a_file, b"x").unwrap();
    let under_file = a_file.join("nested"); // <file>/nested ⇒ ENOTDIR
    let mut app = creating_loop_app(
        &reg_path,
        &under_file,
        Engine::Claude,
        Tier::Standard,
        "some goal",
        job_engine::DEFAULT_CADENCE_S,
    );
    app.submit_create();
    assert!(
        matches!(app.mode, UiMode::Creating(_)),
        "a hard resolve error keeps the form open, got {:?}",
        app.mode
    );
    assert!(app.status.contains("not usable"), "status: {}", app.status);
    assert!(Registry::load(&reg_path).unwrap().projects.is_empty());
}

#[test]
fn submit_create_requires_a_directory_without_discarding_the_form() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let mut app = creating_loop_app(
        &reg_path,
        Path::new(""),
        Engine::Claude,
        Tier::Standard,
        "some goal",
        job_engine::DEFAULT_CADENCE_S,
    );

    app.submit_create();

    assert!(matches!(app.mode, UiMode::Creating(_)));
    assert_eq!(app.status, "create needs a directory");
    assert!(!reg_path.exists(), "validation must not create a registry");
}

#[test]
fn cancelling_directory_creation_preserves_the_completed_form() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let missing = dir.path().join("mistyped-project");
    let mut app = creating_loop_app(
        &reg_path,
        &missing,
        Engine::Codex,
        Tier::Autopilot,
        "ship the whole goal",
        900,
    );
    let UiMode::Creating(form) = &mut app.mode else {
        unreachable!();
    };
    form.field = CreateForm::DECIDER_MODEL;
    form.worker_model = Some("openai.gpt-5.6-sol".into());
    form.model_choices = vec![ModelInfo {
        label: "GPT-5.6 Sol".into(),
        value: "openai.gpt-5.6-sol".into(),
    }];
    form.decider_engine = Engine::Codex;
    form.decider_model = Some("openai.gpt-5.6-terra".into());
    form.decider_model_choices = vec![ModelInfo {
        label: "GPT-5.6 Terra".into(),
        value: "openai.gpt-5.6-terra".into(),
    }];
    app.submit_create();
    assert!(matches!(app.mode, UiMode::ConfirmCreateDir { .. }));

    app.cancel_create_dir();

    let UiMode::Creating(form) = &app.mode else {
        panic!("cancel did not restore the create form");
    };
    assert_eq!(form.dir.as_str(), missing.display().to_string());
    assert_eq!(form.goal.as_str(), "ship the whole goal");
    assert_eq!(form.engine, Engine::Codex);
    assert_eq!(form.tier, Tier::Autopilot);
    assert_eq!(form.cadence_s, 900);
    assert_eq!(form.field, CreateForm::DECIDER_MODEL);
    assert_eq!(form.worker_model.as_deref(), Some("openai.gpt-5.6-sol"));
    assert_eq!(form.model_choices.len(), 1);
    assert_eq!(form.decider_engine, Engine::Codex);
    assert_eq!(form.decider_model.as_deref(), Some("openai.gpt-5.6-terra"));
    assert_eq!(form.decider_model_choices.len(), 1);
    assert!(!missing.exists());
    assert!(app.status.contains("directory not created"));
}

#[test]
fn confirmed_directory_creation_failure_returns_to_the_form() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let parent_file = dir.path().join("parent-file");
    std::fs::write(&parent_file, "not a directory").unwrap();
    let impossible = parent_file.join("child");
    let mut app = creating_loop_app(
        &reg_path,
        &impossible,
        Engine::Claude,
        Tier::Standard,
        "some goal",
        job_engine::DEFAULT_CADENCE_S,
    );
    let UiMode::Creating(form) = &app.mode else {
        unreachable!();
    };
    app.mode = UiMode::ConfirmCreateDir {
        form: form.clone(),
        dir: impossible.display().to_string(),
    };

    app.confirm_create_dir();

    let UiMode::Creating(form) = &app.mode else {
        panic!("mkdir failure did not restore the create form");
    };
    assert_eq!(form.dir.as_str(), impossible.display().to_string());
    assert_eq!(form.goal.as_str(), "some goal");
    assert!(app.status.contains("could not create"), "{}", app.status);
    assert!(
        !reg_path.exists(),
        "a mkdir failure must not register a row"
    );
}

#[test]
fn create_refuses_to_replace_a_corrupt_registry() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    std::fs::write(&reg_path, "{not-json").unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut app = creating_loop_app(
        &reg_path,
        &project,
        Engine::Claude,
        Tier::Standard,
        "some goal",
        job_engine::DEFAULT_CADENCE_S,
    );

    app.submit_create();

    assert!(
        app.status.contains("could not read the session list")
            && app.status.contains("not overwriting"),
        "{}",
        app.status
    );
    assert_eq!(std::fs::read_to_string(&reg_path).unwrap(), "{not-json");
    assert!(
        !project.join(".project-state").exists(),
        "state must not be seeded before the corrupt registry is refused"
    );
}

#[test]
fn submit_create_on_autopilot_rejects_missing_goal() {
    // AUTOPILOT requires a non-empty goal — it is the direction the hands-off loop
    // steers by — and a miss keeps the form open and writes nothing. Standard is the
    // opposite case (`submit_create_on_standard_allows_an_empty_goal`). (The goal is
    // the only create-time gate; the external-action safety floor is enforced at
    // runtime by `policy::ALWAYS_HARD_KINDS`.)
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let proj = std::fs::canonicalize({
        let p = dir.path().join("harness");
        std::fs::create_dir_all(&p).unwrap();
        p
    })
    .unwrap();

    // Missing goal (whitespace only) -> rejected: nothing written, form stays
    // open for correction.
    let mut app = creating_loop_app(
        &reg_path,
        &proj,
        Engine::Claude,
        Tier::Autopilot,
        "   ",
        job_engine::DEFAULT_CADENCE_S,
    );
    app.submit_create();
    assert!(
        matches!(app.mode, UiMode::Creating(_)),
        "form stays open to fix"
    );
    assert!(
        app.status.contains("autopilot") && app.status.contains("goal"),
        "the refusal names autopilot as the reason (a bare \"needs a goal\" now reads \
         as a bug, since Standard doesn't): {}",
        app.status
    );
    assert!(Registry::load(&reg_path).unwrap().projects.is_empty());
    assert!(
        !ProjectPaths::for_session(&proj, "harness")
            .config()
            .exists(),
        "no config written on a rejected submit"
    );
    // The refusal is total: the daemon ensure lives past this gate, so the singleton
    // lock file must not even exist (`lease::try_acquire` opens it `create(true)`,
    // which is what makes its absence proof that no ensure ran).
    assert!(
        !lease::daemon_lock_path(&reg_path, "pm-test").exists(),
        "a refused create must not probe the daemon singleton lock"
    );
}

#[test]
fn submit_create_agent_loop_writes_session_entry_config_brief_and_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let proj = std::fs::canonicalize({
        let p = dir.path().join("bots");
        std::fs::create_dir_all(&p).unwrap();
        p
    })
    .unwrap();

    let mut app = creating_agent_loop_app(
        &reg_path,
        &proj,
        Engine::Codex,
        "Watch the deploy channel and summarize.",
        600,
    );
    app.submit_create();

    // Registry gained an AgentLoop entry: EMPTY coordinator_cmd (JobScheduler-
    // driven, not a coordinator command), engine + cadence carried through, no
    // initial Message, conversation_id None (the first wake pins/captures it).
    let reg = Registry::load(&reg_path).unwrap();
    let e = reg
        .projects
        .iter()
        .find(|p| p.root == proj)
        .expect("agent-loop entry created");
    assert_eq!(e.mode, Mode::AgentLoop);
    assert_eq!(e.engine, Some(Engine::Codex));
    // NONE on Standard since m40: the form does not ask for a cadence there, so nothing
    // records one. `submit_create_seeds_agent_loop_at_selected_tier` covers both tiers; this
    // test's subject is the SHAPE of the whole record, and an unset cadence is part of it.
    assert_eq!(e.cadence_s, None, "a Standard session records no cadence");
    assert!(e.conversation_id.is_none());
    assert_eq!(
        e.initial_prompt.as_deref(),
        Some("Watch the deploy channel and summarize.")
    );
    assert!(e.enabled);

    // Per-session state lives under sessions/<id>/, NOT the shared root dir.
    let session_paths = ProjectPaths::for_session(&proj, &e.id);
    assert!(
        session_paths
            .state_dir()
            .starts_with(proj.join(".project-state").join("sessions")),
        "state is per-session: {:?}",
        session_paths.state_dir()
    );
    let cfg: Config = state::read_json(&session_paths.config()).unwrap();
    assert_eq!(cfg.autonomy, Tier::Standard);
    assert!(cfg.validate().is_ok(), "seeded config must be valid");
    let brief = std::fs::read_to_string(session_paths.brief()).unwrap();
    assert!(
        brief.is_empty(),
        "a Standard Message is not an Autopilot goal: {brief}"
    );
    let control = state::read_control(&session_paths).unwrap();
    assert_eq!(control.human_cadence_s, None);
    assert!(job::load(&session_paths).unwrap().is_none());

    // The form closed on success.
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn submit_create_on_standard_allows_an_empty_goal() {
    // The other half of the Autopilot gate: on STANDARD a goal is OPTIONAL, because
    // the human at the keyboard is the direction. So an empty goal creates the
    // session normally — form closed, entry registered, an empty `brief.md` seeded
    // (the engine's own empty-brief fallback then tells the agent not to invent a
    // mandate; see `job_engine::loop_nudge_prompt`).
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let proj = std::fs::canonicalize({
        let p = dir.path().join("bots");
        std::fs::create_dir_all(&p).unwrap();
        p
    })
    .unwrap();

    let mut app = creating_agent_loop_app(&reg_path, &proj, Engine::Claude, "   ", 300);
    app.submit_create();

    assert!(
        matches!(app.mode, UiMode::Normal),
        "an empty goal on Standard is not a refusal: {}",
        app.status
    );
    let reg = Registry::load(&reg_path).unwrap();
    let e = reg
        .projects
        .iter()
        .find(|p| p.root == proj)
        .expect("Standard session created without a goal");
    let session_paths = ProjectPaths::for_session(&proj, &e.id);
    assert_eq!(
        state::read_json::<Config>(&session_paths.config())
            .unwrap()
            .autonomy,
        Tier::Standard
    );
    assert!(
        goal_is_empty(&session_paths.brief()),
        "brief.md is seeded empty, not invented"
    );
}

#[test]
fn submit_create_succeeds_with_only_message_set() {
    // A Standard Message is optional dispatch intent, not an Autopilot goal.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let proj = std::fs::canonicalize({
        let p = dir.path().join("bots");
        std::fs::create_dir_all(&p).unwrap();
        p
    })
    .unwrap();

    let mut app = creating_agent_loop_app(&reg_path, &proj, Engine::Claude, "just a goal", 300);
    app.submit_create();

    assert!(matches!(app.mode, UiMode::Normal), "form closed on success");
    let reg = Registry::load(&reg_path).unwrap();
    let e = reg
        .projects
        .iter()
        .find(|p| p.root == proj)
        .expect("loop entry created from goal alone");
    assert_eq!(e.mode, Mode::AgentLoop);
    assert_eq!(e.initial_prompt.as_deref(), Some("just a goal"));
    let brief = std::fs::read_to_string(ProjectPaths::for_session(&proj, &e.id).brief()).unwrap();
    assert!(brief.is_empty(), "{brief}");
}

#[test]
fn multiple_agent_loop_sessions_can_share_one_folder() {
    // The whole point of agent-loop: many sessions in ONE folder. Each gets a
    // distinct registry id AND a distinct sessions/<id>/ state subtree; the
    // root-dedup that blocks a duplicate interactive/native entry is skipped.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let proj = std::fs::canonicalize({
        let p = dir.path().join("bots");
        std::fs::create_dir_all(&p).unwrap();
        p
    })
    .unwrap();

    creating_agent_loop_app(&reg_path, &proj, Engine::Claude, "first goal", 300).submit_create();
    creating_agent_loop_app(&reg_path, &proj, Engine::Claude, "second goal", 300).submit_create();

    let reg = Registry::load(&reg_path).unwrap();
    let here: Vec<&ProjectEntry> = reg.projects.iter().filter(|p| p.root == proj).collect();
    assert_eq!(here.len(), 2, "two sessions coexist in one folder");
    assert_ne!(here[0].id, here[1].id, "distinct registry ids");
    assert!(here.iter().all(|p| p.mode == Mode::AgentLoop));
    // Distinct per-session state subtrees (no clobber), both seeded on disk.
    let d0 = ProjectPaths::for_session(&proj, &here[0].id).state_dir();
    let d1 = ProjectPaths::for_session(&proj, &here[1].id).state_dir();
    assert_ne!(d0, d1, "distinct sessions/<id>/ dirs");
    assert!(d0.exists() && d1.exists(), "both ledgers seeded on disk");
    // Each Standard session keeps its own one-shot launch Message.
    assert_eq!(here[0].initial_prompt.as_deref(), Some("first goal"));
    assert_eq!(here[1].initial_prompt.as_deref(), Some("second goal"));
}

#[test]
fn seed_agent_loop_writes_human_owned_inputs_and_leaves_the_ledger_to_pmd() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot.one");

    seed_agent_loop(
        &paths,
        Tier::Autopilot,
        Engine::Claude,
        Engine::Claude,
        None,
        "keep it green",
        Some(450),
        4242,
    )
    .expect("seed_agent_loop");

    let cfg: Config = state::read_json(&paths.config()).unwrap();
    assert_eq!(cfg.autonomy, Tier::Autopilot);
    assert!(cfg.validate().is_ok(), "config must be valid");
    let brief = std::fs::read_to_string(paths.brief()).unwrap();
    assert!(brief.contains("keep it green"), "brief: {brief}");
    assert_eq!(
        state::read_control(&paths).unwrap().human_cadence_s,
        Some(450)
    );
    assert!(job::load(&paths).unwrap().is_none());
}

// --- C1 (S6 r1): pmtui reads/writes agent-loop sessions at the session path --

#[test]
fn a_standard_create_starts_the_agent_so_enter_only_attaches() {
    // User: *"when my mode is standard, after i create the session, why do i need to press enter for
    // it to render the claude/codex? why can it just runs the session"*.
    //
    // It used to be that create only wrote files: `pmd_drives_row` is false for Standard, so the
    // daemon deliberately never touches the row, and the agent first came into existence on Enter's
    // create-and-chat arm. So a fresh Standard row was a row with no process. Now create does the
    // starting and Enter only attaches.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let proj = std::fs::canonicalize({
        let p = dir.path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        p
    })
    .unwrap();
    let pane = FakePane::default();
    let mut app = creating_loop_app(
        &reg_path,
        &proj,
        Engine::Claude,
        Tier::Standard,
        "",
        job_engine::DEFAULT_CADENCE_S,
    );
    app.agent_tmux = Box::new(pane.clone());
    app.submit_create();

    let launches = pane.launches();
    assert_eq!(
        launches.len(),
        1,
        "create must start exactly one agent: {launches:?}"
    );
    let (session, cwd, argv) = &launches[0];
    let reg = Registry::load(&reg_path).unwrap();
    let e = reg
        .projects
        .iter()
        .find(|p| p.root == proj)
        .expect("row created");

    // The session it launched is the row's OWN chat pane, by the deterministic name every other
    // surface derives, so Enter re-attaches this exact session rather than opening a second one.
    assert_eq!(session, &session_name(&e.id, &proj), "wrong tmux session");
    // In the PROJECT ROOT the human typed, not pmtui's cwd (the m14 (c) bug).
    assert_eq!(
        cwd,
        &proj.display().to_string(),
        "the agent must start in the project tree"
    );

    // THE POSTURE, and this is the safety-relevant half: the INTERACTIVE form, so the agent still
    // asks before it writes. The daemon's `build_loop_command` adds `--permission-mode auto`
    // because nobody is watching it; a Standard row is one a human drives, and starting it
    // unattended would silently remove every permission prompt.
    assert!(
        !argv.iter().any(|a| a == "--permission-mode"),
        "a human-driven session must not be launched unattended: {argv:?}"
    );
    assert!(
        argv.iter().any(|a| a == "--session-id"),
        "the conversation must be PINNED so pmd can adopt it later: {argv:?}"
    );

    // And the id is recorded, which is what lets autopilot later RESUME this conversation instead
    // of minting a second one (`JobScheduler::resolve_conversation_id`'s adopt arm).
    let cid = e
        .conversation_id
        .as_deref()
        .expect("the conversation id is seeded");
    assert!(
        argv.contains(&cid.to_string()),
        "the seeded id must be the one the agent was launched on: {argv:?} vs {cid}"
    );
    assert!(app.status.contains("running"), "status: {}", app.status);
    assert!(
        app.status.contains("you drive it"),
        "status: {}",
        app.status
    );
}

#[test]
fn a_standard_codex_create_starts_one_interactive_terminal_without_inventing_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let proj = std::fs::canonicalize({
        let p = dir.path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        p
    })
    .unwrap();
    let pane = FakePane::default();
    let mut app = creating_loop_app(
        &reg_path,
        &proj,
        Engine::Codex,
        Tier::Standard,
        "",
        job_engine::DEFAULT_CADENCE_S,
    );
    app.agent_tmux = Box::new(pane.clone());
    app.submit_create();

    let launches = pane.launches();
    assert_eq!(launches.len(), 1, "codex starts immediately: {launches:?}");
    let reg = Registry::load(&reg_path).unwrap();
    let e = reg
        .projects
        .iter()
        .find(|p| p.root == proj)
        .expect("row created");
    assert!(
        e.conversation_id.is_none(),
        "no id may be invented for codex — it would fork the rollout"
    );
    let (session, cwd, argv) = &launches[0];
    assert_eq!(session, &session_name(&e.id, &proj));
    assert_eq!(cwd, &proj.display().to_string());
    assert_eq!(argv.first().map(String::as_str), Some("codex"));
    assert!(
        !argv.iter().any(|arg| arg == "resume" || arg == "--last"),
        "first creation stays fresh: {argv:?}"
    );
    assert!(
        app.status.contains("running") && app.status.contains("you drive it"),
        "status: {}",
        app.status
    );
}
