//! A saved TRAE UUID can be only a thread name, so only the engine hook can select the source.

use super::*;

#[test]
fn trae_fork_requires_a_valid_hook_even_with_a_saved_uuid() {
    for reported in [None, Some("not-a-thread")] {
        let mut fixture = fork_fixture(Engine::Trae);
        fixture.app.pmtui_bin = Some(PathBuf::from("/opt/am/pmtui"));
        let paths = ProjectPaths::for_session(&fixture.root, "bot");
        if let Some(reported) = reported {
            state::write_text_atomic(&paths.codex_conversation_id(), reported).unwrap();
        } else {
            let _ = std::fs::remove_file(paths.codex_conversation_id());
        }

        fixture.app.fork_selected();

        assert_refused_without_launch(&fixture, "no conversation to fork");
    }
}

#[test]
fn trae_fork_uses_the_hook_even_when_saved_names_disagree() {
    let mut fixture = fork_fixture(Engine::Trae);
    fixture.app.pmtui_bin = Some(PathBuf::from("/opt/am/pmtui"));
    let paths = ProjectPaths::for_session(&fixture.root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.conversation_id = Some("ffffffff-eeee-4ddd-8ccc-bbbbbbbbbbbb".into());
    job::save(&paths, &ledger).unwrap();
    state::write_text_atomic(&paths.codex_conversation_id(), SOURCE_ID).unwrap();

    fixture.app.fork_selected();

    assert_forked(&fixture, Engine::Trae);
    let launches = fixture.pane.launches();
    assert_eq!(launches.len(), 1);
    assert!(
        launches[0]
            .2
            .windows(2)
            .any(|pair| pair == ["fork", SOURCE_ID])
    );
}

#[test]
fn trae_fork_refuses_before_launch_when_pmtui_bin_is_unavailable() {
    let mut fixture = fork_fixture(Engine::Trae);

    fixture.app.fork_selected();

    assert_refused_without_launch(&fixture, "cannot fork TraeX without working pmtui hooks");
}
