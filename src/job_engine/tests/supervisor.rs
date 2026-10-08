//! The consult itself: what gets spawned, what a validated verdict does, the freeze
//! that survives a human attach, and the cases where no consult is started at all.

use super::*;
use crate::state::RiskClass;

// ---------------------------------------------------------------------------------
// m20 — the LLM SUPERVISOR seam.
//
// All of these drive the REAL scheduler over a `FakeDriver`, but the consult's
// done-signal and tee'd log are REAL FILES written exactly as the wrapper script
// writes them, so the observe/reap/validate path is the production one. What a fake
// cannot establish is that a control byte in a reply reaches the pane as a KEYSTROKE —
// that is a tmux fact, and it is pinned by the `#[ignore]`d real-tmux acceptance test
// in `tests/integration/`.
// ---------------------------------------------------------------------------------

#[test]
fn the_consult_system_prompt_carries_the_decider_skill() {
    // Milestone E: the decider skill is appended to the consult's `--system-prompt` (the consult
    // runs `--bare`, so a file-based skill cannot reach it). The C-hardened base prompt stays
    // byte-unchanged; the decider body is additive after it.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    let argv = consult_argv(&fx, seq);
    let sys = argv
        .iter()
        .position(|a| a == "--system-prompt")
        .map(|i| argv[i + 1].clone())
        .expect("the consult argv carries a --system-prompt");
    assert!(
        sys.contains("You are the SUPERVISOR"),
        "keeps the C-hardened base prompt: {sys}"
    );
    assert!(
        sys.contains(crate::skills::DECIDER_SKILL_MD.trim()),
        "appends the decider skill to the system prompt"
    );
}

#[test]
fn a_consultable_auto_flow_stop_spawns_a_detached_pinned_read_only_consult() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    let argv = consult_argv(&fx, seq);
    let joined = argv.join(" ");

    assert!(
        !argv.iter().any(|arg| arg.starts_with("ECC_GATEGUARD=")),
        "supervisor inherits host policy: {argv:?}"
    );
    // Bounded at the SHELL level as well as by the harness deadline.
    assert!(
        joined.contains(&format!(
            "timeout -k {SUPERVISOR_KILL_GRACE_S} {SUPERVISOR_SHELL_TIMEOUT_S} claude"
        )),
        "{joined}"
    );
    // The model is pinned EXPLICITLY. Verified live: an alias silently falls back to
    // the ambient model on a Bedrock deployment, which is the cost blowout this exists
    // to prevent.
    assert!(
        joined.contains(&format!("--model {}", worker::SUPERVISOR_MODEL)),
        "{joined}"
    );
    assert!(joined.contains("--bare"), "{joined}");
    // READ-ONLY, not tool-less: `plan` refuses every write, and the explicit deny list that
    // used to accompany it is GONE. A supervisor that cannot read cannot check the claim it is
    // being asked about, so it escalated to the human for want of a look — the opposite of
    // what autopilot is for. The write ban stays: the main agent is editing this tree.
    assert!(joined.contains("--permission-mode plan"), "{joined}");
    assert!(
        !joined.contains("--disallowed-tools"),
        "the supervisor must not be denied its tools: {joined}"
    );
    assert!(joined.contains("--output-format json"), "{joined}");
    assert!(joined.contains("--json-schema"), "{joined}");
    // NO spend cap by default. A ceiling does not surface as "too expensive" — it TRUNCATES
    // the consult, which the harness reads as a missing reply, which becomes a `Capability`
    // escalation and a step toward the latch. So a default cap silently converts "thinking"
    // into "wake the human". Opt in with `PM_SUPERVISOR_BUDGET_USD`.
    assert!(
        !joined.contains("--max-budget-usd"),
        "a spend cap must be opt-in: {joined}"
    );
    assert!(joined.contains("--no-session-persistence"), "{joined}");
    // NOT the phase-worker shape: a token stream is the wrong output entirely.
    assert!(!joined.contains("stream-json"), "{joined}");
    // The prompt trails `--`, carries the nonce, the goal, and the options BY INDEX.
    let n = argv.len();
    assert_eq!(argv[n - 2], "--");
    let prompt = &argv[n - 1];
    assert!(
        prompt.contains("[0] prettier") && prompt.contains("[1] dprint"),
        "{prompt}"
    );
    assert!(prompt.contains("dprint is already vendored"), "{prompt}");
    assert!(prompt.contains("UNTRUSTED DATA"), "{prompt}");

    // The consult is its OWN detached session, never the worker's: `ensure_session`
    // was not used for it (that would mint/resume the worker's conversation_id).
    assert_eq!(sup_session(&fx, seq).split('-').next(), Some("pmsup"));
    assert_ne!(sup_session(&fx, seq), sess);
    assert!(
        fx.driver.launched().is_empty(),
        "a consult must never go through launch_interactive: {:?}",
        fx.driver.launched()
    );
}

#[test]
fn the_consult_reads_directive_md_fresh_and_omits_it_when_absent() {
    // Task 4: `directive.md` is read fresh into `Consult.directive` at spawn, exactly like the
    // goal. The field is a local of `spawn_advice`, so we pin it through its only downstream
    // effect — the consult prompt — which is the same seam the goal read is pinned through
    // (`a_fat_brief_is_clamped...`). Present ⇒ its (clamped) text rides the consult inside the
    // TRUSTED DIRECTIVE fence; absent ⇒ the field is empty and the fence is omitted, so the
    // consult runs on goal+question as before. `advise_binary` is seeded so the spawn does not
    // depend on a real `claude` on PATH (mirrors `a_missing_claude_binary_latches`).

    // Absent: no `directive.md` ⇒ no DIRECTIVE fence.
    let (mut bare, _s) = marker_fx(Tier::Autopilot, |_| {});
    bare.sched.advise_binary = Some(true);
    let seq = start_consult(&mut bare);
    let prompt = consult_argv(&bare, seq).last().cloned().unwrap_or_default();
    assert!(
        !prompt.contains("STANDING OPERATING CONSTRAINT"),
        "absent directive.md ⇒ the DIRECTIVE fence is omitted: {prompt}"
    );

    // Present: the directive is read fresh into the consult and framed as the trusted constraint.
    let (mut fx, _s2) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.advise_binary = Some(true);
    std::fs::write(fx.paths.directive(), "stop auto-approving test edits\n").unwrap();
    let seq = start_consult(&mut fx);
    let prompt = consult_argv(&fx, seq).last().cloned().unwrap_or_default();
    assert!(
        prompt.contains("stop auto-approving test edits"),
        "the directive text rides the consult: {prompt}"
    );
    assert!(
        prompt.contains("STANDING OPERATING CONSTRAINT"),
        "and is framed as the trusted restrictive directive: {prompt}"
    );
}

#[test]
fn a_validated_verdict_replaces_the_canned_string_with_goal_aware_content() {
    // THE FEATURE. The old note approved without saying WHAT to do; this one names the
    // option the goal implies — and names it from the HARNESS's option list.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    let nonce = consult_nonce(&fx, seq);
    let unrelated = fx.paths.steps_dir().join("legacy-wake.log");
    std::fs::create_dir_all(fx.paths.steps_dir()).unwrap();
    std::fs::write(&unrelated, "keep").unwrap();
    std::fs::write(fx.paths.advice_last_message(seq), "stale codex verdict").unwrap();
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":1,\
             \"option\":\"rm -rf /\",\"reason\":\"dprint is already vendored\"}}"
        )),
        0,
    );
    std::fs::write(
        path_with_suffix(&fx.paths.advice_done_signal(seq), ".code"),
        "0",
    )
    .unwrap();
    std::fs::write(
        path_with_suffix(&fx.paths.advice_done_signal(seq), ".tmp"),
        "0",
    )
    .unwrap();
    // The reap tick applies the verdict to `pending_context` and falls through to the
    // ordinary pane classify; the pane is Busy, so nothing is typed yet.
    fx.driver.set_tail(&sess, BUSY_PANE);
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Monitoring { .. }), "got {out:?}");
    let l = ledger(&fx);
    assert!(
        !matches!(l.run, JobRun::Blocked { .. }),
        "a usable verdict must not escalate: {:?}",
        l.run
    );
    let ctx = l.pending_context.clone().unwrap_or_default();
    assert!(ctx.contains("option 2 — dprint"), "{ctx}");
    assert!(ctx.contains("dprint is already vendored"), "{ctx}");
    assert!(
        !ctx.contains("Auto-approved"),
        "the canned string must be GONE when a supervisor answered: {ctx}"
    );
    assert!(
        !ctx.contains("rm -rf"),
        "Rule 1: supervisor-supplied option text must never survive: {ctx}"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "busy pane defers delivery"
    );
    assert_eq!(l.digest.auto_flow, 1);
    assert_eq!(l.decisions.len(), 1);
    assert_eq!(l.decisions[0].kind, job::DecisionKind::AutoFlow);
    assert!(!fx.paths.advice_done_signal(seq).exists());
    assert!(!path_with_suffix(&fx.paths.advice_done_signal(seq), ".code").exists());
    assert!(!path_with_suffix(&fx.paths.advice_done_signal(seq), ".tmp").exists());
    assert!(!fx.paths.advice_log(seq).exists());
    assert!(!fx.paths.advice_last_message(seq).exists());
    assert!(!advice_wrapper(&fx, seq).exists());
    assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "keep");

    // The next confirmed idle nudge delivers it through the ONE carrier and clears it.
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.set(START + SUPERVISOR_POLL_S + BUSY_RECHECK_S);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    let sent = fx.driver.sent_keys();
    assert_eq!(sent.len(), 1, "delivered once");
    assert!(sent[0].1.contains("option 2 — dprint"), "{}", sent[0].1);
    assert!(
        ledger(&fx).pending_context.is_none(),
        "pending_context is cleared ONLY by the tick that delivered it — and this one did"
    );
}

