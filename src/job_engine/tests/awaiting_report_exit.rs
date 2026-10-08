//! HOW THE AWAITING-REPORT HOLD ENDS when the turn-end hook never fires.
//!
//! `awaiting_report()` is the brake that stops pmd typing into an agent that owes a report
//! (user: *"When the session is chatting or working on sth, and not idle, i don't want autopilot to
//! kick in and queue a prompt"*). Something has to END that hold, or a session that finishes a turn
//! without writing its marker is never driven again.
//!
//! Measured on a live codex session: `turn-complete` stopped growing at 01:45:03 while turns kept
//! completing (a report was accepted at 02:38) even though `-c notify=[…]` was correctly in its argv.
//! `turn_finished_since_nudge()` was therefore false forever, the bounded `marker_less_recheck` exit
//! could never run, and the session held here for 3033 consecutive seconds — 425 adjacent `held`
//! events — freed only when its pane died and was relaunched. Nothing was ever said to the human.
//!
//! THE HOLD ITSELF IS CORRECT and stays: pixels cannot discharge a report debt, because a long silent
//! tool call is byte-stable AND draws a bare composer. What was missing is a BOUND. These pin that the
//! hold reaches a human within the report-debt ceiling, and that nothing the pane paints can defer it.

use super::*;

use super::super::drive::{MARKER_LESS_RECHECK_MAX, REPORT_DEBT_CEILING_S};

/// One confirmed nudge on an idle pane, leaving a report debt and NO turn signal — the exact shape
/// of the live wedge. The clock sits at the nudging instant.
fn nudged_with_no_turn_hook(fx: &mut Fx, sess: &str) {
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch → Monitoring{grace}
    fx.clock.set(START + LAUNCH_GRACE_S);
    fx.driver.set_tail(sess, IDLE_PANE);
    tick_confirmed(fx);
    assert_eq!(fx.driver.sent_keys().len(), 1, "the baseline nudge landed");
    assert!(ledger(fx).awaiting_report(), "and left a report debt");
    assert!(
        !fx.paths.turn_signal().exists(),
        "no turn-end hook is wired, which is the whole point"
    );
}

/// Run `ticks` DUE rechecks on a BYTE-STABLE idle pane, advancing the clock a recheck each time.
/// Returns the last tick's outcome.
fn hold_for(fx: &mut Fx, ticks: u32) -> JobTick {
    let mut last = JobTick::WaitingForIntake;
    for _ in 0..ticks {
        fx.clock.advance(BUSY_RECHECK_S);
        last = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }
    last
}

/// THE WEDGE, BOUNDED. With no turn-end hook the hold is right to persist — the agent may be mid
/// tool call — but it must not persist in silence. Past the report-debt ceiling the human is told, which
/// is the outcome the live session never reached in 3033s.
#[test]
fn a_hold_with_no_turn_hook_reaches_a_human_instead_of_lasting_forever() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);

    // The cadence park comes first, so the hold opens one cadence in; count from there to past
    // the report-debt ceiling.
    let ticks = ((300 + REPORT_DEBT_CEILING_S) / BUSY_RECHECK_S) as u32 + 4;
    let last = hold_for(&mut fx, ticks);

    assert!(
        matches!(last, JobTick::Escalated(_)),
        "a hold past the report-debt ceiling must reach a human, got {last:?}"
    );
    assert_eq!(
        fx.driver.sent_keys().len(),
        1,
        "and it reaches the human WITHOUT typing into a possibly-working agent"
    );
    let led = ledger(&fx);
    assert!(
        led.open_stops
            .iter()
            .any(|stop| stop.kind == StopKind::Stuck),
        "filed as a stall for a human to dismiss: {:?}",
        led.open_stops
    );
}

/// AND A PANE THAT KEEPS PRINTING CANNOT DEFER IT. `pane_progress_recheck` rebases the stall window
/// on every `progress_fingerprint` change, which is right for a BUSY agent — new transcript IS
/// progress — but it is the wrong reading of a report debt: nothing the agent prints discharges a
/// debt only a report can discharge.
///
/// This is the live wedge's own shape, and the reason it escaped the backstop: the codex pane kept
/// redrawing while it shut down, restarting the window faster than it could fill. Proven, not
/// guessed — the byte-stable hold above escalated even before this fix; only the churning one did not.
#[test]
fn a_churning_pane_cannot_hold_off_the_stall_escalation_forever() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);

    let ticks = ((300 + REPORT_DEBT_CEILING_S) / BUSY_RECHECK_S) as u32 + 4;
    let mut last = JobTick::WaitingForIntake;
    for i in 0..ticks {
        fx.clock.advance(BUSY_RECHECK_S);
        // A DIFFERENT transcript every tick, still ending at an idle prompt.
        fx.driver
            .set_tail(&sess, &format!("line {i} of output\n\u{276f} "));
        last = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }

    assert!(
        matches!(last, JobTick::Escalated(_)),
        "an unreported debt is not discharged by pane output, got {last:?}"
    );
}

