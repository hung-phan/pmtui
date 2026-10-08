use serde_json::Value;

use super::*;

const ALL_STATES: [ReceiptState; 11] = [
    ReceiptState::Claimed,
    ReceiptState::Staged,
    ReceiptState::Launching,
    ReceiptState::Ready,
    ReceiptState::NeedsAttention,
    ReceiptState::Failed,
    ReceiptState::OutcomeUnknown,
    ReceiptState::Done,
    ReceiptState::NeedsHuman,
    ReceiptState::EndedWithoutResult,
    ReceiptState::Cancelled,
];

/// Every dotted path in `value` whose last key is `key`.
fn key_paths(value: &Value, key: &str, prefix: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                if name == key {
                    found.push(path.clone());
                }
                key_paths(child, key, &path, found);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                key_paths(child, key, &format!("{prefix}[{index}]"), found);
            }
        }
        _ => {}
    }
}

fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

/// WHAT A PARENT MAY STOP POLLING ON. Every launch outcome and every job outcome is final; the three
/// in-flight states are not. A job's `ready` IS final for the launch — the child came up — which is why
/// a parent that wants the job's own result polls on [`ReceiptState::is_job_terminal`] instead.
#[test]
fn final_states_are_every_launch_outcome_and_every_job_outcome() {
    let finals: Vec<ReceiptState> = ALL_STATES.into_iter().filter(|s| s.is_final()).collect();

    assert_eq!(
        finals,
        vec![
            ReceiptState::Ready,
            ReceiptState::NeedsAttention,
            ReceiptState::Failed,
            ReceiptState::OutcomeUnknown,
            ReceiptState::Done,
            ReceiptState::NeedsHuman,
            ReceiptState::EndedWithoutResult,
            ReceiptState::Cancelled,
        ]
    );
    assert!(
        ![
            ReceiptState::Claimed,
            ReceiptState::Staged,
            ReceiptState::Launching
        ]
        .iter()
        .any(|state| state.is_final()),
        "a request still being worked on is not final"
    );
}

/// WHEN A JOB'S OWN WORK IS OVER, whatever became of it. `Ready` is excluded on purpose: for a job it
/// means the run STARTED, so treating it as terminal would report a running child as finished — and the
/// broker would stop watching the row it still has to retire.
#[test]
fn job_terminal_states_are_the_four_outcomes_plus_failed_and_never_ready() {
    let terminal: Vec<ReceiptState> = ALL_STATES
        .into_iter()
        .filter(|state| state.is_job_terminal())
        .collect();

    assert_eq!(
        terminal,
        vec![
            ReceiptState::Failed,
            ReceiptState::Done,
            ReceiptState::NeedsHuman,
            ReceiptState::EndedWithoutResult,
            ReceiptState::Cancelled,
        ]
    );
    assert!(
        !ReceiptState::Ready.is_job_terminal(),
        "a job's `ready` means its run started, not that it finished"
    );
    for state in terminal {
        assert!(state.is_final(), "{state:?} must also be final");
    }
}

#[test]
fn state_names_are_the_spec_wire_names() {
    let names: Vec<Value> = ALL_STATES
        .iter()
        .map(|s| serde_json::to_value(s).unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "claimed",
            "staged",
            "launching",
            "ready",
            "needs_attention",
            "failed",
            "outcome_unknown",
            "done",
            "needs_human",
            "ended_without_result",
            "cancelled",
        ]
        .map(Value::from)
    );
}

