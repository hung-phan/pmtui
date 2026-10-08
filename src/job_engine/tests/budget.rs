//! The bounded-runtime budgets: a session escalates rather than nudging or driving
//! forever, and a human answer refreshes the window.

use super::*;

// --- bounded budgets -----------------------------------------------------

#[test]
fn nudge_budget_escalates_after_max_wakes_then_answer_refreshes() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(50));
    fx.sched.max_wakes = 2;
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch → Monitoring{START+8}
    fx.driver.set_tail(&sess, IDLE_PANE);
    // Two nudges on the cadence (each confirmed by two consecutive Idle observations,
    // so each lands BUSY_RECHECK_S after its due tick).
    fx.clock.set(START + LAUNCH_GRACE_S);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    // The agent ANSWERS the first nudge before the second one is allowed (m38): without a
    // report the harness treats it as still working and re-checks instead of typing.
    report_progress(&fx, 101, 40);
    fx.clock.set(START + LAUNCH_GRACE_S + BUSY_RECHECK_S + 50);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.driver.sent_keys().len(), 2);
    // The third due tick trips the budget → escalate, not a third nudge. It has to REACH the
    // budget check, which lives in `nudge`, so the agent reports again here too.
    report_progress(&fx, 102, 30);
    fx.clock
        .set(START + LAUNCH_GRACE_S + 2 * BUSY_RECHECK_S + 100);
    assert!(matches!(tick_confirmed(&mut fx), JobTick::Stuck(_)));
    assert_eq!(fx.driver.sent_keys().len(), 2, "budget stops the nudges");
    assert!(matches!(fx.sched.run, JobRun::Blocked { .. }));
    // A human answer refreshes the window → nudges resume.
    let stop_id = ledger(&fx).open_stops[0].id.clone();
    let at = fx.clock.now() + 1;
    state::append_answer(
        &fx.paths,
        &Answer {
            stop_id,
            answer: "keep going".into(),
            note: None,
            answered_by: "user".into(),
            answered_at: at,
        },
    )
    .unwrap();
    fx.clock.set(at);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(
        fx.driver.sent_keys().len(),
        3,
        "answer refreshes the budget → nudges again"
    );
}

#[test]
fn wall_clock_budget_escalates_once_the_window_exceeds_the_bound() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(50));
    fx.sched.max_wall_clock_s = 100;
    let sess = loop_session(&fx);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch → Monitoring{START+8}
    // The window opens on the FIRST tick that DRIVES the live session, not on the first
    // successful send it used to wait for (m18/2) — so the bound covers the whole time
    // the harness has been driving, including ticks that deliver nothing.
    let window_start = START + LAUNCH_GRACE_S;
    fx.clock.set(window_start);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.sched.window_start, Some(window_start));
    // Still inside the 100s window ⇒ the next due tick (cadence 50) nudges. The first
    // nudge landed at window_start+5 (the confirming observation), so the cadence is
    // due at window_start+55. The agent reports first, or m38's awaiting-report gate
    // would (correctly) treat it as still working and re-check instead of nudging.
    report_progress(&fx, 101, 40);
    fx.clock.set(window_start + BUSY_RECHECK_S + 50);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    let nudged_at = fx.clock.now();
    assert!(nudged_at - window_start < 100, "still inside the window");
    // The following due tick lands past the bound ⇒ escalate instead of nudging. Note
    // this is now a PLAIN tick, not `tick_confirmed`: the budget is spent before the
    // confirmation gate is consulted, so the escalation no longer waits for a second
    // Idle observation to authorise a nudge that would be refused anyway.
    fx.clock.set(nudged_at + 50);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Stuck(_)
    ));
    assert_eq!(fx.driver.sent_keys().len(), 2, "budget stops the loop");
}

#[test]
fn spent_wall_clock_budget_propagates_ledger_write_failure() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(50));
    fx.sched.max_wall_clock_s = 1;
    fx.sched.window_start = Some(START);
    let base = ledger(&fx);

    std::fs::remove_dir_all(fx.paths.state_dir()).unwrap();
    std::fs::write(fx.paths.state_dir(), "not a directory").unwrap();

    let error = fx
        .sched
        .budget_backstop(START + 1, &base)
        .expect_err("a spent budget must report a ledger persistence failure");
    assert!(
        format!("{error:#}").contains(".project-state"),
        "the error should identify the ledger write: {error:#}"
    );
}

