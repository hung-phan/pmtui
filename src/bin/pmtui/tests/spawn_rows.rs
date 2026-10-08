//! Rows a spawn request staged: disabled and still mid-launch, every start path refuses them
//! with one message until the broker finishes, and a row whose launch started is ordinary.

use super::*;
use agent_manager::registry::{LaunchRecord, LaunchState, SpawnOutcome};

const STAGED_REFUSAL: &str = "kid is still being created by a spawn request";

struct SpawnRowFixture {
    _dir: tempfile::TempDir,
    app: App,
    pane: FakePane,
    registry: PathBuf,
}

/// One registry row a spawn request created, in the given launch state, loaded the way the
/// dashboard loads it and selected.
fn spawn_row_fixture(
    state: LaunchState,
    outcome: Option<SpawnOutcome>,
    enabled: bool,
) -> SpawnRowFixture {
    let dir = tempfile::tempdir().unwrap();
    let (registry, _root) = reg_with_agent_loop(dir.path(), "kid");
    Registry::update(&registry, |registry| {
        let row = &mut registry.projects[0];
        row.enabled = enabled;
        row.spawned_by = Some("parent".into());
        row.task_title = Some("Fix flaky fork test".into());
        row.initial_prompt = Some("fix the flaky fork test".into());
        row.launch = Some(LaunchRecord {
            request_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            args_hash: "hash".into(),
            state,
            outcome,
            kind: agent_manager::registry::LaunchKind::Chat,
            branch: None,
            base_commit: None,
        });
    })
    .unwrap();
    let pane = FakePane::default();
    let mut app = app_with_driver(Vec::new(), UiMode::Normal, Box::new(pane.clone()));
    app.registry_path = registry.clone();
    app.refresh();
    app.select_project_index(0);
    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("kid")
    );
    SpawnRowFixture {
        _dir: dir,
        app,
        pane,
        registry,
    }
}

fn kid(registry: &Path) -> ProjectEntry {
    Registry::load(registry)
        .unwrap()
        .projects
        .into_iter()
        .find(|entry| entry.id == "kid")
        .expect("the staged row stays in the list")
}

#[test]
fn a_staged_spawn_row_refuses_enter_restart_autopilot_send_pause_and_fork() {
    let mut fixture = spawn_row_fixture(LaunchState::Pending, None, false);
    let before = kid(&fixture.registry);
    // If the Autopilot flip got through, its daemon ensure would find this held lock.
    let _held = hold_current_daemon(&fixture.registry, "pm-test");

    fn refused(app: &mut App, what: &str, act: &dyn Fn(&mut App)) {
        app.status.clear();
        act(app);
        assert_eq!(app.status, STAGED_REFUSAL, "{what}");
        assert!(
            matches!(app.mode, UiMode::Normal),
            "{what} left {:?}",
            app.mode
        );
    }
    refused(&mut fixture.app, "Enter", &|app| {
        handle_key(app, KeyCode::Enter, KeyModifiers::NONE)
    });
    refused(&mut fixture.app, "restart", &|app| app.restart_agent("kid"));
    refused(&mut fixture.app, "m", &|app| {
        handle_key(app, KeyCode::Char('m'), KeyModifiers::NONE)
    });
    refused(&mut fixture.app, "s", &|app| {
        handle_key(app, KeyCode::Char('s'), KeyModifiers::NONE)
    });
    refused(&mut fixture.app, "p", &|app| {
        handle_key(app, KeyCode::Char('p'), KeyModifiers::NONE)
    });
    // `f` refuses from the view (`fork_refusal`) before it queues a fork, as the Board's chip does.
    refused(&mut fixture.app, "f", &|app| {
        handle_key(app, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(!app.run_pending_fork(), "no fork is queued");
    });
    // A view refreshed before the row was staged still cannot fork it: the handler rechecks the
    // registry row itself.
    fixture.app.projects[0].spawn_staged = false;
    refused(&mut fixture.app, "fork", &|app| app.fork_selected());
    fixture.app.projects[0].spawn_staged = true;
    assert_eq!(
        fixture.app.turn_autopilot_on("kid"),
        STAGED_REFUSAL,
        "every route into Autopilot ends in this flip"
    );

    assert!(
        fixture.pane.launches().is_empty(),
        "the broker owns the only launch"
    );
    let registry = Registry::load(&fixture.registry).unwrap();
    assert_eq!(registry.projects.len(), 1, "a refused fork stages nothing");
    assert_eq!(kid(&fixture.registry), before, "no refusal writes the row");
    let config: Config =
        state::read_json(&ProjectPaths::for_session(&before.root, "kid").config()).unwrap();
    assert_eq!(config.autonomy, Tier::Standard);
}

#[test]
fn a_started_spawn_row_is_not_staged() {
    let fixture = spawn_row_fixture(LaunchState::Started, Some(SpawnOutcome::Ready), true);
    let running = kid(&fixture.registry);
    assert!(!running.is_staged_spawn());
    assert_eq!(start_refusal(&running), None);

    // Paused later by a human: still an ordinary row that Enter resumes.
    let mut paused = spawn_row_fixture(LaunchState::Started, Some(SpawnOutcome::Ready), false);
    let entry = kid(&paused.registry);
    assert!(!entry.is_staged_spawn());
    assert_eq!(start_refusal(&entry), None);
    handle_key(&mut paused.app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert_eq!(
        paused.app.status,
        "kid is already paused — Enter resumes it"
    );
}

#[test]
fn a_staged_row_refusal_wins_over_an_incomplete_fork_label() {
    // A fork of a staged row cannot be made, but a hand-edited registry could carry both marks:
    // the row is still waiting for its broker, so that is what the refusal names.
    let fixture = spawn_row_fixture(LaunchState::Attempted, None, false);
    let mut entry = kid(&fixture.registry);
    entry.forked_from = Some("parent".into());
    assert_eq!(start_refusal(&entry).as_deref(), Some(STAGED_REFUSAL));
    entry.launch = None;
    assert_eq!(
        start_refusal(&entry).as_deref(),
        Some("kid is an incomplete fork of parent with no conversation - delete it with d")
    );
}

#[test]
fn m_on_a_staged_row_names_the_spawn_request_even_when_its_settings_are_unreadable() {
    let mut fixture = spawn_row_fixture(LaunchState::Pending, None, false);
    let entry = kid(&fixture.registry);
    let config = ProjectPaths::for_session(&entry.root, &entry.id).config();
    std::fs::write(&config, b"{ not json").unwrap();

    fixture.app.cycle_tier();

    assert_eq!(fixture.app.status, STAGED_REFUSAL);
    assert!(fixture.pane.launches().is_empty());
}
