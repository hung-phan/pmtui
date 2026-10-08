//! What a tick does to a live pane: the dead-pane guard, the mid-cadence marker
//! fast-path that never nudges, the continuous-Busy stall backstop, and the
//! two-observation gate a nudge has to earn.

use super::*;
use crate::state::RiskClass;

fn queued_decision(id: &str) -> crate::job::QueuedAdvice {
    crate::job::QueuedAdvice {
        stop_id: id.into(),
        report_seq: 9,
        draft: crate::worker::StopDraft {
            kind: StopKind::Ambiguity,
            effect: crate::worker::StopEffect::default(),
            question: "Which formatter?".into(),
            options: vec!["prettier".into(), "dprint".into()],
            context_ref: None,
            risk_class: RiskClass::Low,
        },
    }
}

#[test]
fn queued_decisions_escalate_with_audited_transport_reason_when_decider_is_unavailable() {
    for (latched, expected) in [
        (None, "decider unavailable"),
        (
            Some("transport unhealthy"),
            "latched off (transport unhealthy)",
        ),
    ] {
        let (mut fx, _session) = marker_fx(Tier::Autopilot, |_| {});
        let mut current = ledger(&fx);
        current.advice_queue = vec![queued_decision("queued-1")];
        job::save(&fx.paths, &current).unwrap();
        match latched {
            Some(reason) => {
                fx.sched.advise_health.latched = true;
                fx.sched.advise_health.reason = Some(reason.into());
            }
            None => fx.sched.set_supervisor_enabled(false),
        }

        let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

        assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
        let current = ledger(&fx);
        assert!(current.advice_queue.is_empty());
        assert_eq!(current.open_stops.len(), 1);
        assert!(matches!(
            &current.decider_runs[0].outcome,
            crate::job::DeciderOutcome::Skipped { reason, .. } if reason.contains(expected)
        ));
    }
}

#[test]
fn malformed_queued_decision_escalates_without_consuming_the_valid_sibling() {
    let (mut fx, _session) = marker_fx(Tier::Autopilot, |_| {});
    let mut malformed = queued_decision("queued-bad");
    malformed.draft.question.clear();
    malformed.draft.options.clear();
    let mut current = ledger(&fx);
    current.advice_queue = vec![malformed, queued_decision("queued-good")];
    job::save(&fx.paths, &current).unwrap();

    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
    let current = ledger(&fx);
    assert_eq!(current.open_stops.len(), 1);
    assert_eq!(current.advice_queue.len(), 1);
    assert_eq!(current.advice_queue[0].stop_id, "queued-good");
    assert!(matches!(
        &current.decider_runs[0].outcome,
        crate::job::DeciderOutcome::Skipped { reason, .. }
            if reason.contains("not safely consultable")
    ));
}

struct CaptureFails<'a>(&'a FakeDriver);

impl Driver for CaptureFails<'_> {
    fn spawn_step(
        &self,
        session: &str,
        cwd: &Path,
        command: &[String],
        done_signal: &Path,
        log: &Path,
    ) -> Result<tmux::StepHandle> {
        self.0.spawn_step(session, cwd, command, done_signal, log)
    }

    fn is_alive(&self, session: &str) -> Result<bool> {
        self.0.is_alive(session)
    }

    fn capture_tail(&self, session: &str, _lines: usize) -> Result<String> {
        anyhow::bail!("capture failed for {session} (armed)")
    }

    fn terminate(&self, session: &str) -> Result<()> {
        self.0.terminate(session)
    }

    fn has_clients(&self, session: &str) -> Result<bool> {
        self.0.has_clients(session)
    }

    fn pane_dead(&self, session: &str) -> Result<bool> {
        self.0.pane_dead(session)
    }
}

// --- dead pane ------------------------------------------------------------

#[test]
fn a_dead_pane_is_never_nudged_and_surfaces_immediately() {
    // THE BUG (m18/4): a `remain-on-exit` corpse keeps showing the last prompt it
    // painted, so `classify_pane` says Idle and the harness typed nudges into a process
    // that had EXITED — forever, at full cadence. The premise is asserted first (the
    // pane text really does classify Idle), so this cannot pass because the pane looked
    // busy; the ONLY thing standing between the harness and the corpse is the probe.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    assert_eq!(
        tmux::classify_pane(IDLE_PANE),
        PaneActivity::Idle,
        "premise: a corpse's last frame is indistinguishable from an idle prompt"
    );
    fx.driver.set_pane_dead(&sess, true);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    // Surfaced NOW, not after the 30-minute stall backstop.
    let reason = match out {
        JobTick::Stuck(r) => r,
        other => panic!("a dead pane must surface at once, got {other:?}"),
    };
    assert!(
        reason.contains("EXITED"),
        "and it must say what actually happened: {reason}"
    );
    assert!(fx.driver.sent_keys().is_empty(), "never nudge a corpse");
    assert!(matches!(fx.sched.run, JobRun::Blocked { .. }));
    let l = ledger(&fx);
    assert_eq!(l.open_stops.len(), 1);
    assert_eq!(
        l.open_stops[0].kind,
        StopKind::Capability,
        "only a human can close or restart the session — Capability is always Hard, so \
         no tier auto-flows it, and it is not the untrue \"wedged / no progress\" Stuck"
    );
    // Parked Blocked, so later ticks re-emit for the daemon's dedup and STILL never
    // nudge, however idle the corpse looks.
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Escalated(vec![l.open_stops[0].id.clone()])
    );
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn a_dead_pane_closes_the_pending_turn_as_terminal_unavailable() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);

    fx.driver.set_pane_dead(&sess, true);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Stuck(_)
    ));

    assert!(matches!(
        ledger(&fx).turn_trace.last().map(|turn| &turn.outcome),
        Some(crate::job::TurnOutcome::NoReport {
            reason: crate::job::TurnNoReportReason::TerminalUnavailable,
            ..
        })
    ));
}

