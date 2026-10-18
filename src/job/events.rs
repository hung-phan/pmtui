//! The AUTOPILOT EVENT LOG and the decision tallies drawn from it: what the harness did, when, and
//! how often it did each kind of thing.
//!
//! Split out of `job.rs` with the decider records and the situation projection, because one file
//! holding four concerns meant reading all four to change any. This half is append-and-cap: bounded
//! vectors a human reads afterwards, never an input to a decision.
//!
//! Every type keeps its path (`job::AutopilotEvent`, …) through a re-export in the parent, so this
//! is a move and not a rename.

use serde::{Deserialize, Serialize};

use super::HoldReason;
use crate::clock::Epoch;

/// The most recent autopilot decisions kept on the ledger for the dashboard's AUTOPILOT
/// feed. Bounded so the ledger stays small (the oldest are dropped); coalescing keeps a
/// held nudge rechecked every few seconds from filling it. A hundred (user: *"we don't need
/// to keep every logline on autopilot, it is better to keep it at 100 lines"*) — deep enough
/// to scroll back through a session's recent history, still a small, bounded `state.json`.
pub const AUTOPILOT_EVENTS_MAX: usize = 100;

/// One decision `pmd` made while driving this session, for the dashboard to render.
/// Consecutive IDENTICAL decisions coalesce into one entry with a bumped `at` and
/// `count` (see [`AgentLoopState::record_event`]), so a heartbeat held on the same
/// reason across many rechecks is ONE "held ×N" line, not a flood.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutopilotEvent {
    /// When this decision last happened (Unix seconds).
    pub at: Epoch,
    /// How many times in a row the same decision repeated (≥ 1).
    #[serde(default = "one_count")]
    pub count: u32,
    pub kind: AutopilotEventKind,
}

pub(super) fn one_count() -> u32 {
    1
}

/// Monotonic, saturating decider-lane counters kept on the ledger. `disposed` is the
/// EXACT count of accepted marker bumps (structurally guaranteed by the single dispose
/// seam); the per-kind counters are best-effort. `is_zero()` drives skip-serialize so a
/// fresh/idle ledger stays byte-identical to a pre-B one. All fields `#[serde(default)]`
/// so a future additive counter loads against an older ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DecisionCounters {
    #[serde(default)]
    pub disposed: u64,
    #[serde(default)]
    pub working: u64,
    #[serde(default)]
    pub monitoring: u64,
    #[serde(default)]
    pub auto_flow: u64,
    #[serde(default)]
    pub escalated: u64,
    #[serde(default)]
    pub stalled: u64,
}

impl DecisionCounters {
    /// True when every counter is 0 — the skip-serialize predicate.
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

/// The disposition class of one decision `pmd` made while driving. Snake-case on the wire
/// (`auto_flow`) so it reads the same in `raw.jsonl` and `decisions.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Working,
    Monitoring,
    AutoFlow,
    Escalated,
    Stalled,
}

impl DecisionKind {
    /// The token written into `decisions.md` (`- {epoch} pmd {kind}: {summary}`).
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionKind::Working => "working",
            DecisionKind::Monitoring => "monitoring",
            DecisionKind::AutoFlow => "auto_flow",
            DecisionKind::Escalated => "escalated",
            DecisionKind::Stalled => "stalled",
        }
    }
}

/// One decision `pmd` disposed, kept in the bounded [`AgentLoopState::decisions`] slice.
/// Consecutive entries with the same `(kind, summary, stop_ids)` coalesce into one with a
/// bumped `count`/`at` (see [`AgentLoopState::record_decision`]).
///
/// `seq` is `Option<u64>`: `Some` = an accepted marker bump; `None` is reserved for a
/// `Stalled` record raised on a NON-dispose park path (never a row with a bumped seq but
/// stale ids). B only ever writes `Some` (the non-dispose park paths do not record here
/// yet — documented as future).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub at: Epoch,
    #[serde(default = "one_count")]
    pub count: u32,
    pub kind: DecisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_ids: Vec<String>,
}

impl DecisionRecord {
    /// One decision from an accepted marker bump (`seq` present), starting at `count == 1`.
    pub fn at(
        now: Epoch,
        seq: Option<u64>,
        kind: DecisionKind,
        summary: Option<String>,
        stop_ids: Vec<String>,
    ) -> Self {
        Self {
            seq,
            at: now,
            count: one_count(),
            kind,
            summary,
            stop_ids,
        }
    }
}

/// The bounded length of [`AgentLoopState::decisions`] — the oldest are dropped past this.
pub const DECISIONS_SLICE_MAX: usize = 64;

/// The longest detail string kept on a feed event. The feed now WRAPS and SCROLLS (see
/// `preview_autopilot_body`), so this is no longer a display budget — a long status or stop
/// question is shown in FULL there. It is only a ledger-bloat safety valve: it stops a
/// pathological paste from bloating a `state.json` rewritten every tick, set well above any
/// realistic one-line status or stop question so nothing a human would want to read is clipped
/// (the user's report was that a capped detail still read as truncated). [`AUTOPILOT_EVENTS_MAX`]
/// bounds how MANY events persist; this bounds how big each is.
pub(super) const AUTOPILOT_DETAIL_MAX: usize = 2000;

