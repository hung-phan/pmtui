//! Telling the human a session NEWLY needs them, through the terminal they already have
//! open.
//!
//! The dashboard is the only process in a position to do this. `pmd` can notify — it owns
//! the severity ladder and every transport in `agent_manager::escalation` — but a `pmd`
//! this dashboard started has its stderr redirected to `pmd.log` (an inherited terminal fd
//! would let its `eprintln!` corrupt the live frame), and the terminal transport must write
//! to a real tty. MEASURED against cmux: the OSC 777 bytes are consumed when the emitting
//! process's own terminal is the surface's tab, which is exactly where pmtui runs, and are
//! swallowed when a tmux server sits in between — so an agent pane cannot emit it either,
//! with or without `allow-passthrough`.
//!
//! EDGE-TRIGGERED, and seeded silently. A notification is worth having only for what is
//! new: `refresh` runs several times a second, and the first `refresh` is the one inside
//! `App::new`, so every row that already needs the human when the dashboard starts is
//! recorded without a sound. The human is looking at the list at that moment anyway.

use super::*;
use agent_manager::escalation::{Escalation, Notifier, OscNotifier, Severity};
use std::collections::HashSet;

/// The notifier a dashboard uses in production: the terminal transport, with its severity
/// gate open. The gate is `pmd`'s way of being less noisy on a cadence; here the edge
/// below already bounds the volume to one notification per transition, and a reversible
/// question parked for two days is the case this exists for.
pub(crate) fn terminal_notifier() -> Box<dyn Notifier> {
    Box::new(OscNotifier::new(Severity::Info))
}

/// The escalations for rows that crossed INTO needs-you since the last look, advancing
/// `seen` to the current set. `None` means "not seeded yet" and returns nothing.
///
/// Pure, so the edge is tested without a terminal: the send itself is one write that the
/// transport already gates on `is_terminal`.
pub(crate) fn newly_needy(
    views: &[ProjectView],
    seen: &mut Option<HashSet<String>>,
) -> Vec<Escalation> {
    // A PAUSED row is not loud on screen (`rows::project_row_line`), so it is not loud off
    // screen either — the human parked it. `Stuck` is excluded on purpose: it is
    // `needs_attention()` too, but it can flap, and one notification per flap trains the
    // human to ignore all of them.
    let needy: Vec<&ProjectView> = views
        .iter()
        .filter(|v| v.enabled && v.posture == Posture::NeedsYou)
        .collect();
    let now: HashSet<String> = needy.iter().map(|v| v.id.clone()).collect();
    let before = seen.replace(now);
    let Some(before) = before else {
        return Vec::new();
    };
    needy
        .iter()
        .filter(|v| !before.contains(&v.id))
        .map(|v| {
            if v.stops.is_empty() {
                // A needs-you with NO stop is the autopilot-off marker gate (see
                // `view::agent_loop`): the worker reported blocked, and nothing was made
                // answerable because no `pmd` would deliver the answer. The row's own next
                // action is everything the dashboard knows, so it is what we say.
                Escalation {
                    project: v.label().to_string(),
                    title: "needs you".to_string(),
                    body: v.next_action.clone(),
                    severity: Severity::Warn,
                }
            } else {
                Escalation::for_stops(v.label(), &v.stops)
            }
        })
        .collect()
}

impl App {
    /// Notify for every row that newly needs the human. Called at the end of `refresh`, so
    /// it sees exactly the rows the next frame will draw.
    pub(crate) fn notify_newly_needy(&mut self) {
        for e in newly_needy(&self.projects, &mut self.needy_notified) {
            // Best-effort like every other transport: a notification that did not land must
            // never fail the refresh that found it.
            let _ = self.notifier.notify(&e);
        }
    }
}
