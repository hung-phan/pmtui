//! Behavior-focused tests for the spawn request/receipt protocol, grouped by concern.

mod files;
mod prompt;
mod receipt;
mod request;
mod result;

use std::path::PathBuf;

use super::*;
use crate::registry::{Engine, LaunchState};

/// A canonical lowercase request id, the one most tests publish under.
const ID: &str = "550e8400-e29b-41d4-a716-446655440000";
/// A second valid id, for tests that need two requests.
const OTHER_ID: &str = "6ba7b810-9dad-41d1-80b4-00c04fd430c8";

fn args(message: &str) -> SpawnArgs {
    SpawnArgs {
        message: message.into(),
        title: None,
        name: None,
        dir: None,
        agent: None,
        model: None,
    }
}

fn request(id: &str, parent: &str, message: &str) -> SpawnRequest {
    SpawnRequest {
        schema_version: SCHEMA_VERSION,
        request_id: id.into(),
        parent_session: parent.into(),
        created_at: 1_790_000_000,
        args: args(message),
    }
}

fn receipt(id: &str, state: ReceiptState) -> SpawnReceipt {
    SpawnReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        request_id: id.into(),
        state,
        claimed_by: Some("pid:4242".into()),
        session: None,
        launch_state: None,
        error: None,
        next_action: None,
        args_hash: None,
        result: None,
        work: None,
        updated_at: 1_790_000_004,
    }
}

fn ready_receipt(id: &str) -> SpawnReceipt {
    SpawnReceipt {
        session: Some(ReceiptSession {
            id: "service-2".into(),
            title: Some("Fix flaky fork test".into()),
            display_name: None,
            root: PathBuf::from("/workspace/service"),
            agent: Engine::Codex,
            model: None,
            spawned_by: "service".into(),
            tmux_session: "pm-service-2-1a2b3c4d".into(),
            state_dir: PathBuf::from(
                "/workspace/service/.project-state/sessions/service-2-1a2b3c4d",
            ),
        }),
        launch_state: Some(LaunchState::Started),
        ..receipt(id, ReceiptState::Ready)
    }
}