#[test]
fn a_validated_dialog_verdict_selects_the_harness_option_once_without_typing_prose() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_dialog_consult(&mut fx, &sess);
    let nonce = consult_nonce(&fx, seq);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":1,\
             \"option\":\"rm -rf /\",\"reason\":\"dprint is already vendored\"}}"
        )),
        0,
    );
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + SUPERVISOR_POLL_S + 300
        }
    );
    assert_eq!(
        fx.driver.applied_dialog_selections(),
        vec![(sess.clone(), vec![1])],
        "move from the captured highlight to the validated harness index"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "raw selection must not retype question, option, or model prose"
    );
    let l = ledger(&fx);
    assert!(l.pending_context.is_none());
    assert!(l.advice_inflight.is_none());
    assert_eq!(
        l.nudged_at_seq,
        Some(l.last_marker_seq),
        "the selected dialog starts a worker turn and must arm the no-double-input watermark"
    );
    assert!(l.events.iter().any(|e| matches!(
        &e.kind,
        AutopilotEventKind::SupervisorResolved(Some(s)) if s.contains("dprint")
    )));
    assert_eq!(
        l.decider_runs[0].outcome,
        job::DeciderOutcome::Resolved {
            answer: "option 2 — dprint".into(),
            reason: "dprint is already vendored".into(),
        }
    );
    assert_eq!(
        l.decider_runs[0].finished_at,
        Some(START + SUPERVISOR_POLL_S)
    );

    fx.clock.set(START + SUPERVISOR_POLL_S + 5);
    let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.driver.applied_dialog_selections().len(), 1);
}

#[test]
fn marker_consult_lifecycle_is_persisted_for_the_audit_view() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);

    let started = ledger(&fx).decider_runs;
    assert_eq!(started.len(), 1);
    let run = &started[0];
    assert_eq!(run.seq, seq);
    assert_eq!(run.started_at, START);
    assert_eq!(run.finished_at, None);
    assert_eq!(run.engine, Engine::Claude);
    assert_eq!(
        run.model.as_deref(),
        Some(worker::SUPERVISOR_MODEL),
        "audit records the actual resolved model, not only the config's optional override"
    );
    assert_eq!(run.target, job::DeciderTarget::Marker);
    assert_eq!(run.question, "Which formatter for the changelog?");
    assert_eq!(run.options, ["prettier", "dprint"]);
    assert_eq!(run.policy.kind, StopKind::Ambiguity);
    assert_eq!(run.policy.labelled_risk, RiskClass::Low);
    assert_eq!(run.policy.effective_risk, RiskClass::Medium);
    assert_eq!(run.outcome, job::DeciderOutcome::Consulting);

    let nonce = consult_nonce(&fx, seq);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"dprint is already vendored\"}}"
        )),
        0,
    );
    fx.driver.set_tail(&sess, BUSY_PANE);
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    let finished = ledger(&fx).decider_runs;
    assert_eq!(finished.len(), 1, "completion updates the started record");
    assert_eq!(finished[0].finished_at, Some(START + SUPERVISOR_POLL_S));
    assert_eq!(
        finished[0].outcome,
        job::DeciderOutcome::Resolved {
            answer: "option 2 — dprint".into(),
            reason: "dprint is already vendored".into(),
        }
    );
}

#[test]
fn dialog_consult_records_its_fixed_policy_and_target() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_dialog_consult(&mut fx, &sess);

    let runs = ledger(&fx).decider_runs;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].seq, seq);
    assert_eq!(runs[0].target, job::DeciderTarget::Dialog);
    assert_eq!(
        runs[0].question,
        "Which formatter should I use for the changelog?"
    );
    assert_eq!(runs[0].policy.kind, StopKind::Ambiguity);
    assert_eq!(runs[0].policy.labelled_risk, RiskClass::Medium);
    assert_eq!(runs[0].policy.effective_risk, RiskClass::Medium);
    assert_eq!(runs[0].outcome, job::DeciderOutcome::Consulting);
}

#[test]
fn refused_and_failed_consults_leave_terminal_audit_outcomes() {
    let (mut refused, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let refused_seq = start_consult(&mut refused);
    let nonce = consult_nonce(&refused, refused_seq);
    finish_consult(
        &refused,
        refused_seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"refuse\",\
             \"reason\":\"the goal does not determine this choice\"}}"
        )),
        0,
    );
    refused.clock.set(START + SUPERVISOR_POLL_S);
    let _ = refused.sched.tick(&refused.driver, &refused.clock).unwrap();
    let refused_runs = ledger(&refused).decider_runs;
    assert!(matches!(
        &refused_runs[0].outcome,
        job::DeciderOutcome::Refused { reason }
            if reason.contains("the goal does not determine this choice")
    ));
    assert_eq!(refused_runs[0].finished_at, Some(START + SUPERVISOR_POLL_S));

    let (mut failed, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let failed_seq = start_consult(&mut failed);
    finish_consult(&failed, failed_seq, "decider failed", 17);
    failed.clock.set(START + SUPERVISOR_POLL_S);
    let _ = failed.sched.tick(&failed.driver, &failed.clock).unwrap();
    let failed_runs = ledger(&failed).decider_runs;
    assert_eq!(
        failed_runs[0].outcome,
        job::DeciderOutcome::Failed {
            reason: "the supervisor produced no result: the consult exited 17".into(),
        }
    );
    assert_eq!(failed_runs[0].finished_at, Some(START + SUPERVISOR_POLL_S));
}

#[test]
fn decider_audit_is_bounded_capped_and_finishes_the_newest_reused_sequence() {
    let mut state = AgentLoopState::fresh(Engine::Claude, Some(300), START);
    for index in 0..job::DECIDER_RUNS_MAX + 2 {
        state.record_decider_run(job::DeciderRun {
            seq: index as u64,
            started_at: START + index as i64,
            finished_at: None,
            engine: Engine::Claude,
            model: None,
            target: job::DeciderTarget::Marker,
            question: "q".repeat(1_000),
            options: vec!["o".repeat(1_000)],
            reported_kind: None,
            effect: None,
            policy: job::DeciderPolicy {
                kind: StopKind::Ambiguity,
                labelled_risk: RiskClass::Low,
                effective_risk: RiskClass::Medium,
            },
            outcome: job::DeciderOutcome::Consulting,
        });
    }
    assert_eq!(state.decider_runs.len(), job::DECIDER_RUNS_MAX);
    assert_eq!(state.decider_runs[0].seq, 2, "oldest audits are dropped");
    assert!(state.decider_runs[0].question.chars().count() <= 400);
    assert!(state.decider_runs[0].options[0].chars().count() <= 200);

    state.record_decider_run(job::DeciderRun {
        seq: 7,
        started_at: START + 1_000,
        finished_at: None,
        engine: Engine::Codex,
        model: None,
        target: job::DeciderTarget::Dialog,
        question: "new reuse".into(),
        options: Vec::new(),
        reported_kind: None,
        effect: None,
        policy: job::DeciderPolicy {
            kind: StopKind::Ambiguity,
            labelled_risk: RiskClass::Medium,
            effective_risk: RiskClass::Medium,
        },
        outcome: job::DeciderOutcome::Consulting,
    });
    assert!(state.finish_decider_run(
        7,
        START + 1_010,
        job::DeciderOutcome::Resolved {
            answer: "latest".into(),
            reason: "verified".into(),
        }
    ));
    let latest = state.decider_runs.last().unwrap();
    assert_eq!(latest.started_at, START + 1_000);
    assert_eq!(latest.finished_at, Some(START + 1_010));
    assert!(matches!(
        &latest.outcome,
        job::DeciderOutcome::Resolved { answer, .. } if answer == "latest"
    ));

    let expected = state.decider_runs.len();
    let mut json = serde_json::to_value(&state).unwrap();
    json.get_mut("decider_runs")
        .and_then(serde_json::Value::as_array_mut)
        .unwrap()
        .push(serde_json::json!({"future_shape": true}));
    let loaded: AgentLoopState = serde_json::from_value(json).unwrap();
    assert_eq!(
        loaded.decider_runs.len(),
        expected,
        "one malformed cosmetic audit row never takes down the operational ledger"
    );
}

