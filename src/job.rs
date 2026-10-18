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
use crate::pmstate::OpenStop;
use crate::registry::Engine;
use crate::state::{self, ProjectPaths};
use crate::worker::StopDraft;

mod turn_trace;
pub use turn_trace::{
    TURN_TRACE_MAX, TurnDisposition, TurnNoReportReason, TurnOutcome, TurnReviewOutcome, TurnTrace,
    TurnTrigger,
};

// FOUR CONCERNS, FOUR FILES. This module held the run state, the decider's records, the autopilot
// event log and the situation projection in one 1,100-line file, so reading any one of them meant
// reading all four. Each type keeps its old path through these re-exports: `job::DeciderRun` still
// resolves, and no importer changed.
mod decider;
mod events;
mod situation;

pub use decider::{
    ADVICE_QUEUE_MAX, DECIDER_RUNS_MAX, DeciderOutcome, DeciderPolicy, DeciderRun, DeciderTarget,
    ParkedAdvice, QueuedAdvice,
};
pub use events::{
    AUTOPILOT_EVENTS_MAX, AutopilotEvent, AutopilotEventKind, DECISIONS_SLICE_MAX,
    DecisionCounters, DecisionKind, DecisionRecord,
};
pub use situation::LedgerSituation;

// `use`d rather than named as `events::…` in each `#[serde(…)]` attribute, so the attribute strings
// on `AgentLoopState` are the bytes they always were.
use decider::{
    cap_audit_option, cap_audit_text, cap_decider_outcome, de_advice_queue_bounded,
    de_decider_runs_lenient,
};
use events::{cap_detail, de_decisions_lenient, de_events_lenient};

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

fn bool_is_false(value: &bool) -> bool {
    !*value
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
mod tests;
