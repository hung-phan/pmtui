//! Turning the agent-loop ledger's `open_stops` into the human-facing [`Stop`]s an
//! escalation carries, and into the stop ids the sweep's notify-once dedup remembers.

use crate::job;
use crate::pmstate::{OpenStop, StopKind, StopStatus};
use crate::policy;
use crate::state::{ProjectPaths, RiskClass, Stop};

/// The display `Stop`s for the ids among the per-session
/// [`crate::job::AgentLoopState`] ledger's `open_stops`.
pub(super) fn job_stops(paths: &ProjectPaths, ids: &[String]) -> Vec<Stop> {
    let Some(ledger) = job::load(paths).ok().flatten() else {
        return Vec::new();
    };
    open_stops_for_display(&ledger.open_stops, ids)
}

/// Convert the `open_stops` (filtered to `ids`) into the reference [`Stop`] shape
/// [`Escalation::for_stops`](crate::escalation::Escalation::for_stops) consumes.
/// Display-only — the engine already applied tier policy before parking — but
/// `risk_class` is derived from the typed `StopKind` (via
/// `policy::effective_risk_kind`, treating it as `Medium`-labelled) so the
/// human-facing severity matches the policy: `Publish`/`Merge`/`ConfirmDone`/
/// `Stuck`/`Capability` render as Hard (urgent), the rest as Medium.
fn open_stops_for_display(open_stops: &[OpenStop], ids: &[String]) -> Vec<Stop> {
    open_stops
        .iter()
        .filter(|s| ids.iter().any(|id| id == &s.id))
        .map(|s| Stop {
            id: s.id.clone(),
            kind: stop_kind_name(s.kind),
            risk_class: policy::effective_risk_kind(s.kind, RiskClass::Medium),
            // The agent's own question/options when the ledger carries them, so the
            // notification body says WHAT is being decided rather than just its kind.
            // Absent (synthesized stop, or a pre-question ledger) → empty, and
            // `for_stops` falls back to the kind name as before.
            question: s.question.clone().unwrap_or_default(),
            options: s.options.clone(),
            context_ref: s.context_ref.clone(),
            status: match s.status {
                StopStatus::AwaitingReply => "awaiting_reply".to_string(),
                StopStatus::Held => "held".to_string(),
            },
        })
        .collect()
}

/// The ids of the per-session [`crate::job::AgentLoopState`] ledger's `open_stops`
/// (empty when there is no ledger). Used by the `Stuck` dedup to record the parked
/// stops so the follow-up `Escalated([stuck_id])` is recognized as already-surfaced.
pub(super) fn job_open_stop_ids(paths: &ProjectPaths) -> Vec<String> {
    job::load(paths)
        .ok()
        .flatten()
        .map(|l| l.open_stops.iter().map(|st| st.id.clone()).collect())
        .unwrap_or_default()
}

/// The snake_case wire name of a [`StopKind`] (deferring to serde rather than
/// duplicating the mapping), so the converted `Stop.kind` matches the strings
/// `policy`/`escalation` gate on (e.g. `publish`, `confirm_done`, `worker_stuck`).
fn stop_kind_name(kind: StopKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
