use super::*;
use crate::pmstate::StopKind;
use crate::state::RiskClass;

#[test]
fn agent_loop_state_round_trips_full_and_minimal() {
    // Minimal: an old/small ledger with only the required fields loads, and
    // run defaults to Idle.
    let json = r#"{ "created_at": 10, "engine": "claude", "updated_at": 10 }"#;
    let st: AgentLoopState = serde_json::from_str(json).unwrap();
    assert_eq!(st.engine, Engine::Claude);
    assert_eq!(st.run, JobRun::Idle);
    assert!(st.conversation_id.is_none() && st.open_stops.is_empty());
    assert_eq!(st.continuations, 0);
    // A pre-S5a ledger has no `pending_context`; `#[serde(default)]` keeps it
    // loadable (deny_unknown_fields rejects EXTRA keys, not MISSING ones).
    assert!(st.pending_context.is_none());
    // Likewise a pre-Slice-2 ledger has no `last_marker_seq`; defaults to 0.
    assert_eq!(st.last_marker_seq, 0);
    // And a pre-m23 ledger has no `advice_inflight`.
    assert!(st.advice_inflight.is_none());
    // A pre-m38 ledger has no `nudged_at_seq` either. It MUST default to `None` rather than
    // `Some(0)`: `None` means "no nudge is outstanding", so an old ledger loaded by a new
    // daemon is nudgeable, while `Some(0)` would read as "awaiting a report" and park a healthy
    // session until the 30-minute stall backstop fired.
    assert!(st.nudged_at_seq.is_none());
    // A pre-B ledger has none of the decider-lane fields; all default (empty/zero/None).
    assert!(st.digest.is_zero());
    assert!(st.decisions.is_empty());
    assert!(st.decider_runs.is_empty());
    assert_eq!(st.turn_seq, 0);
    assert!(st.turn_trace.is_empty());
    assert!(!st.decider_latched);
    assert!(st.decider_latch_reason.is_none());
    assert!(st.situation.is_none());
    // A pre-D ledger has neither Milestone-D counter; both default to 0.
    assert_eq!(st.stale_plan_streak, 0);
    assert_eq!(st.marker_less_rechecks, 0);
    // A pre-fallback ledger has no `resume_unconfirmed`; it defaults to `false` — a
    // confirmed cid that always RESUMES (never the one-shot create-fallback).
    assert!(!st.resume_unconfirmed);

    // Full: round-trips a Monitoring run + conversation id + cadence + status +
    // a pending auto-flow context to inject on the next wake + the marker watermark.
    let full = AgentLoopState {
        created_at: 1,
        conversation_id: Some("uuid-1".into()),
        // An adopted-but-unproven seed: the round-trip must preserve the fallback gate
        // across a save, or a relaunch after a daemon restart would resume a ghost id.
        resume_unconfirmed: true,
        engine: Engine::Codex,
        cadence_s: Some(300),
        // PINNED, so the round-trip covers the flag too: a ledger that forgot it across a save
        // would hand the dial back to the agent on the next report, which is the bug it exists to
        // stop (user: *"it revert my cadence setting later"*).
        cadence_pinned: true,
        applied_wake_generation: 9,
        run: JobRun::Monitoring { until: 1800 },
        open_stops: Vec::new(),
        last_status: Some("watching the channel".into()),
        last_plan: Some("wire the alert".into()),
        continuations: 2,
        stale_plan_streak: 4,
        marker_less_rechecks: 2,
        nudged_at: Some(1_790_908_704),
        last_marker_seq: 100,
        report_generation: 12,
        last_marker_revision: Some(MarkerRevision {
            modified_ns: 1,
            len: 2,
            inode: 3,
            fingerprint: "abc123".into(),
        }),
        pending_context: Some("auto-approved: proceed".into()),
        advice_queue: Vec::new(),
        nudged_at_seq: Some(100),
        nudged_at_report_generation: Some(12),
        turn_count_at_nudge: Some(7),
        advice_inflight: Some(ParkedAdvice {
            seq: 4,
            stop_ids: vec!["stop-a".into()],
            pane_dialog: false,
        }),
        decider_latched: true,
        decider_latch_reason: Some("transport unavailable".into()),
        decider_runs: Vec::new(),
        turn_seq: 0,
        turn_trace: Vec::new(),
        // A couple of autopilot-feed events, so the round-trip covers the new field
        // (including a coalesced count and a held-reason payload).
        events: vec![
            AutopilotEvent {
                at: 40,
                count: 3,
                kind: AutopilotEventKind::Held(HoldReason::Busy),
            },
            AutopilotEvent {
                at: 41,
                count: 1,
                kind: AutopilotEventKind::Reported(Some("watching".into())),
            },
        ],
        digest: DecisionCounters {
            disposed: 5,
            working: 3,
            monitoring: 1,
            auto_flow: 0,
            escalated: 1,
            stalled: 0,
        },
        decisions: vec![DecisionRecord::at(
            41,
            Some(100),
            DecisionKind::Escalated,
            Some("ship it?".into()),
            vec!["stop-a".into()],
        )],
        situation: Some(LedgerSituation {
            state: WakeState::Blocked,
            status: Some("need a decision".into()),
            open_stops: vec!["stop-a".into()],
            seq: 100,
            at: 42,
        }),
        updated_at: 42,
    };
    let round: AgentLoopState =
        serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
    assert_eq!(round, full);
    assert!(round.resume_unconfirmed, "the fallback gate round-trips");
    assert_eq!(round.stale_plan_streak, 4);
    assert_eq!(round.marker_less_rechecks, 2);
    assert_eq!(round.last_plan.as_deref(), Some("wire the alert"));
    assert_eq!(round.last_marker_seq, 100);
    assert_eq!(round.advice_inflight.as_ref().unwrap().seq, 4);
    assert_eq!(round.events.len(), 2);
    assert_eq!(round.events[0].count, 3);
}