#[test]
fn a_dead_pane_does_not_swallow_the_humans_answer() {
    // The answer-resume path reads a capture too, so it needs the same guard — and the
    // answer must survive the escalation (it stays parked as `pending_context`, the one
    // durable carrier), so reviving the session still delivers it.
    let mut fx = blocked_fx();
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.driver.set_pane_dead(&sess, true);
    fx.clock.set(START + 5);
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id: "stop-x".into(),
            answer: "use postgres".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: START + 5,
        },
    )
    .unwrap();
    assert!(
        matches!(
            fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
            JobTick::Stuck(_)
        ),
        "the answer cannot be typed into a corpse — say so"
    );
    assert!(fx.driver.sent_keys().is_empty());
    assert!(
        ledger(&fx)
            .pending_context
            .as_deref()
            .is_some_and(|c| c.contains("use postgres")),
        "the answer is preserved for a revived session, never silently eaten"
    );
    // Revive the pane (the human restarted the agent in it) ⇒ the parked answer is
    // delivered by the next confirmed heartbeat, unchanged.
    fx.driver.set_pane_dead(&sess, false);
    let stop_id = ledger(&fx).open_stops[0].id.clone();
    let at = fx.clock.now() + 1;
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id,
            answer: "restarted it".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: at,
        },
    )
    .unwrap();
    fx.clock.set(at);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    let sent = fx.driver.sent_keys();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].1.contains("use postgres") && sent[0].1.contains("restarted it"),
        "BOTH answers reach the agent — an answer parked across an escalation is not \
         overwritten by the next one: {}",
        sent[0].1
    );
}

// --- OQ1 not-due mtime fast-path is disposer-only (regression I1) --------

/// A NOT-yet-due parked cadence far in the future, so `now (START) < until`.
const NOT_DUE: Epoch = START + 10_000;

#[test]
fn fast_path_working_marker_idle_pane_never_nudges_and_preserves_cadence() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |l| {
        l.run = JobRun::Monitoring { until: NOT_DUE };
    });
    write_marker(&fx, r#"{"seq":7,"state":"working","status":"indexing"}"#);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring { until: NOT_DUE },
        "a mid-cadence Working bump leaves the cadence untouched"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "the fast-path must NOT nudge mid-cadence"
    );
    assert_eq!(ledger(&fx).last_marker_seq, 7, "the bump is still disposed");
}

#[test]
fn fast_path_working_marker_busy_pane_does_not_shrink_cadence() {
    let (mut fx, sess) = marker_fx(Tier::Standard, |l| {
        l.run = JobRun::Monitoring { until: NOT_DUE };
    });
    fx.driver.set_tail(&sess, BUSY_PANE);
    write_marker(&fx, r#"{"seq":7,"state":"working","status":"indexing"}"#);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring { until: NOT_DUE },
        "the fast-path never re-parks to BUSY_RECHECK_S (no classify)"
    );
    assert!(fx.driver.sent_keys().is_empty());
    assert_eq!(ledger(&fx).last_marker_seq, 7);
}

#[test]
fn fast_path_restart_with_already_disposed_marker_does_not_nudge() {
    // After a daemon restart the in-memory `last_marker_mtime` is None, so an
    // already-disposed on-disk marker looks "advanced" and triggers the fast-path.
    // The persisted `last_marker_seq` guard must no-op the dispose WITHOUT a
    // spurious nudge / cadence change.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |l| {
        l.run = JobRun::Monitoring { until: NOT_DUE };
        l.last_marker_seq = 100;
    });
    write_marker(&fx, BLOCKED_HARD); // seq == 100 == watermark ⇒ stale
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(out, JobTick::Monitoring { until: NOT_DUE });
    assert!(
        fx.driver.sent_keys().is_empty(),
        "the seq guard no-ops without a spurious nudge"
    );
    assert!(!matches!(ledger(&fx).run, JobRun::Blocked { .. }));
}

#[test]
fn fast_path_blocked_marker_parks_within_one_sweep() {
    // The intended fast-path WIN: a mid-cadence Blocked bump escalates immediately.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |l| {
        l.run = JobRun::Monitoring { until: NOT_DUE };
    });
    write_marker(&fx, BLOCKED_HARD);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)), "got {out:?}");
    assert!(matches!(ledger(&fx).run, JobRun::Blocked { .. }));
    assert!(fx.driver.sent_keys().is_empty());
}

#[test]
fn fast_path_human_present_defers_block_then_disposes_it_after_detach() {
    // A human owns the pane: the fast-path DEFERS without nudging and, crucially,
    // WITHOUT recording `last_marker_mtime` — because `observe_marker` short-circuits
    // on an unchanged mtime BEFORE the seq/dispose check, so marking a mid-cadence
    // Blocked bump "seen" here would DROP it after detach. Consuming no state, the
    // block is still disposed on the first sweep after the human leaves.
    let (mut fx, sess) = marker_fx(Tier::Standard, |l| {
        l.run = JobRun::Monitoring { until: NOT_DUE };
    });
    write_marker(&fx, BLOCKED_HARD);
    fx.driver.set_clients(&sess, true); // a human is attached RIGHT NOW
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring { until: NOT_DUE },
        "defer, no dispose"
    );
    assert!(fx.driver.sent_keys().is_empty(), "attached ⇒ no nudge");
    assert!(
        fx.sched.last_marker_stamp.is_none(),
        "the watermark must NOT advance on a human-present defer"
    );
    assert!(
        fx.sched.marker_revision_advanced(),
        "the pending marker still reads as advanced (not consumed)"
    );
    assert!(
        !matches!(ledger(&fx).run, JobRun::Blocked { .. }),
        "deferred, not blocked, while the human is present"
    );
    // Human detaches ⇒ the next sweep disposes the pending block within one sweep
    // (it was NOT dropped by a premature mtime record).
    fx.driver.set_clients(&sess, false);
    let out2 = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out2, JobTick::Escalated(_)), "got {out2:?}");
    assert!(matches!(ledger(&fx).run, JobRun::Blocked { .. }));
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a Blocked park never nudges"
    );
}

#[test]
fn fast_path_auto_flow_without_a_decider_escalates() {
    // The marker is policy-eligible, but a disabled decider cannot authorize it. The
    // NOT-DUE fast path must surface the stop rather than restoring the stale timer.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |l| {
        l.run = JobRun::Monitoring { until: NOT_DUE };
    });
    fx.sched.set_supervisor_enabled(false);
    write_marker(
        &fx,
        r#"{"seq":9,"state":"blocked","stops":[{"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low"}]}"#,
    );
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Escalated(_)));
    assert!(fx.driver.sent_keys().is_empty(), "fast-path never nudges");
    let l = ledger(&fx);
    assert!(l.pending_context.is_none());
    assert!(matches!(l.run, JobRun::Blocked { .. }));
    assert_eq!(l.open_stops[0].kind, StopKind::Capability);
}

