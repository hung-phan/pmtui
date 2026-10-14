use super::*;

const WORKER_CLAIM: &str = "0199aaaa-1111-7aaa-8bbb-ccccdddddddd";
const ENGINE_ID: &str = "0199bbbb-2222-7aaa-8bbb-ccccdddddddd";

fn worker_identity_report(engine: Engine, captured: Option<&str>) -> Fx {
    let mut fx = setup_with(Tier::Standard, engine, Some(300), |ledger| {
        ledger.conversation_id = None;
        ledger.run = JobRun::Monitoring { until: START };
    });
    let session = loop_session(&fx);
    fx.driver.set_alive(&session, true);
    fx.driver.set_tail(&session, IDLE_PANE);
    if let Some(captured) = captured {
        std::fs::create_dir_all(fx.paths.daemon_dir()).unwrap();
        std::fs::write(fx.paths.codex_conversation_id(), captured).unwrap();
    }
    write_marker(
        &fx,
        &format!(
            r#"{{"seq":11,"state":"working","status":"warming up","conversation_id":"{WORKER_CLAIM}"}}"#
        ),
    );
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(ledger(&fx).report_generation, 1);
    assert!(fx.driver.launched().is_empty());
    fx
}

#[test]
fn worker_claim_cannot_fill_an_unknown_engine_assigned_identity() {
    // A report can arrive before the engine's hook; accepting its UUID would make
    // another session's conversation available to ledger readers such as fork.
    for engine in [Engine::Codex, Engine::Trae] {
        let fx = worker_identity_report(engine, None);
        assert_eq!(ledger(&fx).conversation_id, None, "{engine:?}");
        assert!(!fx.paths.codex_conversation_id().exists());
    }
}

#[test]
fn engine_hook_identity_wins_over_a_worker_claim() {
    for engine in [Engine::Codex, Engine::Trae] {
        let fx = worker_identity_report(engine, Some(ENGINE_ID));
        assert_eq!(
            ledger(&fx).conversation_id.as_deref(),
            Some(ENGINE_ID),
            "{engine:?}"
        );
        assert_eq!(
            crate::state::codex_identity::read(&fx.paths).as_deref(),
            Some(ENGINE_ID)
        );
    }
}
