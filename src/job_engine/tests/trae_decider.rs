use super::*;

#[test]
fn trae_decider_uses_a_bounded_read_only_run_and_the_selected_model() {
    let (mut fx, _) = marker_fx(Tier::Autopilot, |_| {});
    set_decider_engine(&fx, Engine::Trae);
    set_decider_model(&fx, Some("trae-test-model"));
    let seq = start_consult(&mut fx);
    let argv = consult_argv(&fx, seq);
    assert!(argv.windows(2).any(|w| w == ["traex", "exec"]));
    assert!(argv.windows(2).any(|w| w == ["-s", "read-only"]));
    assert!(argv.windows(2).any(|w| w == ["-m", "trae-test-model"]));
    assert_eq!(argv[0], "timeout");
    assert!(argv.contains(&"--ephemeral".into()));
    assert!(!argv.iter().any(|arg| arg.contains("bypass")));
    assert!(
        argv.last()
            .unwrap()
            .contains(crate::skills::DECIDER_SKILL_MD.trim())
    );
    assert_eq!(
        fx.sched.advise.as_ref().unwrap().verdict_path.as_ref(),
        Some(&fx.paths.advice_last_message(seq))
    );
    let audit = ledger(&fx).decider_runs.pop().unwrap();
    assert_eq!(audit.engine, Engine::Trae);
    assert_eq!(audit.model.as_deref(), Some("trae-test-model"));
}

#[test]
fn trae_decider_validates_the_final_message_and_ignores_log_chatter() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    set_decider_engine(&fx, Engine::Trae);
    fx.driver.set_tail(&sess, BUSY_PANE);
    let seq = start_consult(&mut fx);
    let nonce = consult_nonce(&fx, seq);
    finish_consult(&fx, seq, "not a verdict", 0);
    std::fs::write(
        fx.paths.advice_last_message(seq),
        serde_json::json!({
            "nonce": nonce, "action": "select_option", "option_index": 1,
            "reason": "dprint is already vendored, which keeps the tooling consistent"
        })
        .to_string(),
    )
    .unwrap();
    fx.clock.set(START + SUPERVISOR_POLL_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let saved = ledger(&fx);
    assert!(saved.pending_context.unwrap().contains("option 2 — dprint"));
    assert_eq!(saved.digest.auto_flow, 1);
    assert!(fx.driver.sent_keys().is_empty());
    assert!(!fx.paths.advice_last_message(seq).exists());
}

#[test]
fn trae_decider_failures_keep_the_decision_for_a_human() {
    for failure in ["missing", "invalid", "exit", "timeout"] {
        let (mut fx, _) = marker_fx(Tier::Autopilot, |_| {});
        set_decider_engine(&fx, Engine::Trae);
        let seq = start_consult(&mut fx);
        if failure != "timeout" {
            finish_consult(
                &fx,
                seq,
                "untrusted log",
                if failure == "exit" { 1 } else { 0 },
            );
        }
        if failure == "invalid" {
            std::fs::write(fx.paths.advice_last_message(seq), "{}").unwrap();
        }
        fx.clock.set(
            START
                + if failure == "timeout" {
                    SUPERVISOR_TIMEOUT_S
                } else {
                    SUPERVISOR_POLL_S
                },
        );
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
        let saved = ledger(&fx);
        assert!(matches!(saved.run, JobRun::Blocked { .. }), "{failure}");
        assert!(saved.pending_context.is_none(), "{failure}");
        assert!(fx.driver.sent_keys().is_empty(), "{failure}");
    }
}
