//! The event feed's vocabulary: what each kind is called, what detail it carries, what gets capped,
//! and what the lenient readers do with a ledger written by an older build.
//!
//! These are table-driven on purpose. Every variant here is something a human reads off the decision
//! lane, and an arm nobody evaluates is an arm nobody has checked the wording of — several were in
//! exactly that state until `job.rs` was split and this module's coverage stopped being averaged in
//! with a thousand lines of neighbours.

use super::*;

/// EVERY `DecisionKind` HAS ITS WIRE WORD, and they are all different.
///
/// This token is written into `decisions.md` and `raw.jsonl`, so it is a format, not a label: two
/// kinds sharing a word would make the ledger ambiguous after the fact, when it is the only record
/// of what the daemon decided.
#[test]
fn every_decision_kind_has_a_distinct_snake_case_token() {
    let all = [
        (DecisionKind::Working, "working"),
        (DecisionKind::Monitoring, "monitoring"),
        (DecisionKind::AutoFlow, "auto_flow"),
        (DecisionKind::Escalated, "escalated"),
        (DecisionKind::Stalled, "stalled"),
    ];
    for (kind, word) in all {
        assert_eq!(kind.as_str(), word, "{kind:?}");
        // The wire form and the token agree, so `decisions.md` and the JSON cannot drift apart.
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            format!("\"{word}\""),
            "{kind:?} serializes differently from its token"
        );
    }
    let words: std::collections::HashSet<&str> = all.iter().map(|(_, w)| *w).collect();
    assert_eq!(words.len(), all.len(), "two kinds share a token");
}

/// EVERY `AutopilotEventKind` HAS A LABEL AND A DEFINED DETAIL, including the two ends of the
/// escalation loop (`stuck`, `you answered`) that nothing exercised.
///
/// The feed reads as pmd talking to itself unless both halves are there, which is why `Answered`
/// exists at all — so its wording is worth pinning rather than assuming.
#[test]
fn every_event_kind_reports_a_label_and_its_detail() {
    use AutopilotEventKind::*;
    let text = || Some("the detail".to_string());
    let cases: Vec<(AutopilotEventKind, &str, Option<String>)> = vec![
        (Launched, "launched", None),
        (Nudged, "nudged", None),
        (
            Held(HoldReason::Busy),
            "held",
            Some(HoldReason::Busy.label().to_string()),
        ),
        (Reported(text()), "reported", text()),
        (AutoAnswered(text()), "auto-answered", text()),
        (SupervisorResolved(text()), "resolved", text()),
        (
            CadenceChanged("5m → 1m".into()),
            "cadence",
            Some("5m → 1m".into()),
        ),
        (Escalated(text()), "needs you", text()),
        (Stuck(text()), "stuck", text()),
        (Answered(text()), "you answered", text()),
    ];
    for (kind, label, detail) in &cases {
        assert_eq!(kind.label(), *label, "{kind:?}");
        assert_eq!(kind.detail(), *detail, "{kind:?}");
    }
    // A detail-carrying kind with nothing in it has no detail, rather than an empty line.
    for kind in [
        Reported(None),
        AutoAnswered(None),
        SupervisorResolved(None),
        Escalated(None),
        Stuck(None),
        Answered(None),
    ] {
        assert_eq!(kind.detail(), None, "{kind:?}");
    }
    let labels: std::collections::HashSet<&str> = cases.iter().map(|(k, ..)| k.label()).collect();
    assert_eq!(labels.len(), cases.len(), "two kinds share a label");
}

/// THE DECISION LANE SHOWS WHAT AUTOPILOT DID, not every tick it took.
#[test]
fn routine_heartbeat_activity_is_not_a_decision() {
    use AutopilotEventKind::*;
    for routine in [Nudged, Held(HoldReason::Transient), Reported(None)] {
        assert!(!routine.is_decision(), "{routine:?}");
    }
    for decision in [
        Launched,
        AutoAnswered(None),
        SupervisorResolved(None),
        CadenceChanged("1m".into()),
        Escalated(None),
        Stuck(None),
        Answered(None),
    ] {
        assert!(decision.is_decision(), "{decision:?}");
    }
}