#[test]
fn parked_advice_defaults_to_marker_delivery_and_round_trips_a_dialog_target() {
    let old: ParkedAdvice = serde_json::from_str(r#"{"seq":3,"stop_ids":["stop-a"]}"#).unwrap();
    assert!(!old.pane_dialog, "legacy records are marker debts");

    let dialog = ParkedAdvice {
        seq: 4,
        stop_ids: Vec::new(),
        pane_dialog: true,
    };
    let json = serde_json::to_string(&dialog).unwrap();
    assert!(json.contains(r#""pane_dialog":true"#));
    assert_eq!(serde_json::from_str::<ParkedAdvice>(&json).unwrap(), dialog);
}

#[test]
fn a_fresh_ledger_omits_the_decider_fields() {
    // skip_serializing_if keeps a fresh ledger loadable by an older daemon (the ledger
    // is deny_unknown_fields), and keeps an idle session's state.json unbloated.
    let json = serde_json::to_string(&AgentLoopState::fresh(Engine::Claude, None, 1)).unwrap();
    assert!(!json.contains("digest"), "{json}");
    assert!(!json.contains("decisions"), "{json}");
    assert!(!json.contains("situation"), "{json}");
}

#[test]
fn a_bogus_decision_is_dropped_and_the_operational_ledger_loads() {
    // The decisions slice is machine-cosmetic: one unparseable entry (a kind a newer
    // pmd added, a hand-edit) must never take the operational ledger down with it.
    let mut st = AgentLoopState::fresh(Engine::Claude, Some(60), 1);
    st.last_marker_seq = 9;
    st.decisions.push(DecisionRecord::at(
        3,
        Some(7),
        DecisionKind::Escalated,
        Some("ship it?".into()),
        vec!["stop-1".into()],
    ));
    let mut v = serde_json::to_value(&st).unwrap();
    v.get_mut("decisions")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"at": 4, "kind": {"teleported": "mars"}}));
    let round: AgentLoopState = serde_json::from_str(&serde_json::to_string(&v).unwrap())
        .expect("the ledger still loads despite a bogus decision");
    assert_eq!(round.last_marker_seq, 9, "operational field intact");
    assert_eq!(round.decisions.len(), 1, "only the bad decision is dropped");
}