/// THE HOOK'S OWN PATH IS UNCHANGED: when a turn provably ended without a marker, the bounded
/// recovery nudge still runs and still escalates at `MARKER_LESS_RECHECK_MAX`, so a session with a
/// working hook self-heals without ever reaching the report-debt ceiling.
#[test]
fn a_working_hook_still_self_heals_before_the_stall_threshold() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);

    let mut last = JobTick::WaitingForIntake;
    for turn in 1..=MARKER_LESS_RECHECK_MAX {
        // The hook fires: a turn ended, still with no marker.
        grow_turn_signal(&fx, turn as usize);
        fx.clock.advance(300 + BUSY_RECHECK_S);
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
        fx.clock.advance(BUSY_RECHECK_S);
        last = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }

    assert!(
        matches!(last, JobTick::Stuck(_)),
        "k marker-less finishes reach a human, got {last:?}"
    );
    assert!(
        ledger(&fx)
            .open_stops
            .iter()
            .any(|stop| stop.kind == StopKind::WorkerStuck),
        "and as the worker not reporting, not a generic stall"
    );
}

/// A LEDGER WRITTEN BEFORE THE CEILING EXISTED has no `nudged_at`, so there is no debt age to measure
/// and nothing is escalated from a value that was never recorded. It gains one at the next nudge.
#[test]
fn a_ledger_with_no_recorded_nudge_time_is_not_escalated() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);
    assert!(
        ledger(&fx).nudged_at.is_some(),
        "a delivered nudge records when it spoke"
    );

    // Blank it, as an older ledger would be, and hold well past the ceiling. The pane CHURNS, which
    // is what rules the pre-existing pane-stall backstop out and leaves only the ceiling under test.
    let mut led = ledger(&fx);
    led.nudged_at = None;
    job::save(&fx.paths, &led).unwrap();
    let ticks = ((300 + REPORT_DEBT_CEILING_S) / BUSY_RECHECK_S) as u32 + 4;
    let mut last = JobTick::WaitingForIntake;
    for i in 0..ticks {
        fx.clock.advance(BUSY_RECHECK_S);
        fx.driver
            .set_tail(&sess, &format!("line {i} of output\n\u{276f} "));
        last = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }

    assert!(
        matches!(last, JobTick::Monitoring { .. }),
        "no recorded nudge time ⇒ no ceiling to breach, got {last:?}"
    );
    assert!(
        ledger(&fx).nudged_at.is_none(),
        "and nothing invented one behind the scenes"
    );
}

/// EVERY PATH THAT OPENS A DEBT RECORDS WHEN IT DID. The ceiling reads one field, so a site that
/// created a debt without stamping it would be silently unbounded — which is exactly what reading the
/// heartbeat-only `turn_trace` would have left the dialog-answer and applied-verdict paths.
#[test]
fn the_debt_clock_is_stamped_beside_every_debt_watermark() {
    let sources = std::fs::read_to_string("src/job_engine/nudge.rs").unwrap()
        + &std::fs::read_to_string("src/job_engine/stops.rs").unwrap()
        + &std::fs::read_to_string("src/job_engine/dialog_advice.rs").unwrap();
    let debts = sources
        .matches("nudged_at_report_generation = Some(")
        .count();
    let stamps = sources.matches(".nudged_at = Some(now)").count();
    assert_eq!(
        debts, stamps,
        "{debts} sites open a report debt but only {stamps} stamp when it opened; an unstamped \
         debt has no ceiling"
    );
}

