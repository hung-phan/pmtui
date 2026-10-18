use super::*;
use tempfile::tempdir;

fn entry(id: &str, enabled: bool) -> ProjectEntry {
    ProjectEntry {
        id: id.into(),
        display_name: None,
        root: PathBuf::from(format!("/tmp/{id}")),
        enabled,
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
fn round_trips() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("registry.json");
    let reg = Registry {
        projects: vec![entry("a", true), entry("b", false)],
    };
    reg.save(&path).unwrap();
    let back = Registry::load(&path).unwrap();
    assert_eq!(back.projects, reg.projects);
    assert_eq!(Engine::Claude.label(), "claude");
    assert_eq!(Engine::Codex.label(), "codex");
}

#[test]
fn missing_registry_is_empty() {
    let dir = tempdir().unwrap();
    let reg = Registry::load(&dir.path().join("nope.json")).unwrap();
    assert!(reg.projects.is_empty());
}

#[test]
fn enabled_default_true_when_field_absent() {
    let json = r#"{ "projects": [ { "id": "a", "root": "/tmp/a" } ] }"#;
    let reg: Registry = serde_json::from_str(json).unwrap();
    assert!(reg.projects[0].enabled);
}

#[test]
fn agent_loop_entry_round_trips_and_missing_mode_defaults() {
    // AgentLoop mode + resume/cadence fields round-trip; a row with no `mode`
    // field still loads and defaults to AgentLoop via serde.
    let json = r#"{ "projects": [
        { "id": "bot", "display_name": "Release work", "root": "/tmp/bot", "mode": "agent_loop",
          "engine": "claude", "task_title": "Ship task view", "forked_from": "parent", "conversation_id": "uuid-1", "cadence_s": 300 },
        { "id": "old", "root": "/tmp/old" } ] }"#;
    let reg: Registry = serde_json::from_str(json).unwrap();
    let bot = &reg.projects[0];
    assert_eq!(bot.mode, Mode::AgentLoop);
    assert_eq!(bot.display_name.as_deref(), Some("Release work"));
    assert_eq!(bot.engine, Some(Engine::Claude));
    assert_eq!(bot.task_title.as_deref(), Some("Ship task view"));
    assert_eq!(bot.forked_from.as_deref(), Some("parent"));
    assert_eq!(bot.conversation_id.as_deref(), Some("uuid-1"));
    assert_eq!(bot.cadence_s, Some(300));
    let old = &reg.projects[1];
    assert!(old.display_name.is_none());
    assert_eq!(old.mode, Mode::AgentLoop);
    assert!(
        old.task_title.is_none()
            && old.forked_from.is_none()
            && old.conversation_id.is_none()
            && old.cadence_s.is_none()
    );
    // Re-serialize + re-parse to confirm the `agent_loop` rename is stable.
    let round: Registry = serde_json::from_str(&serde_json::to_string(&reg).unwrap()).unwrap();
    assert_eq!(round.projects[0].mode, Mode::AgentLoop);
}

#[test]
fn legacy_retired_modes_load_as_agent_loop_never_wipe_the_registry() {
    // FAIL-SAFE: a pre-purge registry.json carrying the retired `"auto"`/`"interactive"`
    // modes must still LOAD (folding to AgentLoop), not hard-error. A parse error here
    // would make `Registry::load(..).unwrap_or_default()` silently return an EMPTY list,
    // and the next `save` would overwrite every on-disk entry — data loss.
    let json = r#"{ "projects": [
        { "id": "phase", "root": "/tmp/phase", "mode": "auto" },
        { "id": "raw",   "root": "/tmp/raw",   "mode": "interactive" },
        { "id": "now",   "root": "/tmp/now",   "mode": "agent_loop" } ] }"#;
    let reg: Registry = serde_json::from_str(json).expect("legacy modes must not error");
    assert_eq!(reg.projects.len(), 3, "no entry may be dropped on load");
    for p in &reg.projects {
        assert_eq!(p.mode, Mode::AgentLoop, "{} should fold to AgentLoop", p.id);
    }
}

#[test]
fn update_appends_without_wiping_and_refuses_a_corrupt_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("registry.json");

    // Absent: `update` starts from default and writes the first entry.
    Registry::update(&path, |r| r.projects.push(entry("a", true))).unwrap();
    assert_eq!(Registry::load(&path).unwrap().projects.len(), 1);

    // Present + valid: a second update APPENDS, keeping the first — no wipe.
    Registry::update(&path, |r| r.projects.push(entry("b", true))).unwrap();
    let ids: Vec<_> = Registry::load(&path)
        .unwrap()
        .projects
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);

    // CORRUPT: a garbage file must make `update` Err WITHOUT saving — the data-loss guard.
    // The old `load().unwrap_or_default()` + push + save would have wiped it to a one-entry
    // list here.
    std::fs::write(&path, b"{ this is not json").unwrap();
    assert!(
        Registry::update(&path, |r| r.projects.push(entry("c", true))).is_err(),
        "update must refuse a corrupt registry, not default-then-save"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{ this is not json",
        "the corrupt bytes are left untouched — nothing was overwritten"
    );
}

