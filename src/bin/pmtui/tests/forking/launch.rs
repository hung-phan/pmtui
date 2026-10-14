//! What a fork hands its child: the human's context, the engine's own branch argument, a terminal
//! that names the child, and the spawn skill its `PMTUI_BIN` can run.

use super::*;

#[test]
fn claude_fork_copies_human_context_and_publishes_only_the_child_identity() {
    let mut fixture = fork_fixture(Engine::Claude);
    let source_paths = ProjectPaths::for_session(&fixture.root, "bot");
    let source_before = std::fs::read(source_paths.brief()).unwrap();

    fixture.app.fork_selected();

    assert_forked(&fixture, Engine::Claude);
    assert_eq!(std::fs::read(source_paths.brief()).unwrap(), source_before);
    let argv = &fixture.pane.launches()[0].2;
    assert!(argv.windows(2).any(|pair| pair == ["--resume", SOURCE_ID]));
    assert!(argv.iter().any(|arg| arg == "--fork-session"));
    assert!(!argv.iter().any(|arg| arg == "--session-id"));
}

#[test]
fn codex_fork_uses_the_native_subcommand_and_captures_the_exact_child() {
    let mut fixture = fork_fixture(Engine::Codex);

    fixture.app.fork_selected();

    assert_forked(&fixture, Engine::Codex);
    let argv = &fixture.pane.launches()[0].2;
    let fork = argv.iter().position(|arg| arg == "fork").unwrap();
    assert_eq!(argv[fork + 1], SOURCE_ID);
    assert!(!argv.iter().any(|arg| arg == "resume"));
}

#[test]
fn trae_fork_uses_native_fork_and_waits_for_the_reported_child_identity() {
    let mut fixture = fork_fixture(Engine::Trae);
    fixture.app.fork_selected();
    assert!(
        fixture.app.status.contains("ready for Enter"),
        "{}",
        fixture.app.status
    );
    assert_forked(&fixture, Engine::Trae);
    let launches = fixture.pane.launches();
    assert_eq!(launches.len(), 1);
    let argv = &launches[0].2;
    assert!(argv.windows(2).any(|pair| pair == ["fork", SOURCE_ID]));
    assert!(!argv.iter().any(|arg| arg == "resume"));
    assert!(argv.iter().any(|arg| arg.contains("hooks.SessionStart")));
}

#[test]
fn trae_fork_uses_the_hook_identity_when_no_daemon_has_adopted_it() {
    let mut fixture = fork_fixture(Engine::Trae);
    let paths = ProjectPaths::for_session(&fixture.root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.conversation_id = None;
    job::save(&paths, &ledger).unwrap();
    Registry::update(&fixture.registry, |registry| {
        registry.projects[0].conversation_id = None;
    })
    .unwrap();
    state::write_text_atomic(&paths.codex_conversation_id(), SOURCE_ID).unwrap();
    fixture.app.fork_selected();
    let registry = Registry::load(&fixture.registry).unwrap();
    assert_eq!(registry.projects.len(), 2, "{}", fixture.app.status);
    assert_eq!(
        registry.projects[1].conversation_id.as_deref(),
        Some(CHILD_ID)
    );
    assert!(
        fixture.pane.launches()[0]
            .2
            .windows(2)
            .any(|pair| pair == ["fork", SOURCE_ID])
    );
}

#[test]
fn trae_fork_discards_a_child_with_missing_or_source_identity() {
    for identity in ["", SOURCE_ID] {
        let mut fixture = fork_fixture_with_identity(Engine::Trae, identity);
        fixture.app.fork_selected();
        assert_fork_discarded(&fixture);
        assert_eq!(fixture.pane.terminated().len(), 1);
    }
}

#[test]
fn a_fork_launches_the_child_terminal_with_the_childs_identity() {
    let mut fixture = fork_fixture(Engine::Claude);
    fixture.app.pmtui_bin = Some(PathBuf::from("/opt/am/pmtui"));

    fixture.app.fork_selected();

    assert_forked(&fixture, Engine::Claude);
    assert_eq!(
        fixture.pane.launched_env(),
        [(
            session_name("bot-fork", &fixture.root),
            tmux::ManagedEnv {
                session_id: "bot-fork".into(),
                state_dir: ProjectPaths::for_session(&fixture.root, "bot-fork").state_dir(),
                pmtui_bin: Some(PathBuf::from("/opt/am/pmtui")),
            },
        )],
        "the child names itself, never its source"
    );
}

#[test]
fn a_fork_ships_the_spawn_skill_only_when_its_terminal_names_pmtui() {
    let spawn_skill =
        |fixture: &ForkFixture| ProjectPaths::new(&fixture.root).canonical_spawn_skill_file();
    let mut fixture = fork_fixture(Engine::Codex);
    fixture.app.fork_selected();
    assert_forked(&fixture, Engine::Codex);
    assert!(!spawn_skill(&fixture).exists(), "no PMTUI_BIN, no skill");

    let mut fixture = fork_fixture(Engine::Codex);
    fixture.app.pmtui_bin = Some(PathBuf::from("/opt/am/pmtui"));
    fixture.app.fork_selected();
    assert_forked(&fixture, Engine::Codex);
    assert_eq!(
        std::fs::read_to_string(spawn_skill(&fixture)).unwrap(),
        agent_manager::skills::SPAWN_SKILL_MD,
        "the child terminal's project root has the skill its PMTUI_BIN can run"
    );
}

#[test]
fn fork_preserves_a_legacy_sessions_visible_work_title() {
    let mut fixture = fork_fixture(Engine::Claude);
    Registry::update(&fixture.registry, |registry| {
        registry.projects[0].task_title = None;
        registry.projects[0].initial_prompt = Some("Legacy task title".into());
    })
    .unwrap();
    let paths = ProjectPaths::for_session(&fixture.root, "bot");
    std::fs::write(paths.brief(), "").unwrap();

    fixture.app.fork_selected();

    let registry = Registry::load(&fixture.registry).unwrap();
    let child = registry
        .projects
        .iter()
        .find(|entry| entry.id == "bot-fork")
        .expect("fork child");
    assert_eq!(child.task_title.as_deref(), Some("Legacy task title"));
}

#[test]
fn fork_supports_a_legacy_registry_only_identity_and_engine_default() {
    let mut fixture = fork_fixture(Engine::Claude);
    let source_paths = ProjectPaths::for_session(&fixture.root, "bot");
    std::fs::remove_file(source_paths.pmstate()).unwrap();
    Registry::update(&fixture.registry, |registry| {
        registry.projects[0].engine = None;
    })
    .unwrap();

    fixture.app.fork_selected();

    assert_forked(&fixture, Engine::Claude);
}

#[test]
fn fork_takes_a_legacy_registry_engine_when_no_ledger_exists() {
    let mut fixture = fork_fixture(Engine::Codex);
    let source_paths = ProjectPaths::for_session(&fixture.root, "bot");
    std::fs::remove_file(source_paths.pmstate()).unwrap();

    fixture.app.fork_selected();

    assert_forked(&fixture, Engine::Codex);
    let argv = &fixture.pane.launches()[0].2;
    assert!(argv.iter().any(|arg| arg == "fork"), "{argv:?}");
}