#[test]
fn a_multi_select_verdict_maps_one_combination_to_checkbox_targets() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(
        &fx,
        "Run integration and real-tmux coverage; unit coverage already passed.",
    );
    fx.driver.set_tail(&sess, MULTI_CHOICE_DIALOG_PANE);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + SUPERVISOR_POLL_S
        }
    );
    let prompt = consult_argv(&fx, 1).last().cloned().unwrap_or_default();
    assert!(prompt.contains("[5] integration + real tmux"), "{prompt}");
    assert!(!prompt.contains("Type something"), "{prompt}");
    let nonce = consult_nonce(&fx, 1);
    finish_consult(
        &fx,
        1,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":5,\
             \"reason\":\"those are the requested remaining layers\"}}"
        )),
        0,
    );
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Monitoring { .. }));
    assert_eq!(
        fx.driver.applied_dialog_selections(),
        vec![(sess, vec![1, 2])]
    );
    assert!(
        ledger(&fx)
            .last_status
            .as_deref()
            .is_some_and(|status| status.contains("options 2, 3"))
    );
}

#[test]
fn a_dialog_that_changes_while_the_decider_thinks_never_receives_the_stale_choice() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_dialog_consult(&mut fx, &sess);
    let nonce = consult_nonce(&fx, seq);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"for the old question\"}}"
        )),
        0,
    );
    fx.driver.set_tail(&sess, CHANGED_CHOICE_DIALOG_PANE);
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Monitoring { .. }));
    assert!(fx.driver.selected_dialog_options().is_empty());
    assert!(
        ledger(&fx).advice_inflight.is_none(),
        "the stale consult is discarded rather than recovered as prose"
    );
}

#[test]
fn a_held_input_lock_prevents_dialog_selection_without_losing_the_verdict() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_dialog_consult(&mut fx, &sess);
    let nonce = consult_nonce(&fx, seq);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"vendored\"}}"
        )),
        0,
    );
    let _held = crate::lease::try_acquire(&fx.paths.input_lock())
        .unwrap()
        .expect("test owns input.lock");
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Monitoring { .. }));
    assert!(fx.driver.selected_dialog_options().is_empty());
    assert!(
        ledger(&fx).advice_inflight.is_some(),
        "the validated verdict remains durable for a later unlocked retry"
    );
}

#[test]
fn a_dialog_key_delivery_failure_escalates_the_real_question() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_dialog_consult(&mut fx, &sess);
    let nonce = consult_nonce(&fx, seq);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"vendored\"}}"
        )),
        0,
    );
    fx.driver.fail_select_dialog(&sess, true);
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    let l = ledger(&fx);
    assert_eq!(
        l.open_stops[0].question.as_deref(),
        Some("Which formatter should I use for the changelog?")
    );
    assert!(fx.driver.selected_dialog_options().is_empty());
}

#[test]
fn a_dialog_that_cannot_prove_interactivity_never_receives_enter() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_dialog_consult(&mut fx, &sess);
    let nonce = consult_nonce(&fx, seq);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"vendored\"}}"
        )),
        0,
    );
    fx.driver.fail_verify_dialog(&sess, true);
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    assert!(fx.driver.selected_dialog_options().is_empty());
    assert!(fx.driver.applied_dialog_selections().is_empty());
}

#[test]
fn a_restart_reconsults_a_live_dialog_instead_of_recovering_it_as_prose() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    let first = start_dialog_consult(&mut fx, &sess);
    assert_eq!(first, 1);
    seed_inflight_advice_artifacts(&fx, first);
    assert!(fx.driver.is_alive(&sup_session(&fx, first)).unwrap());

    let work_dir = fx.sched.work_dir.clone();
    let mut restarted = JobScheduler::new(SESSION_ID, &work_dir, SESSION_ID, Engine::Claude, None);
    restarted.set_decider_binary_available(true);
    restarted.no_progress_threshold = 0;
    restarted.set_claude_home(&fx.claude_home);
    fx.sched = restarted;
    fx.clock.set(START + SUPERVISOR_POLL_S);

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + SUPERVISOR_POLL_S
        }
    );
    let l = ledger(&fx);
    assert!(l.pending_context.is_none());
    assert!(l.advice_inflight.is_none());
    assert!(matches!(
        &l.decider_runs[0].outcome,
        job::DeciderOutcome::Interrupted { reason } if reason.contains("restarted")
    ));
    assert!(!fx.driver.is_alive(&sup_session(&fx, first)).unwrap());
    assert!(!fx.paths.advice_log(first).exists());
    assert!(!fx.paths.advice_last_message(first).exists());
    assert!(!advice_wrapper(&fx, first).exists());

    fx.clock.set(START + SUPERVISOR_POLL_S + 1);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + 2 * SUPERVISOR_POLL_S + 1
        }
    );
    let l = ledger(&fx);
    let parked = l
        .advice_inflight
        .expect("the live prompt starts a fresh consult");
    assert_eq!(parked.seq, 2, "the pre-restart consult seq is not reused");
    assert!(parked.pane_dialog);
}

#[test]
fn failed_advice_artifact_cleanup_is_retried() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    finish_consult(&fx, seq, "unused", 0);
    std::fs::remove_file(fx.paths.advice_log(seq)).unwrap();
    std::fs::create_dir(fx.paths.advice_log(seq)).unwrap();
    fx.clock.set(START + SUPERVISOR_POLL_S);

    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert!(fx.paths.advice_log(seq).is_dir());

    std::fs::remove_dir(fx.paths.advice_log(seq)).unwrap();
    let current = ledger(&fx);
    fx.sched.cleanup_completed_advice_artifacts(&current);

    assert!(!fx.paths.steps_dir().exists());
}

#[test]
fn advice_cleanup_refuses_a_symlinked_steps_directory() {
    use std::os::unix::fs::symlink;

    let fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let outside = fx._dir.path().join("outside-steps");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("advice-9.log"), "keep").unwrap();
    std::fs::create_dir_all(fx.paths.steps_dir().parent().unwrap()).unwrap();
    symlink(&outside, fx.paths.steps_dir()).unwrap();

    assert!(!fx.sched.cleanup_advice_artifacts(9));
    assert_eq!(
        std::fs::read_to_string(outside.join("advice-9.log")).unwrap(),
        "keep"
    );
}

#[test]
fn completed_consult_keeps_its_debt_when_termination_fails() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    let nonce = consult_nonce(&fx, seq);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"vendored\"}}"
        )),
        0,
    );
    let supervisor = sup_session(&fx, seq);
    fx.driver.fail_terminate(&supervisor);
    fx.clock.set(START + SUPERVISOR_POLL_S);

    assert!(fx.sched.tick(&fx.driver, &fx.clock).is_err());
    assert!(fx.sched.advise.is_some());
    assert!(ledger(&fx).advice_inflight.is_some());

    fx.driver.allow_terminate(&supervisor);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert!(ledger(&fx).advice_inflight.is_none());
}

#[test]
fn interrupt_advice_restores_ownership_when_ledger_save_fails() {
    use std::os::unix::fs::PermissionsExt;

    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    let state_dir = fx.paths.state_dir();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let result =
        fx.sched
            .interrupt_advice(&fx.driver, START + SUPERVISOR_POLL_S, "test interruption");

    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err());
    assert_eq!(
        fx.sched.advise.as_ref().map(|inflight| inflight.parked.seq),
        Some(seq)
    );
    assert!(ledger(&fx).advice_inflight.is_some());
}

#[test]
fn a_free_text_verdict_is_delivered_when_no_options_were_enumerated() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(&fx, "Keep the docs terse.");
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[{"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low","question":"What should the new section be called?"}]}"#,
    );
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    let nonce = consult_nonce(&fx, 1);
    let prompt = consult_argv(&fx, 1).last().cloned().unwrap_or_default();
    assert!(prompt.contains("OPTIONS: (none enumerated)"), "{prompt}");
    finish_consult(
        &fx,
        1,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"answer\",\"text\":\"Call it 'Migration \
             notes'.\",\"reason\":\"terse, matches the goal\"}}"
        )),
        0,
    );
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let ctx = ledger(&fx).pending_context.clone().unwrap_or_default();
    assert!(ctx.contains("Migration notes"), "{ctx}");
    assert!(!ctx.contains("Auto-approved"), "{ctx}");
}

