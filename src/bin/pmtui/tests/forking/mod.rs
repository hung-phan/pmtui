//! The fork fixture every fork test shares: a source session with a saved conversation, a fake
//! terminal that answers the child identity probe, and the two verdicts a fork can reach — the
//! child was created, or the fork was discarded with nothing left behind.

use super::*;

const SOURCE_ID: &str = "11111111-aaaa-4bbb-8ccc-555555555555";
const CHILD_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

struct ForkFixture {
    _dir: tempfile::TempDir,
    app: App,
    pane: FakePane,
    registry: PathBuf,
    root: PathBuf,
}

fn install_driver(fixture: &mut ForkFixture, driver: FakePane) {
    fixture.app.agent_tmux = Box::new(driver.clone());
    fixture.pane = driver;
}

fn source_driver(fixture: &ForkFixture, tail: &str) -> FakePane {
    let source_session = session_name("bot", &fixture.root);
    let child_session = session_name("bot-fork", &fixture.root);
    FakePane::with_codex_session(&child_session, CHILD_ID).with(|inner| {
        inner.alive.lock().unwrap().insert(source_session.clone());
        inner.tails.insert(source_session, tail.to_string());
    })
}

fn fork_fixture(engine: Engine) -> ForkFixture {
    fork_fixture_with_identity(engine, CHILD_ID)
}

fn fork_fixture_with_identity(engine: Engine, child_identity: &str) -> ForkFixture {
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_agent_loop(dir.path(), "bot");
    Registry::update(&registry, |registry| {
        let source = &mut registry.projects[0];
        source.engine = Some(engine);
        source.worker_model = Some("worker-model".into());
        source.task_title = Some("Preserve task context".into());
        source.conversation_id = Some(SOURCE_ID.into());
    })
    .unwrap();
    let source_paths = ProjectPaths::for_session(&root, "bot");
    let mut ledger = job::load(&source_paths).unwrap().unwrap();
    ledger.engine = engine;
    ledger.conversation_id = Some(SOURCE_ID.into());
    job::save(&source_paths, &ledger).unwrap();
    state::write_text_atomic(&source_paths.brief(), "preserve this goal").unwrap();
    state::write_text_atomic(&source_paths.directive(), "never publish").unwrap();

    let child_session = session_name("bot-fork", &root);
    let pane = FakePane::with_codex_session(&child_session, child_identity);
    let mut view = ProjectView::read_agent_loop("bot", &source_paths, true, SystemClock.now());
    view.engine = Some(engine);
    let mut app = app_with_driver(vec![view], UiMode::Normal, Box::new(pane.clone()));
    app.registry_path = registry.clone();
    ForkFixture {
        _dir: dir,
        app,
        pane,
        registry,
        root,
    }
}

fn assert_forked(fixture: &ForkFixture, engine: Engine) {
    let registry = Registry::load(&fixture.registry).unwrap();
    assert_eq!(registry.projects.len(), 2);
    let source = registry
        .projects
        .iter()
        .find(|entry| entry.id == "bot")
        .unwrap();
    assert_eq!(source.conversation_id.as_deref(), Some(SOURCE_ID));
    assert!(source.enabled && source.forked_from.is_none());

    let child = registry
        .projects
        .iter()
        .find(|entry| entry.id == "bot-fork")
        .unwrap();
    assert!(child.enabled);
    assert_eq!(child.engine, Some(engine));
    assert_eq!(child.worker_model.as_deref(), Some("worker-model"));
    assert_eq!(child.task_title.as_deref(), Some("Preserve task context"));
    assert_eq!(child.forked_from.as_deref(), Some("bot"));
    assert_eq!(child.conversation_id.as_deref(), Some(CHILD_ID));
    assert_eq!(child.cadence_s, None);

    let child_paths = ProjectPaths::for_session(&fixture.root, "bot-fork");
    let config: Config = state::read_json(&child_paths.config()).unwrap();
    assert_eq!(config.autonomy, Tier::Standard);
    assert_eq!(
        std::fs::read_to_string(child_paths.brief()).unwrap(),
        "preserve this goal"
    );
    assert_eq!(
        std::fs::read_to_string(child_paths.directive()).unwrap(),
        "never publish"
    );
    assert_eq!(
        fixture.app.selected_view().map(|view| view.id.as_str()),
        Some("bot-fork")
    );
    assert_eq!(
        fixture
            .app
            .selected_view()
            .and_then(|view| view.forked_from.as_deref()),
        Some("bot")
    );
    assert!(fixture.app.status.contains("ready for Enter or Message"));
    let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
    terminal.draw(|frame| render(frame, &fixture.app)).unwrap();
    assert!(
        screen_text(&terminal).contains("fork     from bot · shared directory"),
        "fork lineage missing from selected-session detail"
    );
}

/// A failed fork leaves no row and no reserved state behind, only its status report.
fn assert_fork_discarded(fixture: &ForkFixture) {
    let registry = Registry::load(&fixture.registry).unwrap();
    assert!(
        !registry
            .projects
            .iter()
            .any(|entry| entry.id.starts_with("bot-fork")),
        "{:?}",
        registry.projects
    );
    assert!(
        !ProjectPaths::for_session(&fixture.root, "bot-fork")
            .state_dir()
            .exists(),
        "the reserved child state must be removed with its row"
    );
    assert!(
        fixture.app.status.contains("fork discarded"),
        "{}",
        fixture.app.status
    );
}

/// A refusal reaches no terminal, stages no row, and reserves no child state.
fn assert_refused_without_launch(fixture: &ForkFixture, needle: &str) {
    assert!(
        fixture.app.status.contains(needle),
        "expected {needle:?} in {:?}",
        fixture.app.status
    );
    assert!(fixture.pane.launches().is_empty());
    let registry = Registry::load(&fixture.registry).unwrap();
    assert_eq!(registry.projects.len(), 1, "{:?}", registry.projects);
    assert!(
        !ProjectPaths::for_session(&fixture.root, "bot-fork")
            .state_dir()
            .exists(),
        "a refusal must not reserve a child"
    );
}

/// A Codex pane waiting at its composer, with the footer below it.
const IDLE_CODEX_PANE: &str = "\u{2022} Earlier answer\n\n\u{203a} Ask Codex to do anything\n\n  gpt-5 \u{b7} 100% context left\n";

/// A live, idle Codex source whose exact process probe reports `live`.
fn live_codex_source(fixture: &ForkFixture, live: Option<&str>) -> FakePane {
    let source_session = session_name("bot", &fixture.root);
    source_driver(fixture, IDLE_CODEX_PANE).with(|inner| {
        if let Some(live) = live {
            inner
                .codex_session_ids
                .insert(source_session, live.to_string());
        }
    })
}

mod child_identity;
mod cleanup;
mod incomplete;
mod keys;
mod launch;
mod lineage;
mod refusals;
mod source_identity;
mod source_state;