#[test]
fn record_decision_coalesces_and_bounds_the_slice() {
    let mut st = AgentLoopState::fresh(Engine::Claude, None, 0);
    // A RUN of the SAME (kind, summary, stop_ids) is ONE entry: latest time, bumped count.
    for at in 1..=3 {
        st.record_decision(DecisionRecord::at(
            at,
            Some(at as u64),
            DecisionKind::AutoFlow,
            Some("approve dprint".into()),
            vec!["stop-a".into()],
        ));
    }
    assert_eq!(st.decisions.len(), 1);
    assert_eq!(st.decisions[0].count, 3);
    assert_eq!(st.decisions[0].at, 3, "coalescing advances the timestamp");
    // The PER-KIND counter bumps each call; `disposed` is NEVER bumped here (the seam owns it).
    assert_eq!(st.digest.auto_flow, 3);
    assert_eq!(
        st.digest.disposed, 0,
        "record_decision must not bump disposed"
    );
    // Divergent stop_ids create a NEW entry — never a coalesced row with stale ids (Missing-2).
    st.record_decision(DecisionRecord::at(
        4,
        Some(4),
        DecisionKind::AutoFlow,
        Some("approve dprint".into()),
        vec!["stop-b".into()],
    ));
    assert_eq!(st.decisions.len(), 2, "different stop_ids never coalesce");
    // Bounded: push > MAX DISTINCT entries → len == MAX, and the NEWEST survives the drain.
    let mut st2 = AgentLoopState::fresh(Engine::Claude, None, 0);
    let n = DECISIONS_SLICE_MAX as u64 + 5;
    for i in 0..n {
        st2.record_decision(DecisionRecord::at(
            i as Epoch,
            Some(i),
            DecisionKind::Working,
            Some(format!("s{i}")),
            Vec::new(),
        ));
    }
    assert_eq!(st2.decisions.len(), DECISIONS_SLICE_MAX);
    assert_eq!(
        st2.decisions.last().unwrap().summary.as_deref(),
        Some(format!("s{}", n - 1).as_str()),
        "the newest decision survives the bound"
    );
}

#[test]
fn record_event_coalesces_a_run_and_bounds_the_ring() {
    let mut st = AgentLoopState::fresh(Engine::Claude, None, 0);
    // A RUN of the same decision is ONE entry: latest time, bumped count — this is what
    // keeps a heartbeat held on the same reason across many rechecks from flooding it.
    st.record_event(1, AutopilotEventKind::Held(HoldReason::Busy));
    st.record_event(2, AutopilotEventKind::Held(HoldReason::Busy));
    st.record_event(3, AutopilotEventKind::Held(HoldReason::Busy));
    assert_eq!(st.events.len(), 1);
    assert_eq!(st.events[0].count, 3);
    assert_eq!(st.events[0].at, 3, "coalescing advances the timestamp");
    // A different decision starts a new entry…
    st.record_event(4, AutopilotEventKind::Nudged);
    assert_eq!(st.events.len(), 2);
    assert_eq!(st.events[1].count, 1);
    // …and a different HOLD REASON is a different decision, not a Busy coalesce.
    st.record_event(5, AutopilotEventKind::Held(HoldReason::AwaitingReport));
    assert_eq!(st.events.len(), 3);
    // Bounded: past the cap the OLDEST are dropped and the newest kept.
    for i in 0..AUTOPILOT_EVENTS_MAX as i64 {
        st.record_event(100 + i, AutopilotEventKind::Reported(Some(format!("s{i}"))));
    }
    assert_eq!(st.events.len(), AUTOPILOT_EVENTS_MAX);
    assert!(
        matches!(
            st.events.last().unwrap().kind,
            AutopilotEventKind::Reported(_)
        ),
        "the newest decision survives the bound"
    );
}