#[test]
fn the_kill_switch_escalates_instead_of_granting_without_a_decider() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.set_supervisor_enabled(false);
    fx.driver.set_tail(&sess, BUSY_PANE);
    write_goal(&fx, "Keep the changelog tooling consistent.");
    write_marker(&fx, AUTOFLOW_ASKS);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
    assert_eq!(fx.driver.spawn_count(), 0, "no consult when switched off");
    let l = ledger(&fx);
    assert!(matches!(l.run, JobRun::Blocked { .. }));
    assert!(l.pending_context.is_none());
    assert_eq!(l.open_stops[0].kind, StopKind::Capability);
}

#[test]
fn a_fresh_marker_bump_abandons_an_in_flight_consult() {
    // The agent moved on, so the pending opinion is moot: it is dropped and its session
    // killed (no `pmsup-` leak), and a late reply is unreachable — the nonce and option
    // list it would have to validate against are gone.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    let seq = start_consult(&mut fx);
    assert!(fx.driver.is_alive(&sup_session(&fx, seq)).unwrap_or(false));
    // A `working` bump arrives before the consult answered.
    fx.clock.set(START + SUPERVISOR_POLL_S);
    write_marker(&fx, r#"{"seq":50,"state":"working","status":"moved on"}"#);
    backdate_marker(&fx, 2);
    let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        !fx.driver.is_alive(&sup_session(&fx, seq)).unwrap_or(true),
        "the abandoned consult's session must be killed"
    );
    let abandoned = ledger(&fx);
    assert_eq!(abandoned.last_status.as_deref(), Some("moved on"));
    assert!(matches!(
        &abandoned.decider_runs[0].outcome,
        job::DeciderOutcome::Interrupted { reason }
            if reason.contains("agent reported a newer state")
    ));
    assert_eq!(
        abandoned.decider_runs[0].finished_at,
        Some(START + SUPERVISOR_POLL_S)
    );
    // Even if the stale consult now "answers", nothing is applied from it.
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{}\",\"action\":\"select_option\",\"option_index\":0,\
             \"reason\":\"stale\"}}",
            consult_nonce(&fx, seq)
        )),
        0,
    );
    fx.clock.set(START + 400);
    let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let ctx = ledger(&fx).pending_context.clone().unwrap_or_default();
    assert!(
        !ctx.contains("stale") && !ctx.contains("prettier"),
        "a stale consult must never be applied: {ctx:?}"
    );
}

#[test]
fn a_question_less_auto_flow_stop_is_not_consultable_and_escalates() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    write_goal(&fx, "Ship it.");
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[{"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low"}]}"#,
    );
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.driver.spawn_count(), 0);
    assert!(matches!(out, JobTick::Escalated(_)));
    assert!(ledger(&fx).pending_context.is_none());
    assert_eq!(ledger(&fx).open_stops[0].kind, StopKind::Capability);
}

#[test]
fn no_auto_flow_stops_leave_the_supervisor_unengaged() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let next = ledger(&fx);

    let out = fx
        .sched
        .spawn_advice(&fx.driver, START, &next, &[], Engine::Claude, None)
        .unwrap();

    assert_eq!(out, AdviceSpawn::Empty);
    assert_eq!(fx.driver.spawn_count(), 0);
    assert!(fx.sched.advise.is_none());
}

#[test]
fn co_reported_auto_flow_stops_are_consulted_sequentially() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |ledger| {
        ledger.start_turn(
            START - 10,
            job::TurnTrigger::Heartbeat {
                pending_context: false,
                marker_recovery: false,
            },
        );
    });
    write_goal(&fx, "Keep formatting consistent and sort imports.");
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","status":"formatter config is loaded and the tree is clean","next_step":"apply the chosen formatter and sort imports","stops":[
            {"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low","question":"Which formatter?","options":["prettier","dprint"]},
            {"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low","question":"Sort imports?","options":["yes","no"]}
        ]}"#,
    );

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Monitoring { .. }), "got {out:?}");
    assert_eq!(fx.driver.spawn_count(), 1);
    let nonce = consult_nonce(&fx, 1);
    finish_consult(
        &fx,
        1,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":0,\
             \"reason\":\"prettier matches the goal\"}}"
        )),
        0,
    );
    fx.driver.set_tail(&sess, BUSY_PANE);
    fx.clock.set(START + SUPERVISOR_POLL_S);

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Monitoring { .. }), "got {out:?}");
    assert_eq!(fx.driver.spawn_count(), 2);
    let second_prompt = consult_argv(&fx, 2).last().cloned().unwrap_or_default();
    assert!(
        second_prompt.contains("worker's latest reported result: formatter config is loaded"),
        "the sibling decision should retain the accepted worker result: {second_prompt}"
    );
    assert!(
        second_prompt
            .contains("agent's stated next step: apply the chosen formatter and sort imports"),
        "the sibling decision should retain the worker's next step: {second_prompt}"
    );
    assert!(
        !second_prompt.contains("the session supervisor resolved a low-stakes decision"),
        "supervisor audit prose must not replace worker progress: {second_prompt}"
    );
    let current = ledger(&fx);
    assert_eq!(current.decider_runs.len(), 2);
    assert!(matches!(
        current.decider_runs[0].outcome,
        crate::job::DeciderOutcome::Resolved { .. }
    ));
    assert!(matches!(
        current.decider_runs[1].outcome,
        crate::job::DeciderOutcome::Consulting
    ));
    assert!(matches!(
        current.turn_trace.last().map(|turn| &turn.outcome),
        Some(job::TurnOutcome::Reported {
            disposition: job::TurnDisposition::Reviewing,
            ..
        })
    ));
    assert!(
        current
            .pending_context
            .as_deref()
            .is_some_and(|text| text.contains("prettier")),
        "{:?}",
        current.pending_context
    );

    let nonce = consult_nonce(&fx, 2);
    finish_consult(
        &fx,
        2,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"select_option\",\"option_index\":0,\
             \"reason\":\"sorting imports matches the goal\"}}"
        )),
        0,
    );
    fx.clock.set(START + 2 * SUPERVISOR_POLL_S);

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Monitoring { .. }), "got {out:?}");
    let current = ledger(&fx);
    assert!(!matches!(current.run, JobRun::Blocked { .. }));
    assert_eq!(current.decider_runs.len(), 2);
    assert!(
        current
            .decider_runs
            .iter()
            .all(|run| matches!(run.outcome, crate::job::DeciderOutcome::Resolved { .. }))
    );
    assert!(matches!(
        current.turn_trace.last().map(|turn| &turn.outcome),
        Some(job::TurnOutcome::Reported {
            disposition: job::TurnDisposition::AutoFlow,
            ..
        })
    ));
    let pending = current.pending_context.unwrap_or_default();
    assert!(pending.contains("prettier"), "{pending}");
    assert!(pending.contains("yes"), "{pending}");
}

#[test]
fn a_human_owned_stop_pauses_but_does_not_escalate_its_ordinary_sibling() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(
        &fx,
        "Prepare the release notes, but leave publishing to me.",
    );
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[
            {"kind":"publish","effect":{"scope":"external","reversibility":"irreversible","authority":"ordinary"},"risk_class":"hard","question":"Publish the release?","options":["publish","wait"]},
            {"kind":"ambiguity","effect":{"scope":"unknown","reversibility":"reversible","authority":"ordinary"},"risk_class":"low","question":"Which release-note format?","options":["keep-a-changelog","plain markdown"]}
        ]}"#,
    );

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    let JobTick::Escalated(ids) = out else {
        panic!("expected only the human-owned stop to escalate");
    };
    assert_eq!(ids.len(), 1);
    let current = ledger(&fx);
    assert_eq!(current.open_stops.len(), 1);
    assert_eq!(current.open_stops[0].kind, StopKind::Publish);
    assert_eq!(current.advice_queue.len(), 1);
    assert_eq!(fx.driver.spawn_count(), 0);

    fx.clock.set(START + 1);
    push_answer(&fx, &ids[0], Some("wait"), fx.clock.now());
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: START + 1 }
    );
    assert_eq!(fx.driver.spawn_count(), 0);

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Monitoring { .. }), "got {out:?}");
    assert_eq!(fx.driver.spawn_count(), 1);
    let current = ledger(&fx);
    assert!(current.open_stops.is_empty());
    assert!(matches!(
        current.decider_runs.last().map(|run| &run.outcome),
        Some(crate::job::DeciderOutcome::Consulting)
    ));
    assert!(
        current
            .pending_context
            .as_deref()
            .is_some_and(|text| text.contains("wait")),
        "{:?}",
        current.pending_context
    );
}