/// CODEX ENDS THE HOLD ON ITS OWN SCREEN, with no turn-end hook at all. Measured on a real codex
/// 0.160 pane: through a 25-second SILENT tool call it carried `esc to interrupt` in every capture 3s
/// apart and dropped it the instant the turn ended, so a confirmed-idle codex pane means the turn is
/// over. That is what makes this safe where the same gate is unsafe for claude — and necessary,
/// because codex's `notify` hook proved intermittent on a long-lived resumed session.
#[test]
fn a_confirmed_idle_codex_pane_ends_the_hold_without_a_turn_hook() {
    let mut fx = setup(Tier::Autopilot, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);

    // Past the cadence, then the two-capture confirmation gate.
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    fx.clock.advance(BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    let sent = fx.driver.sent_keys();
    assert_eq!(
        sent.len(),
        2,
        "a finished-but-unreported codex is asked to report, not held for 45 minutes"
    );
    assert!(
        sent.last()
            .unwrap()
            .1
            .contains("last turn ended without a decision marker"),
        "and the nudge names the miss: {:?}",
        sent.last()
    );
    assert_eq!(ledger(&fx).marker_less_rechecks, 1);
}

/// A BUSY CODEX PANE IS STILL NEVER TYPED INTO. The exemption is about an IDLE pane being believable,
/// not about ignoring the banner: while `esc to interrupt` is on screen the tick never reaches the
/// idle arm at all, so the agent is left alone exactly as before.
#[test]
fn a_busy_codex_pane_is_left_alone_while_it_owes_a_report() {
    let mut fx = setup(Tier::Autopilot, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);

    // The real chrome codex draws while a turn runs, composer and all.
    fx.driver.set_tail(
        &sess,
        "\u{2022} Running the command exactly as requested.\n\
         \u{2022} Working (7s \u{2022} esc to interrupt) \u{b7} 1 background terminal running\n\
         \u{203a} Ask Codex to do anything\n  GPT-5.6-Sol high \u{b7} Context 2% used\n",
    );
    for _ in 0..6 {
        fx.clock.advance(300 + BUSY_RECHECK_S);
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }

    assert_eq!(
        fx.driver.sent_keys().len(),
        1,
        "a working codex is not interrupted: {:?}",
        fx.driver.sent_keys()
    );
    assert_eq!(
        ledger(&fx).marker_less_rechecks,
        0,
        "and no marker-less recovery is counted against it"
    );
}

/// Make the ledger read like a LONG-LIVED session whose hook has never fired: many accepted reports,
/// no turn signal. Both generations move together so the report DEBT stays open — raising
/// `report_generation` alone would discharge it and the backstop would never be reached.
fn with_reports(fx: &Fx, reports: u64) {
    let mut led = ledger(fx);
    led.report_generation = reports;
    led.nudged_at_report_generation = Some(reports);
    job::save(&fx.paths, &led).unwrap();
    assert!(
        ledger(fx).awaiting_report(),
        "the debt must still be open or the ceiling is never reached"
    );
}

/// A synthesized stop's human-facing text — the `reason` the backstop passed to `park_stuck*`, which
/// doubles as the question because a stall stop has no draft behind it.
fn question(stop: &crate::pmstate::OpenStop) -> String {
    stop.question.clone().unwrap_or_default()
}

/// Hold past the report-debt ceiling and return the ledger's one open stop.
///
/// The pane CHURNS, ending at an idle prompt every tick. That is deliberate and load-bearing: on a
/// byte-stable pane the 1800s `DEFAULT_STALL_BUSY_S` window fills first and escalates its own
/// "busy with no progress" stall, so the 2700s ceiling is never the thing that fires. Churning
/// rebases that window faster than it can fill — which is exactly the live wedge's shape — leaving
/// the report-debt ceiling as the only backstop under test.
fn stop_at_ceiling(fx: &mut Fx, sess: &str, cadence_s: i64) -> crate::pmstate::OpenStop {
    let ticks = ((cadence_s + REPORT_DEBT_CEILING_S) / BUSY_RECHECK_S) as u32 + 4;
    for i in 0..ticks {
        fx.clock.advance(BUSY_RECHECK_S);
        fx.driver
            .set_tail(sess, &format!("line {i} of output\n\u{276f} "));
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }
    let led = ledger(fx);
    assert_eq!(led.open_stops.len(), 1, "one stop: {:?}", led.open_stops);
    led.open_stops[0].clone()
}

/// A DEAD HOOK NAMES ITSELF. The bound added above is correct but its message blames the agent — and
/// for a session whose turn-end hook never fires, the agent is not the one at fault: pmd cannot see
/// that any turn ended, so this recurs every single check-in for as long as the session lives. `pmd
/// doctor` has known this fault since the hook check landed, but a diagnostic a human has to think to
/// run never reaches the human reading "not reporting" on a dashboard.
#[test]
fn a_ceiling_reached_with_a_dead_turn_hook_blames_the_hook_not_the_agent() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);
    with_reports(&fx, 20);

    let stop = stop_at_ceiling(&mut fx, &sess, 300);

    assert_eq!(
        stop.kind,
        StopKind::Capability,
        "the harness cannot observe turn ends; that is not the agent being stuck"
    );
    let text = question(&stop);
    assert!(
        text.contains("turn-end hook has never fired across 20 reports"),
        "the stop must name the cause and its evidence: {text}"
    );
    assert!(
        text.contains("restart the session"),
        "and the remedy: {text}"
    );
    assert!(
        !text.contains("it may be stuck or not reporting"),
        "and must not also offer the wording it replaces: {text}"
    );
}