#[test]
fn enabled_filter() {
    let reg = Registry {
        projects: vec![entry("a", true), entry("b", false), entry("c", true)],
    };
    let ids: Vec<&str> = reg.enabled().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "c"]);
}

#[test]
fn entry_without_worker_model_loads_as_none() {
    let json = r#"{"id":"x","root":"/tmp","enabled":true,"mode":"agent_loop","engine":"claude"}"#;
    let e: ProjectEntry = serde_json::from_str(json).expect("legacy entry must parse");
    assert_eq!(e.worker_model, None);
}

#[test]
fn worker_model_round_trips() {
    let json = r#"{"id":"x","root":"/tmp","enabled":true,"mode":"agent_loop","engine":"codex","worker_model":"openai.gpt-5.6-sol"}"#;
    let e: ProjectEntry = serde_json::from_str(json).expect("must parse");
    assert_eq!(e.worker_model.as_deref(), Some("openai.gpt-5.6-sol"));
}

fn launch(state: LaunchState, outcome: Option<SpawnOutcome>) -> LaunchRecord {
    LaunchRecord {
        request_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        args_hash: "ab12".into(),
        state,
        outcome,
        kind: LaunchKind::Job,
        branch: None,
        base_commit: None,
    }
}

#[test]
fn legacy_entry_without_spawn_fields_loads_with_none() {
    let json = r#"{"id":"x","root":"/tmp","enabled":false,"mode":"agent_loop","engine":"claude"}"#;
    let e: ProjectEntry = serde_json::from_str(json).expect("legacy entry must parse");
    assert_eq!(e.spawned_by, None);
    assert_eq!(e.launch, None);
    assert!(
        !e.is_staged_spawn(),
        "a paused row with no launch record is not a staged spawn"
    );
}

#[test]
fn launch_record_round_trips_snake_case() {
    let cases = [
        (LaunchState::Pending, None, r#""state":"pending""#),
        (LaunchState::Attempted, None, r#""state":"attempted""#),
        (LaunchState::Started, None, r#""state":"started""#),
        (
            LaunchState::FailedBeforeStart,
            None,
            r#""state":"failed_before_start""#,
        ),
        (
            LaunchState::Started,
            Some(SpawnOutcome::Ready),
            r#""outcome":"ready""#,
        ),
        (
            LaunchState::Started,
            Some(SpawnOutcome::NeedsAttention),
            r#""outcome":"needs_attention""#,
        ),
        (
            LaunchState::Attempted,
            Some(SpawnOutcome::OutcomeUnknown),
            r#""outcome":"outcome_unknown""#,
        ),
    ];
    for (state, outcome, needle) in cases {
        let mut e = entry("child", false);
        e.spawned_by = Some("parent".into());
        e.launch = Some(launch(state, outcome));
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(needle), "{needle} missing from {json}");
        assert!(json.contains(r#""spawned_by":"parent""#), "{json}");
        assert!(json.contains(r#""args_hash":"ab12""#), "{json}");
        let back: ProjectEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }
}

#[test]
fn absent_spawn_fields_are_not_serialized() {
    let json = serde_json::to_string(&entry("a", true)).unwrap();
    assert!(!json.contains("spawned_by"), "{json}");
    assert!(!json.contains("launch"), "{json}");
    let json = serde_json::to_string(&launch(LaunchState::Pending, None)).unwrap();
    assert!(!json.contains("outcome"), "{json}");
}

#[test]
fn children_of_counts_enabled_and_disabled_rows() {
    let row = |id: &str, enabled: bool, parent: Option<&str>| {
        let mut e = entry(id, enabled);
        e.spawned_by = parent.map(str::to_string);
        e
    };
    let reg = Registry {
        projects: vec![
            row("p", true, None),
            row("c1", true, Some("p")),
            row("c2", false, Some("p")),
            row("c2-fork", true, Some("p")),
            row("other", true, Some("q")),
        ],
    };
    assert_eq!(reg.children_of("p"), 3);
    assert_eq!(reg.children_of("q"), 1);
    assert_eq!(reg.children_of("c1"), 0);
    assert_eq!(MAX_CHILDREN_PER_PARENT, 5);
}

#[test]
fn only_a_disabled_row_mid_spawn_is_staged() {
    for (state, enabled, staged) in [
        (LaunchState::Pending, false, true),
        (LaunchState::Attempted, false, true),
        (LaunchState::FailedBeforeStart, false, true),
        (LaunchState::Started, false, false),
        (LaunchState::Pending, true, false),
        (LaunchState::Attempted, true, false),
    ] {
        let mut e = entry("child", enabled);
        e.launch = Some(launch(state, None));
        assert_eq!(e.is_staged_spawn(), staged, "{state:?} enabled={enabled}");
    }
}