#[test]
fn a_bogus_event_is_dropped_and_the_operational_ledger_still_loads() {
    // The feed is COSMETIC: one unparseable event (a kind a newer pmd added, a hand-edit)
    // must never take the operational ledger — run, cadence, watermarks — down with it.
    let mut st = AgentLoopState::fresh(Engine::Claude, Some(60), 1);
    st.cadence_pinned = true;
    st.last_marker_seq = 9;
    st.run = JobRun::Monitoring { until: 1800 };
    st.record_event(3, AutopilotEventKind::Reported(Some("indexing".into())));
    // Serialize, then inject a bogus event kind into the array on disk.
    let mut v = serde_json::to_value(&st).unwrap();
    v.get_mut("events")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"at": 4, "count": 1, "kind": {"teleported": "to mars"}}));
    let round: AgentLoopState = serde_json::from_str(&serde_json::to_string(&v).unwrap())
        .expect("the ledger still loads despite a bogus event");
    // Operational fields intact.
    assert_eq!(round.cadence_s, Some(60));
    assert!(round.cadence_pinned);
    assert_eq!(round.last_marker_seq, 9);
    assert_eq!(round.run, JobRun::Monitoring { until: 1800 });
    // The bogus event is dropped; the valid one survives.
    assert_eq!(round.events.len(), 1, "only the bad event is dropped");
    assert!(
        matches!(&round.events[0].kind, AutopilotEventKind::Reported(Some(s)) if s == "indexing")
    );
}

#[test]
fn record_event_caps_a_paragraph_detail_to_one_bounded_line() {
    let mut st = AgentLoopState::fresh(Engine::Claude, None, 0);
    // Longer than the cap by construction, so this exercises the clip whatever the constant is.
    let huge = format!(
        "line one\nline two {}",
        "x".repeat(events::AUTOPILOT_DETAIL_MAX + 500)
    );
    st.record_event(1, AutopilotEventKind::Reported(Some(huge)));
    let d = st.events[0].kind.detail().unwrap();
    assert!(!d.contains('\n'), "newlines are collapsed: {d:?}");
    assert!(
        d.chars().count() <= events::AUTOPILOT_DETAIL_MAX,
        "detail is length-capped ({} chars)",
        d.chars().count()
    );
    assert!(
        d.ends_with('\u{2026}'),
        "a clipped detail admits it with an ellipsis: {d:?}"
    );
    // A realistic one-liner (well under the raised cap) is kept in FULL — no truncation.
    let mut st2 = AgentLoopState::fresh(Engine::Claude, None, 0);
    let full = "reindexed 42 files and re-ran the suite twice; all green, ready for the next nudge";
    st2.record_event(1, AutopilotEventKind::Reported(Some(full.into())));
    assert_eq!(
        st2.events[0].kind.detail().as_deref(),
        Some(full),
        "a normal status is not clipped"
    );
}

#[test]
fn an_absent_parked_consult_is_not_even_serialized() {
    // `skip_serializing_if` keeps a ledger written by THIS daemon loadable by an older
    // one on the overwhelming majority of ticks (no consult in flight), which matters
    // because `AgentLoopState` is `deny_unknown_fields`.
    let json = serde_json::to_string(&AgentLoopState::fresh(Engine::Claude, None, 1)).unwrap();
    assert!(!json.contains("advice_inflight"), "{json}");
}

#[test]
fn job_run_variants_round_trip() {
    for r in [
        JobRun::Idle,
        JobRun::Running {
            seq: 3,
            session: "pmj-x-abcd1234-3".into(),
            deadline: 999,
        },
        JobRun::Monitoring { until: 1234 },
        JobRun::Blocked {
            stop_ids: vec!["stop-1".into()],
            since: 77,
        },
    ] {
        let round: JobRun = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(round, r);
    }
}

#[test]
fn retime_resets_the_plan_staleness_streak() {
    // A human cadence edit is a fresh start for the stall signal (LOCKED D-10) — a
    // fail-safe direction (it can only ever delay a plan-stall escalation).
    let mut st = AgentLoopState::fresh(Engine::Claude, Some(300), 0);
    st.run = JobRun::Monitoring { until: 1000 };
    st.stale_plan_streak = 5;
    st.retime(60, 100);
    assert_eq!(
        st.stale_plan_streak, 0,
        "retime resets the plan-staleness streak"
    );
}