/// One line, bounded — collapse newlines and cap length, so a stored detail can never bloat
/// the ledger (or the render) with a paragraph.
pub(super) fn cap_detail(s: String) -> String {
    let one = s.replace(['\n', '\r'], " ");
    if one.chars().count() <= AUTOPILOT_DETAIL_MAX {
        one
    } else {
        let mut out: String = one
            .chars()
            .take(AUTOPILOT_DETAIL_MAX.saturating_sub(1))
            .collect();
        out.push('\u{2026}');
        out
    }
}

/// Deserialize the [`AgentLoopState::events`] feed LENIENTLY: parse each element on its own
/// and DROP any that fail, rather than failing the whole ledger. The feed is cosmetic, so a
/// single unparseable entry — a kind a newer `pmd` added, a hand-edit — must never take down
/// the operational fields (`run`, `cadence_s`, the watermarks) that share the file.
pub(super) fn de_events_lenient<'de, D>(d: D) -> std::result::Result<Vec<AutopilotEvent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// Deserialize [`AgentLoopState::decisions`] LENIENTLY, exactly like [`de_events_lenient`]:
/// parse each element on its own and DROP any that fail, so one unparseable decision never
/// fails the operational ledger load.
pub(super) fn de_decisions_lenient<'de, D>(
    d: D,
) -> std::result::Result<Vec<DecisionRecord>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// What one [`AutopilotEvent`] records. Variants carry only the short, human-facing
/// detail the dashboard shows — never anything the agent or supervisor authored beyond
/// its own one-line status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotEventKind {
    /// The persistent agent was (re)launched.
    Launched,
    /// A heartbeat nudge was delivered.
    Nudged,
    /// A heartbeat was withheld, and why.
    Held(HoldReason),
    /// The agent reported in (its one-line status, if any).
    Reported(Option<String>),
    /// The daemon auto-answered a low-stakes decision on the human's behalf.
    AutoAnswered(Option<String>),
    /// The session supervisor resolved a low-stakes decision.
    SupervisorResolved(Option<String>),
    /// The base cadence changed (agent-proposed), e.g. "5m → 1m".
    CadenceChanged(String),
    /// The agent raised a question that needs the human (its text, if any).
    Escalated(Option<String>),
    /// The session stalled/backstopped into a Stuck state (the reason, if any).
    Stuck(Option<String>),
    /// A human answered an escalation and unblocked the session (their answer text, if any) —
    /// the other half of the loop, so the feed does not read as pmd talking to itself.
    Answered(Option<String>),
}

impl AutopilotEventKind {
    /// The short label the dashboard prints for this decision.
    pub fn label(&self) -> &'static str {
        match self {
            AutopilotEventKind::Launched => "launched",
            AutopilotEventKind::Nudged => "nudged",
            AutopilotEventKind::Held(_) => "held",
            AutopilotEventKind::Reported(_) => "reported",
            AutopilotEventKind::AutoAnswered(_) => "auto-answered",
            AutopilotEventKind::SupervisorResolved(_) => "resolved",
            AutopilotEventKind::CadenceChanged(_) => "cadence",
            AutopilotEventKind::Escalated(_) => "needs you",
            AutopilotEventKind::Stuck(_) => "stuck",
            AutopilotEventKind::Answered(_) => "you answered",
        }
    }

    /// Whether this event is a DECISION worth reviewing — pmd acting on the human's behalf,
    /// or the escalation lifecycle — as opposed to routine heartbeat activity (a nudge, a
    /// withheld beat, or the agent's own status echo). The dashboard's decision lane (`pmtui`
    /// `v`) filters the feed by this so it shows what autopilot DID, not every tick it took.
    pub fn is_decision(&self) -> bool {
        !matches!(
            self,
            AutopilotEventKind::Nudged
                | AutopilotEventKind::Held(_)
                | AutopilotEventKind::Reported(_)
        )
    }

    /// The trailing detail, if any (a status line, a question, a hold reason, or "5m → 1m").
    pub fn detail(&self) -> Option<String> {
        match self {
            AutopilotEventKind::Held(r) => Some(r.label().to_string()),
            AutopilotEventKind::CadenceChanged(n) => Some(n.clone()),
            AutopilotEventKind::Reported(s)
            | AutopilotEventKind::AutoAnswered(s)
            | AutopilotEventKind::SupervisorResolved(s)
            | AutopilotEventKind::Escalated(s)
            | AutopilotEventKind::Stuck(s)
            | AutopilotEventKind::Answered(s) => s.clone(),
            AutopilotEventKind::Launched | AutopilotEventKind::Nudged => None,
        }
    }

    /// This kind with its free-text detail one-lined and length-capped ([`cap_detail`]), so a
    /// feed entry can never store a paragraph. `Launched`/`Nudged`/`Held` carry no free text.
    pub(super) fn capped(self) -> Self {
        use AutopilotEventKind::*;
        match self {
            Reported(s) => Reported(s.map(cap_detail)),
            AutoAnswered(s) => AutoAnswered(s.map(cap_detail)),
            SupervisorResolved(s) => SupervisorResolved(s.map(cap_detail)),
            Escalated(s) => Escalated(s.map(cap_detail)),
            Stuck(s) => Stuck(s.map(cap_detail)),
            Answered(s) => Answered(s.map(cap_detail)),
            CadenceChanged(n) => CadenceChanged(cap_detail(n)),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests;
