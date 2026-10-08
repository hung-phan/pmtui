//! The bounded-runtime budgets a human never has to remember: how many nudges and
//! how long a session may run unattended before the loop escalates instead of
//! driving on. Both defaults live here with the backstop that spends the wall-clock
//! one, because they are the same promise measured two ways and tuning either
//! without seeing the other is how one silently shortened the other. (The third
//! bound, the continuous-Busy stall, sits with the pane observation that enforces
//! it in [`super::drive`].)

use anyhow::Result;

use crate::clock::Epoch;
use crate::job::AgentLoopState;

use super::{JobScheduler, JobTick};

/// Default bounded-runtime budget: after this many wakes without a human closing or
/// touching the session, the loop ESCALATES ("still running — close?") rather than
/// re-invoking forever (design "borrowed budgets"). A human answer refreshes the
/// window; `0` disables the budget.
///
/// Raised from 200, which was the wrong leash for a dial the human controls. Two budgets guard
/// the same thing here — a session running unattended forever — and this one's length depended
/// on the CADENCE: 200 wakes is 16 hours at the 5-minute default but only 3h20m at the
/// 60-second floor. So turning the cadence up to watch a session closely also silently shortened
/// how long it was allowed to run, and the first thing a human learned about it was a page.
///
/// [`DEFAULT_MAX_WALL_CLOCK_S`] is the budget a human can actually reason about ("a day"), so it
/// is the one that should bind. 2000 keeps this above 24h of wakes at every cadence from the
/// [`crate::job_engine::CADENCE_MIN_S`] floor upward, leaving it as what it should always have
/// been: a backstop against a pathological wake storm, not the thing that ends a healthy
/// overnight run.
pub const DEFAULT_MAX_WAKES: u64 = 2000;
/// Default bounded wall-clock budget (24h): the wall-clock twin of [`DEFAULT_MAX_WAKES`].
/// Once a session's window (opened by the first tick that DRIVES the live session after a
/// reset — see [`JobScheduler::budget_backstop`]) exceeds this, the loop ESCALATES rather
/// than driving forever. A human answer refreshes the window; `0` disables the budget.
pub const DEFAULT_MAX_WALL_CLOCK_S: u64 = 86_400;

impl JobScheduler {
    /// The bounded WALL-CLOCK backstop, run on every tick that does real work on a live
    /// session (both the due [`JobScheduler::drive`] path and the
    /// [`JobScheduler::drive_marker_only`] fast-path). Two jobs:
    ///   1. **Open the window** on the FIRST such tick. It used to open only after a
    ///      SUCCESSFUL `send_keys`, which made the budget conditional on the very event it
    ///      exists to bound.
    ///   2. **Spend it:** once `now - window_start >= max_wall_clock_s`, ESCALATE via
    ///      [`JobScheduler::park_stuck`] (never terminate) — the human can answer "keep
    ///      going", which resets both budgets in [`JobScheduler::on_blocked`].
    ///
    /// Why it cannot live in `nudge`: a session that is driven but never NUDGED had no
    /// bound at all. An agent that keeps writing `state: "monitoring"` markers is disposed
    /// entirely on the marker paths, which return before any send — so the window never
    /// opened, this check never ran, and each valid marker bump additionally CLEARED
    /// `busy_since`, disarming the 30-minute stall backstop too. That session ran forever.
    ///
    /// Returns `Some(tick)` when the budget is spent (the caller returns it as-is), else
    /// `None` (carry on with the tick).
    ///
    /// `wakes` is deliberately NOT bumped here and its budget is deliberately still
    /// checked in [`JobScheduler::nudge`]: a wake IS a delivered nudge. Counting driven
    /// ticks instead would let the daemon's 500ms sweep burn [`DEFAULT_MAX_WAKES`] in under
    /// two minutes and escalate every healthy session.
    pub(super) fn budget_backstop(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
    ) -> Result<Option<JobTick>> {
        let start = *self.window_start.get_or_insert(now);
        if self.max_wall_clock_s > 0 && now - start >= self.max_wall_clock_s as i64 {
            let hours = self.max_wall_clock_s / 3600;
            let reason = format!(
                "agent-loop session still running after {hours} hours without being closed — \
                 close it, or answer to keep it going"
            );
            return Ok(Some(self.park_stuck(now, base, reason)?));
        }
        Ok(None)
    }
}