/// AN INTERMITTENT HOOK IS THE SAME FAULT, and the one actually measured in the wild: `turn-complete`
/// advanced once across roughly seven accepted reports on a live session while its argv was correct.
#[test]
fn a_ceiling_reached_with_an_intermittent_turn_hook_names_the_shortfall() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);
    // One turn signal against twenty reports — firing, but for almost nothing.
    grow_turn_signal(&fx, 1);
    with_reports(&fx, 20);

    let stop = stop_at_ceiling(&mut fx, &sess, 300);

    assert_eq!(stop.kind, StopKind::Capability);
    let text = question(&stop);
    assert!(
        text.contains("fired only 1 times across 20 reports"),
        "the shortfall is the evidence: {text}"
    );
}

/// A FRESH SESSION IS NOT DIAGNOSED. No signal file and no reports is exactly what a session that has
/// simply not finished a turn yet looks like, so the hook cannot be blamed on it — the generic bound
/// still applies, and still reaches the human. This is the pre-existing ceiling behaviour, pinned so
/// the new branch cannot quietly widen onto it.
#[test]
fn a_fresh_session_at_the_ceiling_keeps_the_generic_silence_wording() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);
    assert_eq!(ledger(&fx).report_generation, 0, "nothing reported yet");

    let stop = stop_at_ceiling(&mut fx, &sess, 300);

    assert_eq!(
        stop.kind,
        StopKind::Stuck,
        "an undiagnosable silence is still a stall, not a capability claim"
    );
    let text = question(&stop);
    assert!(
        text.contains("has not reported in"),
        "the generic wording is unchanged: {text}"
    );
    assert!(
        !text.contains("turn-end hook"),
        "and invents no fault it cannot prove: {text}"
    );
}

/// A HEALTHY HOOK IS NEVER BLAMED EITHER: when the signal keeps pace with the reports, silence past
/// the ceiling really is the agent's, and the message stays the generic one.
#[test]
fn a_healthy_hook_at_the_ceiling_keeps_the_generic_silence_wording() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);
    // Signal in step with the reports, so the hook is demonstrably alive.
    grow_turn_signal(&fx, 20);
    with_reports(&fx, 20);

    let stop = stop_at_ceiling(&mut fx, &sess, 300);

    assert!(
        !question(&stop).contains("turn-end hook"),
        "a hook that fires is not the cause of this silence: {}",
        question(&stop)
    );
}

/// CODEX IS NEVER DIAGNOSED THIS WAY. It does not depend on the hook — its own `esc to interrupt`
/// chrome ends the hold — so a codex session reaching the ceiling had a pane that never confirmed
/// idle. Blaming the hook there would point the human at the wrong thing entirely.
#[test]
fn a_codex_session_at_the_ceiling_is_never_blamed_on_the_turn_hook() {
    let mut fx = setup(Tier::Autopilot, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);
    with_reports(&fx, 20);

    // A pane that is BUSY (so the codex idle exemption never discharges the hold) and CHURNING (so the
    // 1800s stall window never fills), leaving the report-debt ceiling as the thing that fires — the
    // only way to reach the branch under test and watch the engine guard refuse it.
    let ticks = ((300 + REPORT_DEBT_CEILING_S) / BUSY_RECHECK_S) as u32 + 4;
    for i in 0..ticks {
        fx.clock.advance(BUSY_RECHECK_S);
        fx.driver.set_tail(
            &sess,
            &format!(
                "\u{2022} step {i}\n\u{2022} Working (7s \u{2022} esc to interrupt)\n\u{203a} Ask Codex to do anything\n"
            ),
        );
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }

    let led = ledger(&fx);
    assert!(
        led.open_stops
            .iter()
            .all(|stop| !question(stop).contains("turn-end hook")),
        "codex does not depend on the hook, so it cannot be the cause: {:?}",
        led.open_stops
    );
}

/// CLAUDE IS UNCHANGED, which is the other half of the contract. Its bare prompt is ambiguous mid-turn,
/// so with no hook it still holds rather than being typed into on a guess.
#[test]
fn a_confirmed_idle_claude_pane_still_holds_without_a_turn_hook() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_with_no_turn_hook(&mut fx, &sess);

    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    fx.clock.advance(BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert_eq!(
        fx.driver.sent_keys().len(),
        1,
        "claude's idle prompt is not proof a turn ended"
    );
    assert_eq!(ledger(&fx).marker_less_rechecks, 0);
}
