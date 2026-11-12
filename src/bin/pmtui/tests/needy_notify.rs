//! The "a session newly needs you" notification: which transitions it fires on, which
//! rows are silent, and the words one carries.

use super::*;
use crate::app::needy_notify::{newly_needy, terminal_notifier};
use agent_manager::escalation::Severity;
use std::collections::HashSet;

fn needy(id: &str, stops: Vec<Stop>) -> ProjectView {
    view(id, Posture::NeedsYou, stops)
}

/// THE FIRST LOOK IS SILENT. `App::new` refreshes before the human has pressed anything,
/// so a dashboard opened onto three blocked sessions must not fire three notifications at
/// a human who is already reading the list.
#[test]
fn the_first_look_records_without_notifying() {
    let views = vec![needy("bot", vec![stop("s1", "publish", RiskClass::Hard)])];
    let mut seen = None;

    assert!(newly_needy(&views, &mut seen).is_empty());
    assert_eq!(seen, Some(HashSet::from(["bot".to_string()])));
}

/// ONE NOTIFICATION PER TRANSITION, and the edge re-arms. The row stays needy for as many
/// refreshes as it takes the human to notice — several a second — so anything but an edge
/// is a notification storm.
#[test]
fn a_row_notifies_once_when_it_crosses_and_again_after_it_clears() {
    let idle = vec![view("bot", Posture::Monitoring, vec![])];
    let blocked = vec![needy("bot", vec![stop("s1", "publish", RiskClass::Hard)])];
    let mut seen = None;
    newly_needy(&idle, &mut seen);

    let first = newly_needy(&blocked, &mut seen);
    assert_eq!(first.len(), 1, "the crossing notifies");
    assert_eq!(first[0].project, "bot");
    assert_eq!(first[0].title, "1 decision needs you");

    assert!(
        newly_needy(&blocked, &mut seen).is_empty(),
        "still needy is not newly needy"
    );
    assert!(
        newly_needy(&idle, &mut seen).is_empty(),
        "answering it is not an event"
    );
    assert_eq!(
        newly_needy(&blocked, &mut seen).len(),
        1,
        "and the next question notifies again"
    );
}

/// The body is what the human needs to answer — the question AND the options the agent
/// offered, through the one builder `pmd` escalates with.
#[test]
fn the_notification_carries_the_question_and_its_options() {
    let asking = vec![needy(
        "bot",
        vec![stop_asking(
            "s1",
            "publish",
            "Ship 0.2.2 now?",
            &["yes", "hold"],
        )],
    )];
    let mut seen = Some(HashSet::new());

    let sent = newly_needy(&asking, &mut seen);

    assert_eq!(sent.len(), 1);
    assert!(sent[0].body.contains("Ship 0.2.2 now?"), "{:?}", sent[0]);
    assert!(sent[0].body.contains("1) yes"), "{:?}", sent[0]);
    assert!(sent[0].body.contains("2) hold"), "{:?}", sent[0]);
    assert_eq!(sent[0].severity, Severity::Urgent, "a Hard stop is urgent");
}

/// A NEEDS-YOU WITH NO STOP is the autopilot-off marker gate: the worker reported blocked
/// and nothing was made answerable, because no `pmd` would deliver the answer. An empty
/// body ("0 decisions need you") would be the one notification that says nothing.
#[test]
fn a_needs_you_with_no_stop_still_says_what_to_do() {
    let mut blocked = needy("bot", vec![]);
    blocked.next_action = "answer the blocked report".into();
    let mut seen = Some(HashSet::new());

    let sent = newly_needy(&[blocked], &mut seen);

    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].title, "needs you");
    assert_eq!(sent[0].body, "answer the blocked report");
}

/// Silent by design: a PAUSED row is one the human parked (it is not drawn loud either),
/// and `Stuck` — which is `needs_attention()` too — can flap, so one notification per flap
/// would teach the human to ignore all of them.
#[test]
fn a_paused_row_and_a_stuck_row_are_both_silent() {
    let mut paused = needy("paused", vec![stop("s1", "publish", RiskClass::Hard)]);
    paused.enabled = false;
    let stuck = view(
        "stuck",
        Posture::Stuck,
        vec![stop("s2", "stuck", RiskClass::Hard)],
    );
    let mut seen = Some(HashSet::new());

    assert!(newly_needy(&[paused, stuck], &mut seen).is_empty());
    assert_eq!(seen, Some(HashSet::new()), "neither is even recorded");
}

/// The dashboard sends through its own notifier seam, on the rows the next frame will draw.
#[test]
fn the_dashboard_sends_what_it_found_through_its_notifier() {
    let capture = CaptureNotifier::default();
    let mut app = app_with(
        vec![needy("bot", vec![stop("s1", "publish", RiskClass::Hard)])],
        UiMode::Normal,
    );
    app.notifier = Box::new(capture.clone());
    // The fixture never refreshed, so this call is the first look: it only records.
    app.notify_newly_needy();
    assert!(capture.sent().is_empty());

    app.projects
        .push(needy("mate", vec![stop("s2", "publish", RiskClass::Hard)]));
    app.notify_newly_needy();

    let sent = capture.sent();
    assert_eq!(sent.len(), 1, "only the new one: {sent:?}");
    assert_eq!(sent[0].project, "mate");
}

/// The production notifier writes OSC 777 to its own stderr, and ONLY when that is a
/// terminal. Under the test harness it is not, so this must be a silent success rather
/// than an error the refresh would have to handle.
#[test]
fn the_terminal_notifier_is_a_silent_no_op_off_a_terminal() {
    let e = agent_manager::escalation::Escalation::for_stops(
        "bot",
        &[stop("s1", "publish", RiskClass::Hard)],
    );
    assert!(terminal_notifier().notify(&e).is_ok());
}