#[test]
fn fresh_is_idle_with_no_conversation() {
    let st = AgentLoopState::fresh(Engine::Claude, Some(300), 5);
    assert_eq!(st.run, JobRun::Idle);
    assert!(st.conversation_id.is_none());
    assert_eq!(st.cadence_s, Some(300));
    assert_eq!(st.created_at, 5);
    assert_eq!(st.updated_at, 5);
}

#[test]
fn wake_report_parses_each_state_and_minimal() {
    // Minimal working report — just the state. A pre-Slice-2 report has no `seq`;
    // `#[serde(default)]` loads it as 0.
    let w: WakeReport = serde_json::from_str(r#"{ "state": "working" }"#).unwrap();
    assert_eq!(w.state, WakeState::Working);
    assert!(w.stops.is_empty() && w.status.is_none() && w.next_check_s.is_none());
    assert_eq!(w.seq, 0);

    // Monitoring with a self-scheduled nap.
    let m: WakeReport =
        serde_json::from_str(r#"{ "state": "monitoring", "next_check_s": 900 }"#).unwrap();
    assert_eq!(m.state, WakeState::Monitoring);
    assert_eq!(m.next_check_s, Some(900));

    // Blocked, carrying a monotonic seq + a stop draft + a captured conversation id.
    let b: WakeReport = serde_json::from_str(
        r#"{ "state": "blocked",
             "seq": 1786519000,
             "status": "need a decision",
             "conversation_id": "sess-abc",
             "stops": [{ "kind": "ambiguity",
                         "effect": { "scope": "local", "reversibility": "reversible",
                                     "authority": "ordinary" },
                         "question": "which channel?",
                         "options": ["a","b"], "risk_class": "medium" }] }"#,
    )
    .unwrap();
    assert_eq!(b.state, WakeState::Blocked);
    assert_eq!(b.seq, 1_786_519_000);
    assert_eq!(b.conversation_id.as_deref(), Some("sess-abc"));
    assert_eq!(b.stops.len(), 1);
    assert_eq!(b.stops[0].kind, StopKind::Ambiguity);
    assert!(!b.stops[0].effect.requires_human());
    assert_eq!(b.stops[0].risk_class, RiskClass::Medium);
}

#[test]
fn stop_effect_escalates_only_explicit_human_owned_axes() {
    use crate::worker::{EffectAuthority, EffectReversibility, EffectScope, StopEffect};

    let local = StopEffect {
        scope: EffectScope::Local,
        reversibility: EffectReversibility::Reversible,
        authority: EffectAuthority::Ordinary,
        unrecognized_metadata: false,
    };
    assert!(!local.requires_human());
    let partial: StopEffect = serde_json::from_str(r#"{"scope":"local"}"#).unwrap();
    assert_eq!(partial.scope, EffectScope::Local);
    assert_eq!(partial.reversibility, EffectReversibility::Unknown);
    assert_eq!(partial.authority, EffectAuthority::Unknown);
    assert!(!partial.requires_human());
    let future: StopEffect = serde_json::from_str(
        r#"{
            "scope":"cross_account",
            "reversibility":"compensatable",
            "authority":"delegated",
            "future_axis":"future_value"
        }"#,
    )
    .unwrap();
    assert_eq!(future.scope, EffectScope::Unknown);
    assert_eq!(future.reversibility, EffectReversibility::Unknown);
    assert_eq!(future.authority, EffectAuthority::Unknown);
    assert!(future.unrecognized_metadata);
    assert!(!future.requires_human());
    assert!(future.summary().contains("unrecognized_metadata=true"));
    assert!(
        serde_json::to_string(&future)
            .unwrap()
            .contains("\"unrecognized_metadata\":true")
    );
    let additive: StopEffect = serde_json::from_str(
        r#"{
            "scope":"local",
            "reversibility":"reversible",
            "authority":"ordinary",
            "new_safety_axis":"unsafe"
        }"#,
    )
    .unwrap();
    assert_eq!(additive.scope, EffectScope::Local);
    assert_eq!(additive.reversibility, EffectReversibility::Reversible);
    assert_eq!(additive.authority, EffectAuthority::Ordinary);
    assert!(additive.unrecognized_metadata);
    assert!(!additive.requires_human());
    assert!(
        !serde_json::to_string(&local)
            .unwrap()
            .contains("unrecognized_metadata")
    );

    for effect in [
        StopEffect {
            scope: EffectScope::External,
            ..local
        },
        StopEffect {
            reversibility: EffectReversibility::Irreversible,
            ..local
        },
        StopEffect {
            authority: EffectAuthority::Privileged,
            ..local
        },
    ] {
        let summary = effect.summary();
        assert!(effect.requires_human(), "{summary}");
    }
    for effect in [
        StopEffect {
            scope: EffectScope::Unknown,
            ..local
        },
        StopEffect {
            reversibility: EffectReversibility::Unknown,
            ..local
        },
        StopEffect {
            authority: EffectAuthority::Unknown,
            ..local
        },
    ] {
        let summary = effect.summary();
        assert!(
            !effect.requires_human(),
            "unknown is evidence for the decider, not a human-owned effect: {summary}"
        );
    }
}

