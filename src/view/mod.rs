//! Read-only project view for the TUI. Derives a glanceable row from a
//! project's `.project-state/` files. The TUI polls this and writes files for
//! actions; it never talks to the daemon directly (files are the source of
//! truth), so it works whether or not `pmd` is running.
//!
//! This file owns the [`ProjectView`] shape itself; `agent_loop` derives it for a
//! `Mode::AgentLoop` session from its per-session ledger ([`ProjectView::read_agent_loop`]).

use crate::clock::Epoch;
use crate::job::{AutopilotEvent, DeciderRun, DecisionCounters, ParkedAdvice, QueuedAdvice};
use crate::registry::{Engine, Mode};
use crate::state::{self, Tier};

mod agent_loop;

/// A glanceable posture for the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    Fresh,
    Working,
    Monitoring,
    NeedsYou,
    Running,
    Stuck,
    Done,
}

impl Posture {
    pub fn label(self) -> &'static str {
        match self {
            Posture::Fresh => "fresh",
            Posture::Working => "working",
            Posture::Monitoring => "monitoring",
            Posture::NeedsYou => "needs you",
            Posture::Running => "running",
            Posture::Stuck => "stuck",
            Posture::Done => "done",
        }
    }

    // There was a `Posture::icon()` here with a SEVENTH glyph vocabulary (`⏸` for
    // needs-you, `✗` for stuck) and zero call sites anywhere in `src/` or `tests/`.
    // The glyphs the dashboard actually draws are the four-bucket partition in
    // `attention::bucket_glyph`, and a second unused table beside the real one is
    // exactly how two surfaces come to disagree. Deleted rather than kept "for later".

    /// Sort key: attention-needing first, in-flight next, terminal last.
    pub fn sort_rank(self) -> u8 {
        match self {
            Posture::NeedsYou | Posture::Stuck => 0,
            Posture::Running | Posture::Working => 1,
            Posture::Monitoring | Posture::Fresh => 2,
            Posture::Done => 3,
        }
    }

    pub fn needs_attention(self) -> bool {
        matches!(self, Posture::NeedsYou | Posture::Stuck)
    }
}

