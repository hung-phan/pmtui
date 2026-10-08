//! Project the job ledger's open stops into the human-facing daemon representation.

use super::*;
use crate::daemon::stops::{job_open_stop_ids, job_stops};

fn open_stop(id: &str, kind: StopKind, status: StopStatus) -> OpenStop {
    OpenStop {
        id: id.into(),
        kind,
        pane_dialog: None,
        channel: None,
        context_ref: Some("decisions.md".into()),
        question: Some(format!("question for {id}")),
        options: vec!["first".into(), "second".into()],
        authorized_responders: Vec::new(),
        message_id: None,
        first_posted: 1,
        last_polled: None,
        last_seen_reply_ts: None,
        status,
    }
}

#[test]
fn display_projection_filters_ids_and_preserves_held_state() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1);
    ledger.open_stops = vec![
        open_stop("keep", StopKind::ConfirmDone, StopStatus::Held),
        open_stop("drop", StopKind::Ambiguity, StopStatus::AwaitingReply),
    ];
    crate::job::save(&paths, &ledger).unwrap();

    let shown = job_stops(&paths, &["keep".into()]);

    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].id, "keep");
    assert_eq!(shown[0].kind, "confirm_done");
    assert_eq!(shown[0].risk_class, crate::state::RiskClass::Hard);
    assert_eq!(shown[0].status, "held");
    assert_eq!(shown[0].question, "question for keep");
    assert_eq!(shown[0].options, ["first", "second"]);
    assert_eq!(shown[0].context_ref.as_deref(), Some("decisions.md"));
}

#[test]
fn missing_ledger_yields_no_display_stops_or_ids() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "missing");

    assert!(job_stops(&paths, &["anything".into()]).is_empty());
    assert!(job_open_stop_ids(&paths).is_empty());
}

#[test]
fn open_stop_ids_follow_ledger_order() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1);
    ledger.open_stops = vec![
        open_stop("first", StopKind::Ambiguity, StopStatus::AwaitingReply),
        open_stop("second", StopKind::Stuck, StopStatus::Held),
    ];
    crate::job::save(&paths, &ledger).unwrap();

    assert_eq!(job_open_stop_ids(&paths), ["first", "second"]);
}
