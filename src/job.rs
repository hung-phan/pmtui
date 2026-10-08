//! Agent-loop (`Mode::AgentLoop`) data types: the per-session ledger the harness
//! owns, and the machine report a resumed worker writes each wake.
//!
//! Worker proposes, harness disposes — as with phase workers. Each wake the agent
//! writes a [`WakeReport`] (its self-assessment); the harness alone maps it onto
//! [`AgentLoopState`] and decides whether to re-invoke, park, or escalate (the
//! disposer lands in S3). There is deliberately **no** machine-set "done": an
//! agent-loop session ends only when a human closes it (see the design doc,
//! `docs/superpowers/specs/2026-08-13-agent-loop-session-heartbeat-design.md`).

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::SystemTime;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::clock::Epoch;
use crate::pmstate::{OpenStop, StopKind};
use crate::registry::Engine;
use crate::state::{self, ProjectPaths, RiskClass};
use crate::worker::StopDraft;

mod turn_trace;
pub use turn_trace::{
    TURN_TRACE_MAX, TurnDisposition, TurnNoReportReason, TurnOutcome, TurnReviewOutcome, TurnTrace,
    TurnTrigger,
};

/// The harness's run state for one agent-loop session. Mirrors
/// `scheduler::RunState` but is serializable (persisted in the per-session ledger)
/// and has **no `Done`** variant — done is human-only. The in-flight worker's
/// crash-recovery handle lives in `driver.json` (reused verbatim), not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JobRun {
    /// No worker in flight and no wake due yet.
    #[default]
    Idle,
    /// A worker spawned this wake; observe its done-signal until `deadline`.
    Running {
        seq: u64,
        session: String,
        deadline: Epoch,
    },
    /// Parked between wakes; the next wake is due at `until` (the cadence timer).
    Monitoring { until: Epoch },
    /// Parked on one or more open stops needing a human decision; only answers
    /// at/after `since` can unblock it (stale-answer guard, as in the phase engine).
    Blocked { stop_ids: Vec<String>, since: Epoch },
}