#[test]
fn persisted_advice_queue_is_bounded_on_load() {
    let draft = crate::worker::StopDraft {
        kind: StopKind::Ambiguity,
        effect: crate::worker::StopEffect::default(),
        question: "Which formatter?".into(),
        options: vec!["prettier".into(), "dprint".into()],
        context_ref: None,
        risk_class: RiskClass::Low,
    };
    let mut state = AgentLoopState::fresh(Engine::Claude, None, 1);
    state.advice_queue = (0..ADVICE_QUEUE_MAX + 2)
        .map(|index| QueuedAdvice {
            stop_id: format!("stop-{index}"),
            report_seq: 7,
            draft: draft.clone(),
        })
        .collect();

    let loaded: AgentLoopState =
        serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();

    assert_eq!(loaded.advice_queue.len(), ADVICE_QUEUE_MAX);
    assert_eq!(loaded.advice_queue[0].stop_id, "stop-0");
    assert_eq!(
        loaded.advice_queue.last().map(|item| item.stop_id.as_str()),
        Some("stop-7")
    );
}

#[test]
fn wake_report_parses_next_step() {
    // A working bump that carries the agent's own next step.
    let r: WakeReport = serde_json::from_str(
        r#"{"state":"working","seq":7,"status":"tests running","next_step":"wire the alert if green"}"#,
    )
    .unwrap();
    assert_eq!(r.next_step.as_deref(), Some("wire the alert if green"));
    // A terse bump with no next_step still parses, defaulting to None.
    let terse: WakeReport = serde_json::from_str(r#"{"state":"working"}"#).unwrap();
    assert_eq!(terse.next_step, None);
}

#[test]
fn old_json_missing_marker_fields_loads_with_defaults() {
    // A pre-Slice-2 ledger has no `last_marker_seq`, and a pre-Slice-2 report has
    // no `seq`; `#[serde(default)]` keeps both loadable at 0 (deny_unknown_fields
    // rejects EXTRA keys, not MISSING ones — the marker watermark starts fresh).
    let st: AgentLoopState =
        serde_json::from_str(r#"{ "created_at": 1, "engine": "claude", "updated_at": 1 }"#)
            .unwrap();
    assert_eq!(st.last_marker_seq, 0);
    assert_eq!(st.report_generation, 0);
    assert!(st.last_marker_revision.is_none());
    assert!(st.nudged_at_report_generation.is_none());
    let w: WakeReport = serde_json::from_str(r#"{ "state": "monitoring" }"#).unwrap();
    assert_eq!(w.seq, 0);
}

#[test]
fn wake_report_rejects_unknown_fields() {
    // Strict parse: contract drift surfaces (the disposer then falls back to a
    // lenient "working" rather than silently trusting a malformed report).
    assert!(
        serde_json::from_str::<WakeReport>(r#"{ "state": "working", "surprise": 1 }"#).is_err()
    );
    let legacy: WakeReport = serde_json::from_str(
        r#"{ "state": "blocked", "stops": [
            { "kind": "ambiguity", "risk_class": "low" }
        ] }"#,
    )
    .unwrap();
    assert!(
        !legacy.stops[0].effect.requires_human(),
        "a legacy stop without effect metadata remains eligible for goal-aware review"
    );
}
