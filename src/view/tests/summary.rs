//! The one readable projection of a session: what it carries, what it refuses to carry, and the
//! two fields a caller will misread if they are not pinned.

use std::path::PathBuf;

use crate::registry::{Engine, Mode, ProjectEntry};
use crate::state::Tier;
use crate::view::{Posture, ProjectView, SessionSummary};

const NOW: crate::clock::Epoch = 1_700_000_000;

fn entry(id: &str) -> ProjectEntry {
    ProjectEntry {
        id: id.into(),
        display_name: None,
        root: PathBuf::from(format!("/tmp/{id}")),
        enabled: true,
        mode: Mode::AgentLoop,
        engine: Some(Engine::Claude),
        worker_model: None,
        initial_prompt: None,
        task_title: None,
        forked_from: None,
        spawned_by: None,
        launch: None,
        conversation_id: None,
        cadence_s: None,
    }
}

/// A view with nothing in it, so each test sets only the field it is about.
fn view() -> ProjectView {
    ProjectView::read_agent_loop(
        "x",
        &crate::state::ProjectPaths::for_session("/nonexistent/pmtui-summary-test", "x"),
        true,
        NOW,
    )
}

/// THE DRIFT FIX, and the reason this type exists at all. `message --list` built its own document
/// by hand and took `display_name` RAW, while the dashboard passes the same field through
/// `normalize_display_name`. One field, two answers, already shipped. Both surfaces now read the
/// projection, so a name the registry rules reject is ABSENT rather than passed through.
#[test]
fn a_name_the_registry_rules_reject_is_absent_rather_than_raw() {
    let mut e = entry("bot");
    e.display_name = Some("  The Build  ".into());
    let s = SessionSummary::of(&e, &view(), NOW, false);
    assert_eq!(
        s.name.as_deref(),
        Some("The Build"),
        "trimmed, as the dashboard trims it"
    );

    // A control byte is rejected by `normalize_display_name`, so no name is reported at all.
    e.display_name = Some("bad\u{7}name".into());
    let s = SessionSummary::of(&e, &view(), NOW, false);
    assert_eq!(s.name, None, "a rejected name is absent, never raw");

    // And an empty label is no label, rather than an empty string a caller would print.
    e.display_name = Some(String::new());
    assert_eq!(SessionSummary::of(&e, &view(), NOW, false).name, None);
}

/// `working` IS A TRI-STATE AND `null` IS NOT IDLE. A caller that reads the absent case as idle
/// reports a working agent as finished, which is the oldest bug in this product — the status glyph
/// means working, not merely alive. `working_confirmed` says whether a live pane backed it up, and
/// is false from any one-shot read because confirmation needs a driver and two ticks.
#[test]
fn working_is_unknown_until_proven_and_confirmation_is_reported_separately() {
    let e = entry("bot");

    let mut v = view();
    v.agent_working = None;
    let s = SessionSummary::of(&e, &v, NOW, true);
    assert_eq!(s.working, None, "not known");
    assert!(
        !s.working_confirmed,
        "nothing to confirm, so confirmation must not read true"
    );

    v.agent_working = Some(true);
    assert!(
        SessionSummary::of(&e, &v, NOW, true).working_confirmed,
        "a pane backed it"
    );
    assert!(
        !SessionSummary::of(&e, &v, NOW, false).working_confirmed,
        "a one-shot read has no pane to back it"
    );
    assert_eq!(SessionSummary::of(&e, &v, NOW, false).working, Some(true));
}

/// A WAIT IS REPORTED ONLY WHILE SOMETHING IS WAITING, and never as a negative number. The age is
/// what an escalation's own staleness is measured from, so a clock that moved backwards must not
/// turn into a wait in the future.
#[test]
fn the_wait_is_clamped_at_zero_and_only_reported_when_a_decision_is_open() {
    let e = entry("bot");

    let mut v = view();
    v.posture = Posture::NeedsYou;
    v.oldest_stop_since = Some(NOW - 420);
    let s = SessionSummary::of(&e, &v, NOW, false);
    assert_eq!(s.waiting_s, Some(420));
    assert!(s.needs_you, "needs_you is the field to branch on");

    // A future timestamp floors at zero rather than going negative.
    v.oldest_stop_since = Some(NOW + 999);
    assert_eq!(SessionSummary::of(&e, &v, NOW, false).waiting_s, Some(0));

    // Nothing open: no wait, whatever the ledger's oldest stop says.
    v.posture = Posture::Monitoring;
    v.oldest_stop_since = Some(NOW - 420);
    let s = SessionSummary::of(&e, &v, NOW, false);
    assert_eq!(s.waiting_s, None, "nothing is waiting");
    assert!(!s.needs_you);
}

/// THE PROJECTION IS SMALL ON PURPOSE. `ProjectView` is the TUI's working set, and serializing it
/// would publish the dashboard's internal model as a contract and hand a peer handles to detached
/// work. These names must never appear in the document.
#[test]
fn the_document_carries_no_dashboard_internals() {
    let mut e = entry("bot");
    e.display_name = Some("The Build".into());
    let json = serde_json::to_string(&SessionSummary::of(&e, &view(), NOW, false)).unwrap();
    for internal in [
        "step_id",
        "turn_trace",
        "decider_runs",
        "autopilot_events",
        "advice_inflight",
        "advice_queue",
        "decision_digest",
        "stops",
    ] {
        assert!(
            !json.contains(internal),
            "{internal} must not reach the public document: {json}"
        );
    }
    // And what it DOES carry is what a coordinator needs to choose whom to talk to.
    for field in [
        "\"id\"",
        "\"state\"",
        "\"needs_you\"",
        "\"live\"",
        "\"attached\"",
    ] {
        assert!(json.contains(field), "{field} is missing: {json}");
    }
}

/// A JOB IS MARKED, because the one thing a caller must not do with a job is message it.
#[test]
fn a_job_row_says_so() {
    use crate::registry::{LaunchKind, LaunchRecord, LaunchState};
    let mut e = entry("kid");
    assert!(!SessionSummary::of(&e, &view(), NOW, false).job);
    e.launch = Some(LaunchRecord {
        request_id: "r".into(),
        args_hash: "h".into(),
        state: LaunchState::Started,
        outcome: None,
        kind: LaunchKind::Job,
        branch: None,
        base_commit: None,
    });
    assert!(SessionSummary::of(&e, &view(), NOW, false).job);
}

/// The tier comes from the session's own config through the view, not from the registry row, and
/// is absent before one is written — a default here would claim an autonomy nobody chose.
#[test]
fn the_tier_is_the_views_and_is_absent_before_a_config_exists() {
    let e = entry("bot");
    let mut v = view();
    assert_eq!(v.tier, None, "no config was written");
    assert_eq!(SessionSummary::of(&e, &v, NOW, false).tier, None);
    v.tier = Some(Tier::Autopilot);
    assert_eq!(
        SessionSummary::of(&e, &v, NOW, false).tier,
        Some(Tier::Autopilot)
    );
}
