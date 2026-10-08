//! `pmtui spawn --status`: every child this session asked for, in one call.
//!
//! A parent cannot know how long its child will take, so any `--wait` it picks is a guess. This is the
//! answer to that: dispatch, do your own work, and when you next come up for air ask what became of all
//! of them. No ids to carry between turns and no clock to tune — and still no bound imposed on a child,
//! because the bound stays the parent's (`--cancel`) rather than the dashboard's.
//!
//! READ-ONLY and sandbox-safe, exactly like the rest of the command: it reads its OWN session's
//! `spawn-requests/` directory through the same hardened reader, and never touches the registry, tmux,
//! or the user's home.

use agent_manager::clock::Epoch;
use agent_manager::spawn::{self, ReceiptState};
use serde::Serialize;

/// One child, as its parent's receipt describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ChildStatus {
    pub request_id: String,
    /// The child's session id, absent only for a request answered before a row existed.
    pub id: Option<String>,
    pub title: Option<String>,
    /// The receipt's state, verbatim — `ready` means the job is RUNNING.
    pub state: ReceiptState,
    /// What the child reported, or why it ended without reporting.
    pub summary: Option<String>,
    pub updated_at: Epoch,
}

/// Every receipt in the calling session's requests directory, oldest first.
///
/// Oldest first because a parent reads this like a log: the newest line is the thing that just changed.
pub(crate) fn run_status(env: &dyn Fn(&str) -> Option<String>) -> Result<Vec<ChildStatus>, String> {
    let (_session, state_dir) = crate::spawn_cli::session_env(env)?;
    let dir = spawn::requests_dir(&state_dir);
    let mut children: Vec<ChildStatus> = spawn::list_receipt_ids(&dir)
        .into_iter()
        .filter_map(|id| {
            let receipt = spawn::read_receipt(&dir, &id).ok().flatten()?;
            Some(ChildStatus {
                request_id: receipt.request_id,
                id: receipt.session.as_ref().map(|s| s.id.clone()),
                title: receipt.session.as_ref().and_then(|s| s.title.clone()),
                state: receipt.state,
                // The result's summary when the child reported one, else the error that explains why it
                // did not. ONE field, because a parent reads one line per child.
                summary: receipt
                    .result
                    .as_ref()
                    .map(|found| found.summary.clone())
                    .or_else(|| receipt.error.as_ref().map(|e| e.message.clone())),
                updated_at: receipt.updated_at,
            })
        })
        .collect();
    children.sort_by(|a, b| {
        a.updated_at
            .cmp(&b.updated_at)
            .then_with(|| a.request_id.cmp(&b.request_id))
    });
    Ok(children)
}

/// The word a parent acts on. `ready` is deliberately NOT called "ready": for a job it means the run is
/// still going, and treating a launch as an answer is the whole mistake this listing exists to prevent.
pub(crate) fn status_word(state: ReceiptState) -> &'static str {
    match state {
        ReceiptState::Claimed | ReceiptState::Staged | ReceiptState::Launching => "starting",
        ReceiptState::Ready => "running",
        ReceiptState::Done => "done",
        ReceiptState::NeedsHuman => "needs a human",
        ReceiptState::Failed => "failed",
        ReceiptState::EndedWithoutResult => "ended without a result",
        ReceiptState::Cancelled => "cancelled",
        ReceiptState::NeedsAttention => "needs attention",
        ReceiptState::OutcomeUnknown => "outcome unknown",
    }
}

/// One line per child: `<state> <id> "<title>" — <summary>`, newest last. Says so plainly when there are
/// none, rather than printing nothing and leaving a parent to wonder whether the command worked.
pub(crate) fn render_status_plain(children: &[ChildStatus]) -> String {
    if children.is_empty() {
        return "no children: this session has not spawned any".to_string();
    }
    children
        .iter()
        .map(|child| {
            let id = child.id.as_deref().unwrap_or(&child.request_id);
            let title = child
                .title
                .as_deref()
                .map(|t| format!(" {t:?}"))
                .unwrap_or_default();
            let summary = child
                .summary
                .as_deref()
                .map(|s| format!(" \u{2014} {}", s.lines().next().unwrap_or("").trim()))
                .unwrap_or_default();
            format!("{} {id}{title}{summary}", status_word(child.state))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Serialize)]
struct StatusEnvelope<'a> {
    schema_version: u32,
    children: &'a [ChildStatus],
}

/// `{"schema_version", "children":[…]}` — an ARRAY even for one child, so a parent's parse never has to
/// branch on how many it has.
pub(crate) fn render_status_json(children: &[ChildStatus]) -> String {
    serde_json::to_string_pretty(&StatusEnvelope {
        schema_version: spawn::SCHEMA_VERSION,
        children,
    })
    .unwrap_or_default()
}