#[test]
fn fast_path_monitoring_marker_parks_the_nap_and_returns_matching_until() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |l| {
        l.run = JobRun::Monitoring { until: NOT_DUE };
    });
    write_marker(&fx, r#"{"seq":5,"state":"monitoring","next_check_s":900}"#);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(out, JobTick::Monitoring { until: START + 900 });
    assert!(fx.driver.sent_keys().is_empty());
    assert_eq!(ledger(&fx).run, JobRun::Monitoring { until: START + 900 });
}

#[test]
fn fast_path_mid_write_marker_rechecks_without_counting_or_recording() {
    // A fresh-mtime malformed (plausibly mid-write) marker on the NOT-DUE fast-path
    // is an OQ3 short recheck: it re-parks BUSY_RECHECK_S, does NOT bump the stall
    // counter, and does NOT record the mtime (so it is re-read next tick).
    let (mut fx, _sess) = marker_fx(Tier::Standard, |l| {
        l.run = JobRun::Monitoring { until: NOT_DUE };
    });
    write_marker(&fx, "{ not json"); // fresh real mtime
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        }
    );
    assert_eq!(
        ledger(&fx).continuations,
        0,
        "a fresh (mid-write) malformed marker is not counted"
    );
    assert!(
        fx.sched.last_marker_stamp.is_none(),
        "the mtime is not recorded, so the settled marker is re-read next tick"
    );
    assert!(fx.driver.sent_keys().is_empty());
}

// --- stall backstop (Slice 3) -------------------------------------------

#[test]
fn busy_under_the_bound_reparks_recheck_without_escalating() {
    // A pane that is Busy but for LESS than DEFAULT_STALL_BUSY_S must keep re-parking
    // the short BUSY_RECHECK_S recheck (unchanged behavior) — the window opens on the
    // first observation and is not re-based on later observations.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        },
        "first Busy re-parks a short recheck"
    );
    assert_eq!(
        fx.sched.busy_since,
        Some(START),
        "the first Busy observation opens the stall window"
    );
    // A second Busy tick, still well under the bound: re-park again, no escalation,
    // and the window stays anchored at the FIRST observation.
    let t2 = START + BUSY_RECHECK_S;
    fx.clock.set(t2);
    let out2 = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out2,
        JobTick::Monitoring {
            until: t2 + BUSY_RECHECK_S
        }
    );
    assert_eq!(
        fx.sched.busy_since,
        Some(START),
        "the window is not re-based on later Busy observations"
    );
    assert!(
        !matches!(fx.sched.run, JobRun::Blocked { .. }),
        "under bound ⇒ not stuck"
    );
    assert!(fx.driver.sent_keys().is_empty(), "Busy never nudges");
}

#[test]
fn missing_reloaded_ledger_and_capture_error_use_the_safe_recheck() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let session = loop_session(&fx);
    fx.driver.set_alive(&session, true);
    fx.sched.last_marker_stamp = Some(super::super::marker::MarkerStamp {
        modified: std::time::SystemTime::UNIX_EPOCH,
        len: 0,
        inode: 0,
    });
    let base = ledger(&fx);
    let config: Config = state::read_json(&fx.paths.config()).unwrap();
    std::fs::remove_file(fx.paths.pmstate()).unwrap();
    let driver = CaptureFails(&fx.driver);

    let tick = fx.sched.drive(&driver, START, &base, &config).unwrap();

    assert_eq!(
        tick,
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        }
    );
    assert_eq!(fx.sched.busy_since, Some(START));
    assert!(
        fx.paths.pmstate().exists(),
        "the base ledger is restored when the reloaded ledger disappeared"
    );
}

#[test]
fn busy_at_or_past_the_bound_escalates_a_dismissable_stuck() {
    // The I1 silent-stall + bad-`--resume` closure: a pane continuously Busy for
    // >= DEFAULT_STALL_BUSY_S with no progress escalates a human-dismissable Stuck.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.sched.busy_since, Some(START));
    // Advance the fake clock so now - busy_since >= the bound.
    let t = START + DEFAULT_STALL_BUSY_S as i64;
    fx.clock.set(t);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Stuck(_)),
        "wedged ⇒ Stuck, got {out:?}"
    );
    let l = ledger(&fx);
    assert!(
        matches!(l.run, JobRun::Blocked { .. }),
        "a stall parks Blocked for the human"
    );
    assert_eq!(
        l.open_stops.len(),
        1,
        "an open stop is recorded (dismissable)"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a stall parks, never nudges"
    );
}

#[test]
fn meaningful_busy_transcript_progress_restarts_the_inactivity_window() {
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(
        &sess,
        "• Ran cargo test\n• Working (3s • esc to interrupt)\n› Ask Codex to do anything",
    );
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.sched.busy_since, Some(START));

    let progressed_at = START + DEFAULT_STALL_BUSY_S as i64 - 5;
    fx.clock.set(progressed_at);
    fx.driver.set_tail(
        &sess,
        "• Ran cargo test\n  1535 tests passed\n• Working (1795s • esc to interrupt)\n› Ask Codex to do anything",
    );
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        fx.sched.busy_since,
        Some(progressed_at),
        "meaningful transcript output restarts inactivity from the observation time"
    );

    fx.clock.set(START + DEFAULT_STALL_BUSY_S as i64 + 5);
    fx.driver.set_tail(
        &sess,
        "• Ran cargo test\n  1535 tests passed\n• Working (1805s • esc to interrupt)\n› Ask Codex to do anything",
    );
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert!(
        !matches!(fx.sched.run, JobRun::Blocked { .. }),
        "counter-only churn cannot hide the real transcript progress or trigger the old deadline"
    );
}

#[test]
fn busy_counter_churn_does_not_hide_a_silent_stall() {
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(
        &sess,
        "• Ran cargo test\n• Working (3s • esc to interrupt)\n› Ask Codex to do anything",
    );
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    fx.clock.set(START + DEFAULT_STALL_BUSY_S as i64);
    fx.driver.set_tail(
        &sess,
        "• Ran cargo test\n• Working (1800s • esc to interrupt)\n› Ask Codex to do anything",
    );
    assert!(
        matches!(
            fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
            JobTick::Stuck(_)
        ),
        "elapsed-counter churn normalizes away and cannot keep a silent pane alive"
    );
}