#[test]
fn receipt_json_uses_snake_case_and_never_contains_a_message_field() {
    let full = SpawnReceipt {
        state: ReceiptState::OutcomeUnknown,
        error: Some(SpawnError {
            code: ErrorCode::LaunchFailed,
            message: "tmux new-session exited 1".into(),
        }),
        next_action: Some(NextAction {
            kind: NextActionKind::Attach,
            argv: [
                "tmux",
                "-L",
                "am",
                "attach-session",
                "-t",
                "=pm-service-2-1a2b3c4d",
            ]
            .map(String::from)
            .to_vec(),
        }),
        args_hash: Some(args("Fix it").args_hash()),
        ..ready_receipt(ID)
    };

    let value = serde_json::to_value(&full).unwrap();

    let mut messages = Vec::new();
    key_paths(&value, "message", "", &mut messages);
    assert_eq!(messages, ["error.message"], "the Message is never echoed");
    assert_eq!(
        keys(&value),
        [
            "args_hash",
            "claimed_by",
            "error",
            "launch_state",
            "next_action",
            "request_id",
            "schema_version",
            "session",
            "state",
            "updated_at",
        ]
    );
    assert_eq!(
        keys(&value["session"]),
        [
            "agent",
            "display_name",
            "id",
            "model",
            "root",
            "spawned_by",
            "state_dir",
            "title",
            "tmux_session",
        ]
    );
    assert_eq!(value["state"], "outcome_unknown");
    assert_eq!(value["launch_state"], "started");
    assert_eq!(value["error"]["code"], "launch_failed");
    assert_eq!(value["next_action"]["kind"], "attach");
    assert_eq!(value["session"]["agent"], "codex");
    assert_eq!(value["updated_at"], 1_790_000_004);
    assert_eq!(serde_json::from_value::<SpawnReceipt>(value).unwrap(), full);

    let bare = serde_json::to_value(receipt(ID, ReceiptState::Claimed)).unwrap();
    let mut messages = Vec::new();
    key_paths(&bare, "message", "", &mut messages);
    assert!(messages.is_empty());
    for absent in [
        "session",
        "launch_state",
        "error",
        "next_action",
        "args_hash",
    ] {
        assert_eq!(bare[absent], Value::Null, "{absent} serializes as null");
    }
}

#[test]
fn a_receipt_written_before_args_hash_existed_loads_without_one() {
    let mut value = serde_json::to_value(ready_receipt(ID)).unwrap();
    value.as_object_mut().unwrap().remove("args_hash");
    let old: SpawnReceipt = serde_json::from_value(value).unwrap();
    assert_eq!(old.args_hash, None);
    assert_eq!(old.state, ReceiptState::Ready);
}

#[test]
fn error_codes_and_next_action_kinds_use_the_spec_names() {
    let codes = [
        (ErrorCode::NotInASession, "not_in_a_session"),
        (ErrorCode::InvalidArgument, "invalid_argument"),
        (ErrorCode::RequestConflict, "request_conflict"),
        (ErrorCode::RequestUnwritable, "request_unwritable"),
        (ErrorCode::ParentNotFound, "parent_not_found"),
        (ErrorCode::NestedSpawnRefused, "nested_spawn_refused"),
        (ErrorCode::ChildLimitReached, "child_limit_reached"),
        (ErrorCode::DirNotFound, "dir_not_found"),
        (ErrorCode::DirNotAllowed, "dir_not_allowed"),
        (ErrorCode::MessageTooLong, "message_too_long"),
        (ErrorCode::InvalidRequest, "invalid_request"),
        (ErrorCode::RegistryUnreadable, "registry_unreadable"),
        (ErrorCode::LaunchFailed, "launch_failed"),
        (ErrorCode::ReadinessUnknown, "readiness_unknown"),
    ];
    for (code, name) in codes {
        assert_eq!(serde_json::to_value(code).unwrap(), name);
        assert_eq!(
            serde_json::from_value::<ErrorCode>(Value::from(name)).unwrap(),
            code
        );
    }
    for (kind, name) in [
        (NextActionKind::Wait, "wait"),
        (NextActionKind::Attach, "attach"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), name);
    }
    for (state, name) in [
        (LaunchState::Pending, "pending"),
        (LaunchState::Attempted, "attempted"),
        (LaunchState::Started, "started"),
        (LaunchState::FailedBeforeStart, "failed_before_start"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), name);
    }
}