/// EVERY FREE-TEXT ARM IS CAPPED AND ONE-LINED, so one feed entry can never store a paragraph.
///
/// A feed entry is drawn on one row. An agent is the author of most of this text, so the bound is a
/// property of the type rather than of any caller remembering to trim.
#[test]
fn every_free_text_event_is_one_lined_and_capped() {
    use AutopilotEventKind::*;
    let long = || Some("x".repeat(AUTOPILOT_DETAIL_MAX + 500));
    let built: Vec<AutopilotEventKind> = vec![
        Reported(long()),
        AutoAnswered(long()),
        SupervisorResolved(long()),
        Escalated(long()),
        Stuck(long()),
        Answered(long()),
        CadenceChanged("x".repeat(AUTOPILOT_DETAIL_MAX + 500)),
    ];
    for kind in built {
        let label = kind.label();
        let detail = kind.capped().detail().expect("it carries text");
        assert!(
            detail.chars().count() <= AUTOPILOT_DETAIL_MAX,
            "{label}: {} chars survived the cap",
            detail.chars().count()
        );
    }
    // A newline is collapsed, not kept: a feed entry is one row.
    let multi = Reported(Some("first\nsecond".into())).capped();
    let shown = multi.detail().expect("text");
    assert!(!shown.contains('\n'), "{shown:?}");
    // The arms with no free text pass through untouched.
    assert_eq!(Launched.capped(), Launched);
    assert_eq!(Nudged.capped(), Nudged);
    assert_eq!(
        Held(HoldReason::Busy).capped(),
        Held(HoldReason::Busy),
        "a hold's reason is an enum, so there is nothing to cap"
    );
}

/// A LEDGER FROM ANOTHER BUILD STILL LOADS. The feed and the decision slice are COSMETIC, so one
/// unreadable entry must not cost the operational state beside it — the run state, the cadence, the
/// pending context. Each reader drops what it cannot parse and keeps the rest.
#[test]
fn the_lenient_readers_drop_only_what_they_cannot_parse() {
    #[derive(serde::Deserialize)]
    struct Feed {
        #[serde(default, deserialize_with = "de_events_lenient")]
        events: Vec<AutopilotEvent>,
        #[serde(default, deserialize_with = "de_decisions_lenient")]
        decisions: Vec<DecisionRecord>,
    }

    let json = r#"{
      "events": [
        { "at": 1, "kind": "launched" },
        { "at": 2, "kind": { "invented_by_a_newer_build": "???" } },
        { "at": 3, "kind": "nudged" }
      ],
      "decisions": [
        { "at": 1, "kind": "working", "summary": "kept" },
        { "at": 2, "kind": "a_kind_this_build_never_heard_of", "summary": "dropped" }
      ]
    }"#;
    let feed: Feed = serde_json::from_str(json).expect("the whole document still loads");
    assert_eq!(
        feed.events.len(),
        2,
        "the two readable events survive: {:?}",
        feed.events
    );
    assert_eq!(feed.events[0].kind, AutopilotEventKind::Launched);
    assert_eq!(feed.events[1].kind, AutopilotEventKind::Nudged);
    assert_eq!(feed.decisions.len(), 1, "{:?}", feed.decisions);
    assert_eq!(feed.decisions[0].kind, DecisionKind::Working);
    assert_eq!(feed.decisions[0].summary.as_deref(), Some("kept"));
}

/// The counters are a tally, and `is_zero` is what keeps an untouched one out of the written ledger.
#[test]
fn the_decision_counters_report_emptiness() {
    let mut counters = DecisionCounters::default();
    assert!(counters.is_zero());
    counters.working = 1;
    assert!(!counters.is_zero());
}