/// Everything the TUI shows for one project.
#[derive(Debug, Clone)]
pub struct ProjectView {
    pub id: String,
    /// Optional human-readable label from the registry. Runtime actions still use [`Self::id`].
    pub display_name: Option<String>,
    /// Human-owned task title or fallback intent shown by the Task view.
    pub work_summary: Option<String>,
    /// Project-directory basename shown as compact Task metadata.
    pub project_name: Option<String>,
    /// Source managed-session id when this row was created by `f`.
    pub forked_from: Option<String>,
    /// Whether this row is a fork that never captured its own conversation (set by pmtui
    /// `refresh`). Such a row refuses to start by any route, so only deleting it applies.
    pub incomplete_fork: bool,
    /// Spawn lineage: the raw id of the session whose agent requested this row.
    pub spawned_by: Option<String>,
    /// [`Self::spawned_by`] as the dashboard shows it: the parent row's display label, or the
    /// raw id once the parent row is gone (set by pmtui `refresh`).
    pub spawned_by_label: Option<String>,
    /// Whether the spawn broker still owes this disabled row its one launch
    /// ([`crate::registry::ProjectEntry::is_staged_spawn`], set by pmtui `refresh`). Such a row
    /// reads `starting…` where a paused row reads `paused`; it is never an
    /// [`Self::incomplete_fork`].
    pub spawn_staged: bool,
    /// Whether this row is a spawned JOB ([`crate::registry::ProjectEntry::is_job`], filled in by
    /// pmtui `refresh`). A job's pane is a machine-readable event stream, not prose, so the preview
    /// renders its `job.log` instead of capturing the pane — and its lifecycle keys (resume, restart,
    /// Message) are refused. Distinct from [`Self::spawned_by`], which a chat child carries too.
    pub job: bool,
    /// The commit a JOB left on its own branch, when it ran in a worktree and committed something.
    /// `Some` is what makes `a` (Apply) available: there is work to bring into the human's checkout.
    pub job_commit: Option<String>,
    /// The branch that commit is on, for the confirmation and the status line.
    pub job_branch: Option<String>,
    pub enabled: bool,
    pub tier: Option<Tier>,
    pub posture: Posture,
    pub next_action: String,
    pub step_id: u64,
    pub last_activity: Option<Epoch>,
    pub stops: Vec<state::Stop>,
    /// When the OLDEST currently-open stop was first posted (`OpenStop.first_posted`),
    /// or `None` when nothing is open.
    ///
    /// It has been on disk since day one and was rendered nowhere: `agent_loop_stops`
    /// maps `OpenStop` to `state::Stop`, which has no timestamp, so the fact dropped
    /// out. Nothing anywhere said a decision had been waiting three hours, and `!!`
    /// looked identical for one stop and for five.
    pub oldest_stop_since: Option<Epoch>,
    /// The run mode (from the registry, set by the caller) — always [`Mode::AgentLoop`].
    pub mode: Mode,
    /// Which CLI it runs.
    pub engine: Option<Engine>,
    /// Whether this row's OWN session tmux is currently alive (checked against tmux by the
    /// caller, not derived from files). For a `Tier::Standard` row it is the
    /// project terminal, set in pmtui `refresh`. `status_category` buckets on this
    /// field: live ⇒ running `●`, else idle `○`. Left `false` for Autopilot rows (they
    /// bucket on their pmd-maintained ledger posture).
    pub session_live: bool,
    /// Whether a human client is attached or in the short attach-intent window.
    /// pmd defers input while this is true and resumes automatically on detach.
    pub human_attached: bool,
    /// Whether this agent-loop session's agent is actively WORKING right now (`Some(true)`) or
    /// alive-but-idle at its prompt (`Some(false)`); `None` when it can't be told (⇒ the display
    /// treats a live row as running). It has two sources, one per tier, both meaning the same thing.
    /// DRIVEN (Autopilot, `run == Monitoring`): mid-turn vs finished-and-waiting-for-the-next-nudge,
    /// derived in [`ProjectView::read_agent_loop`] by comparing the live `turn_signal` file size to
    /// the ledger's `turn_count_at_nudge` baseline (M73); `None` when the turn-end hook isn't wired.
    /// STANDARD (human-driven): pmd never classifies its pane, so pmtui `refresh` does — it captures
    /// the live conversation pane and runs `classify_pane` behind the same two-observation
    /// `idle_fingerprint` stability gate the daemon uses, writing `Some(true)` for a busy/streaming
    /// pane and `Some(false)` only for a byte-stable idle prompt (M76). `status_category` reads it so
    /// an agent-loop row shows `●` while working and `○` while genuinely idle at its prompt — both tiers.
    pub agent_working: Option<bool>,
    /// The most recent autopilot decisions `pmd` recorded on this session's ledger — the
    /// AUTOPILOT feed the preview renders so a driven row shows WHAT the daemon did
    /// (nudged / held / reported / escalated / …), not only the agent's own pane. Copied
    /// verbatim from [`crate::job::AgentLoopState::events`] in [`ProjectView::read_agent_loop`];
    /// empty for every non-agent-loop row and for a session pmd has not touched yet.
    pub autopilot_events: Vec<AutopilotEvent>,
    /// Bounded pmd-owned heartbeat turns correlated to accepted worker reports.
    pub turn_trace: Vec<crate::job::TurnTrace>,
    /// Lifetime decision counters and structured decider audit, copied from the pmd-owned ledger.
    pub decision_digest: DecisionCounters,
    pub advice_inflight: Option<ParkedAdvice>,
    /// Whether the deterministic `pmsup-*` terminal for [`Self::advice_inflight`] is live.
    /// Durable consult debt alone means pending recovery, not that a decider is still running.
    pub decider_live: bool,
    pub advice_queue: Vec<QueuedAdvice>,
    pub decider_runs: Vec<DeciderRun>,
    /// Configured decider selection. `None` only when config is absent or unreadable.
    pub decider_engine: Option<Engine>,
    pub decider_model: Option<String>,
}

impl ProjectView {
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.id)
    }
}

/// Cycle a tier for the TUI "change tier" control. The dial is a two-level
/// toggle: `Standard` (collaborative) ↔ `Autopilot` (hands-off).
pub fn next_tier(t: Tier) -> Tier {
    match t {
        Tier::Autopilot => Tier::Standard,
        Tier::Standard => Tier::Autopilot,
    }
}

/// Reverse of [`next_tier`] (for a left/right control). With only two levels
/// this is symmetric with [`next_tier`].
pub fn prev_tier(t: Tier) -> Tier {
    match t {
        Tier::Autopilot => Tier::Standard,
        Tier::Standard => Tier::Autopilot,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "tests/codex_monitoring.rs"]
mod codex_monitoring_tests;