#[test]
fn pane_progress_never_clears_report_debt_or_authorizes_input() {
    let mut fx = setup(Tier::Autopilot, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    let baseline_sends = fx.driver.sent_keys().len();

    fx.driver.set_tail(
        &sess,
        "• Ran cargo test\n• Working (3s • esc to interrupt)\n› Ask Codex to do anything",
    );
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let progressed_at = fx.clock.now() + DEFAULT_STALL_BUSY_S as i64 - 5;
    fx.clock.set(progressed_at);
    fx.driver.set_tail(
        &sess,
        "• Ran cargo test\n  1535 tests passed\n• Working (1795s • esc to interrupt)\n› Ask Codex to do anything",
    );
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(ledger(&fx).awaiting_report());
    assert_eq!(fx.driver.sent_keys().len(), baseline_sends);
    assert_eq!(fx.sched.busy_since, Some(progressed_at));
}

#[test]
fn changing_claude_false_idle_transcript_restarts_inactivity_without_nudging() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    let baseline_sends = fx.driver.sent_keys().len();

    fx.driver.set_tail(&sess, "● first response line\n❯ ");
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let progressed_at = fx.clock.now() + DEFAULT_STALL_BUSY_S as i64 - 5;
    fx.clock.set(progressed_at);
    fx.driver
        .set_tail(&sess, "● first response line\n  second response line\n❯ ");
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(ledger(&fx).awaiting_report());
    assert_eq!(fx.driver.sent_keys().len(), baseline_sends);
    assert_eq!(
        fx.sched.busy_since,
        Some(progressed_at),
        "Claude output growth is progress even when its visible prompt classifies Idle"
    );
}

#[test]
fn busy_then_a_working_marker_bump_resets_the_stall_timer() {
    // A progress signal (a valid marker bump, ANY state) clears the stall window, so
    // the agent is not escalated even after the clock passes the original bound.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.sched.busy_since, Some(START));
    // The agent signals progress and the pane returns to idle.
    write_marker(&fx, r#"{"seq":7,"state":"working","status":"indexing"}"#);
    fx.driver.set_tail(&sess, IDLE_PANE);
    let t2 = START + BUSY_RECHECK_S;
    fx.clock.set(t2);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        fx.sched.busy_since, None,
        "a valid marker bump cleared the stall timer"
    );
    assert_eq!(
        fx.driver.sent_keys().len(),
        1,
        "Working falls through to a (confirmed) nudge"
    );
    // Even far past the original bound, a fresh Busy only REOPENS the window from the
    // new now — no escalation.
    fx.driver.set_tail(&sess, BUSY_PANE);
    let t3 = fx.clock.now() + DEFAULT_STALL_BUSY_S as i64;
    fx.clock.set(t3);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Monitoring { .. }),
        "reopened, not escalated: {out:?}"
    );
    assert_eq!(
        fx.sched.busy_since,
        Some(t3),
        "the stall timer restarts from the new now, not the original open"
    );
    assert!(!matches!(fx.sched.run, JobRun::Blocked { .. }));
}

#[test]
fn busy_then_idle_nudge_resets_then_a_later_busy_restarts_the_timer() {
    // The idle/nudge reset: a pane that returns to an idle prompt is interacting, not
    // stalled, so the nudge path clears the timer; a later Busy restarts it fresh.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.sched.busy_since, Some(START));
    fx.driver.set_tail(&sess, IDLE_PANE);
    let t2 = START + BUSY_RECHECK_S;
    fx.clock.set(t2);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.driver.sent_keys().len(), 1, "confirmed idle ⇒ nudged");
    assert_eq!(
        fx.sched.busy_since, None,
        "the idle nudge reset the stall timer"
    );
    // A later Busy tick restarts the window from the new now.
    fx.driver.set_tail(&sess, BUSY_PANE);
    let t3 = fx.clock.now() + 300; // the next cadence is due
    fx.clock.set(t3);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        fx.sched.busy_since,
        Some(t3),
        "a later Busy restarts the timer from the new now"
    );
}

#[test]
fn human_present_during_busy_resets_the_stall_timer() {
    // A human attached is not a stall (the pane looks Busy because they're typing):
    // the defer gate clears the timer, and no escalation fires however long they stay.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.sched.busy_since, Some(START));
    fx.driver.set_clients(&sess, true); // a human is attached RIGHT NOW
    for t in [START + 10, START + DEFAULT_STALL_BUSY_S as i64 + 1000] {
        fx.clock.set(t);
        assert!(matches!(
            fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
            JobTick::Monitoring { .. }
        ));
        assert_eq!(
            fx.sched.busy_since, None,
            "attached ⇒ the stall timer stays cleared"
        );
        assert!(
            !matches!(fx.sched.run, JobRun::Blocked { .. }),
            "attached ⇒ never stuck"
        );
    }
    assert!(fx.driver.sent_keys().is_empty(), "no nudge while attached");
}

#[test]
fn answer_after_a_stall_escalation_resets_busy_since() {
    // Parity with wakes/window_start: a human answer to the Stuck escalation resumes
    // the loop and clears the stall window, so answering "keep going" actually works.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.driver.set_tail(&sess, BUSY_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let t = START + DEFAULT_STALL_BUSY_S as i64;
    fx.clock.set(t);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Stuck(_)
    ));
    assert!(matches!(fx.sched.run, JobRun::Blocked { .. }));
    let stop_id = ledger(&fx).open_stops[0].id.clone();
    // The human answers; the pane is idle so the resume nudges with the answer text.
    fx.driver.set_tail(&sess, IDLE_PANE);
    let at = t + 1;
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id,
            answer: "keep going".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: at,
        },
    )
    .unwrap();
    fx.clock.set(at);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        fx.sched.busy_since, None,
        "the human answer reset the stall timer (parity with wakes/window_start)"
    );
    assert!(
        fx.driver
            .sent_keys()
            .iter()
            .any(|s| s.1.contains("keep going")),
        "the answer resumes by nudging the live session"
    );
}

// --- two-observation idle confirmation gate ------------------------------