/// The per-session ledger for an agent-loop session, at
/// `<root>/.project-state/sessions/<id>/state.json`. One per session, so multiple
/// sessions can share a project folder (see [`crate::state::ProjectPaths::for_session`]).
///
/// Authority for runtime fields (`conversation_id`, `cadence_s`, `run`, `open_stops`,
/// `last_status`, `continuations`) is this ledger; the corresponding registry
/// fields only *seed* a fresh session — the disposer never reads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkerRevision {
    pub modified_ns: u64,
    pub len: u64,
    pub inode: u64,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLoopState {
    pub created_at: Epoch,
    /// The pinned (claude) or captured (codex) conversation id resumed each wake.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// `conversation_id` is a registry SEED not yet PROVEN to exist in the engine — this
    /// gates the one-shot resume→create fallback. A human who creates a Standard session
    /// and flips to Autopilot WITHOUT chatting first seeds a `conversation_id` the engine
    /// never persisted (claude only persists a conversation after its first turn), so the
    /// first launch's `claude --resume <seed>` prints "No conversation found" and exits —
    /// an infinite relaunch loop. When this is `true`, the NEXT relaunch (reached only
    /// because the prior seed-resume launch died) falls back to CREATE
    /// (`--session-id <seed>`) exactly once, then clears the flag so every later relaunch
    /// resumes the now-real conversation instead of looping create→create. Set `true`
    /// only when adopting a registry seed (never for a freshly minted uuid, which IS
    /// created); cleared the moment the create-fallback fires and when a real marker bump
    /// proves the agent ran a turn on this conversation. `#[serde(default)]`: a ledger
    /// written before this field existed loads as `false` (a confirmed cid), the safe
    /// default that always resumes.
    #[serde(default)]
    pub resume_unconfirmed: bool,
    pub engine: Engine,
    /// Per-session heartbeat cadence in seconds; `None` ⇒ the harness default.
    #[serde(default)]
    pub cadence_s: Option<u64>,
    /// The HUMAN set [`Self::cadence_s`] explicitly, so the agent's own proposals no longer
    /// overwrite it.
    ///
    /// Both parties may re-time a session — the user asked for exactly that: *"i want to have a way to
    /// update cadence as well. we would need to able to change it or ask agent to change it
    /// adaptively"*. What was missing was who wins when they disagree, and the answer that shipped was
    /// "whoever wrote last", which meant the agent: user, *"5 minutes takes effect and it revert my
    /// cadence setting later"*. A dial that springs back to a value the human did not choose is the
    /// same defect m45 fixed for the autonomy dial — a human-owned control is not the harness's to
    /// write.
    ///
    /// So the dial has an OWNER: the agent by default (adaptive, which is the requested feature), the
    /// human from the moment they turn it (see [`Self::retime`]). A refused proposal is REPORTED via
    /// `last_status` rather than dropped, so an agent that wants a different rhythm can still say so.
    ///
    /// `#[serde(default)]`: a ledger written before this field existed loads as agent-owned, which is
    /// the behaviour it had.
    #[serde(default)]
    pub cadence_pinned: bool,
    /// Highest pmtui wake request applied by pmd from `control.json`.
    #[serde(default)]
    pub applied_wake_generation: u64,
    #[serde(default)]
    pub run: JobRun,
    #[serde(default)]
    pub open_stops: Vec<OpenStop>,
    /// The last human-facing status line the agent reported (for the dashboard).
    #[serde(default)]
    pub last_status: Option<String>,
    /// The agent's last reported `next_step` (mirror of WakeReport.next_step),
    /// kept-prior when a report omits it. Echoed verbatim into the nudge.
    #[serde(default)]
    pub last_plan: Option<String>,
    /// Consecutive wakes with no parseable progress; a bounded stall guard raises
    /// a `Stuck` escalation at the configured threshold rather than spinning.
    #[serde(default)]
    pub continuations: u32,
    /// FO-1 (Milestone D): consecutive accepted marker bumps whose `status` AND
    /// `next_step` were byte-identical (trimmed) to the prior report's stored values —
    /// the "alive but not advancing" streak. `#[serde(default)]` (plain, like
    /// `continuations`) so a pre-D ledger loads at 0. Reset on a changed plan (in the
    /// disposer), in `on_blocked`, and in `retime`. Milestone E renders a nudge line once
    /// this reaches its signal threshold (E's own const, currently 3); the disposer
    /// escalates a `WorkerStuck` once it reaches `marker::DEFAULT_STALE_PLAN_STALL`.
    #[serde(default)]
    pub stale_plan_streak: u32,
    /// FO-2 (Milestone D): consecutive "marker-less rechecks" — ticks on which a turn
    /// completed since the last nudge but the agent wrote no fresh marker. Bounds a fast
    /// `WorkerStuck` backstop (`drive::MARKER_LESS_RECHECK_MAX`) independent of the 30-min
    /// `DEFAULT_STALL_BUSY_S`. Reset on ANY accepted marker bump (in the disposer) and in
    /// `on_blocked`. `#[serde(default)]` so a pre-D ledger loads at 0.
    #[serde(default)]
    pub marker_less_rechecks: u32,
    /// Worker-authored diagnostic stamp from the most recently accepted marker. Audit-only:
    /// scheduling, deduplication, and turn/decider correlation use pmd-owned report identity.
    #[serde(default)]
    pub last_marker_seq: u64,
    /// Monotonic pmd-owned identity for accepted worker reports. Unlike worker `seq`, this cannot
    /// jump into the future or move backward. Seeded lazily from [`Self::digest`] when an older
    /// ledger first accepts a report.
    #[serde(default)]
    pub report_generation: u64,
    /// Exact atomic marker revision last accepted by pmd. Metadata distinguishes two deliberate
    /// rewrites with identical status/plan, while the semantic hash guards content identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_marker_revision: Option<MarkerRevision>,
    /// Context to inject into the NEXT wake's prompt, then consume (cleared on the
    /// next spawn). Set when the harness auto-flows a batch of low-stakes stops so
    /// the resumed agent is told its decision was auto-approved and does not re-ask.
    /// The human-answer path feeds its own context directly (a non-empty `extra`);
    /// this durably carries the auto-flow context across the `Monitoring` park.
    #[serde(default)]
    pub pending_context: Option<String>,
    /// Ordinary worker decisions waiting for independent goal-aware review. A marker may report
    /// several decisions at once, but each verdict authorizes exactly one; this durable queue lets
    /// pmd consult them sequentially across sweeps and daemon restarts before waking the worker.
    #[serde(
        default,
        deserialize_with = "de_advice_queue_bounded",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub advice_queue: Vec<QueuedAdvice>,
    /// Legacy worker-sequence nudge watermark retained only while loading ledgers written before
    /// [`Self::nudged_at_report_generation`]. New scheduling never depends on this value.
    ///
    /// This is the state half of "never interrupt an agent that is working". User: *"When the
    /// session is chatting or working on sth, and not idle, i don't want autopilot to kick in and
    /// queue a prompt. That is not good and can disrupt ongoing work on claude/codex."*
    ///
    /// The pane heuristic (`tmux::classify_pane`) was the only guard before this, and it is
    /// best-effort BY CONSTRUCTION: it infers from pixels, so a working agent whose busy marker
    /// happens to be outside the captured tail reads as Idle and gets typed into — where the CLI
    /// queues the text and runs it after the turn in flight, which is exactly the disruption the
    /// user described. The agent's own report is not a heuristic.
    ///
    /// Kept durable for compatibility until the next delivered nudge records pmd-owned identity.
    #[serde(default)]
    pub nudged_at_seq: Option<u64>,
    /// [`Self::report_generation`] as it stood when pmd last nudged the worker. This is the
    /// authoritative report-debt watermark. `None` on older ledgers falls back to the legacy
    /// worker-sequence comparison until the next delivered nudge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nudged_at_report_generation: Option<u64>,
    /// WHEN pmd last addressed the worker, so the age of an outstanding report debt is readable.
    ///
    /// Set wherever a debt is OPENED — beside [`Self::nudged_at_report_generation`] at every one of
    /// its three sites — because a ceiling that silently does not apply on some of them is not a
    /// ceiling. (`turn_trace`'s open entry carries the same instant, but only for heartbeat nudges:
    /// its trigger enum is deliberately `Heartbeat`-only, so the dialog-answer and applied-verdict
    /// paths have no trace and would have gone unbounded.)
    ///
    /// Durable on purpose. The in-memory stall window is lost across a daemon restart, and a session
    /// held in silence outlives one. `None` on an older ledger simply means no ceiling until the next
    /// nudge records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nudged_at: Option<Epoch>,
    /// The turn-complete count ([`ProjectPaths::turn_signal`] file SIZE) at the LAST
    /// delivered nudge — the DASHBOARD's baseline for telling "working" from "waiting"
    /// (M72). The daemon can't surface that sub-state itself: it parks `Monitoring` and is
    /// blind between nudges, whereas the engine's turn-end hook appends to the signal file in
    /// real time. So the view reads the signal file each refresh and compares: a size still
    /// equal to this baseline ⇒ the agent has not finished the nudged turn ⇒ WORKING; a
    /// larger size ⇒ a turn completed ⇒ idle at its prompt, WAITING for the next nudge. Set
    /// on a delivered nudge (mirrors the in-memory `JobScheduler::turns_at_nudge`, which the
    /// nudge GATE uses); `#[serde(default)]` so a pre-M72 ledger loads (→ `None` ⇒ the view
    /// can't tell ⇒ shows the driven session as running, the M72 default).
    #[serde(default)]
    pub turn_count_at_nudge: Option<u64>,
    /// Bookkeeping for a supervisor consult the harness spawned and has not yet
    /// honoured — the DURABILITY half of the m20 supervisor (see [`ParkedAdvice`]).
    /// `None` on the overwhelming majority of ticks, so it is skipped when serializing
    /// and a ledger written by a pre-m23 daemon still loads (`#[serde(default)]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advice_inflight: Option<ParkedAdvice>,
    /// The decider transport was latched off for this session after repeated failures/refusals.
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub decider_latched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decider_latch_reason: Option<String>,
    /// Bounded, structured audit history for read-only decider consults. Operational behavior never
    /// depends on it: pmd writes it alongside the existing lifecycle state and pmtui only reads it.
    #[serde(
        default,
        deserialize_with = "de_decider_runs_lenient",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub decider_runs: Vec<DeciderRun>,
    /// Monotonic pmd-owned identifier for the bounded turn trace.
    #[serde(default)]
    pub turn_seq: u64,
    /// Successful heartbeat inputs correlated to the next accepted worker report. Audit-only:
    /// scheduling and authority never depend on this bounded, leniently loaded history.
    #[serde(
        default,
        deserialize_with = "turn_trace::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub turn_trace: Vec<TurnTrace>,
    /// The most recent autopilot decisions `pmd` made while driving this session — the
    /// AUTOPILOT feed the dashboard renders. Bounded to [`AUTOPILOT_EVENTS_MAX`]; the
    /// harness is the sole writer (see [`AgentLoopState::record_event`]). `#[serde(default,
    /// skip_serializing_if)]` so a ledger written before this field existed loads (→ empty)
    /// and an idle session's ledger is not bloated by an empty array.
    ///
    /// Two robustness choices, both because this feed is COSMETIC and must never take the
    /// operational ledger down with it: it deserializes LENIENTLY (see [`de_events_lenient`])
    /// — a single unparseable entry (a kind a newer `pmd` added, a hand-edit) is dropped, not
    /// propagated, so `run`/`cadence_s`/the watermarks still load. And unlike `advice_inflight`,
    /// `events` is non-empty in steady state, so — combined with `deny_unknown_fields` — once
    /// any event is recorded an OLDER binary can no longer load this ledger; forward-compat
    /// (running a downgrade on newer data) is intentionally not preserved for this field.
    #[serde(
        default,
        deserialize_with = "de_events_lenient",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub events: Vec<AutopilotEvent>,
    /// Lifetime-monotonic decider-lane counters (never reset by on_blocked/retime).
    /// `#[serde(default, skip_serializing_if)]` so a pre-B ledger loads (→ zero) and a
    /// fresh/idle ledger is not bloated by an all-zero object.
    #[serde(default, skip_serializing_if = "DecisionCounters::is_zero")]
    pub digest: DecisionCounters,
    /// The bounded, coalesced recent-decisions slice. Deserialized LENIENTLY (a single bad
    /// entry is dropped, never propagated) — this feed is machine-cosmetic, exactly like
    /// [`Self::events`].
    #[serde(
        default,
        deserialize_with = "de_decisions_lenient",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub decisions: Vec<DecisionRecord>,
    /// A snapshot of the last disposed report — the "situation" Milestone C projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub situation: Option<LedgerSituation>,
    pub updated_at: Epoch,
}

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

