//! Tests for the shared stop vocabulary: the serde contract of the persisted
//! records (snake_case names, `deny_unknown_fields`, and which keys may be absent
//! in a record written before a field existed).

use super::*;

#[test]
fn open_stop_deserializes_without_question_or_options() {
    // BACK-COMPAT GUARD. Every `state.json` on disk today was written before
    // `question`/`options` existed. `OpenStop` is `deny_unknown_fields`, so it
    // cannot tolerate a *new* key — but it MUST tolerate a *missing* one, or a
    // reader would fail to load a pre-upgrade ledger. Both fields are
    // `#[serde(default)]`; this pins that.
    let json = r#"{ "id": "stop-1", "kind": "confirm_done", "first_posted": 42,
                    "status": "awaiting_reply" }"#;
    let mut s: OpenStop = serde_json::from_str(json).unwrap();
    assert_eq!(s.id, "stop-1");
    assert_eq!(s.kind, StopKind::ConfirmDone);
    assert!(s.question.is_none(), "absent question defaults to None");
    assert!(s.options.is_empty(), "absent options default to empty");
    assert!(
        s.pane_dialog.is_none(),
        "legacy stops have no dialog snapshot"
    );
    assert!(!s.is_pane_dialog());
    s.id = "stop-old-dialog-42".into();
    assert!(
        s.is_pane_dialog(),
        "pre-field dialog ids remain recognizable after upgrade"
    );
    s.id = "stop-project-dialog-name-0-42".into();
    assert!(
        !s.is_pane_dialog(),
        "a project id containing dialog must not reclassify a marker stop"
    );
}

#[test]
fn open_stop_round_trips_question_and_options() {
    let mut s: OpenStop = serde_json::from_str(
        r#"{ "id": "s", "kind": "ambiguity", "first_posted": 1,
             "status": "awaiting_reply" }"#,
    )
    .unwrap();
    s.question = Some("Confirm and close, or keep going?".into());
    s.options = vec!["close".into(), "keep going".into()];
    s.pane_dialog = Some(PaneDialogStop {
        fingerprint: 42,
        dashboard_answerable: true,
    });
    let back: OpenStop = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
}

#[test]
fn normalize_question_blanks_to_none() {
    // A worker that drafts a stop but leaves `question` empty must not persist
    // `Some("")` — readers treat `Some` as "there is something to show".
    assert_eq!(normalize_question(""), None);
    assert_eq!(normalize_question("   \n\t "), None);
    // Real text is kept, trimmed.
    assert_eq!(
        normalize_question("  which path?\n"),
        Some("which path?".to_string())
    );
}

#[test]
fn stop_kind_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&StopKind::ConfirmDone).unwrap(),
        "\"confirm_done\""
    );
    assert_eq!(
        serde_json::to_string(&StopKind::WorkerStuck).unwrap(),
        "\"worker_stuck\""
    );
}
