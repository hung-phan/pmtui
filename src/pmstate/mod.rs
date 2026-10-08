//! The shared stop/operation vocabulary the ledgers persist. These types began as
//! part of the (now-removed) phase ledger, but they are what the LIVE agent-loop path
//! depends on: `StopKind`/`StopStatus`/`OpenStop` are the persisted decision-point
//! shape (mirrored in [`crate::job::AgentLoopState`]), `Operation`/`OpStatus` are the
//! idempotency ledger (`crate::ops`), and [`normalize_question`] is the one canonical
//! draft→persisted normaliser both writers share.
//!
//! `tests` is the only part that lives apart, pinning the wire names and the keys a
//! ledger written before a field existed may omit.

use serde::{Deserialize, Serialize};

use crate::clock::Epoch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopKind {
    Publish,
    Merge,
    ConfirmDone,
    Ambiguity,
    Stuck,
    ExpertNeeded,
    WorkerStuck,
    Capability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopStatus {
    AwaitingReply,
    Held,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneDialogStop {
    pub fingerprint: u64,
    pub dashboard_answerable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenStop {
    pub id: String,
    pub kind: StopKind,
    /// This stop came from a live in-pane dialog, so a human answer must be applied as a
    /// revalidated option selection rather than pasted into the worker as prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_dialog: Option<PaneDialogStop>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub context_ref: Option<String>,
    /// What the agent actually asked, carried over verbatim from the marker's
    /// [`crate::worker::StopDraft`]. `kind` alone tells a human that a decision is
    /// needed but not *what* is being decided, so this is the only channel for the
    /// question itself. `None` for a synthesized stop (`stuck`, the `confirm_done`
    /// park) and for every ledger written before this field existed — the renderers
    /// fall back to the kind name.
    #[serde(default)]
    pub question: Option<String>,
    /// The choices the agent offered alongside `question`, in the order it listed
    /// them. Empty when it asked open-ended (or never asked).
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub authorized_responders: Vec<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    pub first_posted: Epoch,
    #[serde(default)]
    pub last_polled: Option<Epoch>,
    #[serde(default)]
    pub last_seen_reply_ts: Option<Epoch>,
    pub status: StopStatus,
}

impl OpenStop {
    pub fn is_pane_dialog(&self) -> bool {
        self.pane_dialog.is_some() || is_legacy_pane_dialog_id(&self.id)
    }

    pub fn dashboard_answerable_dialog(&self) -> bool {
        self.pane_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.dashboard_answerable)
    }
}

fn is_legacy_pane_dialog_id(id: &str) -> bool {
    id.strip_prefix("stop-")
        .and_then(|rest| rest.rsplit_once("-dialog-"))
        .is_some_and(|(project, timestamp)| {
            !project.is_empty() && timestamp.parse::<Epoch>().is_ok()
        })
}

/// Normalise a drafted question into the persisted shape: an empty or
/// whitespace-only draft is *no* question ([`None`]), never `Some("")`, so every
/// reader can treat `Some` as "there is something worth showing" instead of
/// re-testing for blankness. The one canonical normaliser both writer paths share.
pub fn normalize_question(question: &str) -> Option<String> {
    let trimmed = question.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpStatus {
    Pending,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub id: String,
    pub key: String,
    pub status: OpStatus,
    pub started_at: Epoch,
}

#[cfg(test)]
mod tests;
