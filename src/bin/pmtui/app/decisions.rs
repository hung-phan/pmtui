//! The PER-SESSION DECISION LANE (`v`): opening the selected session's decision log. The review
//! surface is [`crate::render_decisions`]; the per-session "seen" watermark lives on [`App`]
//! (`decisions_seen`) and RESETS each pmtui launch — a durable, cross-restart watermark is a
//! deliberate follow-up, kept off the single-writer ledger pmtui must never touch.

use crate::*;

impl App {
    /// Open the SELECTED session's decision log. Captures that session's previous "seen" instant
    /// into the mode (so the view can draw the "new since you last looked" divider) and advances
    /// its watermark to now. PER-SESSION on purpose — a decision only reads in its own session's
    /// context, so `v` never mixes the fleet's logs into one stream. Refuses with a status when
    /// nothing is selected.
    pub(crate) fn open_decisions(&mut self) {
        // Extract everything the mode needs while the immutable borrow is alive, so the watermark
        // write below is a clean, separate mutable borrow.
        let (id, n, queued, driven) = match self.selected_view() {
            Some(v) => (
                v.id.clone(),
                v.decider_runs.len()
                    + v.autopilot_events
                        .iter()
                        .filter(|event| event.kind.is_decision())
                        .count(),
                v.advice_queue.len(),
                agent_manager::daemon::pmd_drives_row(v.mode, v.tier),
            ),
            None => {
                self.status =
                    "nothing is selected — j/k pick a session, then v to review its audit".into();
                return;
            }
        };
        // A Standard row is human-driven, so pmd records no decisions for it — the lane would always
        // be empty. Refuse with the way forward instead (the keybar hides the chip there anyway).
        if !driven {
            self.status = format!(
                "{id} is on Standard — you drive it, so there are no autopilot decisions to review; m turns autopilot on"
            );
            return;
        }
        let since = self.decisions_seen.get(&id).copied();
        self.decisions_seen.insert(id.clone(), SystemClock.now());
        self.mode = UiMode::Decisions {
            tab: AuditTab::Decisions,
            scroll: 0,
            other_scroll: 0,
            since,
            id: id.clone(),
        };
        self.status = if n == 0 && queued == 0 {
            format!("{id}: no autopilot decisions yet")
        } else {
            format!(
                "reviewing {n} audit row{} and {queued} queued decision{} for {id}",
                if n == 1 { "" } else { "s" },
                if queued == 1 { "" } else { "s" }
            )
        };
    }
}
