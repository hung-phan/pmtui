//! An unreadable tier cannot weaken Claude's retained hook evidence.

use super::*;

#[test]
fn claude_fork_refuses_an_idle_prompt_with_an_outstanding_turn_and_unknown_tier() {
    let mut fixture = fork_fixture(Engine::Claude);
    let paths = ProjectPaths::for_session(&fixture.root, "bot");
    let config: Config = state::read_json(&paths.config()).unwrap();
    std::fs::remove_file(paths.config()).unwrap();
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.run = job::JobRun::Monitoring {
        until: SystemClock.now() + 300,
    };
    ledger.turn_count_at_nudge = Some(1);
    job::save(&paths, &ledger).unwrap();
    state::write_text_atomic(&paths.turn_signal(), ".").unwrap();
    let driver = source_driver(&fixture, IDLE_CLAUDE_PANE);
    install_driver(&mut fixture, driver);

    fixture.app.fork_selected();

    assert_refused_without_launch(&fixture, "still working");
    state::write_text_atomic(&paths.turn_signal(), "..").unwrap();
    fixture.app.fork_selected();
    assert_refused_without_launch(&fixture, "settings unreadable");
    state::write_json_atomic(&paths.config(), &config).unwrap();
    fixture.app.fork_selected();
    assert_forked(&fixture, Engine::Claude);
}

#[test]
fn claude_and_codex_forks_report_conversation_mismatch_before_engine_mismatch() {
    for engine in [Engine::Claude, Engine::Codex] {
        let mut fixture = fork_fixture(engine);
        Registry::update(&fixture.registry, |registry| {
            registry.projects[0].engine = Some(if engine == Engine::Claude {
                Engine::Codex
            } else {
                Engine::Claude
            });
            registry.projects[0].conversation_id = Some(CHILD_ID.into());
        })
        .unwrap();

        fixture.app.fork_selected();

        assert_refused_without_launch(&fixture, "ledger conversation");
    }
}
