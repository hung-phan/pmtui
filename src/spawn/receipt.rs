//! The dashboard-owned receipt: how far a request got, and what the agent should do next.
//! A receipt never carries the Message.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::clock::Epoch;
use crate::registry::{Engine, LaunchState};

/// Where a request stands. The first three states are in progress; the rest are final.
///
/// The LAUNCH outcomes (`Ready`, `NeedsAttention`, `Failed`, `OutcomeUnknown`) are v1's and keep
/// their meaning, except that `Ready` now reads as "launched and running" — for a job row it is no
/// longer where the story ends. The JOB outcomes below it are what the broker records when a job
/// child's process is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    Claimed,
    Staged,
    Launching,
    Ready,
    NeedsAttention,
    Failed,
    OutcomeUnknown,
    /// The job finished its task. Its row is retired.
    Done,
    /// The job stopped because it needs a person. Its row is kept for one.
    NeedsHuman,
    /// The job's process is gone and left no usable report: it crashed or was killed.
    EndedWithoutResult,
    /// The job was stopped deliberately, by its parent or by a human.
    Cancelled,
}

impl ReceiptState {
    /// True for every launch outcome and every job outcome — everything a parent may stop polling on.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            ReceiptState::Ready
                | ReceiptState::NeedsAttention
                | ReceiptState::Failed
                | ReceiptState::OutcomeUnknown
                | ReceiptState::Done
                | ReceiptState::NeedsHuman
                | ReceiptState::EndedWithoutResult
                | ReceiptState::Cancelled
        )
    }

    /// True once a JOB's own work has ended, whatever became of it. `Ready` is excluded: for a job it
    /// means the run is still going, which is why the broker keeps watching a settled `Ready` row.
    pub fn is_job_terminal(self) -> bool {
        matches!(
            self,
            ReceiptState::Done
                | ReceiptState::NeedsHuman
                | ReceiptState::EndedWithoutResult
                | ReceiptState::Cancelled
                | ReceiptState::Failed
        )
    }
}

/// Why a spawn did not produce a ready child. The first four are raised by the command, the
/// rest by the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotInASession,
    InvalidArgument,
    RequestConflict,
    RequestUnwritable,
    ParentNotFound,
    NestedSpawnRefused,
    ChildLimitReached,
    DirNotFound,
    /// The directory exists but is neither inside the parent's root nor another session's root.
    DirNotAllowed,
    MessageTooLong,
    InvalidRequest,
    RegistryUnreadable,
    LaunchFailed,
    ReadinessUnknown,
    /// A job child ran and reported that its task failed. Raised so a `failed` receipt ALWAYS carries
    /// an `error.code` — v1's contract is "act on `error.code`", and a job failure must not be the one
    /// `failed` that has none. The result's summary is the message, and `result` carries the rest.
    JobFailed,
}

/// A machine-readable code with a human-readable explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextActionKind {
    Wait,
    Attach,
}

/// What to run next, as an exact argv (executable and target included).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextAction {
    pub kind: NextActionKind,
    pub argv: Vec<String>,
}

/// The child session a receipt reports on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptSession {
    pub id: String,
    pub title: Option<String>,
    pub display_name: Option<String>,
    pub root: PathBuf,
    pub agent: Engine,
    pub model: Option<String>,
    pub spawned_by: String,
    pub tmux_session: String,
    pub state_dir: PathBuf,
}

/// The dashboard's answer to one request, replaced atomically as the request advances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnReceipt {
    pub schema_version: u32,
    pub request_id: String,
    pub state: ReceiptState,
    pub claimed_by: Option<String>,
    pub session: Option<ReceiptSession>,
    pub launch_state: Option<LaunchState>,
    pub error: Option<SpawnError>,
    pub next_action: Option<NextAction>,
    /// [`SpawnArgs::args_hash`](super::SpawnArgs::args_hash) of the arguments this request id was
    /// answered for; `None` for a request whose content could not be read, and for a receipt
    /// written before the field existed. Receipts outlive their requests, so the command compares
    /// it on a replay: another hash under the same id is a conflict even after cleanup.
    #[serde(default)]
    pub args_hash: Option<String>,
    /// What a job child reported when it ended, absent until then and absent for a chat child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<super::JobResult>,
    /// WHERE THE WORK IS, for a job that ran in its own git worktree. Absent for a chat child, for a
    /// project that is not a repository, and until the run ends. A parent reads this to say what its
    /// child produced; nothing here is integrated without a human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<ReceiptWork>,
    pub updated_at: Epoch,
}

/// The branch a job committed on, and what it changed there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptWork {
    /// The branch the child owned. It outlives the worktree directory and the row.
    pub branch: String,
    /// The commit it produced, absent when it committed nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Paths its commits changed, so a parent can say what moved without reading a diff.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched: Vec<String>,
    /// It left changes it never committed. They exist only in its worktree, which is therefore kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub uncommitted: bool,
}