fn bool_is_false(value: &bool) -> bool {
    !*value
}

/// Maximum retained decider consults. Audit rows are richer than the compact event feed, so keep a
/// smaller history while still covering a long interactive session.
pub const DECIDER_RUNS_MAX: usize = 50;
/// Maximum ordinary decisions accepted from one marker for serial review. Additional decisions are
/// audited and handed to the human instead of creating an unbounded model-call queue.
pub const ADVICE_QUEUE_MAX: usize = 8;

fn de_advice_queue_bounded<'de, D>(deserializer: D) -> Result<Vec<QueuedAdvice>, D::Error>
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

fn one_count() -> u32 {
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

/// A snapshot of the last report `pmd` disposed — the current "situation" the supervisor
/// projection (Milestone C) reads. Projected fresh at spawn, but persisted here so it
/// never goes stale across a park (set by the single dispose seam on EVERY accepted bump).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerSituation {
    pub state: WakeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_stops: Vec<String>,
    pub seq: u64,
    pub at: Epoch,
}

impl LedgerSituation {
    /// Snapshot the just-disposed `report` (its state/status/seq) plus the stop ids the
    /// session is open on at disposition time.
    pub fn from_report(report: &WakeReport, open_stops: &[String], now: Epoch) -> Self {
        Self {
            state: report.state,
            status: report.status.clone(),
            open_stops: open_stops.to_vec(),
            seq: report.seq,
            at: now,
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
const AUTOPILOT_DETAIL_MAX: usize = 2000;
const DECIDER_AUDIT_TEXT_MAX: usize = 400;
const DECIDER_AUDIT_OPTION_MAX: usize = 200;

/// One line, bounded — collapse newlines and cap length, so a stored detail can never bloat
/// the ledger (or the render) with a paragraph.
fn cap_detail(s: String) -> String {
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

fn cap_audit_text(s: String) -> String {
    cap_one_line(s, DECIDER_AUDIT_TEXT_MAX)
}

fn cap_audit_option(s: String) -> String {
    cap_one_line(s, DECIDER_AUDIT_OPTION_MAX)
}

fn cap_one_line(s: String, max: usize) -> String {
    let one = s.replace(['\n', '\r'], " ");
    if one.chars().count() <= max {
        return one;
    }
    let mut out: String = one.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

fn cap_decider_outcome(outcome: DeciderOutcome) -> DeciderOutcome {
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

/// Deserialize the [`AgentLoopState::events`] feed LENIENTLY: parse each element on its own
/// and DROP any that fail, rather than failing the whole ledger. The feed is cosmetic, so a
/// single unparseable entry — a kind a newer `pmd` added, a hand-edit — must never take down
/// the operational fields (`run`, `cadence_s`, the watermarks) that share the file.
fn de_events_lenient<'de, D>(d: D) -> std::result::Result<Vec<AutopilotEvent>, D::Error>
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
fn de_decisions_lenient<'de, D>(d: D) -> std::result::Result<Vec<DecisionRecord>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// Deserialize the cosmetic decider audit independently from operational state.
fn de_decider_runs_lenient<'de, D>(d: D) -> std::result::Result<Vec<DeciderRun>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// Why `pmd` HELD a heartbeat instead of nudging — the sub-state the drive loop computes
/// each tick and, before this feed, never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    /// The pane classified Busy — the agent is visibly working.
    Busy,
    /// A nudge is outstanding: the agent has not reported since we last spoke.
    AwaitingReport,
    /// A turn is in progress (the turn-end hook has not fired since the nudge).
    TurnInProgress,
    /// The pane looks idle but has not held still long enough to confirm it.
    IdleUnconfirmed,
    /// A transient tmux capture/send error — recheck shortly.
    Transient,
}

impl HoldReason {
    /// A short human-facing reason for the AUTOPILOT feed.
    pub fn label(&self) -> &'static str {
        match self {
            HoldReason::Busy => "agent working",
            HoldReason::AwaitingReport => "waiting on its report",
            HoldReason::TurnInProgress => "mid-turn",
            HoldReason::IdleUnconfirmed => "confirming idle",
            HoldReason::Transient => "recheck",
        }
    }
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
    fn capped(self) -> Self {
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

impl AgentLoopState {
    /// Append a driving decision to the [`Self::events`] feed, COALESCING a run of the
    /// same decision into one entry (bump `at`, `count += 1`) so a heartbeat held on the
    /// same reason across many rechecks stays a single "held ×N" line. Bounded to
    /// [`AUTOPILOT_EVENTS_MAX`] — the oldest are dropped. The harness is the sole caller
    /// (mirrors every other ledger write); the dashboard only reads.
    pub fn record_event(&mut self, now: Epoch, kind: AutopilotEventKind) {
        // Cap the detail BEFORE comparing, so coalescing sees the stored (capped) form.
        let kind = kind.capped();
        if let Some(last) = self.events.last_mut()
            && last.kind == kind
        {
            last.at = now;
            last.count = last.count.saturating_add(1);
            return;
        }
        self.events.push(AutopilotEvent {
            at: now,
            count: 1,
            kind,
        });
        if self.events.len() > AUTOPILOT_EVENTS_MAX {
            let drop = self.events.len() - AUTOPILOT_EVENTS_MAX;
            self.events.drain(0..drop);
        }
    }

    /// Record one disposed decision into the bounded [`Self::decisions`] slice, COALESCING a
    /// run of the same `(kind, summary, stop_ids)` into one entry (bump `at`, `count += 1`)
    /// exactly as [`Self::record_event`] does for the feed. Bumps the PER-KIND `digest`
    /// counter (best-effort); the exact `digest.disposed` bump is the dispose seam's job, so
    /// it is deliberately NOT touched here (a `Stalled` record raised on a non-dispose park
    /// path would otherwise over-count `disposed`). The harness is the sole caller.
    pub fn record_decision(&mut self, mut entry: DecisionRecord) {
        match entry.kind {
            DecisionKind::Working => self.digest.working = self.digest.working.saturating_add(1),
            DecisionKind::Monitoring => {
                self.digest.monitoring = self.digest.monitoring.saturating_add(1)
            }
            DecisionKind::AutoFlow => {
                self.digest.auto_flow = self.digest.auto_flow.saturating_add(1)
            }
            DecisionKind::Escalated => {
                self.digest.escalated = self.digest.escalated.saturating_add(1)
            }
            DecisionKind::Stalled => self.digest.stalled = self.digest.stalled.saturating_add(1),
        }
        // Cap the summary BEFORE comparing, so coalescing sees the stored (capped) form.
        entry.summary = entry.summary.map(cap_detail);
        if let Some(last) = self.decisions.last_mut()
            && last.kind == entry.kind
            && last.summary == entry.summary
            && last.stop_ids == entry.stop_ids
        {
            last.at = entry.at;
            last.count = last.count.saturating_add(1);
            // Keep the freshest accepted seq (a coalesced accepted bump overrides an older).
            if entry.seq.is_some() {
                last.seq = entry.seq;
            }
            return;
        }
        self.decisions.push(entry);
        if self.decisions.len() > DECISIONS_SLICE_MAX {
            let drop = self.decisions.len() - DECISIONS_SLICE_MAX;
            self.decisions.drain(0..drop);
        }
    }

    /// Start one decider audit record. Sequence numbers may repeat after a daemon restart, so
    /// completion searches the newest matching `Consulting` record rather than treating `seq` as a
    /// permanent identifier.
    pub fn record_decider_run(&mut self, mut run: DeciderRun) {
        run.question = cap_audit_text(run.question);
        run.options = run.options.into_iter().map(cap_audit_option).collect();
        run.model = run.model.map(cap_audit_text);
        run.outcome = cap_decider_outcome(run.outcome);
        self.decider_runs.push(run);
        if self.decider_runs.len() > DECIDER_RUNS_MAX {
            let drop = self.decider_runs.len() - DECIDER_RUNS_MAX;
            self.decider_runs.drain(0..drop);
        }
    }

    /// Finish the newest matching live consult. Returns false only for an old ledger that did not
    /// yet record structured audit starts; operational verdict handling is unaffected.
    pub fn finish_decider_run(&mut self, seq: u64, now: Epoch, outcome: DeciderOutcome) -> bool {
        let Some(run) = self
            .decider_runs
            .iter_mut()
            .rev()
            .find(|run| run.seq == seq && run.outcome == DeciderOutcome::Consulting)
        else {
            return false;
        };
        run.finished_at = Some(now);
        run.outcome = cap_decider_outcome(outcome);
        true
    }

    /// Is a nudge OUTSTANDING — we spoke to the agent and it has not reported since?
    ///
    /// The daemon's second brake on driving, independent of the park: [`JobScheduler::drive`] routes
    /// a session in this state to `busy_recheck` and does NOT nudge, however idle its pane looks.
    ///
    /// A METHOD, not the comparison written twice, because the dashboard has to describe exactly the
    /// brake the daemon applies. It did not, and the gap was a real bug: the preview counted down
    /// `check in 0s` while this predicate held, so the human watched a timer expire and nothing
    /// happen — user: *"i change cadence value to 1m from 5m. The check in go to 0 but no message is
    /// sent"*. Two copies of one rule is how a dashboard ends up describing a schedule nobody keeps.
    pub fn awaiting_report(&self) -> bool {
        self.nudged_at_report_generation.map_or_else(
            || {
                self.nudged_at_seq
                    .is_some_and(|seq| self.last_marker_seq <= seq)
            },
            |generation| self.report_generation <= generation,
        )
    }

    /// WHAT A HUMAN TURNING THE CADENCE DIAL MEANS TO THE SCHEDULER: record the new interval, start
    /// it FROM NOW, and stop waiting for the last nudge's report.
    ///
    /// All three, in one place, because doing two of them is a bug that has now shipped twice. User,
    /// at m41: *"changing the cadence needs to cancel current check in and start again … i update it
    /// to 1m and it doesn't take effect"* — that was the park. Then, at m48: *"i change cadence value
    /// to 1m from 5m. The check in go to 0 but no message is sent"* — that was [`Self::awaiting_report`],
    /// the OTHER brake, which `drive` honours over any schedule.
    ///
    /// Clearing the watermark does not reopen m39 (*"i don't want autopilot to kick in and queue a
    /// prompt"* on a working agent): the pane classifier is still the primary guard and
    /// `IDLE_CONFIRMATIONS_REQUIRED` still demands two consecutive Idle observations. The watermark is
    /// a PROXY for "working", and an explicit human action is a stronger signal than a proxy.
    ///
    /// Lives HERE rather than in `pmtui`'s `apply_cadence_edit` so the scheduler's own tests can drive
    /// the human's edit without restating what it does — a second copy of this rule is precisely how
    /// the first half-fix survived a green suite.
    pub fn retime(&mut self, secs: u64, now: Epoch) {
        self.cadence_s = Some(secs);
        // THE HUMAN NOW OWNS THE DIAL — see `cadence_pinned`. Set here, in the human's edit path, and
        // deliberately NOT at creation: the create form picks a STARTING rhythm, while turning the dial
        // mid-run is a correction to pacing the human has just watched.
        self.cadence_pinned = true;
        // A human cadence edit resets the FO-1 plan-staleness streak — a fail-safe wipe of
        // the accumulating signal (it can only delay a stall escalation, never fire one).
        self.stale_plan_streak = 0;
        if let JobRun::Monitoring { .. } = self.run {
            self.run = JobRun::Monitoring {
                until: now + secs as i64,
            };
        }
        self.nudged_at_seq = None;
        self.nudged_at_report_generation = None;
    }

    /// A freshly created session: idle, no conversation yet, no stops.
    pub fn fresh(engine: Engine, cadence_s: Option<u64>, now: Epoch) -> Self {
        Self {
            created_at: now,
            conversation_id: None,
            resume_unconfirmed: false,
            engine,
            cadence_s,
            cadence_pinned: false,
            applied_wake_generation: 0,
            run: JobRun::Idle,
            open_stops: Vec::new(),
            last_status: None,
            last_plan: None,
            continuations: 0,
            stale_plan_streak: 0,
            marker_less_rechecks: 0,
            nudged_at: None,
            last_marker_seq: 0,
            report_generation: 0,
            last_marker_revision: None,
            pending_context: None,
            advice_queue: Vec::new(),
            nudged_at_seq: None,
            nudged_at_report_generation: None,
            turn_count_at_nudge: None,
            advice_inflight: None,
            decider_latched: false,
            decider_latch_reason: None,
            decider_runs: Vec::new(),
            turn_seq: 0,
            turn_trace: Vec::new(),
            events: Vec::new(),
            digest: DecisionCounters::default(),
            decisions: Vec::new(),
            situation: None,
            updated_at: now,
        }
    }
}

/// Load the per-session agent-loop ledger, or `None` if the session has none yet.
/// Mirrors [`crate::pmstate::load`] but reads the [`AgentLoopState`] schema at the
/// per-session `state.json` (see [`ProjectPaths::for_session`]).
pub fn load(paths: &ProjectPaths) -> Result<Option<AgentLoopState>> {
    state::read_json_opt(&paths.pmstate())
}

/// Persist the per-session agent-loop ledger atomically (the harness is the sole
/// writer, exactly as with the phase ledger).
pub fn save(paths: &ProjectPaths, s: &AgentLoopState) -> Result<()> {
    state::write_json_atomic(&paths.pmstate(), s)
}

/// Stable semantic identity for one parsed worker report. The canonical struct serialization
/// ignores JSON whitespace/key ordering while retaining every accepted field, including the
/// worker-authored audit `seq`.
pub fn report_fingerprint(report: &WakeReport) -> Result<String> {
    use sha2::{Digest, Sha256};

    let mut semantic = report.clone();
    semantic.seq = 0;
    let canonical = serde_json::to_vec(&semantic)?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

/// Best-effort revision identity for read-only diagnostics. The marker disposer uses the same
/// fields but obtains metadata and bytes from one open descriptor to avoid replacement races.
pub fn marker_revision(path: &Path, report: &WakeReport) -> Result<MarkerRevision> {
    let metadata = std::fs::metadata(path)?;
    let modified_ns = metadata
        .modified()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    Ok(MarkerRevision {
        modified_ns,
        len: metadata.len(),
        inode: metadata.ino(),
        fingerprint: report_fingerprint(report)?,
    })
}

/// The agent's self-assessment at the end of one wake. The agent writes it as its
/// final act; the harness treats every field as a *proposal* validated against policy, never
/// obeyed. A missing/unparseable report is handled leniently by the disposer
/// (treated as `working`, bumping the stall counter), never a silent stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeReport {
    pub state: WakeState,
    /// Worker-authored diagnostic stamp, normally current Unix seconds. It is preserved in audit
    /// records but never controls freshness, scheduling, or authority; pmd owns report generation
    /// and deduplicates the complete parsed marker.
    #[serde(default)]
    pub seq: u64,
    /// Stops the agent is raising this wake (routed through the tier policy).
    #[serde(default)]
    pub stops: Vec<StopDraft>,
    /// For `monitoring`: how long to nap before the next wake (agent self-schedules
    /// a longer sleep than the base cadence when it is waiting on something).
    ///
    /// ONE nap, not a new rhythm — contrast [`WakeReport::cadence_s`].
    #[serde(default)]
    pub next_check_s: Option<u64>,
    /// A proposed new BASE cadence, in seconds, persisted into the ledger.
    ///
    /// This is the ADAPTIVE dial: `next_check_s` says "leave me alone until X" for one
    /// park, whereas this says "this project's natural rhythm is X" and outlives the wake.
    /// An agent that has learned its work arrives hourly should not have to re-request a
    /// long nap every single wake, and one in a tight edit loop should be able to ask for
    /// closer nudges without a human opening the dashboard.
    ///
    /// A PROPOSAL, like every other field here: `JobScheduler::dispose` clamps it into
    /// `[CADENCE_MIN_S, CADENCE_MAX_S]` before writing, so an agent cannot talk the harness
    /// into a nudge storm or into parking itself forever. A change is also surfaced on the
    /// dashboard status, because a session that silently re-times itself is exactly the kind
    /// of invisible behaviour change this harness exists to prevent.
    #[serde(default)]
    pub cadence_s: Option<u64>,
    /// A one-line human-facing status for the dashboard.
    #[serde(default)]
    pub status: Option<String>,
    /// The agent's own one-line plan for its next wake. Echoed VERBATIM back into
    /// the nudge's "What to do now" — a briefing off the agent's own words, never
    /// pmd re-teaching. Optional: a terse bump omits it and behaves exactly as before.
    #[serde(default)]
    pub next_step: Option<String>,
    /// On the first (create) wake, the conversation id the agent settled on — used
    /// to capture codex's assigned id (claude's is pinned by the harness).
    #[serde(default)]
    pub conversation_id: Option<String>,
}

/// What the agent says its state is at the end of a wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeState {
    /// Made progress; re-invoke on the next cadence tick.
    Working,
    /// Waiting on independently observable work to inspect on a later wake; park until
    /// `next_check_s`.
    Monitoring,
    /// Needs a human decision; the harness routes `stops` through the tier policy.
    Blocked,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pmstate::StopKind;
    use crate::state::RiskClass;

    #[test]
    fn agent_loop_state_round_trips_full_and_minimal() {
        // Minimal: an old/small ledger with only the required fields loads, and
        // run defaults to Idle.
        let json = r#"{ "created_at": 10, "engine": "claude", "updated_at": 10 }"#;
        let st: AgentLoopState = serde_json::from_str(json).unwrap();
        assert_eq!(st.engine, Engine::Claude);
        assert_eq!(st.run, JobRun::Idle);
        assert!(st.conversation_id.is_none() && st.open_stops.is_empty());
        assert_eq!(st.continuations, 0);
        // A pre-S5a ledger has no `pending_context`; `#[serde(default)]` keeps it
        // loadable (deny_unknown_fields rejects EXTRA keys, not MISSING ones).
        assert!(st.pending_context.is_none());
        // Likewise a pre-Slice-2 ledger has no `last_marker_seq`; defaults to 0.
        assert_eq!(st.last_marker_seq, 0);
        // And a pre-m23 ledger has no `advice_inflight`.
        assert!(st.advice_inflight.is_none());
        // A pre-m38 ledger has no `nudged_at_seq` either. It MUST default to `None` rather than
        // `Some(0)`: `None` means "no nudge is outstanding", so an old ledger loaded by a new
        // daemon is nudgeable, while `Some(0)` would read as "awaiting a report" and park a healthy
        // session until the 30-minute stall backstop fired.
        assert!(st.nudged_at_seq.is_none());
        // A pre-B ledger has none of the decider-lane fields; all default (empty/zero/None).
        assert!(st.digest.is_zero());
        assert!(st.decisions.is_empty());
        assert!(st.decider_runs.is_empty());
        assert_eq!(st.turn_seq, 0);
        assert!(st.turn_trace.is_empty());
        assert!(!st.decider_latched);
        assert!(st.decider_latch_reason.is_none());
        assert!(st.situation.is_none());
        // A pre-D ledger has neither Milestone-D counter; both default to 0.
        assert_eq!(st.stale_plan_streak, 0);
        assert_eq!(st.marker_less_rechecks, 0);
        // A pre-fallback ledger has no `resume_unconfirmed`; it defaults to `false` — a
        // confirmed cid that always RESUMES (never the one-shot create-fallback).
        assert!(!st.resume_unconfirmed);

        // Full: round-trips a Monitoring run + conversation id + cadence + status +
        // a pending auto-flow context to inject on the next wake + the marker watermark.
        let full = AgentLoopState {
            created_at: 1,
            conversation_id: Some("uuid-1".into()),
            // An adopted-but-unproven seed: the round-trip must preserve the fallback gate
            // across a save, or a relaunch after a daemon restart would resume a ghost id.
            resume_unconfirmed: true,
            engine: Engine::Codex,
            cadence_s: Some(300),
            // PINNED, so the round-trip covers the flag too: a ledger that forgot it across a save
            // would hand the dial back to the agent on the next report, which is the bug it exists to
            // stop (user: *"it revert my cadence setting later"*).
            cadence_pinned: true,
            applied_wake_generation: 9,
            run: JobRun::Monitoring { until: 1800 },
            open_stops: Vec::new(),
            last_status: Some("watching the channel".into()),
            last_plan: Some("wire the alert".into()),
            continuations: 2,
            stale_plan_streak: 4,
            marker_less_rechecks: 2,
            nudged_at: Some(1_790_908_704),
            last_marker_seq: 100,
            report_generation: 12,
            last_marker_revision: Some(MarkerRevision {
                modified_ns: 1,
                len: 2,
                inode: 3,
                fingerprint: "abc123".into(),
            }),
            pending_context: Some("auto-approved: proceed".into()),
            advice_queue: Vec::new(),
            nudged_at_seq: Some(100),
            nudged_at_report_generation: Some(12),
            turn_count_at_nudge: Some(7),
            advice_inflight: Some(ParkedAdvice {
                seq: 4,
                stop_ids: vec!["stop-a".into()],
                pane_dialog: false,
            }),
            decider_latched: true,
            decider_latch_reason: Some("transport unavailable".into()),
            decider_runs: Vec::new(),
            turn_seq: 0,
            turn_trace: Vec::new(),
            // A couple of autopilot-feed events, so the round-trip covers the new field
            // (including a coalesced count and a held-reason payload).
            events: vec![
                AutopilotEvent {
                    at: 40,
                    count: 3,
                    kind: AutopilotEventKind::Held(HoldReason::Busy),
                },
                AutopilotEvent {
                    at: 41,
                    count: 1,
                    kind: AutopilotEventKind::Reported(Some("watching".into())),
                },
            ],
            digest: DecisionCounters {
                disposed: 5,
                working: 3,
                monitoring: 1,
                auto_flow: 0,
                escalated: 1,
                stalled: 0,
            },
            decisions: vec![DecisionRecord::at(
                41,
                Some(100),
                DecisionKind::Escalated,
                Some("ship it?".into()),
                vec!["stop-a".into()],
            )],
            situation: Some(LedgerSituation {
                state: WakeState::Blocked,
                status: Some("need a decision".into()),
                open_stops: vec!["stop-a".into()],
                seq: 100,
                at: 42,
            }),
            updated_at: 42,
        };
        let round: AgentLoopState =
            serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(round, full);
        assert!(round.resume_unconfirmed, "the fallback gate round-trips");
        assert_eq!(round.stale_plan_streak, 4);
        assert_eq!(round.marker_less_rechecks, 2);
        assert_eq!(round.last_plan.as_deref(), Some("wire the alert"));
        assert_eq!(round.last_marker_seq, 100);
        assert_eq!(round.advice_inflight.as_ref().unwrap().seq, 4);
        assert_eq!(round.events.len(), 2);
        assert_eq!(round.events[0].count, 3);
    }

    #[test]
    fn parked_advice_defaults_to_marker_delivery_and_round_trips_a_dialog_target() {
        let old: ParkedAdvice = serde_json::from_str(r#"{"seq":3,"stop_ids":["stop-a"]}"#).unwrap();
        assert!(!old.pane_dialog, "legacy records are marker debts");

        let dialog = ParkedAdvice {
            seq: 4,
            stop_ids: Vec::new(),
            pane_dialog: true,
        };
        let json = serde_json::to_string(&dialog).unwrap();
        assert!(json.contains(r#""pane_dialog":true"#));
        assert_eq!(serde_json::from_str::<ParkedAdvice>(&json).unwrap(), dialog);
    }

    #[test]
    fn a_fresh_ledger_omits_the_decider_fields() {
        // skip_serializing_if keeps a fresh ledger loadable by an older daemon (the ledger
        // is deny_unknown_fields), and keeps an idle session's state.json unbloated.
        let json = serde_json::to_string(&AgentLoopState::fresh(Engine::Claude, None, 1)).unwrap();
        assert!(!json.contains("digest"), "{json}");
        assert!(!json.contains("decisions"), "{json}");
        assert!(!json.contains("situation"), "{json}");
    }

    #[test]
    fn a_bogus_decision_is_dropped_and_the_operational_ledger_loads() {
        // The decisions slice is machine-cosmetic: one unparseable entry (a kind a newer
        // pmd added, a hand-edit) must never take the operational ledger down with it.
        let mut st = AgentLoopState::fresh(Engine::Claude, Some(60), 1);
        st.last_marker_seq = 9;
        st.decisions.push(DecisionRecord::at(
            3,
            Some(7),
            DecisionKind::Escalated,
            Some("ship it?".into()),
            vec!["stop-1".into()],
        ));
        let mut v = serde_json::to_value(&st).unwrap();
        v.get_mut("decisions")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"at": 4, "kind": {"teleported": "mars"}}));
        let round: AgentLoopState = serde_json::from_str(&serde_json::to_string(&v).unwrap())
            .expect("the ledger still loads despite a bogus decision");
        assert_eq!(round.last_marker_seq, 9, "operational field intact");
        assert_eq!(round.decisions.len(), 1, "only the bad decision is dropped");
    }

    #[test]
    fn record_decision_coalesces_and_bounds_the_slice() {
        let mut st = AgentLoopState::fresh(Engine::Claude, None, 0);
        // A RUN of the SAME (kind, summary, stop_ids) is ONE entry: latest time, bumped count.
        for at in 1..=3 {
            st.record_decision(DecisionRecord::at(
                at,
                Some(at as u64),
                DecisionKind::AutoFlow,
                Some("approve dprint".into()),
                vec!["stop-a".into()],
            ));
        }
        assert_eq!(st.decisions.len(), 1);
        assert_eq!(st.decisions[0].count, 3);
        assert_eq!(st.decisions[0].at, 3, "coalescing advances the timestamp");
        // The PER-KIND counter bumps each call; `disposed` is NEVER bumped here (the seam owns it).
        assert_eq!(st.digest.auto_flow, 3);
        assert_eq!(
            st.digest.disposed, 0,
            "record_decision must not bump disposed"
        );
        // Divergent stop_ids create a NEW entry — never a coalesced row with stale ids (Missing-2).
        st.record_decision(DecisionRecord::at(
            4,
            Some(4),
            DecisionKind::AutoFlow,
            Some("approve dprint".into()),
            vec!["stop-b".into()],
        ));
        assert_eq!(st.decisions.len(), 2, "different stop_ids never coalesce");
        // Bounded: push > MAX DISTINCT entries → len == MAX, and the NEWEST survives the drain.
        let mut st2 = AgentLoopState::fresh(Engine::Claude, None, 0);
        let n = DECISIONS_SLICE_MAX as u64 + 5;
        for i in 0..n {
            st2.record_decision(DecisionRecord::at(
                i as Epoch,
                Some(i),
                DecisionKind::Working,
                Some(format!("s{i}")),
                Vec::new(),
            ));
        }
        assert_eq!(st2.decisions.len(), DECISIONS_SLICE_MAX);
        assert_eq!(
            st2.decisions.last().unwrap().summary.as_deref(),
            Some(format!("s{}", n - 1).as_str()),
            "the newest decision survives the bound"
        );
    }

    #[test]
    fn record_event_coalesces_a_run_and_bounds_the_ring() {
        let mut st = AgentLoopState::fresh(Engine::Claude, None, 0);
        // A RUN of the same decision is ONE entry: latest time, bumped count — this is what
        // keeps a heartbeat held on the same reason across many rechecks from flooding it.
        st.record_event(1, AutopilotEventKind::Held(HoldReason::Busy));
        st.record_event(2, AutopilotEventKind::Held(HoldReason::Busy));
        st.record_event(3, AutopilotEventKind::Held(HoldReason::Busy));
        assert_eq!(st.events.len(), 1);
        assert_eq!(st.events[0].count, 3);
        assert_eq!(st.events[0].at, 3, "coalescing advances the timestamp");
        // A different decision starts a new entry…
        st.record_event(4, AutopilotEventKind::Nudged);
        assert_eq!(st.events.len(), 2);
        assert_eq!(st.events[1].count, 1);
        // …and a different HOLD REASON is a different decision, not a Busy coalesce.
        st.record_event(5, AutopilotEventKind::Held(HoldReason::AwaitingReport));
        assert_eq!(st.events.len(), 3);
        // Bounded: past the cap the OLDEST are dropped and the newest kept.
        for i in 0..AUTOPILOT_EVENTS_MAX as i64 {
            st.record_event(100 + i, AutopilotEventKind::Reported(Some(format!("s{i}"))));
        }
        assert_eq!(st.events.len(), AUTOPILOT_EVENTS_MAX);
        assert!(
            matches!(
                st.events.last().unwrap().kind,
                AutopilotEventKind::Reported(_)
            ),
            "the newest decision survives the bound"
        );
    }

    #[test]
    fn a_bogus_event_is_dropped_and_the_operational_ledger_still_loads() {
        // The feed is COSMETIC: one unparseable event (a kind a newer pmd added, a hand-edit)
        // must never take the operational ledger — run, cadence, watermarks — down with it.
        let mut st = AgentLoopState::fresh(Engine::Claude, Some(60), 1);
        st.cadence_pinned = true;
        st.last_marker_seq = 9;
        st.run = JobRun::Monitoring { until: 1800 };
        st.record_event(3, AutopilotEventKind::Reported(Some("indexing".into())));
        // Serialize, then inject a bogus event kind into the array on disk.
        let mut v = serde_json::to_value(&st).unwrap();
        v.get_mut("events")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"at": 4, "count": 1, "kind": {"teleported": "to mars"}}));
        let round: AgentLoopState = serde_json::from_str(&serde_json::to_string(&v).unwrap())
            .expect("the ledger still loads despite a bogus event");
        // Operational fields intact.
        assert_eq!(round.cadence_s, Some(60));
        assert!(round.cadence_pinned);
        assert_eq!(round.last_marker_seq, 9);
        assert_eq!(round.run, JobRun::Monitoring { until: 1800 });
        // The bogus event is dropped; the valid one survives.
        assert_eq!(round.events.len(), 1, "only the bad event is dropped");
        assert!(
            matches!(&round.events[0].kind, AutopilotEventKind::Reported(Some(s)) if s == "indexing")
        );
    }

    #[test]
    fn record_event_caps_a_paragraph_detail_to_one_bounded_line() {
        let mut st = AgentLoopState::fresh(Engine::Claude, None, 0);
        // Longer than the cap by construction, so this exercises the clip whatever the constant is.
        let huge = format!(
            "line one\nline two {}",
            "x".repeat(AUTOPILOT_DETAIL_MAX + 500)
        );
        st.record_event(1, AutopilotEventKind::Reported(Some(huge)));
        let d = st.events[0].kind.detail().unwrap();
        assert!(!d.contains('\n'), "newlines are collapsed: {d:?}");
        assert!(
            d.chars().count() <= AUTOPILOT_DETAIL_MAX,
            "detail is length-capped ({} chars)",
            d.chars().count()
        );
        assert!(
            d.ends_with('\u{2026}'),
            "a clipped detail admits it with an ellipsis: {d:?}"
        );
        // A realistic one-liner (well under the raised cap) is kept in FULL — no truncation.
        let mut st2 = AgentLoopState::fresh(Engine::Claude, None, 0);
        let full =
            "reindexed 42 files and re-ran the suite twice; all green, ready for the next nudge";
        st2.record_event(1, AutopilotEventKind::Reported(Some(full.into())));
        assert_eq!(
            st2.events[0].kind.detail().as_deref(),
            Some(full),
            "a normal status is not clipped"
        );
    }

    #[test]
    fn an_absent_parked_consult_is_not_even_serialized() {
        // `skip_serializing_if` keeps a ledger written by THIS daemon loadable by an older
        // one on the overwhelming majority of ticks (no consult in flight), which matters
        // because `AgentLoopState` is `deny_unknown_fields`.
        let json = serde_json::to_string(&AgentLoopState::fresh(Engine::Claude, None, 1)).unwrap();
        assert!(!json.contains("advice_inflight"), "{json}");
    }

    #[test]
    fn job_run_variants_round_trip() {
        for r in [
            JobRun::Idle,
            JobRun::Running {
                seq: 3,
                session: "pmj-x-abcd1234-3".into(),
                deadline: 999,
            },
            JobRun::Monitoring { until: 1234 },
            JobRun::Blocked {
                stop_ids: vec!["stop-1".into()],
                since: 77,
            },
        ] {
            let round: JobRun = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
            assert_eq!(round, r);
        }
    }

    #[test]
    fn retime_resets_the_plan_staleness_streak() {
        // A human cadence edit is a fresh start for the stall signal (LOCKED D-10) — a
        // fail-safe direction (it can only ever delay a plan-stall escalation).
        let mut st = AgentLoopState::fresh(Engine::Claude, Some(300), 0);
        st.run = JobRun::Monitoring { until: 1000 };
        st.stale_plan_streak = 5;
        st.retime(60, 100);
        assert_eq!(
            st.stale_plan_streak, 0,
            "retime resets the plan-staleness streak"
        );
    }

    #[test]
    fn fresh_is_idle_with_no_conversation() {
        let st = AgentLoopState::fresh(Engine::Claude, Some(300), 5);
        assert_eq!(st.run, JobRun::Idle);
        assert!(st.conversation_id.is_none());
        assert_eq!(st.cadence_s, Some(300));
        assert_eq!(st.created_at, 5);
        assert_eq!(st.updated_at, 5);
    }

    #[test]
    fn wake_report_parses_each_state_and_minimal() {
        // Minimal working report — just the state. A pre-Slice-2 report has no `seq`;
        // `#[serde(default)]` loads it as 0.
        let w: WakeReport = serde_json::from_str(r#"{ "state": "working" }"#).unwrap();
        assert_eq!(w.state, WakeState::Working);
        assert!(w.stops.is_empty() && w.status.is_none() && w.next_check_s.is_none());
        assert_eq!(w.seq, 0);

        // Monitoring with a self-scheduled nap.
        let m: WakeReport =
            serde_json::from_str(r#"{ "state": "monitoring", "next_check_s": 900 }"#).unwrap();
        assert_eq!(m.state, WakeState::Monitoring);
        assert_eq!(m.next_check_s, Some(900));

        // Blocked, carrying a monotonic seq + a stop draft + a captured conversation id.
        let b: WakeReport = serde_json::from_str(
            r#"{ "state": "blocked",
                 "seq": 1786519000,
                 "status": "need a decision",
                 "conversation_id": "sess-abc",
                 "stops": [{ "kind": "ambiguity",
                             "effect": { "scope": "local", "reversibility": "reversible",
                                         "authority": "ordinary" },
                             "question": "which channel?",
                             "options": ["a","b"], "risk_class": "medium" }] }"#,
        )
        .unwrap();
        assert_eq!(b.state, WakeState::Blocked);
        assert_eq!(b.seq, 1_786_519_000);
        assert_eq!(b.conversation_id.as_deref(), Some("sess-abc"));
        assert_eq!(b.stops.len(), 1);
        assert_eq!(b.stops[0].kind, StopKind::Ambiguity);
        assert!(!b.stops[0].effect.requires_human());
        assert_eq!(b.stops[0].risk_class, RiskClass::Medium);
    }

    #[test]
    fn stop_effect_escalates_only_explicit_human_owned_axes() {
        use crate::worker::{EffectAuthority, EffectReversibility, EffectScope, StopEffect};

        let local = StopEffect {
            scope: EffectScope::Local,
            reversibility: EffectReversibility::Reversible,
            authority: EffectAuthority::Ordinary,
            unrecognized_metadata: false,
        };
        assert!(!local.requires_human());
        let partial: StopEffect = serde_json::from_str(r#"{"scope":"local"}"#).unwrap();
        assert_eq!(partial.scope, EffectScope::Local);
        assert_eq!(partial.reversibility, EffectReversibility::Unknown);
        assert_eq!(partial.authority, EffectAuthority::Unknown);
        assert!(!partial.requires_human());
        let future: StopEffect = serde_json::from_str(
            r#"{
                "scope":"cross_account",
                "reversibility":"compensatable",
                "authority":"delegated",
                "future_axis":"future_value"
            }"#,
        )
        .unwrap();
        assert_eq!(future.scope, EffectScope::Unknown);
        assert_eq!(future.reversibility, EffectReversibility::Unknown);
        assert_eq!(future.authority, EffectAuthority::Unknown);
        assert!(future.unrecognized_metadata);
        assert!(!future.requires_human());
        assert!(future.summary().contains("unrecognized_metadata=true"));
        assert!(
            serde_json::to_string(&future)
                .unwrap()
                .contains("\"unrecognized_metadata\":true")
        );
        let additive: StopEffect = serde_json::from_str(
            r#"{
                "scope":"local",
                "reversibility":"reversible",
                "authority":"ordinary",
                "new_safety_axis":"unsafe"
            }"#,
        )
        .unwrap();
        assert_eq!(additive.scope, EffectScope::Local);
        assert_eq!(additive.reversibility, EffectReversibility::Reversible);
        assert_eq!(additive.authority, EffectAuthority::Ordinary);
        assert!(additive.unrecognized_metadata);
        assert!(!additive.requires_human());
        assert!(
            !serde_json::to_string(&local)
                .unwrap()
                .contains("unrecognized_metadata")
        );

        for effect in [
            StopEffect {
                scope: EffectScope::External,
                ..local
            },
            StopEffect {
                reversibility: EffectReversibility::Irreversible,
                ..local
            },
            StopEffect {
                authority: EffectAuthority::Privileged,
                ..local
            },
        ] {
            let summary = effect.summary();
            assert!(effect.requires_human(), "{summary}");
        }
        for effect in [
            StopEffect {
                scope: EffectScope::Unknown,
                ..local
            },
            StopEffect {
                reversibility: EffectReversibility::Unknown,
                ..local
            },
            StopEffect {
                authority: EffectAuthority::Unknown,
                ..local
            },
        ] {
            let summary = effect.summary();
            assert!(
                !effect.requires_human(),
                "unknown is evidence for the decider, not a human-owned effect: {summary}"
            );
        }
    }

    #[test]
    fn persisted_advice_queue_is_bounded_on_load() {
        let draft = crate::worker::StopDraft {
            kind: StopKind::Ambiguity,
            effect: crate::worker::StopEffect::default(),
            question: "Which formatter?".into(),
            options: vec!["prettier".into(), "dprint".into()],
            context_ref: None,
            risk_class: RiskClass::Low,
        };
        let mut state = AgentLoopState::fresh(Engine::Claude, None, 1);
        state.advice_queue = (0..ADVICE_QUEUE_MAX + 2)
            .map(|index| QueuedAdvice {
                stop_id: format!("stop-{index}"),
                report_seq: 7,
                draft: draft.clone(),
            })
            .collect();

        let loaded: AgentLoopState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();

        assert_eq!(loaded.advice_queue.len(), ADVICE_QUEUE_MAX);
        assert_eq!(loaded.advice_queue[0].stop_id, "stop-0");
        assert_eq!(
            loaded.advice_queue.last().map(|item| item.stop_id.as_str()),
            Some("stop-7")
        );
    }

    #[test]
    fn wake_report_parses_next_step() {
        // A working bump that carries the agent's own next step.
        let r: WakeReport = serde_json::from_str(
            r#"{"state":"working","seq":7,"status":"tests running","next_step":"wire the alert if green"}"#,
        )
        .unwrap();
        assert_eq!(r.next_step.as_deref(), Some("wire the alert if green"));
        // A terse bump with no next_step still parses, defaulting to None.
        let terse: WakeReport = serde_json::from_str(r#"{"state":"working"}"#).unwrap();
        assert_eq!(terse.next_step, None);
    }

    #[test]
    fn old_json_missing_marker_fields_loads_with_defaults() {
        // A pre-Slice-2 ledger has no `last_marker_seq`, and a pre-Slice-2 report has
        // no `seq`; `#[serde(default)]` keeps both loadable at 0 (deny_unknown_fields
        // rejects EXTRA keys, not MISSING ones — the marker watermark starts fresh).
        let st: AgentLoopState =
            serde_json::from_str(r#"{ "created_at": 1, "engine": "claude", "updated_at": 1 }"#)
                .unwrap();
        assert_eq!(st.last_marker_seq, 0);
        assert_eq!(st.report_generation, 0);
        assert!(st.last_marker_revision.is_none());
        assert!(st.nudged_at_report_generation.is_none());
        let w: WakeReport = serde_json::from_str(r#"{ "state": "monitoring" }"#).unwrap();
        assert_eq!(w.seq, 0);
    }

    #[test]
    fn wake_report_rejects_unknown_fields() {
        // Strict parse: contract drift surfaces (the disposer then falls back to a
        // lenient "working" rather than silently trusting a malformed report).
        assert!(
            serde_json::from_str::<WakeReport>(r#"{ "state": "working", "surprise": 1 }"#).is_err()
        );
        let legacy: WakeReport = serde_json::from_str(
            r#"{ "state": "blocked", "stops": [
                { "kind": "ambiguity", "risk_class": "low" }
            ] }"#,
        )
        .unwrap();
        assert!(
            !legacy.stops[0].effect.requires_human(),
            "a legacy stop without effect metadata remains eligible for goal-aware review"
        );
    }
}
