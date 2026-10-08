//! What pmd reads about one registry row before it drives that row: where the
//! row's state lives on disk, the autonomy tier recorded there — and, from the mode
//! plus that tier, whether pmd drives the row at all.
//!
//! One unit because all three are pure reads of a [`ProjectEntry`] plus the disk:
//! each answers one question and changes nothing, which is what keeps the
//! drive-or-skip decision directly testable apart from the sweep that asks it.

use crate::registry::{Mode, ProjectEntry};
use crate::state::{self, Config, ProjectPaths, Tier};

/// Whether `pmd` should DRIVE this row at all this sweep — the TIER half of the
/// enablement decision (the other halves being `enabled`/`poisoned`). Kept PURE
/// (mode + the tier on disk in, a bool out) so the decision is directly testable, and
/// so a later milestone can change what "autopilot ON" *does* — a nudge today, an LLM
/// supervisor later — without touching this gate.
///
/// **DRIVING IS OPT-IN: only an explicit `Autopilot` drives.** A missing or unreadable
/// config (`None`) therefore means DON'T.
///
/// This polarity was the other way round, and it was a bug the user hit: `None ⇒ drive`
/// meant any `config.json` this exact schema could not parse — a missing field, a foreign
/// schema, an empty object — made pmd keep typing into a session the human had switched to
/// Standard. `App::cycle_tier` refuses to write in that same state ("config unreadable —
/// tier unchanged"), so no key could stop it either. That is what *"why my session with
/// autopilot off receive the prompt"* was.
///
/// Silence is the one real cost of the new direction, and [`Daemon`](super::Daemon)'s
/// reconcile pays it off by warning once per id when it skips a row whose tier it could
/// not read at all — a broken session must not become an invisible one.
pub fn pmd_drives_row(mode: Mode, tier: Option<Tier>) -> bool {
    match mode {
        Mode::AgentLoop => tier == Some(Tier::Autopilot),
    }
}

/// The autonomy tier recorded on disk for `p`, read through the SAME path pmtui's `m`
/// (`cycle_tier`) WRITES and `JobScheduler` reads — [`entry_paths`], i.e. per-session.
/// `None` when the config is absent or unreadable (see [`pmd_drives_row`] for why that
/// now means "do NOT drive").
///
/// `None` is far rarer than it used to be: `Config::autonomy` is defaulted, so a config that
/// PARSES always has a tier. Only a missing file or malformed JSON reaches `None`.
///
/// Re-read on EVERY sweep on purpose, mirroring `JobScheduler::tick`'s own per-tick
/// config read: a `m` flip must take effect on the next sweep with no daemon restart.
pub(super) fn entry_tier(p: &ProjectEntry) -> Option<Tier> {
    state::read_json::<Config>(&entry_paths(p).config())
        .ok()
        .map(|c| c.autonomy)
}

/// The state paths a runner drives through. An agent-loop session re-bases under
/// `.project-state/sessions/<id>/` (so two sessions can share a folder, each with
/// its own ledger, driver.json and lease).
pub(super) fn entry_paths(p: &ProjectEntry) -> ProjectPaths {
    ProjectPaths::for_session(&p.root, &p.id)
}

/// Whether this registry row still requires a resident pmd process.
///
/// `tier` is the current sweep's single config read. `last_known_tier` is used only when that read
/// fails: a row previously observed on Autopilot remains a pmd responsibility while its config
/// recovers if its runner is already waiting for a human, but the drive path still receives `None`
/// and therefore cannot type. A known Standard tier remains off, and a new row with no trustworthy
/// tier fails closed.
pub(super) fn entry_needs_pmd(
    p: &ProjectEntry,
    tier: Option<Tier>,
    last_known_tier: Option<Tier>,
    waiting_for_human: bool,
) -> bool {
    p.enabled
        && (pmd_drives_row(p.mode, tier)
            || (tier.is_none() && waiting_for_human && pmd_drives_row(p.mode, last_known_tier)))
}
