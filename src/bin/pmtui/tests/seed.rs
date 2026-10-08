//! Session path selection, id generation, and initial on-disk state.

use super::*;

fn entry(root: &Path, id: &str) -> ProjectEntry {
    ProjectEntry {
        id: id.into(),
        display_name: None,
        root: root.to_path_buf(),
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
    }
}

#[test]
fn agent_loop_entries_use_their_session_state_subtree() {
    let root = PathBuf::from("/tmp/shared-project");
    let entry = entry(&root, "release.bot");
    assert_eq!(
        entry_state_paths(&entry).state_dir(),
        ProjectPaths::for_session(&root, "release.bot").state_dir()
    );
}

#[test]
fn sanitize_id_replaces_unsafe_characters_and_has_a_fallback() {
    assert_eq!(sanitize_id(" release/bot.v2 "), "release-bot-v2");
    assert_eq!(sanitize_id("safe_ID-2"), "safe_ID-2");
    assert_eq!(sanitize_id(" /.. "), "session");
}

#[test]
fn reserve_unique_id_skips_every_existing_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let reg = Registry {
        projects: ["bot", "bot-2", "bot-3"]
            .into_iter()
            .map(|id| entry(root, id))
            .collect(),
    };

    assert_eq!(reserve_unique_id(&reg, root, "new").unwrap(), "new");
    assert_eq!(reserve_unique_id(&reg, root, "bot").unwrap(), "bot-4");
}

#[test]
fn reserve_unique_id_never_reuses_preserved_or_concurrently_claimed_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let reg = Registry::default();
    std::fs::create_dir_all(ProjectPaths::for_session(root, "bot").state_dir()).unwrap();

    assert_eq!(reserve_unique_id(&reg, root, "bot").unwrap(), "bot-2");
    assert_eq!(
        reserve_unique_id(&reg, root, "bot").unwrap(),
        "bot-3",
        "a second registry racing on the same root must lose the first atomic claim"
    );
}

#[test]
fn reserve_unique_id_reports_parent_creation_failure() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("not-a-directory");
    std::fs::write(&root, "blocker").unwrap();

    let error = reserve_unique_id(&Registry::default(), &root, "bot")
        .unwrap_err()
        .to_string();
    assert!(error.contains("create"), "{error}");
    assert!(error.contains(".project-state"), "{error}");
}

#[cfg(unix)]
#[test]
fn reserve_unique_id_reports_atomic_claim_failure() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join(state::STATE_DIR).join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let original = std::fs::metadata(&sessions).unwrap().permissions();
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o555)).unwrap();

    let result = reserve_unique_id(&Registry::default(), dir.path(), "bot");
    std::fs::set_permissions(&sessions, original).unwrap();

    let error = result.unwrap_err().to_string();
    assert!(error.contains("create"), "{error}");
    assert!(error.contains("bot-"), "{error}");
}

#[test]
fn seed_writes_human_inputs_and_leaves_the_ledger_absent() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");

    seed_agent_loop(
        &paths,
        Tier::Autopilot,
        Engine::Claude,
        Engine::Codex,
        Some("gpt-5.5".into()),
        "ship safely\nwithout downtime",
        None,
        42,
    )
    .unwrap();

    let config: Config = state::read_json(&paths.config()).unwrap();
    assert_eq!(config.autonomy, Tier::Autopilot);
    assert_eq!(config.decider_engine, Engine::Codex);
    assert_eq!(config.decider_model.as_deref(), Some("gpt-5.5"));
    assert_eq!(
        state::read_control(&paths).unwrap(),
        state::Control {
            human_cadence_s: None,
            wake_generation: 0,
        }
    );
    assert_eq!(
        std::fs::read_to_string(paths.brief()).unwrap(),
        "ship safely\nwithout downtime"
    );
    assert!(!paths.pmstate().exists());
}

#[test]
fn seed_reports_state_directory_creation_failures_with_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-directory");
    std::fs::write(&blocker, "file").unwrap();
    let paths = ProjectPaths::for_session(&blocker, "bot");

    let error = seed_agent_loop(
        &paths,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        None,
        0,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("create"), "{error}");
    assert!(
        error.contains(&paths.state_dir().display().to_string()),
        "{error}"
    );
}

#[test]
fn seed_reports_brief_replacement_failures_with_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(paths.brief()).unwrap();

    let error = seed_agent_loop(
        &paths,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        0,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("write"), "{error}");
    assert!(
        error.contains(&paths.brief().display().to_string()),
        "{error}"
    );
}

#[test]
fn seed_reports_control_replacement_failures() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(paths.control()).unwrap();

    let error = seed_agent_loop(
        &paths,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        0,
    )
    .unwrap_err()
    .to_string();

    assert!(!error.is_empty());
    assert!(!paths.brief().exists());
}