#[test]
fn one_idle_observation_does_not_nudge_but_two_consecutive_ones_do() {
    // Defense in depth against a future false-Idle classification: a nudge needs the
    // pane to look idle across TWO captures. The first only arms the gate and
    // consumes NOTHING that belongs to the AGENT (no wake, no pending_context).
    let (mut fx, sess) = marker_fx(Tier::Standard, |l| {
        l.pending_context = Some("carry me".into());
    });
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        },
        "one Idle re-parks the SHORT recheck, exactly like the Busy arm"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a single Idle observation must NOT nudge"
    );
    assert_eq!(fx.sched.idle_confirmations, 1);
    assert_eq!(
        fx.sched.wakes, 0,
        "no wake consumed while awaiting confirmation"
    );
    // The wall-clock window, by contrast, DOES open here: it measures how long the
    // harness has been driving this session, and this tick drove it (it probed the
    // pane and re-parked a cadence). Opening it only on a successful send is what made
    // `max_wall_clock_s` unenforceable for a session that never nudges — see
    // `wall_clock_budget_bounds_a_session_that_never_nudges`.
    assert_eq!(
        fx.sched.window_start,
        Some(START),
        "the wall-clock window tracks driven time, not delivered nudges"
    );
    let l = ledger(&fx);
    assert_eq!(
        l.run,
        JobRun::Monitoring {
            until: START + BUSY_RECHECK_S
        },
        "the ledger carries the short recheck"
    );
    assert_eq!(
        l.pending_context.as_deref(),
        Some("carry me"),
        "pending_context intact while awaiting confirmation"
    );
    // The SECOND consecutive Idle confirms the prompt: nudge + resume the cadence.
    let t2 = START + BUSY_RECHECK_S;
    fx.clock.set(t2);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: t2 + 300 },
        "the confirming observation nudges and resumes the cadence park"
    );
    let sent = fx.driver.sent_keys();
    assert_eq!(sent.len(), 1, "nudged exactly once");
    assert_eq!(sent[0].0, sess, "nudge targets the loop session");
    assert!(
        sent[0].1.contains("carry me"),
        "the parked context is delivered"
    );
    assert_eq!(
        fx.sched.idle_confirmations, 0,
        "the gate resets as part of nudging, so the NEXT nudge re-earns two Idles"
    );
    assert_eq!(fx.sched.wakes, 1, "the delivered nudge counts one wake");
}

#[test]
fn a_busy_observation_between_two_idles_resets_the_confirmation_gate() {
    // The two Idle observations must be CONSECUTIVE: Idle → Busy → Idle is still only
    // the first of a fresh pair, so it must not nudge.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.sched.idle_confirmations, 1, "Idle #1 arms the gate");
    // A Busy observation voids it.
    fx.driver.set_tail(&sess, BUSY_PANE);
    let t2 = START + BUSY_RECHECK_S;
    fx.clock.set(t2);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: t2 + BUSY_RECHECK_S
        }
    );
    assert_eq!(
        fx.sched.idle_confirmations, 0,
        "Busy resets the gate — the Idles must be consecutive"
    );
    // So the next Idle is only the FIRST of a new pair ⇒ still no nudge.
    fx.driver.set_tail(&sess, IDLE_PANE);
    let t3 = t2 + BUSY_RECHECK_S;
    fx.clock.set(t3);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: t3 + BUSY_RECHECK_S
        }
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "the Busy in between means this Idle is not a confirmation"
    );
    assert_eq!(fx.sched.idle_confirmations, 1);
    // The one after it finally nudges.
    let t4 = t3 + BUSY_RECHECK_S;
    fx.clock.set(t4);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: t4 + 300 }
    );
    assert_eq!(
        fx.driver.sent_keys().len(),
        1,
        "two consecutive Idles ⇒ nudge"
    );
}

#[test]
fn idle_awaiting_confirmation_never_advances_the_stall_backstop() {
    // The gate must not interact with the `busy_since` stall backstop: an
    // Idle-but-unconfirmed tick is not a Busy observation, so it neither opens nor
    // advances the stall window and can never age into a bogus `Stuck`.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        }
    );
    assert_eq!(
        fx.sched.busy_since, None,
        "an Idle observation must not open the stall window"
    );
    // Sit in the awaiting-confirmation state far LONGER than the stall bound (an
    // extreme the 5s recheck makes impossible in production, but it proves the state
    // itself accumulates no stall time): the confirming Idle still NUDGES.
    let t = START + DEFAULT_STALL_BUSY_S as i64 + 1000;
    fx.clock.set(t);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring { until: t + 300 },
        "confirmed idle ⇒ nudge, never a bogus Stuck: got {out:?}"
    );
    assert_eq!(fx.driver.sent_keys().len(), 1);
    assert_eq!(fx.sched.busy_since, None);
    assert!(!matches!(fx.sched.run, JobRun::Blocked { .. }));
}

#[test]
fn a_streaming_pane_that_changes_between_idle_captures_never_nudges() {
    // Bug A, pinned deterministically. `classify_pane` returns Idle for a claude that is
    // STREAMING an answer (no busy marker is on screen mid-stream in v2.1.x), so a
    // count-only gate would type a nudge into a working agent after two such reads. The
    // real signal is that the TRANSCRIPT CHANGES between the two BUSY_RECHECK_S-apart
    // captures: the gate advances only on a byte-stable transcript, so a growing pane
    // re-arms forever. (The real-tmux acceptance test proves a genuine streaming claude
    // actually changes like this; a FakeDriver returns a fixed capture, so the FACT lives
    // there and the DECISION lives here.)
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});

    // First Idle observation: a `●` response block above the bare prompt. Arms the gate.
    fx.driver.set_tail(&sess, "● answering\n  point one\n❯ ");
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        },
        "the first Idle only arms the gate"
    );
    assert_eq!(fx.sched.idle_confirmations, 1, "armed by the first Idle");

    // The stream GREW by the next due tick — still classifies Idle (bare prompt, no busy
    // marker on screen), but the transcript above the prompt changed.
    fx.driver
        .set_tail(&sess, "● answering\n  point one\n  point two\n❯ ");
    let t2 = START + BUSY_RECHECK_S;
    fx.clock.set(t2);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: t2 + BUSY_RECHECK_S
        },
        "a changed transcript re-parks the recheck instead of nudging"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a streaming (changing) pane must NEVER be nudged, however idle each frame looks"
    );
    assert_eq!(
        fx.sched.idle_confirmations, 1,
        "the changed transcript re-armed the gate to a single observation"
    );

    // Once it SETTLES — two byte-identical captures — the gate confirms and nudges.
    let t3 = t2 + BUSY_RECHECK_S;
    fx.clock.set(t3);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: t3 + 300 },
        "a settled, byte-stable pane finally confirms and nudges the cadence"
    );
    let sent = fx.driver.sent_keys();
    assert_eq!(
        sent.len(),
        1,
        "exactly one nudge, only after the pane settled"
    );
    assert_eq!(sent[0].0, sess, "the nudge targets the loop session");
    assert_eq!(
        fx.sched.idle_confirmations, 0,
        "the delivered nudge reset the gate"
    );
}