#[test]
fn an_unconsultable_decision_escalates_alone_then_the_valid_sibling_is_consulted() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    write_goal(&fx, "Keep repository formatting consistent.");
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[
            {"kind":"ambiguity","risk_class":"low"},
            {"kind":"ambiguity","risk_class":"low","question":"Which formatter?","options":["prettier","dprint"]}
        ]}"#,
    );

    let JobTick::Escalated(ids) = fx.sched.tick(&fx.driver, &fx.clock).unwrap() else {
        panic!("the malformed current decision should reach the human");
    };
    assert_eq!(ids.len(), 1);
    let current = ledger(&fx);
    assert_eq!(current.open_stops.len(), 1);
    assert_eq!(current.advice_queue.len(), 1);
    assert_eq!(fx.driver.spawn_count(), 0);

    fx.clock.set(START + 1);
    push_answer(
        &fx,
        &ids[0],
        Some("skip the malformed question"),
        fx.clock.now(),
    );
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.driver.spawn_count(), 1);
    assert!(matches!(
        ledger(&fx).decider_runs.last().map(|run| &run.outcome),
        Some(crate::job::DeciderOutcome::Consulting)
    ));
}

#[test]
fn a_refused_decision_escalates_alone_and_preserves_queued_siblings() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |ledger| {
        ledger.start_turn(
            START - 10,
            job::TurnTrigger::Heartbeat {
                pending_context: false,
                marker_recovery: false,
            },
        );
    });
    write_goal(&fx, "Keep formatting consistent and sort imports.");
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[
            {"kind":"ambiguity","risk_class":"low","question":"Which formatter?","options":["prettier","dprint"]},
            {"kind":"ambiguity","risk_class":"low","question":"Sort imports?","options":["yes","no"]}
        ]}"#,
    );
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    let nonce = consult_nonce(&fx, 1);
    finish_consult(
        &fx,
        1,
        &consult_reply(&format!(
            "{{\"nonce\":\"{nonce}\",\"action\":\"refuse\",\
             \"reason\":\"the goal does not choose a formatter\"}}"
        )),
        0,
    );
    fx.clock.set(START + SUPERVISOR_POLL_S);

    let JobTick::Escalated(ids) = fx.sched.tick(&fx.driver, &fx.clock).unwrap() else {
        panic!("the refused current decision should reach the human");
    };
    assert_eq!(ids.len(), 1);
    let refused = ledger(&fx);
    assert_eq!(refused.advice_queue.len(), 1);
    assert!(matches!(
        refused.turn_trace.last().map(|turn| &turn.outcome),
        Some(job::TurnOutcome::Reported {
            disposition: job::TurnDisposition::Escalated,
            ..
        })
    ));

    fx.clock.set(START + SUPERVISOR_POLL_S + 1);
    push_answer(&fx, &ids[0], Some("use dprint"), fx.clock.now());
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.driver.spawn_count(), 2);
    assert!(matches!(
        ledger(&fx).decider_runs.last().map(|run| &run.outcome),
        Some(crate::job::DeciderOutcome::Consulting)
    ));
}

#[test]
fn a_consult_session_that_dies_without_a_signal_escalates() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    let supervisor_session = sup_session(&fx, seq);
    fx.driver.set_alive(&supervisor_session, false);
    fx.clock.set(START + SUPERVISOR_POLL_S);

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
    let state = ledger(&fx);
    assert!(
        state
            .last_status
            .as_deref()
            .is_some_and(|status| status.contains("died without recording an exit code")),
        "{:?}",
        state.last_status
    );
    assert!(state.pending_context.is_none());
    assert!(state.advice_inflight.is_none());
}

#[test]
fn advise_step_without_a_consult_is_not_engaged() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let base = ledger(&fx);

    let out = fx.sched.advise_step(&fx.driver, START, &base).unwrap();

    assert!(matches!(out, AdviseStep::NotEngaged));
    assert_eq!(ledger(&fx), base);
}

#[test]
fn a_consult_that_exceeds_the_harness_deadline_escalates() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    fx.clock.set(START + SUPERVISOR_TIMEOUT_S);

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
    let state = ledger(&fx);
    assert!(
        state
            .last_status
            .as_deref()
            .is_some_and(|status| status.contains("no answer within")),
        "{:?}",
        state.last_status
    );
    assert!(
        !fx.driver.is_alive(&sup_session(&fx, seq)).unwrap_or(true),
        "the timed-out consult is reaped"
    );
    assert!(state.pending_context.is_none());
}

#[test]
fn a_nonzero_consult_exit_escalates_without_queuing_approval() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    finish_consult(&fx, seq, "decider failed", 17);
    fx.clock.set(START + SUPERVISOR_POLL_S);

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
    let state = ledger(&fx);
    assert!(
        state
            .last_status
            .as_deref()
            .is_some_and(|status| status.contains("consult exited 17")),
        "{:?}",
        state.last_status
    );
    assert!(state.pending_context.is_none());
    assert!(state.advice_inflight.is_none());
}

#[test]
fn abandonment_keeps_the_decider_debt_when_termination_fails() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    let supervisor = sup_session(&fx, seq);
    fx.driver.fail_terminate(&supervisor);

    assert!(fx.sched.abandon_advice(&fx.driver, &ledger(&fx)).is_err());
    assert!(fx.sched.advise.is_some());
    assert!(ledger(&fx).advice_inflight.is_some());

    fx.driver.allow_terminate(&supervisor);
    assert_eq!(
        fx.sched.abandon_advice(&fx.driver, &ledger(&fx)).unwrap(),
        Some(AbandonedAdvice {
            decider_seq: seq,
            report_seq: Some(1),
        })
    );
    assert!(fx.sched.advise.is_none());
}

#[test]
fn abandonment_terminates_a_recovered_orphan_before_clearing_its_debt() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = 41;
    let supervisor = sup_session(&fx, seq);
    fx.driver.set_alive(&supervisor, true);
    fx.sched.advise_orphaned = Some(job::ParkedAdvice {
        seq,
        stop_ids: vec!["stop-41".into()],
        pane_dialog: false,
    });

    assert_eq!(
        fx.sched.abandon_advice(&fx.driver, &ledger(&fx)).unwrap(),
        Some(AbandonedAdvice {
            decider_seq: seq,
            report_seq: None,
        })
    );
    assert!(!fx.driver.is_alive(&supervisor).unwrap());
    assert!(fx.sched.advise_orphaned.is_none());
}

#[test]
fn timeout_keeps_the_decider_debt_when_termination_fails() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    let supervisor = sup_session(&fx, seq);
    fx.driver.fail_terminate(&supervisor);
    fx.clock.set(START + SUPERVISOR_TIMEOUT_S);

    assert!(fx.sched.tick(&fx.driver, &fx.clock).is_err());
    assert!(fx.sched.advise.is_some());
    assert!(ledger(&fx).advice_inflight.is_some());

    fx.driver.allow_terminate(&supervisor);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert!(ledger(&fx).advice_inflight.is_none());
}

#[test]
fn a_malformed_verdict_escalates_without_queuing_approval() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    finish_consult(&fx, seq, "not a verdict", 0);
    fx.clock.set(START + SUPERVISOR_POLL_S);

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
    let state = ledger(&fx);
    assert_eq!(state.open_stops[0].kind, StopKind::Capability);
    assert!(state.pending_context.is_none());
    assert!(state.advice_inflight.is_none());
}

#[test]
fn an_unreadable_consult_signal_escalates_instead_of_poisoning_the_tick() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    std::fs::create_dir_all(fx.paths.advice_done_signal(seq)).unwrap();
    fx.clock.set(START + SUPERVISOR_POLL_S);

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
    let state = ledger(&fx);
    assert!(
        state
            .last_status
            .as_deref()
            .is_some_and(|status| status.contains("done-signal was unreadable")),
        "{:?}",
        state.last_status
    );
    assert!(state.pending_context.is_none());
    assert!(state.advice_inflight.is_none());
}

#[test]
fn unreadable_consult_signal_keeps_the_debt_when_termination_fails() {
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let seq = start_consult(&mut fx);
    std::fs::create_dir_all(fx.paths.advice_done_signal(seq)).unwrap();
    let supervisor = sup_session(&fx, seq);
    fx.driver.fail_terminate(&supervisor);
    fx.clock.set(START + SUPERVISOR_POLL_S);

    assert!(fx.sched.tick(&fx.driver, &fx.clock).is_err());
    assert!(ledger(&fx).advice_inflight.is_some());

    fx.driver.allow_terminate(&supervisor);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    assert!(ledger(&fx).advice_inflight.is_none());
}

#[test]
fn an_undelivered_human_answer_survives_a_supervisor_decision_landing_on_top() {
    // The m18 invariant, extended to the new writer: `pending_context` is APPENDED to,
    // never assigned over, so a human answer that is parked and undelivered is not
    // destroyed by a supervisor decision arriving a tick later. Both are things
    // somebody said.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.pending_context = Some("the human said: prefer whatever is vendored".into());
    });
    fx.driver.set_tail(&sess, BUSY_PANE);
    let seq = start_consult_keeping_context(&mut fx);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"vendored already\"}}",
            consult_nonce(&fx, seq)
        )),
        0,
    );
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let ctx = ledger(&fx).pending_context.clone().unwrap_or_default();
    assert!(
        ctx.contains("the human said"),
        "human answer dropped: {ctx}"
    );
    assert!(ctx.contains("dprint"), "supervisor decision dropped: {ctx}");
}

