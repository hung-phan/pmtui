//! The decider audit's bounds and its tolerance for a ledger written by another build.
//!
//! Two properties, both of them about not trusting what is on disk: the advice queue is BOUNDED
//! whatever a file claims, and every piece of supervisor-authored prose is one-lined and capped
//! before it is stored. These arms went unevaluated while this module was 200 lines inside a
//! 1,100-line `job.rs`, where its coverage averaged in with everything else.

use super::*;

/// THE ADVICE QUEUE IS BOUNDED BY THE READER, not by whoever wrote the file.
///
/// `pmd` is the only writer of `state.json`, but a ledger is a file on disk in a directory the
/// session's own agent can write. A queue read back unbounded would be a growth path for anything
/// that could append to it, and the cap exists on the READ side so no writer has to be trusted.
#[test]
fn a_longer_advice_queue_than_the_cap_is_truncated_on_read() {
    #[derive(serde::Deserialize)]
    struct Ledger {
        #[serde(default, deserialize_with = "de_advice_queue_bounded")]
        advice_queue: Vec<QueuedAdvice>,
    }
    let one = r#"{"stop_id":"s","report_seq":1,"draft":{"kind":"ambiguity","risk_class":"low"}}"#;
    let over = (0..ADVICE_QUEUE_MAX + 7)
        .map(|_| one)
        .collect::<Vec<_>>()
        .join(",");
    let ledger: Ledger =
        serde_json::from_str(&format!(r#"{{ "advice_queue": [{over}] }}"#)).expect("it loads");
    assert_eq!(
        ledger.advice_queue.len(),
        ADVICE_QUEUE_MAX,
        "the reader caps the queue however long the file is"
    );

    // Under the cap, nothing is dropped.
    let under: Ledger =
        serde_json::from_str(&format!(r#"{{ "advice_queue": [{one},{one}] }}"#)).expect("it loads");
    assert_eq!(under.advice_queue.len(), 2);
}

/// THE DECIDER AUDIT IS COSMETIC, so one unreadable run must not cost the operational state beside
/// it. The reader keeps what it understands and drops the rest, rather than failing the whole ledger.
#[test]
fn an_unreadable_decider_run_is_dropped_not_fatal() {
    #[derive(serde::Deserialize)]
    struct Ledger {
        #[serde(default, deserialize_with = "de_decider_runs_lenient")]
        decider_runs: Vec<DeciderRun>,
    }
    // The two readable runs are built and serialized rather than hand-written: `DeciderRun` denies
    // unknown fields, so a hand-written fixture that drifts from the struct would silently become the
    // "unreadable" case and the test would pass for the wrong reason.
    let readable = |seq: u64| {
        serde_json::to_string(&DeciderRun {
            seq,
            started_at: 10 * seq as i64,
            finished_at: None,
            engine: crate::registry::Engine::Claude,
            model: None,
            target: DeciderTarget::Marker,
            question: "proceed?".into(),
            options: Vec::new(),
            reported_kind: None,
            effect: None,
            policy: DeciderPolicy {
                kind: crate::pmstate::StopKind::Ambiguity,
                labelled_risk: crate::state::RiskClass::Low,
                effective_risk: crate::state::RiskClass::Low,
            },
            outcome: DeciderOutcome::Consulting,
        })
        .expect("serialize")
    };
    let json = format!(
        r#"{{ "decider_runs": [{}, {{ "seq": 2, "invented_by_a_newer_build": true }}, {}] }}"#,
        readable(1),
        readable(3)
    );
    let ledger: Ledger = serde_json::from_str(&json).expect("the document still loads");
    let seqs: Vec<u64> = ledger.decider_runs.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 3], "the readable runs survive in order");
}

/// EVERY OUTCOME CARRYING PROSE IS ONE-LINED AND CAPPED, `Recovered` included.
///
/// This text is SUPERVISOR-AUTHORED — a model's words, stored on the ledger and shown to a human —
/// so the bound is a property of the type rather than of any caller remembering to trim. `Recovered`
/// was the arm no test evaluated, and it is the one that carries an ANSWER rather than a reason.
#[test]
fn every_prose_outcome_is_capped_including_recovered() {
    let long = "y".repeat(DECIDER_AUDIT_TEXT_MAX + 400);
    let cases = [
        DeciderOutcome::Resolved {
            answer: long.clone(),
            reason: long.clone(),
        },
        DeciderOutcome::Refused {
            reason: long.clone(),
        },
        DeciderOutcome::Failed {
            reason: long.clone(),
        },
        DeciderOutcome::Recovered {
            answer: long.clone(),
        },
        DeciderOutcome::Interrupted {
            reason: long.clone(),
        },
        DeciderOutcome::Skipped {
            reason: long.clone(),
            reported_kind: None,
            effect: None,
        },
    ];
    for outcome in cases {
        let capped = cap_decider_outcome(outcome.clone());
        for text in texts(&capped) {
            assert!(
                text.chars().count() <= DECIDER_AUDIT_TEXT_MAX,
                "{outcome:?} kept {} chars",
                text.chars().count()
            );
            assert!(!text.contains('\n'), "{outcome:?} kept a newline");
        }
    }
    // A newline inside a short answer is still collapsed: the audit line is one row.
    let multi = cap_decider_outcome(DeciderOutcome::Recovered {
        answer: "first\nsecond".into(),
    });
    assert_eq!(texts(&multi), vec!["first second".to_string()], "{multi:?}");
    // `Consulting` carries nothing, so it passes through as itself.
    assert_eq!(
        cap_decider_outcome(DeciderOutcome::Consulting),
        DeciderOutcome::Consulting
    );
}

/// Every string an outcome carries, for the assertions above.
fn texts(outcome: &DeciderOutcome) -> Vec<String> {
    match outcome {
        DeciderOutcome::Consulting => Vec::new(),
        DeciderOutcome::Skipped { reason, .. }
        | DeciderOutcome::Refused { reason }
        | DeciderOutcome::Failed { reason }
        | DeciderOutcome::Interrupted { reason } => vec![reason.clone()],
        DeciderOutcome::Recovered { answer } => vec![answer.clone()],
        DeciderOutcome::Resolved { answer, reason } => vec![answer.clone(), reason.clone()],
    }
}
