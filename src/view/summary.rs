//! The one READABLE projection of a session, for everything outside the dashboard's own frame.
//!
//! WHY A PROJECTION AND NOT `ProjectView`: that struct is the TUI's working set — 34 fields
//! including `step_id`, `turn_trace`, `decider_runs`, the parked-advice queue and `stops` with
//! their internals. Serializing it would make the dashboard's internal model a public contract
//! that can never change, and would hand a peer agent handles to detached work. So this is a
//! deliberately small read of it.
//!
//! WHY ONE TYPE AND ONE CONSTRUCTOR: the alternative is already shipped and already wrong.
//! `message --list` built its own `Peer` by hand and took `display_name` RAW
//! (`message_cli.rs`), while the dashboard passes the same field through
//! `registry::normalize_display_name` — so two surfaces disagreed about the one state-ish field
//! that document carried. Every field added by hand to a second surface is another of those.
//! [`SessionSummary::of`] is therefore the only way to make one.
//!
//! WHAT `working` MEANS, precisely, because this is the field a caller will misread. Its source is
//! the agent's own turn-end signal FILE, written by the engine's hook — not a screen scrape — and
//! `ProjectView::read_agent_loop` forces it to `None` on a Standard row, where pmd never nudges
//! and "mid-turn toward the next nudge" has no meaning. The dashboard then REFINES it by merging a
//! pane observation over the hook's answer (`merge_activity`), using cross-tick state to confirm
//! idle. That refinement needs a live driver and two ticks, so a one-shot read cannot have it:
//! `None` here means NOT KNOWN, never "idle". A caller that treats `null` as idle will report a
//! working agent as finished, which is the oldest bug in this product.

use serde::{Deserialize, Serialize};

use crate::clock::Epoch;
use crate::registry::{Engine, Mode, ProjectEntry};
use crate::state::Tier;
use crate::view::ProjectView;

/// What one session looks like from outside the dashboard process.
///
/// Field order is the order a reader wants it: who it is, what it is, how it is, and whether
/// anyone is there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// THE ADDRESS. Unique by construction, and what every command takes.
    pub id: String,
    /// The label a human gave it, NORMALIZED the way the dashboard shows it — a name it rejects
    /// is absent here rather than passed through raw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The project directory. Already public through `message --list`, so not newly exposed.
    pub root: String,
    /// `claude`, `codex`, or `default` when the row names none.
    pub engine: String,
    /// A one-shot headless child that reports a result and exits. Nothing reads a message to one.
    pub job: bool,
    /// The session whose agent asked for this row, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawned_by: Option<String>,

    /// Whether anything drives it: `false` is paused.
    pub enabled: bool,
    /// The autonomy dial, absent when no per-session config has been written yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    pub mode: Mode,

    /// The glanceable posture, as the dashboard words it: `needs you`, `working`, `monitoring`,
    /// `running`, `stuck`, `done`, `fresh`.
    pub state: String,
    /// Whether a human decision is open. The ONE field a coordinator should branch on.
    pub needs_you: bool,
    /// The dashboard's short hint for what happens next. Prose for a human, never a contract.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub next_action: String,
    /// How long the OLDEST open decision has been waiting. Absent when nothing is waiting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_s: Option<i64>,

    /// Mid-turn, per the agent's own turn-end signal file. `null` is NOT KNOWN — see the module
    /// doc. Never read it as idle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working: Option<bool>,
    /// Whether a live pane confirmed `working`. False from a one-shot read, which has no driver
    /// and no cross-tick state to confirm idle with.
    pub working_confirmed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<Epoch>,

    /// Its agent's pane is up, so a message could land.
    pub live: bool,
    /// A human is in that pane, so nothing else may type into it.
    pub attached: bool,
}

impl SessionSummary {
    /// Project one row. `confirmed` says whether `view.agent_working` carries a pane confirmation
    /// on top of the hook's file — true only from the dashboard's own refresh.
    ///
    /// Takes the registry entry as well as the view because identity lives on the row and
    /// `read_agent_loop` does not fill it: a caller outside the dashboard has both.
    pub fn of(entry: &ProjectEntry, view: &ProjectView, now: Epoch, confirmed: bool) -> Self {
        Self {
            id: entry.id.clone(),
            // The DRIFT FIX. `message --list` took this raw; the dashboard normalizes it.
            name: crate::registry::normalize_display_name(
                entry.display_name.as_deref().unwrap_or_default(),
            )
            .ok()
            .flatten(),
            root: entry.root.to_string_lossy().into_owned(),
            engine: entry.engine.map_or("default", Engine::label).to_string(),
            job: entry.is_job(),
            spawned_by: entry.spawned_by.clone(),
            enabled: entry.enabled,
            tier: view.tier,
            mode: entry.mode,
            state: view.posture.label().to_string(),
            needs_you: view.posture.needs_attention(),
            next_action: view.next_action.clone(),
            // Clamped at zero: a clock that moved backwards must not report a negative wait.
            waiting_s: view
                .oldest_stop_since
                .map(|since| (now - since).max(0))
                .filter(|_| view.posture.needs_attention()),
            working: view.agent_working,
            working_confirmed: confirmed && view.agent_working.is_some(),
            last_activity: view.last_activity,
            live: view.session_live,
            attached: view.human_attached,
        }
    }
}
