//! The DECIDER's vocabulary: what a consult was asked about, what came back, and the audit trail
//! the ledger keeps of both.
//!
//! Split out of `job.rs`, which had grown to hold four unrelated concerns in one file — the session
//! run state, this, the autopilot event log, and the situation projection — so no one of them could
//! be read or changed without the other three. Nothing here is a decision: `advise` makes the call
//! and `job_engine::supervisor` applies it. These are the records.
//!
//! Every type keeps its path (`job::DeciderRun`, …) through a re-export in the parent, so this is a
//! move and not a rename.

use serde::{Deserialize, Serialize};

use super::{bool_is_false, cap_one_line};
use crate::clock::Epoch;
use crate::pmstate::StopKind;
use crate::registry::Engine;
use crate::state::RiskClass;

/// A supervisor consult that is spawned-but-unhonoured, recorded on the ledger so a
/// `pmd` restart cannot lose the approval the worker is waiting for.
///
/// WHY this must be durable at all: the consult is started from the auto-flow branch of
/// the marker disposer, which has ALREADY consumed the marker `seq` (`last_marker_seq`
/// is the anti-replay watermark, so the same bump is never disposed twice). A daemon
/// that dies mid-consult and comes back with only in-memory state would therefore nudge
/// a bare heartbeat and NEVER answer the worker's question — strictly worse than before
/// the supervisor existed, and invisible until the 30-minute stall backstop fires. With
/// this record the restarted daemon knows whether it owes the worker a marker answer
/// or must re-detect a live pane dialog.
///
/// Deliberately holds IDs/a target bit and not prose: marker notes are formatted in
/// exactly one place, pane dialogs are re-read from the terminal, and nothing
/// supervisor-authored is persisted here (this record is written before any reply exists).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParkedAdvice {
    /// The consult counter — the `<seq>` in its `pmsup-` session name and in its
    /// done-signal/log paths. Persisted so a restarted daemon RESUMES the counter
    /// instead of restarting it at 0 and reading a dead consult's exit code as a fresh
    /// one's.
    pub seq: u64,
    /// The auto-flow stop ids the deterministic policy already approved. Empty for
    /// a pane-dialog consult, whose live terminal is the source of truth.
    pub stop_ids: Vec<String>,
    /// This debt targets an in-pane dialog rather than a consumed worker marker.
    /// A restarted daemon must re-detect and re-consult that live dialog; it must
    /// never recover this record as prose in `pending_context`.
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub pane_dialog: bool,
}

/// One worker-authored decision waiting for a decider verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedAdvice {
    pub stop_id: String,
    pub report_seq: u64,
    pub draft: crate::worker::StopDraft,
}

/// Maximum retained decider consults. Audit rows are richer than the compact event feed, so keep a
/// smaller history while still covering a long interactive session.
pub const DECIDER_RUNS_MAX: usize = 50;
/// Maximum ordinary decisions accepted from one marker for serial review. Additional decisions are
/// audited and handed to the human instead of creating an unbounded model-call queue.
pub const ADVICE_QUEUE_MAX: usize = 8;

pub(super) fn de_advice_queue_bounded<'de, D>(
    deserializer: D,
) -> Result<Vec<QueuedAdvice>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut queue = Vec::<QueuedAdvice>::deserialize(deserializer)?;
    queue.truncate(ADVICE_QUEUE_MAX);
    Ok(queue)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeciderTarget {
    Marker,
    Dialog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeciderPolicy {
    pub kind: StopKind,
    pub labelled_risk: RiskClass,
    pub effective_risk: RiskClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeciderOutcome {
    Consulting,
    Skipped {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reported_kind: Option<StopKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect: Option<crate::worker::StopEffect>,
    },
    Resolved {
        answer: String,
        reason: String,
    },
    Refused {
        reason: String,
    },
    Failed {
        reason: String,
    },
    Recovered {
        answer: String,
    },
    Interrupted {
        reason: String,
    },
}

/// One decider decision from preflight through its terminal outcome. Most records are
/// read-only model consults; [`DeciderOutcome::Skipped`] records a deterministic preflight
/// decision for which no model was called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeciderRun {
    pub seq: u64,
    pub started_at: Epoch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Epoch>,
    pub engine: Engine,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub target: DeciderTarget,
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The worker's typed report is untrusted evidence, not authority. Keeping it on every audit
    /// row makes both called and skipped decisions explainable without exposing model reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_kind: Option<StopKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<crate::worker::StopEffect>,
    pub policy: DeciderPolicy,
    pub outcome: DeciderOutcome,
}
const DECIDER_AUDIT_TEXT_MAX: usize = 400;
const DECIDER_AUDIT_OPTION_MAX: usize = 200;

pub(super) fn cap_audit_text(s: String) -> String {
    cap_one_line(s, DECIDER_AUDIT_TEXT_MAX)
}

pub(super) fn cap_audit_option(s: String) -> String {
    cap_one_line(s, DECIDER_AUDIT_OPTION_MAX)
}

pub(super) fn cap_decider_outcome(outcome: DeciderOutcome) -> DeciderOutcome {
    match outcome {
        DeciderOutcome::Consulting => DeciderOutcome::Consulting,
        DeciderOutcome::Skipped {
            reason,
            reported_kind,
            effect,
        } => DeciderOutcome::Skipped {
            reason: cap_audit_text(reason),
            reported_kind,
            effect,
        },
        DeciderOutcome::Resolved { answer, reason } => DeciderOutcome::Resolved {
            answer: cap_audit_text(answer),
            reason: cap_audit_text(reason),
        },
        DeciderOutcome::Refused { reason } => DeciderOutcome::Refused {
            reason: cap_audit_text(reason),
        },
        DeciderOutcome::Failed { reason } => DeciderOutcome::Failed {
            reason: cap_audit_text(reason),
        },
        DeciderOutcome::Recovered { answer } => DeciderOutcome::Recovered {
            answer: cap_audit_text(answer),
        },
        DeciderOutcome::Interrupted { reason } => DeciderOutcome::Interrupted {
            reason: cap_audit_text(reason),
        },
    }
}

/// Deserialize the cosmetic decider audit independently from operational state.
pub(super) fn de_decider_runs_lenient<'de, D>(
    d: D,
) -> std::result::Result<Vec<DeciderRun>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

#[cfg(test)]
mod tests;