// --- M71: the turn-end event gate outranks the pane heuristic ------------------

/// Write the per-session turn-complete signal at `turns` bytes (the engine hook appends one
/// byte per completed turn, so size == turn count).
fn write_turn_signal(fx: &Fx, turns: usize) {
    let p = fx.paths.turn_signal();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, vec![b'.'; turns]).unwrap();
}

#[test]
fn the_turn_event_blocks_a_nudge_while_a_turn_is_in_progress_even_on_an_idle_pane() {
    // The definitive Bug-A guard: while NO turn has completed since our nudge, the agent is
    // provably still working — so an idle-LOOKING pane (the streaming/quiet false-idle) must
    // NOT be nudged, whatever the fingerprint would say. Once the turn completes the event
    // stops BLOCKING but does NOT itself authorise the nudge — a completed turn defers to the
    // fingerprint (see the human-interaction regression below), which on a byte-stable idle
    // pane confirms over two captures (m70), not an immediate keystroke.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    // Post-nudge: 3 turns had completed when we nudged, still 3 now (mid-turn on our nudge).
    fx.sched.turns_at_nudge = Some(3);
    write_turn_signal(&fx, 3);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        },
        "no turn completed since our nudge ⇒ re-check, never nudge"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a working agent (per the turn event) must not be nudged even on an idle-looking pane"
    );
    // The turn completes (hook appends a 4th byte). The event no longer blocks; a completed
    // turn defers to the fingerprint, whose FIRST idle observation only arms.
    write_turn_signal(&fx, 4);
    let t2 = START + BUSY_RECHECK_S;
    fx.clock.set(t2);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: t2 + BUSY_RECHECK_S
        },
        "a completed turn defers to the fingerprint: the first idle only arms"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "still no nudge — the fingerprint has not confirmed yet"
    );
    assert_eq!(fx.sched.idle_confirmations, 1, "the fingerprint gate armed");
    // Second byte-stable idle capture ⇒ confirmed ⇒ nudge.
    let t3 = t2 + BUSY_RECHECK_S;
    fx.clock.set(t3);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { until: t3 + 300 },
        "two byte-stable idle captures after the turn ended ⇒ nudge"
    );
    let sent = fx.driver.sent_keys();
    assert_eq!(sent.len(), 1, "nudged exactly once");
    assert_eq!(sent[0].0, sess);
    // Re-baselined to the current count for the next cycle.
    assert_eq!(fx.sched.turns_at_nudge, Some(4));
}

#[test]
fn a_completed_turn_does_not_bypass_the_fingerprint_so_a_human_started_turn_is_safe() {
    // FINDING-1 REGRESSION (M71 review): a human attached to the SAME session (pmtui Enter)
    // drives turns through the same hooked process, so the turn count advances past our nudge
    // baseline WITHOUT the daemon re-baselining. `count > baseline` therefore proves only "a
    // turn ended since my nudge", NOT "idle now" — the human can leave a NEW turn mid-flight.
    // So a completed-turn count must DEFER to the fingerprint (m70), never bypass it; here the
    // pane is STREAMING a human-started turn and the daemon must stay silent.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.sched.turns_at_nudge = Some(5);
    write_turn_signal(&fx, 7); // 2 human turns completed since our nudge (count 5 → 7)
    // A streaming pane: classifies Idle (bare prompt, no busy marker) but its transcript
    // grows between captures — exactly the false-idle the fingerprint exists to catch.
    fx.driver.set_tail(&sess, "● working on it\n  line one\n❯ ");
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        },
        "a completed-turn count must defer to the fingerprint, not nudge on its own"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "armed the fingerprint, did not nudge"
    );
    // The human's turn keeps streaming (transcript changes) ⇒ the fingerprint RE-ARMS ⇒ still
    // no nudge, though every frame classifies Idle. This is the m70 protection the event's
    // positive direction must never bypass.
    fx.driver
        .set_tail(&sess, "● working on it\n  line one\n  line two\n❯ ");
    let t2 = START + BUSY_RECHECK_S;
    fx.clock.set(t2);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: t2 + BUSY_RECHECK_S
        }
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "a human-started streaming turn must NEVER be nudged (finding-1 regression)"
    );
    assert_eq!(
        fx.sched.idle_confirmations, 1,
        "the changed transcript re-armed the gate"
    );
}

#[test]
fn no_turn_signal_file_falls_back_to_the_fingerprint_gate() {
    // The engine-agnostic FALLBACK: with no signal file (hook not wired / never fired), the
    // gate is exactly m70 — the first Idle only ARMS, requiring a second byte-stable capture.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    // No write_turn_signal ⇒ the file is absent ⇒ `turn_in_progress()` is false ⇒ fingerprint.
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + BUSY_RECHECK_S
        },
        "absent event ⇒ fingerprint fallback ⇒ the first Idle only arms"
    );
    assert!(fx.driver.sent_keys().is_empty(), "no immediate nudge");
    assert_eq!(
        fx.sched.idle_confirmations, 1,
        "the fingerprint gate armed (m70 path), proving the event did not decide"
    );
}

// --- FO-2 marker-less-finish recheck (Milestone D) --------------------------

/// Launch, cold-start grace, then ONE confirmed nudge on an idle pane — the common start
/// for an FO-2 test. Leaves the clock at the nudging instant; the nudge baselines
/// `turns_at_nudge` (turn_count == 0, no signal yet) and `nudged_at_seq` (== 0, no marker),
/// so `awaiting_report()` holds afterward.
fn nudged_then_awaiting(fx: &mut Fx, sess: &str) {
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch → Monitoring{grace}
    fx.clock.set(START + LAUNCH_GRACE_S);
    fx.driver.set_tail(sess, IDLE_PANE);
    tick_confirmed(fx);
    assert_eq!(fx.driver.sent_keys().len(), 1, "the baseline nudge landed");
    assert!(
        ledger(fx).awaiting_report(),
        "and left an unanswered watermark"
    );
}