/// [`start_consult`] for a fixture that already carries a `pending_context` (which
/// must SURVIVE the spawn, so the "no approval yet" assertion cannot apply).
fn start_consult_keeping_context(fx: &mut Fx) -> u64 {
    write_goal(
        fx,
        "Keep the changelog tooling consistent; dprint is already vendored.",
    );
    write_marker(fx, AUTOFLOW_ASKS);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + SUPERVISOR_POLL_S
        }
    );
    assert!(
        ledger(fx)
            .pending_context
            .as_deref()
            .is_some_and(|c| c.contains("the human said")),
        "the spawn must not clear an undelivered answer"
    );
    1
}

#[test]
fn a_human_attach_freezes_the_consult_instead_of_blaming_the_supervisor() {
    // m23 bug 3. `drive`'s human-present gate returns ~44 lines before `advise_step`, so
    // while a client is attached the consult is neither polled nor reaped. Unfrozen, the
    // deadline expires unattended and the DETACH tick reports "the supervisor produced no
    // result: no answer within 90s" about a supervisor that answered in ~0s — and counts
    // it toward `SUPERVISOR_DEAD_AFTER`, so three routine attaches latch the feature off
    // with nothing said to the user.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, IDLE_PANE);
    let seq = start_consult(&mut fx);
    // The supervisor answers essentially immediately...
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"dprint is already vendored\"}}",
            consult_nonce(&fx, seq)
        )),
        0,
    );
    // ...but a human is at the pane, so nothing is polled or typed.
    fx.driver.set_clients(&sess, true);
    fx.clock.set(START + 60);
    let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        fx.driver.sent_keys().is_empty(),
        "never type at a pane a human is using"
    );
    assert!(
        ledger(&fx).pending_context.is_none(),
        "and nothing is applied while they are there"
    );
    // They detach PAST the original deadline (START + SUPERVISOR_TIMEOUT_S).
    fx.driver.set_clients(&sess, false);
    fx.clock.set(START + SUPERVISOR_TIMEOUT_S + 5);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        !matches!(out, JobTick::Escalated(_)),
        "a routine attach must not escalate a low-stakes decision: {out:?}"
    );
    let l = ledger(&fx);
    assert!(
        l.pending_context
            .as_deref()
            .is_some_and(|c| c.contains("dprint")),
        "the verdict the supervisor DID produce must still be applied: {:?}",
        l.pending_context
    );
    assert!(
        !l.last_status
            .as_deref()
            .unwrap_or_default()
            .contains("no answer within"),
        "and the supervisor must not be blamed in writing for the attach: {:?}",
        l.last_status
    );
    assert_eq!(
        fx.sched.advise_health.no_result_streak, 0,
        "an attach is no evidence the transport is broken, so it must not feed the latch"
    );
    assert!(!fx.sched.advise_health.latched);
}

#[test]
fn an_attach_outlasting_the_freeze_allowance_escalates_honestly_without_latching() {
    // The BOUND on the freeze, and why it is bounded. The same gate precedes the marker
    // disposer, so a human who hand-answers in the pane produces no marker bump and
    // nothing abandons the consult; an unbounded freeze would eventually type an
    // arbitrarily stale verdict naming a concrete action they already took. So the freeze
    // is granted ONCE. Past it the consult is escalated — never silently dropped, which
    // would consume the marker bump and deliver nothing — with an HONEST reason that does
    // not feed the latch.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, IDLE_PANE);
    let seq = start_consult(&mut fx);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"stale by now\"}}",
            consult_nonce(&fx, seq)
        )),
        0,
    );
    fx.driver.set_clients(&sess, true);
    // Two attached ticks: the first grants the freeze, the second must NOT grant another.
    for at in [START + 10, START + SUPERVISOR_TIMEOUT_S + 10] {
        fx.clock.set(at);
        let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }
    fx.driver.set_clients(&sess, false);
    fx.clock.set(START + 4 * SUPERVISOR_TIMEOUT_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Escalated(_)),
        "the worker's real question must reach the human: {out:?}"
    );
    let l = ledger(&fx);
    assert_eq!(
        l.open_stops[0].kind,
        StopKind::Capability,
        "never WorkerStuck"
    );
    assert!(
        l.open_stops[0]
            .question
            .as_deref()
            .is_some_and(|q| q.contains("formatter")),
        "carrying the WORKER's question, not a summary of a harness failure: {:?}",
        l.open_stops[0].question
    );
    let status = l.last_status.unwrap_or_default();
    assert!(status.contains("freeze allowance"), "{status}");
    assert!(
        !status.contains("no answer within"),
        "a supervisor that answered must not be blamed: {status}"
    );
    assert!(
        !l.pending_context
            .as_deref()
            .unwrap_or_default()
            .contains("stale by now"),
        "and the stale verdict is NOT typed at the agent"
    );
    assert_eq!(fx.sched.advise_health.no_result_streak, 0);
    assert!(matches!(
        &l.decider_runs[0].outcome,
        job::DeciderOutcome::Interrupted { reason } if reason.contains("freeze allowance")
    ));
    assert_eq!(
        l.decider_runs[0].finished_at,
        Some(START + 4 * SUPERVISOR_TIMEOUT_S)
    );
    assert!(
        l.advice_inflight.is_none(),
        "the debt is dropped with the consult"
    );
}

#[test]
fn a_fresh_consult_never_reaps_a_finished_ones_done_signal() {
    // Legacy pre-audit ledgers have no completed sequence to restore, so spawn cleanup must
    // still prevent a reused done-signal from being read as a fresh consult's result.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    let seq = start_consult(&mut fx);
    finish_consult(
        &fx,
        seq,
        &consult_reply(&format!(
            "{{\"nonce\":\"{}\",\"action\":\"select_option\",\"option_index\":1,\
             \"reason\":\"vendored\"}}",
            consult_nonce(&fx, seq)
        )),
        0,
    );
    fx.clock.set(START + SUPERVISOR_POLL_S);
    let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        ledger(&fx).advice_inflight.is_none(),
        "premise: the first consult finished, so nothing is recorded to resume from"
    );
    assert!(
        !fx.paths.advice_done_signal(seq).exists(),
        "completed consult artifacts are cleaned after their audit is saved"
    );
    std::fs::create_dir_all(fx.paths.steps_dir()).unwrap();
    std::fs::write(fx.paths.advice_done_signal(seq), "0\n").unwrap();
    std::fs::write(fx.paths.advice_log(seq), "stale legacy reply").unwrap();
    let mut legacy = ledger(&fx);
    legacy.decider_runs.clear();
    job::save(&fx.paths, &legacy).unwrap();

    // A pre-audit ledger has no completed sequence to restore. pmd restarts at 0 and the
    // agent asks again, so spawn cleanup must still protect the reused path.
    let root = fx.sched.work_dir.clone();
    fx.sched = JobScheduler::new(SESSION_ID, root, SESSION_ID, Engine::Claude, None);
    fx.sched.set_decider_binary_available(true); // keep FakeDriver's capability after restart
    fx.sched.set_claude_home(&fx.claude_home); // keep the seed-probe off the real ~/.claude
    assert_eq!(
        fx.sched.advise_seq, 0,
        "premise: nothing to resume the counter from"
    );
    let mut now = START + 400;
    fx.clock.set(now);
    write_marker(&fx, &AUTOFLOW_ASKS.replace("\"seq\":9", "\"seq\":900"));
    backdate_marker(&fx, 2);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: now + SUPERVISOR_POLL_S
        },
        "the second consult is spawned at the SAME seq"
    );
    // The next sweep must see a consult that is still RUNNING, not the old exit code.
    now += SUPERVISOR_POLL_S;
    fx.clock.set(now);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: now + SUPERVISOR_POLL_S
        },
        "a dead consult's done-signal must not be reaped as the fresh one's: {out:?}"
    );
    assert!(
        !matches!(ledger(&fx).run, JobRun::Blocked { .. }),
        "and nothing is escalated"
    );
    assert_eq!(fx.sched.advise_health.refusal_streak, 0);
}