#[test]
fn chat_defer_does_not_age_the_wall_clock_budget() {
    // Deferring for an attached human must PAUSE the window: a long attach then
    // detach must not trip a spurious "stuck after Nh".
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(50));
    fx.sched.max_wall_clock_s = 100;
    let sess = loop_session(&fx);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch
    fx.clock.set(START + LAUNCH_GRACE_S);
    tick_confirmed(&mut fx); // first (confirmed) nudge opens the window
    assert_eq!(fx.driver.sent_keys().len(), 1);
    // Human attaches; advance WELL past the budget across due sweeps — each defers,
    // pausing the window (never Stuck).
    fx.driver.set_clients(&sess, true);
    for t in [START + 58, START + 200, START + 500, START + 1000] {
        fx.clock.set(t);
        assert!(matches!(
            fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
            JobTick::Monitoring { .. }
        ));
        assert_eq!(fx.driver.sent_keys().len(), 1, "no nudge while attached");
    }
    // Detach ⇒ the next due nudge fires (window was paused), not Stuck. The defers
    // also cleared the confirmation gate, so it re-earns both observations here — and the
    // agent must have reported since the first nudge (m38).
    report_progress(&fx, 101, 40);
    fx.driver.set_clients(&sess, false);
    fx.clock.set(START + 1001);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.driver.sent_keys().len(), 2);
}

#[test]
fn wall_clock_budget_bounds_a_session_that_never_nudges() {
    // THE BUG (m18/2): both bounded-runtime budgets were enforced only INSIDE the
    // send path, and `window_start` was opened only AFTER a successful send. A session
    // the harness drives but never NUDGES therefore had no bound at all: an agent that
    // keeps self-scheduling `monitoring` naps is disposed by the marker fast-path,
    // which returns before any nudge, so the window never opened, the wall-clock check
    // never ran, and the `busy_since` stall backstop was cleared by every marker bump.
    // A runaway session ran forever, unattended and unsurfaced.
    //
    // The fix opens the window on the first tick that does WORK and checks the budget
    // there, so this reaches `park_stuck`. Deliberately asserts a path with ZERO
    // nudges, so it cannot pass for the old reason.
    let (mut fx, sess) = marker_fx(Tier::Standard, |_| {});
    fx.sched.max_wall_clock_s = 100;
    // Nap 1000s at a time, so the DUE nudge path is never reached again.
    write_marker(
        &fx,
        r#"{"seq":1,"state":"monitoring","status":"polling CI","next_check_s":1000}"#,
    );
    backdate_marker(&fx, 30);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + 1000
        },
        "the agent's self-scheduled nap parks the cadence"
    );
    assert_eq!(
        fx.sched.window_start,
        Some(START),
        "the wall-clock window opens on the first tick that does work, not on a send"
    );
    // Still inside the budget: a fresh nap is disposed on the mtime fast-path.
    fx.clock.set(START + 50);
    write_marker(
        &fx,
        r#"{"seq":2,"state":"monitoring","status":"still polling","next_check_s":1000}"#,
    );
    backdate_marker(&fx, 20);
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + 50 + 1000
        }
    );
    // Past the budget: the SAME never-nudging path must now escalate.
    fx.clock.set(START + 150);
    write_marker(
        &fx,
        r#"{"seq":3,"state":"monitoring","status":"still polling","next_check_s":1000}"#,
    );
    backdate_marker(&fx, 10);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        matches!(out, JobTick::Stuck(_)),
        "a session past max_wall_clock_s must surface, got {out:?}"
    );
    assert!(matches!(fx.sched.run, JobRun::Blocked { .. }));
    assert_eq!(ledger(&fx).open_stops.len(), 1, "parked on a human");
    assert!(
        fx.driver.sent_keys().is_empty(),
        "this whole run nudged ZERO times — the budget cannot have come from the \
         send path (session {sess})"
    );
}
