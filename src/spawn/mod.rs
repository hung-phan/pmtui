//! The spawn protocol: how an agent inside a managed session asks the dashboard for a
//! Standard child session, and how the dashboard answers.
//!
//! The agent (through `pmtui spawn`) publishes one request file into its own session's
//! state directory, which every sandbox can write; the dashboard is the only process that
//! turns a request into a registry row, and it reports progress in a receipt beside it.
//!
//! One concern per file: `request` is the agent-owned request schema plus the normalized
//! argument hash that makes a replay recognizable, `receipt` is the dashboard-owned receipt
//! schema, `result` is the job report the HARNESS writes when a child exits, and `files` is the
//! hardened file I/O both sides go through: a no-clobber publish,
//! a scan that never follows a symlink or blocks on a FIFO, and atomic receipt replacement.
//! Every item is re-exported here.

mod files;
mod prompt;
mod receipt;
mod request;
mod result;

#[cfg(test)]
mod tests;

/// The `schema_version` of every REQUEST this build writes and accepts. Unchanged by the job work: a
/// request's fields are the same ones v1 published, so an older reader still accepts a new request.
pub const SCHEMA_VERSION: u32 = 1;

/// The `schema_version` of every RECEIPT this build writes.
///
/// The receipt schema a dashboard writes. v1 answered a request; v2 added a job's `result` and its job
/// states; v3 added the `work` block a job's own git worktree produces. A reader tolerates a newer minor
/// shape because every added field is defaulted, so an older receipt still loads.
pub const RECEIPT_SCHEMA_VERSION: u32 = 3;
/// The directory, inside a session's state directory, that holds its requests and receipts.
pub const REQUESTS_DIR: &str = "spawn-requests";
/// The largest request (or receipt) file the dashboard reads.
pub const MAX_REQUEST_BYTES: u64 = 64 * 1024;
/// The most children of all parents that may be LIVE at once, across the whole dashboard.
///
/// THE ONLY REAL CEILING, and it counts the right thing. Its predecessor counted ROWS per parent,
/// so a job that failed or was cancelled held a place for as long as its row did and five corpses
/// could block a parent for good. [`MAX_UNFINISHED_PER_PARENT`] reads like a ceiling and is not
/// one: a job leaves the queue the moment it has LAUNCHED, which frees window room at once.
///
/// This counts LIVE children plus the ones already on their way, so it bounds the processes
/// actually running on the machine — which is what a human cares about, since each child is a real
/// engine process in its own terminal. GLOBAL rather than per-parent, because depth is unbounded:
/// a per-parent budget renewed at every level bounds nothing.
///
/// Over the ceiling the dashboard simply DOES NOT CLAIM the request. It stays durable and is
/// claimed when a child exits, and the caller reads `queued` — exit 3, "ask again under the same
/// request id", which is already in the skill. That is the whole difference from the cap that was
/// removed: a slot here genuinely frees, so deferring is honest rather than a wait with no end.
pub const MAX_LIVE_CHILDREN: usize = 16;

/// The most unfinished requests the dashboard processes per parent directory at once.
pub const MAX_UNFINISHED_PER_PARENT: usize = 16;
/// The most receipts one `pmtui spawn --status` lists. Nothing caps how many children a parent asks
/// for, so this bounds one listing rather than the fan-out; it is slack for a long-lived session's
/// tombstones rather than a limit anyone should reach.
pub const MAX_RECEIPTS_LISTED: usize = 256;
/// The most unsettled request entries the dashboard opens per parent directory in one refresh; a
/// settled request (its receipt already final) is skipped without opening anything.
pub const MAX_OPENED_PER_PARENT: usize = 64;
/// The longest `task_title` a spawned child may carry, in characters.
pub const TITLE_MAX_CHARS: usize = 120;
/// How long a request file is kept after its receipt became final. The receipt itself is kept
/// for good, as a tombstone.
pub const RETENTION_SECS: i64 = 7 * 24 * 3600;

/// What a request or receipt WITHOUT a `kind` field asked for: a job.
///
/// The serde default for BOTH files, and deliberately not [`LaunchKind`]'s own `Default` (`Chat`),
/// which is right for a ROW — v1 staged only interactive children, so a row written then was one.
/// A FILE without the field was written by a build whose only form was the job, so reading it as
/// `Chat` would claim an agent asked for a session it never asked for. One function, because the
/// two readers must not disagree.
pub fn default_kind() -> crate::registry::LaunchKind {
    crate::registry::LaunchKind::Job
}

pub use files::{
    Listed, Publish, ScanEntry, cancel_path, cancel_requested, is_valid_request_id,
    list_receipt_ids, list_requests, load_request_entry, publish_cancel, publish_request,
    read_receipt, receipt_path, remove_cancel, remove_request, request_path, requests_dir,
    scan_requests, write_receipt,
};
pub use prompt::job_prompt;
pub use receipt::{
    ErrorCode, NextAction, NextActionKind, ReceiptSession, ReceiptState, ReceiptWork, SpawnError,
    SpawnReceipt,
};
pub use request::{SpawnArgs, SpawnRequest};
pub use result::{
    DETAIL_MAX, JobOutcome, JobResult, SUMMARY_MAX, TRUNCATION_MARKER, codex_thread_id,
    read_job_result, result_schema_json,
};