#[test]
fn a_restart_advances_past_completed_decision_audit_sequences() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let mut current = ledger(&fx);
    current.record_decider_run(job::DeciderRun {
        seq: 7,
        started_at: START,
        finished_at: Some(START),
        engine: Engine::Claude,
        model: None,
        target: job::DeciderTarget::Marker,
        question: "Which formatter?".into(),
        options: vec!["prettier".into(), "dprint".into()],
        reported_kind: Some(StopKind::Ambiguity),
        effect: None,
        policy: job::DeciderPolicy {
            kind: StopKind::Ambiguity,
            labelled_risk: RiskClass::Low,
            effective_risk: RiskClass::Medium,
        },
        outcome: job::DeciderOutcome::Skipped {
            reason: "typed policy escalated".into(),
            reported_kind: Some(StopKind::Ambiguity),
            effect: None,
        },
    });
    job::save(&fx.paths, &current).unwrap();

    let root = fx.sched.work_dir.clone();
    fx.sched = JobScheduler::new(SESSION_ID, root, SESSION_ID, Engine::Claude, None);

    assert_eq!(fx.sched.advise_seq, 7);
    assert_eq!(fx.sched.next_decider_seq(), 8);
}

#[test]
fn a_fat_brief_is_clamped_instead_of_spending_the_consult_budget() {
    // Bug 4: `brief.md` is unbounded human input and rode into EVERY consult of the session
    // uncounted, so a pasted design doc consumed the consult itself — which surfaces as three
    // `Capability` escalations and a latch, for a reason no escalation text mentions. (It was
    // originally described as spending `--max-budget-usd`; that cap is opt-in now, and the
    // clamp matters regardless because a fat goal crowds out the actual question.)
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    let fat = format!("Ship the uploader. {}", "padding ".repeat(8_000));
    write_goal(&fx, &fat);
    write_marker(&fx, AUTOFLOW_ASKS);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + SUPERVISOR_POLL_S
        },
        "a long brief still gets a consult — clamped, not disabled: {out:?}"
    );
    let argv = consult_argv(&fx, 1);
    let prompt = argv.last().unwrap();
    assert!(
        prompt.len() < fat.len() / 2,
        "the goal must be CLAMPED, not pasted whole: prompt {} bytes vs brief {} bytes",
        prompt.len(),
        fat.len()
    );
    assert!(
        prompt.contains("Ship the uploader"),
        "the beginning of the goal is kept"
    );
    assert!(
        prompt.contains("truncated"),
        "and the truncation is ANNOUNCED, so the supervisor refuses rather than judging \
         against a mandate that was cut off: {}",
        &prompt[..200.min(prompt.len())]
    );
}

#[test]
fn a_missing_claude_binary_latches_and_escalates_without_auto_approval() {
    // Bug 4: a missing `claude` does NOT make `spawn_step` fail — the spawn succeeds and
    // the shell exits 127, which arrives as an ordinary "no result". So a codex-only box
    // paid repeated process failures. Probe PATH once, latch, and surface the decision.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    // The probe is a private cache, so the test seeds it rather than mutating process env
    // (`set_var` is `unsafe` in edition 2024 and would race every other test).
    fx.sched.set_decider_binary_available(false);
    write_goal(&fx, "Keep the changelog tooling consistent.");
    write_marker(&fx, AUTOFLOW_ASKS);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)), "{out:?}");
    assert_eq!(fx.driver.spawn_count(), 0, "nothing is spawned");
    let l = ledger(&fx);
    assert!(
        matches!(l.run, JobRun::Blocked { .. }),
        "the decision reaches the human: {:?}",
        l.run
    );
    assert!(l.pending_context.is_none());
    assert!(
        fx.sched.advise_health.latched,
        "and the session stops re-probing a supervisor it cannot run"
    );
    assert!(l.advice_inflight.is_none());
}

#[test]
fn the_path_probe_recognises_an_executable_and_fails_safe() {
    // The probe must not answer "absent" for anything but a genuine absence: a false
    // negative silently switches the supervisor off.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("pm-probe-bin");
    std::fs::write(&bin, "#!/bin/sh\n").unwrap();
    assert!(
        !is_executable_file(&bin),
        "a data file on PATH is not an executable"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable_file(&bin));
    }
    assert!(
        !is_executable_file(dir.path()),
        "a directory is not a binary"
    );
    assert!(!is_executable_file(&dir.path().join("nope")));
    // A path WITH a separator is probed as-is rather than searched for.
    assert!(!binary_on_path(
        &dir.path().join("nope").display().to_string()
    ));
    // `sh` is on PATH on every platform this runs on, and is what the consult's wrapper
    // itself needs, so this also pins that the search side works at all.
    assert!(binary_on_path("sh"), "PATH search found nothing at all");
}

use crate::job::{DecisionKind, DecisionRecord, LedgerSituation, WakeState};

#[test]
fn project_situation_includes_only_completed_history_and_reads_live_open_stops() {
    let mut led = AgentLoopState::fresh(Engine::Claude, None, START);
    led.last_plan = Some("PLAN-sentinel".into());
    // Only completed decisions are persisted; both are prior context.
    led.decisions.push(DecisionRecord::at(
        10,
        Some(1),
        DecisionKind::AutoFlow,
        Some("PRIOR-sentinel".into()),
        vec![],
    ));
    led.decisions.push(DecisionRecord::at(
        20,
        Some(2),
        DecisionKind::AutoFlow,
        Some("RECENT-sentinel".into()),
        vec![],
    ));
    // A SKEWED situation snapshot (CF-3 / B-Minor-1): state Blocked with a STALE open-stop id
    // that no longer reflects the live ledger.
    led.situation = Some(LedgerSituation {
        state: WakeState::Blocked,
        status: None,
        open_stops: vec!["STALE-STOP-must-not-appear".into()],
        seq: 2,
        at: 20,
    });
    // The LIVE open_stops is empty (auto-flow never persists its stops open).
    led.open_stops = vec![];

    let s = project_situation(&led);
    assert!(
        s.contains("PLAN-sentinel"),
        "the agent plan is projected: {s}"
    );
    assert!(
        s.contains("PRIOR-sentinel"),
        "the prior decision is projected: {s}"
    );
    assert!(
        s.contains("RECENT-sentinel"),
        "all persisted decisions are completed prior context: {s}"
    );
    assert!(
        !s.contains("STALE-STOP-must-not-appear"),
        "coherence: open-stops come from LIVE open_stops, never the skewed situation snapshot: {s}"
    );

    led.open_stops = vec![open_stop(
        "LIVE-STOP".into(),
        StopKind::Ambiguity,
        None,
        "Which formatter?",
        &["prettier".into(), "dprint".into()],
        START,
    )];
    let s = project_situation(&led);
    assert!(
        s.contains("still awaiting a human decision on stop LIVE-STOP"),
        "live open stops are projected: {s}"
    );

    // An OBJECTIVE D counter surfaces (a fact about progress, not precedent).
    led.stale_plan_streak = 3;
    assert!(
        project_situation(&led).contains("restated the same plan 3×"),
        "the objective stall streak is surfaced"
    );

    // C-5: a thin/fresh ledger projects NOTHING (the fence is then omitted, consult runs).
    let thin = AgentLoopState::fresh(Engine::Claude, None, START);
    assert_eq!(
        project_situation(&thin),
        "",
        "a thin ledger projects an empty situation"
    );
}

#[test]
fn project_situation_uses_the_latest_worker_result_not_supervisor_status() {
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, START);
    ledger.last_plan = Some("run the focused regression test".into());
    ledger.last_status =
        Some("the session supervisor resolved a low-stakes decision on your behalf".into());
    ledger.situation = Some(LedgerSituation {
        state: WakeState::Blocked,
        status: Some("implemented the parser fix; compilation is clean".into()),
        open_stops: Vec::new(),
        seq: 7,
        at: START,
    });

    let situation = project_situation(&ledger);

    assert!(
        situation.contains("worker's latest reported result: implemented the parser fix"),
        "accepted worker result should ground the next decision: {situation}"
    );
    assert!(
        situation.contains("agent's stated next step: run the focused regression test"),
        "the worker's next step should remain visible: {situation}"
    );
    assert!(
        !situation.contains("supervisor resolved"),
        "supervisor audit prose must not masquerade as worker progress: {situation}"
    );
}

// ---------------------------------------------------------------------------------
// Task 4 — the consult is dispatched on `config.decider_engine`.
//
// Both tests seed the IDENTICAL auto-flow report/goal (via `start_consult`) so the ONLY
// difference is the engine written to `config.json`. `advise_binary` is seeded so neither
// spawn depends on a real `codex`/`claude` on PATH (mirrors the directive test), and the
// engine-aware binary probe caches a single `Option<bool>`, so seeding `true` answers for
// whichever binary the selected engine names.
// ---------------------------------------------------------------------------------