#[test]
fn a_marker_less_finish_nudges_the_agent_to_report() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    // A turn completes (hook appends a byte) but NO fresh marker is written.
    grow_turn_signal(&fx, 1);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Monitoring { .. }),
        "the first idle capture parks for confirmation: {out:?}"
    );
    assert_eq!(
        fx.driver.sent_keys().len(),
        1,
        "turn completion alone does not prove a newer turn is absent"
    );
    fx.clock.advance(BUSY_RECHECK_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Monitoring { .. }),
        "the confirmed marker recovery nudge parks the cadence: {out:?}"
    );
    let sent = fx.driver.sent_keys();
    assert_eq!(
        sent.len(),
        2,
        "the finished-but-unreported agent is nudged to report, not held silently"
    );
    assert!(
        sent.last()
            .unwrap()
            .1
            .contains("last turn ended without a decision marker"),
        "the nudge names the marker-less finish: {:?}",
        sent.last()
    );
    assert_eq!(
        ledger(&fx).marker_less_rechecks,
        1,
        "the marker-less counter increments"
    );
}

#[test]
fn no_turn_signal_leaves_the_awaiting_report_hold_intact() {
    // With NO completed turn since the nudge, the hold is EXACTLY today's: route to
    // busy_recheck (AwaitingReport), no nudge, and the marker-less counter never moves.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    // No grow_turn_signal ⇒ turn_finished_since_nudge() is false.
    fx.clock.advance(300 + BUSY_RECHECK_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Monitoring { .. }));
    assert_eq!(
        fx.driver.sent_keys().len(),
        1,
        "awaiting_report still holds — no nudge"
    );
    assert_eq!(
        ledger(&fx).marker_less_rechecks,
        0,
        "no completed turn ⇒ no marker-less recheck"
    );
}

#[test]
fn a_turn_still_in_flight_does_not_relax_the_hold() {
    // turn-signal size EQUAL to the nudge baseline means the nudged turn is still running —
    // NOT finished — so the hold is not relaxed and the counter stays 0.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    grow_turn_signal(&fx, 2); // 2 turns done BEFORE the nudge → baseline becomes 2
    nudged_then_awaiting(&mut fx, &sess);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        ledger(&fx).marker_less_rechecks,
        0,
        "size == baseline is mid-turn, not a finish"
    );
}

use super::super::drive::MARKER_LESS_RECHECK_MAX;

#[test]
fn marker_less_finishes_escalate_after_k_without_a_report() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    let k = MARKER_LESS_RECHECK_MAX as usize;
    let mut last = JobTick::WaitingForIntake;
    for i in 1..=k {
        grow_turn_signal(&fx, i); // a fresh completed turn each round, still no marker
        fx.clock.advance(300 + BUSY_RECHECK_S);
        let _ = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
        fx.clock.advance(BUSY_RECHECK_S);
        last = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }
    assert!(
        matches!(last, JobTick::Stuck(_)),
        "K marker-less FINISHES escalate a WorkerStuck (fast, not the 30-min stall): {last:?}"
    );
    let l = ledger(&fx);
    assert!(matches!(l.run, JobRun::Blocked { .. }));
    assert_eq!(l.open_stops[0].kind, StopKind::WorkerStuck);
    // Rounds 1..K-1 each delivered a corrective nudge; round K escalated instead. So the sent
    // count is the baseline nudge + (K-1) corrections — the agent was TOLD it wasn't reporting
    // before the human was ever bothered.
    assert_eq!(
        fx.driver.sent_keys().len(),
        k,
        "nudged each finish under the bound (baseline + K-1), then escalated at the bound"
    );
    assert!(
        l.open_stops[0]
            .question
            .as_deref()
            .unwrap_or_default()
            .contains("without ever writing its decision marker"),
        "the escalation says the agent finished repeatedly without reporting"
    );
}

#[test]
fn an_accepted_marker_bump_resets_the_marker_less_counter() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    // One marker-less recheck (counter → 1).
    grow_turn_signal(&fx, 1);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    fx.clock.advance(BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).marker_less_rechecks, 1);
    // The agent finally writes a marker: any accepted bump resets the counter.
    write_marker(&fx, r#"{"seq":50,"state":"working","status":"back at it"}"#);
    backdate_marker(&fx, 2);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        ledger(&fx).marker_less_rechecks,
        0,
        "an accepted bump resets the marker-less counter"
    );
}

// --- audit fixes (2026-08-21): escalation-edge regressions ------------------

#[test]
fn a_relaunch_voids_the_awaiting_report_hold_so_the_fresh_agent_is_not_falsely_held() {
    // A session that died mid-turn is relaunched. The ledger's `nudged_at_seq` (set when we
    // nudged the now-dead pane) must be cleared on cold start, or `awaiting_report()` stays
    // true across the relaunch and HOLDS the fresh agent — never nudging it, then escalating a
    // misleading "busy with no progress" Stuck. A just-booted agent owes no report.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    assert!(
        ledger(&fx).awaiting_report(),
        "precondition: awaiting a report"
    );
    // The pane dies; the next due tick relaunches it (cold start).
    fx.driver.set_alive(&sess, false);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Monitoring { .. }),
        "relaunch parks on cold-start grace: {out:?}"
    );
    let relaunched = ledger(&fx);
    assert!(
        !relaunched.awaiting_report(),
        "the relaunch cleared nudged_at_seq — the fresh agent is not held awaiting a dead pane's report"
    );
    assert!(matches!(
        relaunched.turn_trace.last().map(|turn| &turn.outcome),
        Some(crate::job::TurnOutcome::NoReport {
            reason: crate::job::TurnNoReportReason::Relaunched,
            ..
        })
    ));
}

