use super::*;

fn latched_fixture() -> Fx {
    let mut fx = blocked_fx();
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    fx.sched
        .advise_health
        .latch_off(SESSION_ID, "model unavailable");
    fx.sched.advise_health.no_result_streak = SUPERVISOR_DEAD_AFTER;
    fx.sched.advise_health.refusal_streak = SUPERVISOR_MAX_REFUSALS;
    let mut saved = ledger(&fx);
    fx.sched.save_ledger(&mut saved).unwrap();
    fx
}

fn restart(fx: &mut Fx) {
    fx.sched = JobScheduler::new(SESSION_ID, fx._dir.path(), SESSION_ID, Engine::Claude, None);
}

fn assert_recovered(fx: &Fx) {
    assert!(!fx.sched.advise_health.latched);
    assert_eq!(fx.sched.advise_health.reason, None);
    assert!(!fx.sched.advise_health.warned);
    assert_eq!(fx.sched.advise_health.no_result_streak, 0);
    assert_eq!(fx.sched.advise_health.refusal_streak, 0);
    assert_eq!(fx.sched.advise_binary, None);
    let saved = ledger(fx);
    assert!(!saved.decider_latched);
    assert_eq!(saved.decider_latch_reason, None);
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn engine_change_recovers_missing_binary_before_another_decision() {
    let (mut fx, _) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_decider_binary_available(false);
    write_marker(&fx, AUTOFLOW_ASKS);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert!(ledger(&fx).decider_latched);
    assert_eq!(fx.sched.advise_binary, Some(false));

    set_decider_engine(&fx, Engine::Trae);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_recovered(&fx);
    assert!(!ledger(&fx).open_stops.is_empty());
}

#[test]
fn model_change_resets_all_transport_health() {
    let mut fx = latched_fixture();
    set_decider_model(&fx, Some("replacement-model"));
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_recovered(&fx);
}

#[test]
fn removing_model_override_also_resets_health() {
    let mut fx = latched_fixture();
    set_decider_model(&fx, Some("bad-model"));
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    fx.sched
        .advise_health
        .latch_off(SESSION_ID, "model unavailable");
    fx.sched.set_decider_binary_available(false);
    let mut saved = ledger(&fx);
    fx.sched.save_ledger(&mut saved).unwrap();

    set_decider_model(&fx, None);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_recovered(&fx);
}

#[test]
fn unchanged_selection_preserves_latch_and_cache() {
    let mut fx = latched_fixture();
    set_decider_engine(&fx, Engine::Claude);
    set_decider_model(&fx, None);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(ledger(&fx).decider_latched);
    assert_eq!(fx.sched.advise_binary, Some(true));
    assert_eq!(
        fx.sched.advise_health.no_result_streak,
        SUPERVISOR_DEAD_AFTER
    );

    restart(&mut fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(ledger(&fx).decider_latched);
    assert_eq!(
        fx.sched.advise_health.reason.as_deref(),
        Some("model unavailable")
    );
}

#[test]
fn selection_changed_while_pmd_was_stopped_recovers() {
    for engine_change in [true, false] {
        let mut fx = latched_fixture();
        if engine_change {
            set_decider_engine(&fx, Engine::Trae);
        } else {
            set_decider_model(&fx, Some("replacement-model"));
        }
        restart(&mut fx);
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
        assert_recovered(&fx);
    }
}

#[test]
fn legacy_latch_without_selection_is_not_cleared_by_restart() {
    let mut fx = setup_with(Tier::Standard, Engine::Claude, None, |saved| {
        saved.decider_latched = true;
        saved.decider_latch_reason = Some("legacy failure".into());
        saved.run = JobRun::Blocked {
            stop_ids: vec![],
            since: START,
        };
    });
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    restart(&mut fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(ledger(&fx).decider_latched);
    assert_eq!(
        fx.sched.advise_health.reason.as_deref(),
        Some("legacy failure")
    );
}

#[test]
fn old_consult_failure_does_not_latch_new_selection() {
    let (mut fx, _) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    fx.sched.advise_health.no_result_streak = SUPERVISOR_DEAD_AFTER - 1;
    set_decider_engine(&fx, Engine::Trae);
    finish_consult(&fx, seq, "", 1);
    fx.clock.set(START + SUPERVISOR_POLL_S);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert_eq!(ledger(&fx).decider_runs[0].engine, Engine::Claude);
    assert!(matches!(
        ledger(&fx).decider_runs[0].outcome,
        job::DeciderOutcome::Failed { .. }
    ));

    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_recovered(&fx);
}

#[test]
fn engine_change_keeps_original_inflight_verdict_source() {
    let (mut fx, _) = marker_fx(Tier::Autopilot, |_| {});
    set_decider_engine(&fx, Engine::Codex);
    let seq = start_consult(&mut fx);
    let inflight = fx.sched.advise.as_ref().unwrap();
    let verdict_path = inflight.verdict_path.clone().unwrap();
    let nonce = inflight.consult.nonce.clone();
    set_decider_engine(&fx, Engine::Trae);
    finish_consult(&fx, seq, "not the verdict", 0);
    std::fs::write(
        &verdict_path,
        format!(
            r#"{{"nonce":"{nonce}","action":"select_option","option_index":1,"reason":"Use the vendored formatter for the changelog goal."}}"#
        ),
    )
    .unwrap();
    fx.clock.set(START + SUPERVISOR_POLL_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let saved = ledger(&fx);
    assert_eq!(saved.decider_runs[0].engine, Engine::Codex);
    assert!(
        matches!(
            saved.decider_runs[0].outcome,
            job::DeciderOutcome::Resolved { .. }
        ),
        "{:?}",
        saved.decider_runs[0].outcome
    );
}

#[test]
fn changed_environment_model_recovers_after_restart() {
    const CHILD: &str = "PM_TRAE_HEALTH_TEST";
    if std::env::var_os(CHILD).is_some() {
        let mut fx = latched_fixture();
        set_decider_engine(&fx, Engine::Trae);
        let mut saved = ledger(&fx);
        saved.decider_health_selection = Some(job::DeciderHealthSelection {
            engine: Engine::Trae,
            model: Some("old-model".into()),
        });
        job::save(&fx.paths, &saved).unwrap();
        restart(&mut fx);
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
        assert_recovered(&fx);
        assert_eq!(
            ledger(&fx)
                .decider_health_selection
                .unwrap()
                .model
                .as_deref(),
            Some("new-model")
        );
        return;
    }
    // A child owns the environment so parallel tests cannot change its selection.
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "job_engine::tests::decider_selection_recovery::changed_environment_model_recovers_after_restart",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env(worker::SUPERVISOR_TRAE_MODEL_ENV, " new-model ")
        .status()
        .unwrap();
    assert!(result.success());
}