#[test]
fn a_codex_session_builds_the_codex_consult_argv_and_arms_the_last_message_file() {
    // A per-session `decider_engine = codex` dispatches the consult through the codex builder —
    // `codex exec -s read-only … --output-last-message <advice-N.last>` — and arms the reap to
    // read the verdict from that isolated file rather than the tee'd log.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.advise_binary = Some(true);
    set_decider_engine(&fx, Engine::Codex);
    let seq = start_consult(&mut fx);
    let argv = consult_argv(&fx, seq);

    assert!(
        argv.contains(&"codex".to_string()) && argv.contains(&"exec".to_string()),
        "the codex path shells `codex exec`: {argv:?}"
    );
    assert!(
        argv.windows(2).any(|w| w == ["-s", "read-only"]),
        "read-only (codex's `--permission-mode plan`): {argv:?}"
    );
    assert!(
        argv.iter().any(|a| a.ends_with(".last")),
        "codex arms --output-last-message: {argv:?}"
    );
    assert!(
        !argv.contains(&"claude".to_string()),
        "the codex consult never shells claude: {argv:?}"
    );

    // The in-flight record points the reap at the last-message file, not the tee'd log.
    let verdict_path = fx
        .sched
        .advise
        .as_ref()
        .expect("a consult is in flight after start_consult")
        .verdict_path
        .clone();
    assert_eq!(
        verdict_path,
        Some(fx.paths.advice_last_message(seq)),
        "codex points the reap at the isolated last-message file"
    );
}

#[test]
fn a_claude_session_still_builds_the_claude_consult_argv_with_no_verdict_file() {
    // Regression guard: the DEFAULT engine (claude) still builds the claude argv and arms NO
    // verdict file — so the reap reads the tee'd log, byte-for-byte as before this feature.
    // Identical seeded report/goal to the codex twin; only `decider_engine` differs (here it is
    // left at `setup_with`'s default of claude).
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.advise_binary = Some(true);
    let seq = start_consult(&mut fx);
    let argv = consult_argv(&fx, seq);

    assert!(
        argv.contains(&"claude".to_string()) && argv.contains(&"--json-schema".to_string()),
        "the claude path shells `claude … --json-schema`: {argv:?}"
    );
    assert!(
        !argv.contains(&"codex".to_string()),
        "the claude consult never shells codex: {argv:?}"
    );

    // No verdict file ⇒ the reap reads the tee'd log, exactly as before this feature.
    let verdict_path = fx
        .sched
        .advise
        .as_ref()
        .expect("a consult is in flight after start_consult")
        .verdict_path
        .clone();
    assert_eq!(verdict_path, None, "the claude path arms no verdict file");
}

// ---------------------------------------------------------------------------------
// Task 3 — the consult honors the per-session `config.decider_model`.
//
// The builders already accept a model; the change is that `spawn_advice` feeds them
// `config.decider_model` FIRST, falling back to the env/const behaviour only when it is
// `None`. The builder-level tests below pin the argv contract each engine exposes; the two
// end-to-end tests drive `spawn_advice` over the FakeDriver and assert the SEEDED model
// reaches the launched argv — which is the byte a builder-only test cannot establish.
// ---------------------------------------------------------------------------------

#[test]
fn decider_model_some_overrides_the_claude_default() {
    let argv = crate::worker::build_supervisor_command(
        "global.anthropic.claude-opus-5",
        "sys",
        "{}",
        "prompt",
        60,
        5,
        None,
    );
    assert!(
        argv.join(" ")
            .contains("--model global.anthropic.claude-opus-5"),
        "{argv:?}"
    );
}

#[test]
fn decider_model_some_sets_codex_m() {
    let argv = crate::worker::build_supervisor_command_codex(
        Some("openai.gpt-5.6-sol"),
        "sys",
        "prompt",
        "/tmp/last",
        60,
        5,
    );
    assert!(
        argv.windows(2).any(|w| w == ["-m", "openai.gpt-5.6-sol"]),
        "{argv:?}"
    );
}

#[test]
fn a_claude_consult_pins_the_per_session_decider_model() {
    // End-to-end through `spawn_advice`: a per-session `decider_model` (distinct from the
    // `SUPERVISOR_MODEL` default) is the model the claude consult argv carries. Fails before
    // Task 3 — the arm ignored config and always resolved the env/const default.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.advise_binary = Some(true);
    set_decider_model(&fx, Some("global.anthropic.claude-opus-5"));
    let seq = start_consult(&mut fx);
    let argv = consult_argv(&fx, seq);

    assert!(
        argv.windows(2)
            .any(|w| w == ["--model", "global.anthropic.claude-opus-5"]),
        "the claude consult pins config.decider_model: {argv:?}"
    );
}

#[test]
fn a_codex_consult_pins_the_per_session_decider_model() {
    // End-to-end through `spawn_advice`: on codex the same per-session `decider_model` is passed
    // as `-m <value>` (codex otherwise resolves its own default and omits `-m`).
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.advise_binary = Some(true);
    set_decider_engine(&fx, Engine::Codex);
    set_decider_model(&fx, Some("openai.gpt-5.6-sol"));
    let seq = start_consult(&mut fx);
    let argv = consult_argv(&fx, seq);

    assert!(
        argv.windows(2).any(|w| w == ["-m", "openai.gpt-5.6-sol"]),
        "the codex consult pins config.decider_model: {argv:?}"
    );
}

#[test]
fn a_whitespace_decider_model_falls_back_to_the_claude_default() {
    // A whitespace `decider_model` is "no model set": the claude consult resolves the same
    // env→const fallback `None` does (here `SUPERVISOR_MODEL`, env unset), NEVER pinning the
    // blank string as `--model`.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.advise_binary = Some(true);
    set_decider_model(&fx, Some("   "));
    let seq = start_consult(&mut fx);
    let argv = consult_argv(&fx, seq);

    assert!(
        argv.windows(2)
            .any(|w| w == ["--model", worker::SUPERVISOR_MODEL]),
        "a blank decider_model falls back to the default model: {argv:?}"
    );
}

#[test]
fn a_whitespace_decider_model_omits_codex_m() {
    // The codex twin: a whitespace `decider_model` omits `-m` entirely (env unset ⇒ `None`),
    // exactly as `None` does — codex then resolves its own default.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    fx.sched.advise_binary = Some(true);
    set_decider_engine(&fx, Engine::Codex);
    set_decider_model(&fx, Some("   "));
    let seq = start_consult(&mut fx);
    let argv = consult_argv(&fx, seq);

    assert!(
        !argv.iter().any(|a| a == "-m"),
        "a blank decider_model omits `-m`: {argv:?}"
    );
}

#[test]
fn environment_fallbacks_and_path_probe_run_in_an_isolated_process() {
    const MODE_ENV: &str = "PM_SUPERVISOR_COVERAGE_CHILD";
    const TEST_NAME: &str = concat!(
        "job_engine::tests::supervisor::",
        "environment_fallbacks_and_path_probe_run_in_an_isolated_process"
    );

    match std::env::var(MODE_ENV).as_deref() {
        Ok("configured") => {
            let (mut claude, _sess) = marker_fx(Tier::Autopilot, |_| {});
            claude.sched.advise_binary = None;
            let seq = start_consult(&mut claude);
            let argv = consult_argv(&claude, seq);
            assert!(
                argv.windows(2)
                    .any(|w| w == ["--model", "env-claude-model"]),
                "{argv:?}"
            );
            assert!(
                argv.windows(2).any(|w| w == ["--max-budget-usd", "1.25"]),
                "{argv:?}"
            );

            let (mut codex, _sess) = marker_fx(Tier::Autopilot, |_| {});
            codex.sched.advise_binary = None;
            set_decider_engine(&codex, Engine::Codex);
            let seq = start_consult(&mut codex);
            let argv = consult_argv(&codex, seq);
            assert!(
                argv.windows(2).any(|w| w == ["-m", "env-codex-model"]),
                "{argv:?}"
            );
            return;
        }
        Ok("path-missing") => {
            assert!(
                binary_on_path("definitely-not-a-real-supervisor-binary"),
                "an unavailable PATH must fail safe toward present"
            );
            return;
        }
        _ => {}
    }

    let bin_dir = tempfile::tempdir().unwrap();
    for name in ["claude", "codex"] {
        let path = bin_dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    let current_exe = std::env::current_exe().unwrap();
    let configured = std::process::Command::new(&current_exe)
        .arg(TEST_NAME)
        .arg("--exact")
        .arg("--nocapture")
        .env(MODE_ENV, "configured")
        .env("PATH", bin_dir.path())
        .env(worker::SUPERVISOR_MODEL_ENV, "env-claude-model")
        .env(worker::SUPERVISOR_BUDGET_USD_ENV, " 1.25 ")
        .env(worker::SUPERVISOR_CODEX_MODEL_ENV, " env-codex-model ")
        .status()
        .unwrap();
    assert!(configured.success(), "configured child failed");

    let path_missing = std::process::Command::new(current_exe)
        .arg(TEST_NAME)
        .arg("--exact")
        .arg("--nocapture")
        .env(MODE_ENV, "path-missing")
        .env_remove("PATH")
        .status()
        .unwrap();
    assert!(path_missing.success(), "PATH-missing child failed");
}