#[test]
fn a_reported_idle_agent_is_nudged_after_a_human_attaches_then_detaches() {
    // REGRESSION (2026-08-21 "monitoring · confirming…" wedge): the exact live shape. The agent
    // has REPORTED (so `awaiting_report()` is false) and is idle at its prompt. A human attaches
    // to the same hooked pmloop- session to watch, then detaches. The human-defer gate USED TO
    // re-baseline `turns_at_nudge` to the live turn count; once the human left, `turn_in_progress()`
    // (turn_signal <= turns_at_nudge) read the equal counts as "still mid-turn" every tick, so the
    // idle agent was NEVER nudged again — the row sat on "monitoring · confirming…" forever.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess); // 1 nudge; turns_at_nudge = Some(0), awaiting_report

    // The agent works several turns, then reports a self-scheduled nap → clears awaiting_report.
    grow_turn_signal(&fx, 5);
    write_marker(
        &fx,
        r#"{"seq":9,"state":"monitoring","status":"polling","next_check_s":300}"#,
    );
    fx.clock.advance(BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // disposes the nap (no nudge), clears awaiting
    assert!(
        !ledger(&fx).awaiting_report(),
        "the nap report cleared the awaiting-report hold"
    );
    let sent_before = fx.driver.sent_keys().len();

    // A human attaches to watch, then detaches.
    fx.driver.set_clients(&sess, true);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // defer while attached
    fx.driver.set_clients(&sess, false);

    // Idle at the prompt, nap long overdue: the agent MUST be nudged, not wedged on turn_in_progress.
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    tick_confirmed(&mut fx);
    assert!(
        fx.driver.sent_keys().len() > sent_before,
        "the idle, already-reported agent is nudged after the human detaches — not wedged on \
         turn_in_progress: {:?}",
        fx.driver.sent_keys()
    );
}

#[test]
fn accepted_monitoring_report_clears_a_missed_codex_turn_signal() {
    // Observed Codex failure: the worker wrote an accepted monitoring report but its optional
    // notify hook did not append the turn-complete byte. The unchanged nudge baseline then held
    // `TurnInProgress` 360 times at 5s intervals and raised a false 1800s Stuck. The report itself
    // is pmd-owned proof that this wake yielded, so it must retire both turn baselines.
    let mut fx = setup(Tier::Autopilot, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    grow_turn_signal(&fx, 0); // wired hook, but no completion byte for the upcoming turn
    nudged_then_awaiting(&mut fx, &sess);
    assert_eq!(fx.sched.turns_at_nudge, Some(0));
    assert_eq!(ledger(&fx).turn_count_at_nudge, Some(0));

    write_marker(
        &fx,
        r#"{"seq":9,"state":"monitoring","status":"external build is running","next_check_s":300}"#,
    );
    fx.clock.advance(BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    let reported = ledger(&fx);
    assert!(!reported.awaiting_report());
    assert_eq!(
        fx.sched.turns_at_nudge, None,
        "the accepted monitoring report retires the in-memory turn baseline"
    );
    assert_eq!(
        reported.turn_count_at_nudge, None,
        "the persisted display/restart baseline is retired with it"
    );

    // When the monitoring nap expires, the unchanged zero-byte signal must not hold the session
    // in `TurnInProgress`; two stable Idle observations should deliver the next heartbeat.
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    tick_confirmed(&mut fx);
    assert_eq!(
        fx.driver.sent_keys().len(),
        2,
        "the idle session receives its next heartbeat instead of aging into a false Stuck"
    );
}

#[test]
fn stable_idle_codex_recovers_after_a_working_report_misses_its_turn_signal() {
    // Observed Codex failure: the worker wrote a fresh Working report and returned to its
    // composer, but the optional notify hook did not append a byte. The accepted report clears
    // awaiting_report, while the equal turn baseline still reads as in progress. Codex provides
    // a stronger terminal signal than Claude here: its explicit busy banner disappears at the
    // stable idle composer, so two matching idle captures must recover the heartbeat.
    let mut fx = setup(Tier::Autopilot, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    grow_turn_signal(&fx, 0);
    nudged_then_awaiting(&mut fx, &sess);

    write_marker(
        &fx,
        r#"{"seq":9,"state":"working","status":"external build remains active"}"#,
    );
    backdate_marker(&fx, 2);
    fx.clock.advance(BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    let reported = ledger(&fx);
    assert!(!reported.awaiting_report());
    assert_eq!(
        reported.turn_count_at_nudge,
        Some(0),
        "Working reports retain the hook baseline because they may be written mid-turn"
    );

    fx.driver
        .set_tail(&sess, "finished checking\n› Ask Codex to do anything");
    fx.clock.advance(300 + BUSY_RECHECK_S);
    tick_confirmed(&mut fx);

    assert_eq!(
        fx.driver.sent_keys().len(),
        2,
        "stable Codex idleness must deliver the next heartbeat instead of holding forever"
    );
}

#[test]
fn the_human_defer_gate_leaves_the_nudge_turn_count_untouched() {
    // A human attached to the SAME hooked pmloop- session drives turns, growing the turn signal
    // past our nudge baseline. The defer gate must DEFER without escalating — and it must NOT
    // re-baseline `turns_at_nudge` (that is "turn count at our last NUDGE"). Re-baselining it here
    // wedged `turn_in_progress()` after the human detached from an idle agent — see
    // `a_reported_idle_agent_is_nudged_after_a_human_attaches_then_detaches`. Human turns still
    // cannot escalate a spurious WorkerStuck: `marker_less_recheck` self-limits by nudging first.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess); // turns_at_nudge = Some(0), awaiting_report
    grow_turn_signal(&fx, 3); // a human drives 3 turns on the same session...
    fx.driver.set_clients(&sess, true); // ...while attached
    fx.clock.advance(300 + BUSY_RECHECK_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Monitoring { .. }),
        "attached ⇒ defer, never escalate: {out:?}"
    );
    assert_eq!(
        fx.sched.turns_at_nudge,
        Some(0),
        "the human-defer gate must NOT re-baseline turns_at_nudge to the live count (that wedged \
         turn_in_progress once the human detached from an idle agent)"
    );
    assert_eq!(
        ledger(&fx).marker_less_rechecks,
        0,
        "deferring while attached touches nothing — no marker-less inflation"
    );
    assert_eq!(
        fx.driver.sent_keys().len(),
        1,
        "only the baseline nudge — none delivered while attached"
    );
}

#[test]
fn a_failed_nudge_send_opens_the_busy_stall_window() {
    // Before: a transient send failure was a bare re-park with no `busy_since`, so a
    // PERSISTENTLY failing send only surfaced after the 24h wall-clock. Now it routes through
    // `busy_recheck`, OPENING the stall window so `DEFAULT_STALL_BUSY_S` can escalate a
    // human-dismissable Stuck. Consumes nothing (nothing delivered, retry intact).
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch → grace
    fx.clock.set(START + LAUNCH_GRACE_S);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.driver.fail_send_keys(&sess, true); // every nudge send fails
    let out = tick_confirmed(&mut fx); // 2nd idle obs reaches nudge(); send fails → busy_recheck
    assert!(
        matches!(out, JobTick::Monitoring { .. }),
        "a failed send re-parks for retry: {out:?}"
    );
    assert!(fx.driver.sent_keys().is_empty(), "nothing was delivered");
    assert!(
        ledger(&fx).turn_trace.is_empty(),
        "a failed send must not create a phantom turn"
    );
    assert!(
        fx.sched.busy_since.is_some(),
        "a failed send opens the stall window so the 30-min backstop applies (not just 24h)"
    );
}
