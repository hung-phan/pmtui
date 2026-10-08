# Milestone D — decide follow-ons (FO-1 plan-staleness stall + FO-2 marker-less-finish recheck) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Catch two stall modes today's counters miss — an agent that bumps a fresh marker every wake but keeps restating the same plan (FO-1), and an agent that finishes a turn and goes idle but never writes its marker (FO-2) — using only objective counters, never harness interpretation of prose.

**Architecture:** Two new `#[serde(default)]` `u32` fields on `AgentLoopState` (`stale_plan_streak`, `marker_less_rechecks`). FO-1 accrues `stale_plan_streak` in the marker disposer's shared pre-match seam (both `last_status` AND `next_step` trim-identical to the prior), exposes it as a signal at **T1** for Milestone E to render, and escalates a `WorkerStuck` via the existing `park_stuck_kind` backstop at **T2** — for Working AND Monitoring alike. FO-2 adds a `turn_finished_since_nudge()` predicate that relaxes the `awaiting_report()` hold in `idle_observed` into a **bounded marker-less recheck** that never blind-nudges and escalates `WorkerStuck` after **K** consecutive rechecks (a fast backstop independent of the 30-min `DEFAULT_STALL_BUSY_S`). `policy::decide_kind` stays byte-for-byte pure — all escalation goes through `park_stuck_kind`, which never consults the policy.

**Tech Stack:** Rust 2024 edition, `serde`/`serde_json`, `anyhow`, the in-tree `job_engine` scheduler + its `FakeDriver`/`FakeClock` unit-test fixtures. Build/test with `cargo`. Prefix every cargo/git invocation with `ECC_GATEGUARD=off`.

## Global Constraints

Binding rules for every task (copied from the design's LOCKED DECISIONS + D bullets):

- **Two thresholds on a dedicated `stale_plan_streak` counter.** Concrete values chosen for D: **T1 = 3** (the signal Milestone E renders — *not* a D production const; documented on the field, E adds its own const), **T2 = 6** as `pub(super) const DEFAULT_STALE_PLAN_STALL: u32 = 6` (the escalation threshold, styled after `DEFAULT_STALL_BUSY_S`), **K = 3** as `pub(super) const MARKER_LESS_RECHECK_MAX: u32 = 3`. T1 only EXPOSES a signal (the counter value); **D must NOT put any prose into the nudge** — E renders the "you've restated this plan N×" line.
- **Both-fields rule.** `stale_plan_streak` increments only when BOTH `last_status` AND `next_step` are byte-identical to the prior report's stored values. Trim both sides; the keep-prior block already stores the trimmed plan. A terse bump (no/empty `next_step`) leaves the streak UNCHANGED (neither bumps nor resets). Any other change resets to 0.
- **Shared pre-match check.** The T1 signal is the counter itself; the T2 threshold check lives in the SHARED pre-match location of `dispose_report` (before the `match report.state`), so a self-napping Monitoring agent restating the same plan escalates too — not only the Working arm.
- **`decide_kind` stays PURE.** No milestone threads ledger prose or counters into `policy::decide_kind`. All D escalation goes through `park_stuck_kind(now, &next, StopKind::WorkerStuck, reason)`, which parks `Blocked` directly and never calls `decide_kind`. (WARNING, load-bearing: `StopKind::WorkerStuck` floors to `Medium` in `policy::effective_risk_kind`, which would `AutoFlow` under Autopilot IF routed through `decide_kind`. It is safe here ONLY because `park_stuck_kind` bypasses the policy entirely — never route a `WorkerStuck` synthesized stop through `decide_kind`.)
- **Counters are lifetime-scoped but reset on progress / `on_blocked` / `retime`.** `stale_plan_streak` resets on a changed plan (in accrual), in `on_blocked` (beside `continuations = 0`), and in `retime`. `marker_less_rechecks` resets on ANY accepted marker bump (in `dispose_report`) and in `on_blocked`. All resets are fail-safe: they can only ever DELAY an escalation, never fire one early.
- **FO-2 preserves a fast bounded backstop.** The relaxed marker-less recheck escalates after K consecutive rechecks, independent of `DEFAULT_STALL_BUSY_S` and `max_wakes`. It NEVER blind-nudges (no fresh marker ⇒ nothing new to say) and NEVER opens `busy_since`.
- **FO-2 lives only in `idle_observed`** (the full `drive` path), never `drive_marker_only` — the I1 cadence guard is untouched.
- **Serde back-compat.** Both new fields are plain `#[serde(default)] pub … : u32` (like the sibling `continuations`), so a pre-D ledger loads at 0 and `deny_unknown_fields` still accepts a minimal ledger (it rejects EXTRA keys, not MISSING ones).

---

### Task 1: `stale_plan_streak` field + accrual + reset-on-change + round-trip

**Files:**
- Modify: `src/job.rs:52` (`AgentLoopState` struct — add two fields after `continuations` at `:93-94`), `src/job.rs:642` (`fresh`), `src/job.rs:781` (the full round-trip literal) + `src/job.rs:756-777` (minimal round-trip asserts)
- Modify: `src/job_engine/marker.rs:147-156` (`dispose_report` keep-prior block — add the FO-1 accrual right after)
- Test: `src/job.rs` `#[cfg(test)] mod tests` (round-trip), `src/job_engine/tests/marker.rs` (accrual behaviour)

**Interfaces:**
- Produces: `AgentLoopState.stale_plan_streak: u32` (`#[serde(default)]`, plain), `AgentLoopState.marker_less_rechecks: u32` (`#[serde(default)]`, plain — added now so both round-trip together and FO-2 has its field; incremented in Task 3). Accrual mutates `next.stale_plan_streak` in `dispose_report` before the `match report.state`.
- Consumes: the existing `dispose_report(&mut self, driver, now, ledger: &AgentLoopState, config, report: WakeReport)` (`marker.rs:132`); the keep-prior block that reads `report.next_step` (`marker.rs:154`); `ledger.last_status` / `ledger.last_plan` as the PRIOR values.

- [ ] **Step 1: Add the round-trip test assertions (failing) for the two new fields**

In `src/job.rs`, in `agent_loop_state_round_trips_full_and_minimal` (`:752`), extend the minimal-load section (after the pre-B assertions near `:774-777`):

```rust
        // A pre-D ledger has neither Milestone-D counter; both default to 0.
        assert_eq!(st.stale_plan_streak, 0);
        assert_eq!(st.marker_less_rechecks, 0);
```

and in the `full` literal (`:781`), add the two fields (place them right after `continuations: 2,` at `:794`):

```rust
            stale_plan_streak: 4,
            marker_less_rechecks: 2,
```

and after `assert_eq!(round, full);` (`:843`) add:

```rust
        assert_eq!(round.stale_plan_streak, 4);
        assert_eq!(round.marker_less_rechecks, 2);
```

- [ ] **Step 2: Run it to confirm it fails to compile (fields do not exist yet)**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job::tests::agent_loop_state_round_trips_full_and_minimal`
Expected: FAIL — `error[E0560]: struct AgentLoopState has no field named stale_plan_streak` (and the same for `marker_less_rechecks`).

- [ ] **Step 3: Add the two fields to `AgentLoopState`**

In `src/job.rs`, immediately after the `continuations` field (`:93-94`):

```rust
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
```

- [ ] **Step 4: Initialize both fields in `fresh`**

In `src/job.rs` `fresh` (`:642`), add both after `continuations: 0,` (`:653`):

```rust
            stale_plan_streak: 0,
            marker_less_rechecks: 0,
```

- [ ] **Step 5: Run the round-trip test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job::tests::agent_loop_state_round_trips_full_and_minimal`
Expected: PASS. Also run `ECC_GATEGUARD=off cargo test -p agent-manager --lib job::tests::a_fresh_ledger_omits_the_decider_fields` — still PASS (the two new fields are plain `u32`, they don't add the strings `digest`/`decisions`/`situation` the test scans for).

Non-vacuity: the test constructs a `full` ledger with `stale_plan_streak: 4` / `marker_less_rechecks: 2`, serializes and deserializes it, and asserts equality — a field that failed to serialize (or defaulted on load) would come back `0 != 4`. The minimal case asserts a JSON with neither field loads them as `0`, pinning `#[serde(default)]`.

- [ ] **Step 6: Write the FO-1 accrual behaviour tests (failing)**

Add to `src/job_engine/tests/marker.rs`:

```rust
// --- FO-1 plan-staleness streak (Milestone D) --------------------------------

/// Drive ONE `working` bump carrying `status` + `next_step` on an idle pane, one cadence
/// later; returns the tick. Plain tick (not `tick_confirmed`), so it works whether the
/// disposition arms/monitors (below threshold) or escalates (at threshold).
fn dispose_plan(fx: &mut Fx, sess: &str, seq: u64, status: &str, next_step: &str, age_s: u64) -> JobTick {
    write_marker(
        fx,
        &format!(r#"{{"seq":{seq},"state":"working","status":"{status}","next_step":"{next_step}"}}"#),
    );
    backdate_marker(fx, age_s);
    fx.driver.set_tail(sess, IDLE_PANE);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap()
}

#[test]
fn restated_plan_bumps_dedicated_streak_not_continuations() {
    // Seed the prior status+plan so the FIRST bump below is already a repeat.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("indexing".into());
        l.last_plan = Some("wire the alert".into());
    });
    for seq in 1..=3u64 {
        dispose_plan(&mut fx, &sess, seq, "indexing", "wire the alert", 10 - seq);
    }
    let l = ledger(&fx);
    assert_eq!(l.stale_plan_streak, 3, "three identical restatements => streak 3");
    // A DEDICATED counter: a stream of VALID bumps never touches the malformed-marker
    // stall counter `continuations`.
    assert_eq!(l.continuations, 0, "valid bumps never touch continuations");
}

#[test]
fn changing_the_plan_resets_the_streak() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("indexing".into());
        l.last_plan = Some("wire the alert".into());
    });
    for seq in 1..=2u64 {
        dispose_plan(&mut fx, &sess, seq, "indexing", "wire the alert", 10 - seq);
    }
    assert_eq!(ledger(&fx).stale_plan_streak, 2);
    // A DIFFERENT next_step resets the streak to 0.
    dispose_plan(&mut fx, &sess, 3, "indexing", "NOW something else", 2);
    assert_eq!(ledger(&fx).stale_plan_streak, 0, "a changed plan resets the streak");
}

#[test]
fn a_terse_bump_neither_bumps_nor_resets() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("indexing".into());
        l.last_plan = Some("wire the alert".into());
        l.stale_plan_streak = 2; // pre-seed a streak
    });
    // A TERSE bump (no next_step) — exactly what `report_progress` writes.
    write_marker(&fx, r#"{"seq":1,"state":"working","status":"indexing"}"#);
    backdate_marker(&fx, 4);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let l = ledger(&fx);
    assert_eq!(l.stale_plan_streak, 2, "a terse bump leaves the streak UNTOUCHED");
    assert_eq!(l.last_plan.as_deref(), Some("wire the alert"), "keep-prior preserves the plan");
}

#[test]
fn same_plan_but_changed_status_does_not_accrue() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("step 0".into());
        l.last_plan = Some("wire the alert".into());
    });
    // Same next_step, MOVING status each bump — a healthy long grind.
    for seq in 1..=4u64 {
        dispose_plan(&mut fx, &sess, seq, &format!("step {seq}"), "wire the alert", 10 - seq);
    }
    assert_eq!(
        ledger(&fx).stale_plan_streak, 0,
        "a moving status keeps the streak at 0 (BOTH fields must match)"
    );
}
```

- [ ] **Step 7: Run them to verify they fail on the value**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::marker::restated_plan_bumps_dedicated_streak_not_continuations`
Expected: FAIL — `assertion failed: left: 0, right: 3` (the field exists and defaults to 0, but nothing accrues it yet).

- [ ] **Step 8: Add the FO-1 accrual in `dispose_report`**

In `src/job_engine/marker.rs`, immediately after the keep-prior block that closes at `:156` (the `if let Some(step) = report.next_step … { next.last_plan = Some(step); }`), insert:

```rust
        // === FO-1 (Milestone D): plan-staleness streak accrual ===
        // Compare THIS report against the PRIOR stored values (`ledger`, before the
        // keep-prior overwrite above) — the streak counts CONSECUTIVE reports whose BOTH
        // human-facing fields (status AND next_step) are trim-equal to the prior. Trim both
        // sides so trailing-whitespace drift cannot spuriously reset it (the keep-prior
        // block already stores the trimmed plan). Pure equality — no prose interpretation.
        match report.next_step.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            // A terse bump (no/empty next_step) preserves the prior plan (keep-prior above)
            // and leaves the streak UNTOUCHED — we cannot judge staleness without a
            // restated plan, and a diligent progress bump must not reset it.
            None => {}
            Some(step) => {
                let same_plan = ledger.last_plan.as_deref().map(str::trim) == Some(step);
                let same_status = report.status.as_deref().map(str::trim)
                    == ledger.last_status.as_deref().map(str::trim);
                if same_plan && same_status {
                    next.stale_plan_streak = next.stale_plan_streak.saturating_add(1);
                } else {
                    next.stale_plan_streak = 0;
                }
            }
        }
```

- [ ] **Step 9: Run the FO-1 accrual tests to verify they pass**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::marker::`
Expected: PASS for the four new tests, and no regression in the existing marker tests.

Non-vacuity for each: `restated…` — a no-op accrual leaves `0 != 3`; an accrual keyed on `continuations` leaves `continuations == 3`. `changing_the_plan_resets…` — without the `else` reset the streak stays `3 != 0`. `a_terse_bump…` — pre-seeded to 2; a reset-on-terse would give `0 != 2`, an increment-on-terse `3 != 2`. `same_plan_but_changed_status…` — a `next_step`-only rule would give `4 != 0`.

- [ ] **Step 10: Commit**

```bash
git add src/job.rs src/job_engine/marker.rs
git commit -m "feat(ledger-d): stale_plan_streak + marker_less_rechecks fields; FO-1 accrual (both-fields, trim) + round-trip"
```

---

### Task 2: FO-1 T2 escalation at the shared pre-match location + `on_blocked`/`retime` resets

**Files:**
- Modify: `src/job_engine/marker.rs` (add `pub(super) const DEFAULT_STALE_PLAN_STALL`; add the T2 check in `dispose_report` after the B seam at `:203`, before the `match` at `:205`)
- Modify: `src/job_engine/stops.rs:89` (`on_blocked` — reset `stale_plan_streak` beside `base.continuations = 0`)
- Modify: `src/job.rs:627` (`retime` — reset `stale_plan_streak`)
- Test: `src/job_engine/tests/marker.rs` (escalation, Working + Monitoring), `src/job_engine/tests/stops.rs` (`on_blocked` reset), `src/job.rs` tests (`retime` reset)

**Interfaces:**
- Consumes: `next.stale_plan_streak` (Task 1); `self.park_stuck_kind(now, &next, StopKind::WorkerStuck, reason) -> Result<JobTick>` (`stops.rs:284`, returns `JobTick::Stuck`, parks `Blocked`, bypasses `decide_kind`); the B seam having already bumped `next.digest.disposed` + set `next.situation` + appended `raw.jsonl` (`marker.rs:190-203`).
- Produces: `pub(super) const DEFAULT_STALE_PLAN_STALL: u32 = 6` in `marker.rs`; a `Blocked` park on a `WorkerStuck` stop once the streak reaches T2.

- [ ] **Step 1: Write the T2 escalation tests (failing)**

Add to `src/job_engine/tests/marker.rs` (reuses `dispose_plan` from Task 1):

```rust
use super::super::marker::DEFAULT_STALE_PLAN_STALL;

#[test]
fn repeated_plan_past_threshold_escalates_worker_stuck() {
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("indexing".into());
        l.last_plan = Some("wire the alert".into());
    });
    let t2 = DEFAULT_STALE_PLAN_STALL as u64;
    // Drive to ONE below the threshold: every tick still Monitors, streak just under T2.
    for seq in 1..t2 {
        let tick = dispose_plan(&mut fx, &sess, seq, "indexing", "wire the alert", (t2 + 2 - seq) * 2);
        assert!(matches!(tick, JobTick::Monitoring { .. }), "pre-threshold monitors: {tick:?}");
    }
    assert_eq!(ledger(&fx).stale_plan_streak, DEFAULT_STALE_PLAN_STALL - 1, "one below T2");
    // The threshold bump escalates — it is the COUNT that triggers, not this bump's content.
    let tick = dispose_plan(&mut fx, &sess, t2, "indexing", "wire the alert", 2);
    assert!(matches!(tick, JobTick::Stuck(_)), "reaching T2 escalates: {tick:?}");
    let l = ledger(&fx);
    assert!(matches!(l.run, JobRun::Blocked { .. }), "escalation parks Blocked");
    assert_eq!(l.open_stops.len(), 1);
    assert_eq!(l.open_stops[0].kind, StopKind::WorkerStuck, "the honest 'alive but not advancing' kind");
}

#[test]
fn monitoring_state_plan_staleness_also_escalates() {
    // The threshold check is PRE-MATCH, so a self-napping Monitoring agent restating the
    // same plan escalates too — not only the Working arm.
    let (mut fx, sess) = marker_fx(Tier::Autopilot, |l| {
        l.last_status = Some("polling".into());
        l.last_plan = Some("poll the queue".into());
    });
    let t2 = DEFAULT_STALE_PLAN_STALL as u64;
    let mut last = JobTick::WaitingForIntake;
    for seq in 1..=t2 {
        write_marker(&fx, &format!(
            r#"{{"seq":{seq},"state":"monitoring","status":"polling","next_step":"poll the queue","next_check_s":120}}"#
        ));
        backdate_marker(&fx, (t2 + 2 - seq) * 2);
        fx.driver.set_tail(&sess, IDLE_PANE);
        fx.clock.advance(300 + BUSY_RECHECK_S);
        last = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }
    assert!(matches!(last, JobTick::Stuck(_)), "a stale plan across monitoring reports escalates: {last:?}");
    assert!(matches!(ledger(&fx).run, JobRun::Blocked { .. }));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::marker::repeated_plan_past_threshold_escalates_worker_stuck`
Expected: FAIL to compile first (`DEFAULT_STALE_PLAN_STALL` not found); after the const lands but before the check, FAIL on `matches!(tick, JobTick::Stuck(_))` (the threshold tick still Monitors).

- [ ] **Step 3: Add the T2 const**

In `src/job_engine/marker.rs`, near the top-level consts (after `MARKER_MIDWRITE_GRACE` at `:44`):

```rust
/// FO-1 (Milestone D) escalation threshold (T2): once `stale_plan_streak` reaches this
/// many consecutive byte-identical (status AND next_step) reports, the disposer escalates
/// a `WorkerStuck` — the "alive but not advancing" twin of `DEFAULT_STALL_BUSY_S`. The
/// lower SIGNAL threshold (T1 = 3), at which Milestone E's nudge surfaces "you've restated
/// this plan N×", is E's own const; D only exposes the counter. Could become a per-session
/// `Config` field later (mirrors `DEFAULT_STALL_BUSY_S`).
pub(super) const DEFAULT_STALE_PLAN_STALL: u32 = 6;
```

- [ ] **Step 4: Add the T2 check in `dispose_report` (shared, pre-match)**

In `src/job_engine/marker.rs`, immediately after the Milestone-B seam block that ends at `:203` (the `raw.jsonl` append) and BEFORE `match report.state {` (`:205`):

```rust
        // === FO-1 T2 (Milestone D): escalate a plan that never advances ===
        // Placed in the SHARED pre-match location so a Working AND a Monitoring bump both
        // escalate (Important-1). This is a BACKSTOP, not a policy decision: it goes through
        // `park_stuck_kind` (which parks `Blocked` directly and NEVER consults
        // `decide_kind`), exactly as the busy-stall / dead-pane / malformed-marker
        // escalations do — so `decide_kind` stays byte-for-byte pure. The B seam above has
        // already counted this accepted bump (`digest.disposed`) and emitted its full
        // `raw.jsonl` line, so the escalation loses no fidelity; `park_stuck_kind` clones
        // `next` (streak + disposed + situation) and persists it.
        if next.stale_plan_streak >= DEFAULT_STALE_PLAN_STALL {
            let reason = format!(
                "agent has restated the same plan {}× without the work advancing — it may be \
                 stuck in a loop; close it, or answer to keep it going",
                next.stale_plan_streak
            );
            return Ok(Some(self.park_stuck_kind(
                now,
                &next,
                StopKind::WorkerStuck,
                reason,
            )?));
        }
```

- [ ] **Step 5: Run the escalation tests to verify they pass**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::marker::`
Expected: PASS for both new tests; the pre-threshold ticks still Monitor and the threshold tick returns `Stuck` + parks `Blocked` on a `WorkerStuck`.

Non-vacuity: `repeated_plan_past_threshold…` asserts the pre-threshold ticks are `Monitoring` (proving it is the COUNT, not the content, that triggers) and the threshold tick is `Stuck` — a wrong threshold (fires early/late) breaks one or the other; `open_stops[0].kind == WorkerStuck` catches an escalation via the wrong kind. `monitoring_state…` fails if the check were placed in the Working arm only (a Monitoring bump would nap, never Stuck).

- [ ] **Step 6: Write the `on_blocked` + `retime` reset tests (failing)**

Add to `src/job_engine/tests/stops.rs`:

```rust
#[test]
fn human_answer_resets_plan_staleness_in_on_blocked() {
    // A session parked Blocked with an accrued streak: a human answer that unblocks it
    // resets the streak (beside continuations), so answering "keep going" clears it.
    let mut fx = blocked_fx();
    let mut l = ledger(&fx);
    l.stale_plan_streak = 4;
    l.continuations = 3;
    job::save(&fx.paths, &l).unwrap();
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    // Answer the open stop at/after the park start.
    fx.clock.set(START + 1);
    push_answer(&fx, "stop-x", Some("go ahead"), fx.clock.now());
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let after = ledger(&fx);
    assert_eq!(after.stale_plan_streak, 0, "on_blocked resets the plan-staleness streak");
    assert_eq!(after.continuations, 0, "…beside continuations (unchanged behaviour)");
}
```

Add to `src/job.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn retime_resets_the_plan_staleness_streak() {
        // A human cadence edit is a fresh start for the stall signal (LOCKED D-10) — a
        // fail-safe direction (it can only ever delay a plan-stall escalation).
        let mut st = AgentLoopState::fresh(Engine::Claude, Some(300), 0);
        st.run = JobRun::Monitoring { until: 1000 };
        st.stale_plan_streak = 5;
        st.retime(60, 100);
        assert_eq!(st.stale_plan_streak, 0, "retime resets the plan-staleness streak");
    }
```

- [ ] **Step 7: Run to verify they fail**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::stops::human_answer_resets_plan_staleness_in_on_blocked job::tests::retime_resets_the_plan_staleness_streak`
Expected: FAIL — `left: 5, right: 0` (nothing resets the streak yet).

- [ ] **Step 8: Add the resets**

In `src/job_engine/stops.rs` `on_blocked`, after `base.continuations = 0;` (`:90`):

```rust
        // A human decision is a fresh start for the FO-1 stall signal too (LOCKED D-10).
        base.stale_plan_streak = 0;
```

In `src/job.rs` `retime` (`:627`), after `self.cadence_pinned = true;` (`:632`):

```rust
        // A human cadence edit resets the FO-1 plan-staleness streak — a fail-safe wipe of
        // the accumulating signal (it can only delay a stall escalation, never fire one).
        self.stale_plan_streak = 0;
```

- [ ] **Step 9: Run to verify they pass**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::stops:: job::tests::retime_resets_the_plan_staleness_streak`
Expected: PASS.

Non-vacuity: both pre-seed a non-zero streak (5 / 4) and assert it becomes 0 — a missing reset leaves the seeded value. The `on_blocked` test also re-asserts `continuations == 0` to prove the new line rides beside the existing reset without disturbing it.

- [ ] **Step 10: Commit**

```bash
git add src/job_engine/marker.rs src/job_engine/stops.rs src/job.rs
git commit -m "feat(ledger-d): FO-1 T2 escalation (WorkerStuck, pre-match, Working+Monitoring) + on_blocked/retime resets"
```

---

### Task 3: FO-2 `turn_finished_since_nudge()` + relaxed gate → bounded marker-less recheck (counts, no nudge)

**Files:**
- Modify: `src/job_engine/drive.rs` (add `pub(super) const MARKER_LESS_RECHECK_MAX`; add `fn turn_finished_since_nudge`; add `fn marker_less_recheck`; relax the `awaiting_report` gate in `idle_observed` at `:303`)
- Test: `src/job_engine/tests/drive.rs`

**Interfaces:**
- Consumes: `self.turns_at_nudge: Option<u64>` (`mod.rs:188`), `self.paths.turn_signal()`, `base.awaiting_report()` (`job.rs:605`), `self.reset_idle_gate()` (`mod.rs:508`), `self.save_ledger`, `AutopilotEventKind::Held(HoldReason::AwaitingReport)` (`job.rs`), `BUSY_RECHECK_S` (`drive.rs:22`), `AgentLoopState.marker_less_rechecks` (Task 1).
- Produces: `fn turn_finished_since_nudge(&self) -> bool`; `fn marker_less_recheck(&mut self, now, base) -> Result<JobTick>` (increments `marker_less_rechecks`, re-parks `Monitoring{now+BUSY_RECHECK_S}`, never nudges; the escalate-after-K branch is added in Task 4); `pub(super) const MARKER_LESS_RECHECK_MAX: u32 = 3`.

- [ ] **Step 1: Write the FO-2 recheck tests (failing)**

Add to `src/job_engine/tests/drive.rs`:

```rust
// --- FO-2 marker-less-finish recheck (Milestone D) --------------------------

/// Launch, cold-start grace, then ONE confirmed nudge on an idle pane — the common start
/// for an FO-2 test. Leaves the clock at the nudging instant; the nudge baselines
/// `turns_at_nudge` (turn_count == 0, no signal yet) and `nudged_at_seq` (== 0, no marker),
/// so `awaiting_report()` holds afterward.
fn nudged_then_awaiting(fx: &mut Fx, sess: &str) {
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch → Monitoring{grace}
    fx.clock.set(START + LAUNCH_GRACE_S);
    fx.driver.set_tail(sess, IDLE_PANE);
    tick_confirmed(fx);
    assert_eq!(fx.driver.sent_keys().len(), 1, "the baseline nudge landed");
    assert!(ledger(fx).awaiting_report(), "and left an unanswered watermark");
}

#[test]
fn a_completed_turn_rechecks_a_marker_less_finish() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    // A turn completes (hook appends a byte) but NO fresh marker is written.
    grow_turn_signal(&fx, 1);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Monitoring { .. }), "a bounded recheck, not an escalation: {out:?}");
    assert_eq!(fx.driver.sent_keys().len(), 1, "still no blind nudge without a fresh marker");
    assert_eq!(ledger(&fx).marker_less_rechecks, 1, "the marker-less-recheck counter increments");
}

#[test]
fn no_turn_signal_leaves_the_awaiting_report_hold_intact() {
    // With NO completed turn since the nudge, the hold is EXACTLY today's: route to
    // busy_recheck (AwaitingReport), no nudge, and the marker-less counter never moves.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    // No grow_turn_signal ⇒ turn_finished_since_nudge() is false.
    fx.clock.advance(300 + BUSY_RECHECK_S);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(matches!(out, JobTick::Monitoring { .. }));
    assert_eq!(fx.driver.sent_keys().len(), 1, "awaiting_report still holds — no nudge");
    assert_eq!(ledger(&fx).marker_less_rechecks, 0, "no completed turn ⇒ no marker-less recheck");
}

#[test]
fn a_turn_still_in_flight_does_not_relax_the_hold() {
    // turn-signal size EQUAL to the nudge baseline means the nudged turn is still running —
    // NOT finished — so the hold is not relaxed and the counter stays 0.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    grow_turn_signal(&fx, 2); // 2 turns done BEFORE the nudge → baseline becomes 2
    nudged_then_awaiting(&mut fx, &sess);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).marker_less_rechecks, 0, "size == baseline is mid-turn, not a finish");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::drive::a_completed_turn_rechecks_a_marker_less_finish`
Expected: FAIL — today the `awaiting_report()` hold routes to `busy_recheck` regardless of the turn signal, so `marker_less_rechecks` stays `0 != 1`.

- [ ] **Step 3: Add the const + predicate**

In `src/job_engine/drive.rs`, after `DEFAULT_STALL_BUSY_S` (`:54`):

```rust
/// FO-2 (Milestone D) marker-less-recheck backstop (K): after this many CONSECUTIVE
/// completed-turn-but-marker-less rechecks the loop escalates a `WorkerStuck` — a FAST
/// bound independent of the 30-min `DEFAULT_STALL_BUSY_S` and of `max_wakes`. Reset on any
/// accepted marker bump and on `on_blocked`.
pub(super) const MARKER_LESS_RECHECK_MAX: u32 = 3;
```

In `src/job_engine/drive.rs`, beside `turn_in_progress` (after it, near `:254`):

```rust
    /// FO-2: whether a turn has COMPLETED since our last nudge — the turn-complete signal
    /// exists AND its byte count has advanced PAST [`JobScheduler::turns_at_nudge`]. The
    /// exact inverse of `turn_in_progress`'s `now <= base`. Returns `false` with no baseline
    /// or no signal file (hook unwired), so a build without the turn-end event behaves
    /// byte-for-byte as today. Read-only stat, panic-free.
    fn turn_finished_since_nudge(&self) -> bool {
        match (
            std::fs::metadata(self.paths.turn_signal())
                .map(|m| m.len())
                .ok(),
            self.turns_at_nudge,
        ) {
            (Some(now), Some(base)) => now > base,
            _ => false,
        }
    }
```

- [ ] **Step 4: Add `marker_less_recheck` (increment + re-park, no escalation yet)**

In `src/job_engine/drive.rs`, in `impl JobScheduler` (place after `idle_observed`, before `busy_recheck`):

```rust
    /// FO-2: a turn COMPLETED since our last nudge but the agent wrote NO fresh marker (a
    /// "marker-less finish"). This relaxes the awaiting-report hold into a BOUNDED recheck:
    /// count it on the ledger and re-park a short recheck. It deliberately does NOT nudge
    /// (no fresh marker ⇒ nothing new to say — hence "no blind nudge") and does NOT open
    /// `busy_since` (an idle pane is not a Busy stall). The escalate-after-K backstop is
    /// added in Milestone-D Task 4.
    fn marker_less_recheck(&mut self, now: Epoch, base: &AgentLoopState) -> Result<JobTick> {
        // Void any armed Idle observations, exactly as busy_recheck does — the confirmation
        // gate must re-earn two consecutive stable captures after this hold.
        self.reset_idle_gate();
        let mut next = base.clone();
        next.marker_less_rechecks = next.marker_less_rechecks.saturating_add(1);
        let until = now + BUSY_RECHECK_S;
        next.run = JobRun::Monitoring { until };
        next.updated_at = now;
        next.record_event(now, AutopilotEventKind::Held(HoldReason::AwaitingReport));
        self.save_ledger(&mut next)?;
        self.run = JobRun::Monitoring { until };
        Ok(JobTick::Monitoring { until })
    }
```

- [ ] **Step 5: Relax the `awaiting_report` gate in `idle_observed`**

In `src/job_engine/drive.rs` `idle_observed`, replace the current gate (`:303-305`):

```rust
        if base.awaiting_report() {
            return self.busy_recheck(now, base, HoldReason::AwaitingReport);
        }
```

with:

```rust
        if base.awaiting_report() {
            // FO-2 (Milestone D): a turn that COMPLETED since our nudge WITHOUT a fresh
            // marker relaxes the hold into a bounded marker-less recheck; a turn still in
            // flight (no completed turn) holds exactly as before. With no turn-end hook /
            // no baseline `turn_finished_since_nudge` is false, so this is byte-identical to
            // today, and the existing turn-event tests (which set `turns_at_nudge` but leave
            // `nudged_at_seq` unset, so `awaiting_report()` is false) never reach this arm.
            if self.turn_finished_since_nudge() {
                return self.marker_less_recheck(now, base);
            }
            return self.busy_recheck(now, base, HoldReason::AwaitingReport);
        }
```

- [ ] **Step 6: Run the FO-2 recheck tests to verify they pass**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::drive::`
Expected: PASS for the three new tests and NO regression in the existing `the_turn_event_blocks_a_nudge…` / `a_completed_turn_does_not_bypass_the_fingerprint…` / `no_turn_signal_file_falls_back…` tests (all of which run with `awaiting_report() == false`, so the relaxed arm is unreached).

Non-vacuity: `a_completed_turn_rechecks…` grows the signal PAST the baseline and asserts `marker_less_rechecks == 1` while `sent_keys` stays at 1 — a gate that ignored `turn_finished` leaves the counter at 0; one that blind-nudged bumps `sent_keys`. `no_turn_signal…` and `a_turn_still_in_flight…` assert the counter stays 0 when there is no finish (and when size == baseline) — a predicate that returned `true` too eagerly would move it.

- [ ] **Step 7: Commit**

```bash
git add src/job_engine/drive.rs
git commit -m "feat(ledger-d): FO-2 turn_finished_since_nudge + relaxed awaiting_report gate → bounded marker-less recheck (no blind nudge)"
```

---

### Task 4: FO-2 escalate-after-K + reset on accepted bump / `on_blocked`

**Files:**
- Modify: `src/job_engine/drive.rs` (`marker_less_recheck` — add the escalate-after-K branch)
- Modify: `src/job_engine/marker.rs` (`dispose_report` — reset `marker_less_rechecks = 0` on every accepted bump)
- Modify: `src/job_engine/stops.rs:90` (`on_blocked` — reset `marker_less_rechecks`)
- Test: `src/job_engine/tests/drive.rs`, `src/job_engine/tests/marker.rs`, `src/job_engine/tests/stops.rs`

**Interfaces:**
- Consumes: `MARKER_LESS_RECHECK_MAX` (Task 3); `self.park_stuck_kind(now, &next, StopKind::WorkerStuck, reason)` (already called by `dead_pane_escalation` in `drive.rs:558`, so it is in scope); `next.marker_less_rechecks` (Task 1/3).
- Produces: a `WorkerStuck` escalation after K consecutive marker-less rechecks; `marker_less_rechecks` reset to 0 on any accepted marker bump and on `on_blocked`.

- [ ] **Step 1: Write the escalate-after-K + reset tests (failing)**

Add to `src/job_engine/tests/drive.rs` (reuses `nudged_then_awaiting` from Task 3):

```rust
use super::super::drive::MARKER_LESS_RECHECK_MAX;

#[test]
fn marker_less_recheck_escalates_after_k() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    let k = MARKER_LESS_RECHECK_MAX as usize;
    let mut last = JobTick::WaitingForIntake;
    for i in 1..=k {
        grow_turn_signal(&fx, i); // a fresh completed turn each recheck, still no marker
        fx.clock.advance(300 + BUSY_RECHECK_S);
        last = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }
    assert!(
        matches!(last, JobTick::Stuck(_)),
        "K marker-less rechecks escalate a WorkerStuck (fast, not the 30-min stall): {last:?}"
    );
    let l = ledger(&fx);
    assert!(matches!(l.run, JobRun::Blocked { .. }));
    assert_eq!(l.open_stops[0].kind, StopKind::WorkerStuck);
    assert_eq!(fx.driver.sent_keys().len(), 1, "escalated, never blind-nudged");
}

#[test]
fn an_accepted_marker_bump_resets_the_marker_less_counter() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    nudged_then_awaiting(&mut fx, &sess);
    // One marker-less recheck (counter → 1).
    grow_turn_signal(&fx, 1);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).marker_less_rechecks, 1);
    // The agent finally writes a marker: any accepted bump resets the counter.
    write_marker(&fx, r#"{"seq":50,"state":"working","status":"back at it"}"#);
    backdate_marker(&fx, 2);
    fx.clock.advance(300 + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).marker_less_rechecks, 0, "an accepted bump resets the marker-less counter");
}
```

Add to `src/job_engine/tests/stops.rs`:

```rust
#[test]
fn human_answer_resets_the_marker_less_counter_in_on_blocked() {
    let mut fx = blocked_fx();
    let mut l = ledger(&fx);
    l.marker_less_rechecks = 2;
    job::save(&fx.paths, &l).unwrap();
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.clock.set(START + 1);
    push_answer(&fx, "stop-x", Some("go ahead"), fx.clock.now());
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).marker_less_rechecks, 0, "on_blocked resets the marker-less counter");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::drive::marker_less_recheck_escalates_after_k`
Expected: FAIL — without the K branch, the Kth recheck returns `Monitoring` (`last` is `Monitoring`, not `Stuck`); `an_accepted_marker_bump…` fails with `1 != 0`.

- [ ] **Step 3: Add the escalate-after-K branch**

In `src/job_engine/drive.rs` `marker_less_recheck`, immediately after the `saturating_add(1)` line:

```rust
        if next.marker_less_rechecks >= MARKER_LESS_RECHECK_MAX {
            // A FAST bounded backstop, independent of DEFAULT_STALL_BUSY_S / max_wakes.
            // `park_stuck_kind` clones `next` (carrying the final count) and persists it.
            let reason = format!(
                "agent finished {} turns since the last nudge without writing its decision \
                 marker — it may be stuck or not reporting; close it, or answer to keep it going",
                next.marker_less_rechecks
            );
            return self.park_stuck_kind(now, &next, StopKind::WorkerStuck, reason);
        }
```

(so the method body is: `reset_idle_gate` → clone → increment → this K check → else re-park.)

- [ ] **Step 4: Reset the counter on every accepted bump**

In `src/job_engine/marker.rs` `dispose_report`, in the FO-1 accrual block added in Task 1 (right after the `match report.next_step … {}`), append:

```rust
        // FO-2 (Milestone D): any accepted marker bump means the agent reported — a
        // marker-less finish is over, so the recheck counter resets on every valid bump.
        next.marker_less_rechecks = 0;
```

- [ ] **Step 5: Reset the counter in `on_blocked`**

In `src/job_engine/stops.rs` `on_blocked`, beside the Task-2 `base.stale_plan_streak = 0;`:

```rust
        base.marker_less_rechecks = 0;
```

- [ ] **Step 6: Run to verify they pass**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::drive:: job_engine::tests::stops::`
Expected: PASS. In `marker_less_recheck_escalates_after_k`, the counter reaches K on the Kth recheck and escalates on that tick (so the loop's `last` is `Stuck`); the reset test drops it back to 0 on a valid bump.

Non-vacuity: `escalates_after_k` asserts the Kth tick is `Stuck` with a `WorkerStuck` stop and `sent_keys` unchanged — a missing K branch leaves `Monitoring`; escalating via the wrong kind fails the `open_stops[0].kind` check; a blind nudge fails `sent_keys().len() == 1`. `an_accepted_marker_bump…` and the `on_blocked` test each pre-establish a non-zero counter and assert it returns to 0 — a missing reset leaves it.

- [ ] **Step 7: Commit**

```bash
git add src/job_engine/drive.rs src/job_engine/marker.rs src/job_engine/stops.rs
git commit -m "feat(ledger-d): FO-2 escalate-after-K WorkerStuck backstop + reset on accepted bump / on_blocked"
```

---

### Task 5: Un-ignore + reconcile the S-D acceptance scenarios (the exit gate)

**Files:**
- Modify: `src/job_engine/tests/scenarios.rs` (the `D_T1`/`D_T2`/`D_K` consts at `:989-991`; un-ignore `s_d1..s_d9`; reconcile `s_d8`'s loop + `s_d9`'s setup; remove the two now-superseded `[BASE]` controls)

**Interfaces:**
- Consumes: everything from Tasks 1–4. Reads new fields via `ledger_u64(&led, "stale_plan_streak" | "marker_less_rechecks")` (`scenarios.rs:996`) — which now read the REAL serialized values.
- Produces: `s_d1..s_d9` green; the two obsolete `[BASE]` controls removed.

- [ ] **Step 1: Reconcile the scenario threshold consts to the chosen production values**

In `src/job_engine/tests/scenarios.rs` (`:989-991`), change `D_T2` from `5` to `6` (matching `DEFAULT_STALE_PLAN_STALL`); leave `D_T1 = 3` and `D_K = 3`:

```rust
const D_T1: u64 = 3;
const D_T2: u64 = 6;
const D_K: u64 = 3;
```

- [ ] **Step 2: Remove the two `[BASE]` controls D intentionally invalidates**

Delete `s_d1_base_no_stale_plan_streak_today` (`:1030`) and `s_d7_base_marker_less_finish_rechecks_and_does_not_nudge_today` (`:1251`). Rationale: each asserts the pre-D state (`stale_plan_streak == 0` over 6 identical reports / `marker_less_rechecks == 0` after a completed marker-less turn). D now accrues those counters by design, so the "today" assertions are false — the `[ACC:D]` successors (`s_d1_plan_stall_t1_sets_the_nudge_flag`, `s_d7_marker_less_finish_rechecks_and_counts`) pin the post-D reality. This is the standard end-of-life for a `[BASE]`/`[ACC]` pair once the milestone lands. (The other baselines — S1–S11, CF-4 — are unaffected: `report_progress` writes NO `next_step`, so it is a terse bump that leaves the streak untouched, and none grow a turn signal while `awaiting_report()` holds.)

- [ ] **Step 3: Reconcile `s_d8`'s loop bound**

In `s_d8_marker_less_recheck_escalates_after_k` (`:1314`), change the loop from `for i in 1..=(D_K + 1)` to:

```rust
    for i in 1..=D_K {
        grow_turn_signal(&fx, i as usize); // a fresh completed turn each recheck, still no marker
        fx.clock.advance(300 + BUSY_RECHECK_S);
        last = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    }
```

Rationale: the marker-less counter increments on the FIRST recheck (`s_d7` pins `marker_less_rechecks >= 1` after ONE relaxed tick), so escalation fires on the Kth recheck, not the K+1th. A `1..=(D_K+1)` loop would run one extra tick after the `Blocked` park, whose `on_blocked` re-emits `Escalated` — making `last` `Escalated`, not `Stuck`.

- [ ] **Step 4: Reconcile `s_d9`'s setup so the relaxed path is actually reached**

In `s_d9_still_changing_transcript_rearms_on_the_relaxed_path` (`:1338`), set the pane idle before the baseline nudge so `tick_confirmed` actually nudges (establishing `nudged_at_seq` + `turns_at_nudge`, without which `awaiting_report()` is false and the relaxed path is never reached). Insert `fx.driver.set_tail(&sess, IDLE_PANE);` immediately before `tick_confirmed(&mut fx);` (`:1343`). The subsequent streaming-tail steps are unchanged. Update the trailing comment to: `// D: on the relaxed marker-less path the harness rechecks (never blind-nudges), so no keystroke lands regardless of transcript.`

- [ ] **Step 5: Un-ignore `s_d1..s_d9`**

Remove the `#[ignore = "acceptance: Milestone D"]` attribute from each of the nine `[ACC:D]` tests: `s_d1_plan_stall_t1_sets_the_nudge_flag`, `s_d2_plan_stall_t2_escalates_worker_stuck`, `s_d3_changing_the_plan_resets_the_streak`, `s_d4_same_plan_changed_status_does_not_accrue`, `s_d5_monitoring_state_plan_staleness_also_escalates`, `s_d6_human_answer_resets_plan_staleness`, `s_d7_marker_less_finish_rechecks_and_counts`, `s_d8_marker_less_recheck_escalates_after_k`, `s_d9_still_changing_transcript_rearms_on_the_relaxed_path`.

Leave the Milestone-C (`s_c1..s_c5`) and Milestone-E (`s_e1..s_e5`) tests `#[ignore]`d. In particular **`s_e3_stall_streak_fixed_line` STAYS ignored-for-E**: although it depends on D's `stale_plan_streak`, it asserts the *nudge TEXT* (`t.contains("restated")`), which is E's rendering — D only exposes the counter. The COUNTER behaviour it relies on is already gated by `s_d1` (which asserts `stale_plan_streak >= D_T1`, no text).

- [ ] **Step 6: Run the S-D scenarios to verify they pass**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager --lib job_engine::tests::scenarios::s_d`
Expected: PASS for `s_d1_plan_stall_t1_sets_the_nudge_flag`, `s_d2`, `s_d3`, `s_d4`, `s_d5`, `s_d6`, `s_d7_marker_less_finish_rechecks_and_counts`, `s_d8`, `s_d9`. The C/E ignored tests still report as ignored.

Walk-through of the chosen thresholds against the loops:
- `s_d1` (`1..=D_T1+1` = 4 identical reports, `last_status` seeded `None`): streak 0,1,2,3 → reaches T1=3, all ticks `Monitoring` (3 < T2=6). Asserts `stale_plan_streak >= 3`. ✓
- `s_d2` (`1..=D_T2+1` = 7): streak 0..6 → reaches T2=6 on the 7th → `Stuck` + `Blocked`. ✓
- `s_d5` (monitoring, `1..=D_T2+1` = 7): same accrual pre-match → escalates. ✓
- `s_d3` (`1..=D_T1` then a changed plan): streak 0,1,2 → reset to 0. ✓
- `s_d4` (moving status, `1..=D_T2+2` = 8): both-fields rule keeps streak at 0, all `Monitoring`. ✓
- `s_d6` (`1..=D_T1` working, then Blocked+answer): streak 0,1,2; the terse `BLOCKED_HARD` leaves it at 2; `on_blocked` resets to 0. ✓
- `s_d7` (one relaxed tick): `marker_less_rechecks` 1, `Monitoring`, no nudge. ✓
- `s_d8` (`1..=D_K` = 3): counter 1,2,3 → escalates on the 3rd → `Stuck`. ✓
- `s_d9` (relaxed path, 2 rechecks): counter 1,2 (< K=3), no escalation, no nudge. ✓

Non-vacuity: the scenario runner ships its own negative controls (`assert_outcome_panics_on_a_wrong_expected_tick`, `runner_matches_a_hand_driven_equivalent`) proving the harness can FAIL; each S-D asserts a concrete counter value / tick kind that a broken impl moves.

- [ ] **Step 7: Commit**

```bash
git add src/job_engine/tests/scenarios.rs
git commit -m "test(ledger-d): un-ignore + reconcile s_d1..s_d9 (T2=6, s_d8 loop, s_d9 setup); drop superseded [BASE] controls"
```

---

### Task 6: Green gate — full suite, clippy, fmt

**Files:** none (verification only).

- [ ] **Step 1: Run the full test suite**

Run: `ECC_GATEGUARD=off cargo test -p agent-manager 2>&1 | tail -n 40`
Expected: all tests pass (including the un-ignored `s_d1..s_d9`, the new unit tests, and every pre-existing test — especially the M86 firewall `the_nudge_is_a_pure_function_of_agent_authored_inputs` in `job_engine/tests/nudge.rs`, which must stay green: D adds no prose to the nudge, so a leak of `stale_plan_streak`/`marker_less_rechecks` into the delivered bytes would fail it).

- [ ] **Step 2: Clippy clean**

Run: `ECC_GATEGUARD=off cargo clippy -p agent-manager --all-targets -- -D warnings 2>&1 | tail -n 40`
Expected: no warnings. (Confirm no dead-code const: D defines only `DEFAULT_STALE_PLAN_STALL` and `MARKER_LESS_RECHECK_MAX`, both used; T1 is documented, not a const.)

- [ ] **Step 3: rustfmt clean**

Run: `ECC_GATEGUARD=off cargo fmt -p agent-manager -- --check`
Expected: no diff (run `ECC_GATEGUARD=off cargo fmt -p agent-manager` and re-commit if it reformats).

- [ ] **Step 4: Commit any fmt/clippy fixups**

```bash
git add -A
git commit -m "chore(ledger-d): rustfmt + clippy clean"
```

---

## Self-Review

**1. Spec coverage (Milestone D bullets + LOCKED DECISIONS D-8/9/10 + S-D1..9):**

| Requirement | Task |
|---|---|
| `stale_plan_streak: u32` field, `#[serde(default)]` after `continuations`; `fresh`; round-trip | Task 1 (Steps 1,3,4,5) |
| marker-less-recheck counter field, round-trip | Task 1 (added alongside; exercised Tasks 3–4) |
| Accrual in the keep-prior block; both-fields (status AND next_step); trim both sides / store trimmed | Task 1 (Step 8) |
| Reset on a changed plan | Task 1 (Step 8, else branch) — `s_d3`, `changing_the_plan_resets_the_streak` |
| Terse bump neither bumps nor resets | Task 1 — `a_terse_bump_neither_bumps_nor_resets` |
| Both-fields prevents moving-status accrual | Task 1 — `same_plan…`, `s_d4` |
| T2 escalation at the SHARED pre-match location, Working AND Monitoring, via `park_stuck_kind(WorkerStuck)` | Task 2 — `repeated_plan…`, `monitoring_state…`, `s_d2`, `s_d5` |
| `on_blocked` reset (beside `continuations = 0`) | Task 2 (streak) + Task 4 (marker-less) — `human_answer_resets…`, `s_d6` |
| `retime` reset (LOCKED D-10) | Task 2 — `retime_resets…` |
| FO-2 `turn_finished_since_nudge()` predicate + relaxed `awaiting_report` gate | Task 3 — `a_completed_turn_rechecks…`, `no_turn_signal…`, `a_turn_still_in_flight…`, `s_d7`, `s_d9` |
| Bounded marker-less recheck, no blind nudge | Task 3 — `s_d7` |
| Escalate-after-K WorkerStuck (fast backstop) | Task 4 — `marker_less_recheck_escalates_after_k`, `s_d8` |
| Reset marker-less counter on accepted bump + `on_blocked` | Task 4 — `an_accepted_marker_bump…`, `human_answer_resets_the_marker_less_counter…` |
| `decide_kind` stays pure (WorkerStuck via `park_stuck_kind` only) | All escalation paths route through `park_stuck_kind`; no `policy.rs` change |
| T1 exposes a signal only; D adds no nudge prose | No nudge edits in D; firewall test guarded in Task 6 |
| Un-ignore + reconcile `s_d1..s_d9`; keep nudge-text `s_e3` for E | Task 5 |
| Green gate (cargo test, clippy, fmt) | Task 6 |

No gaps.

**2. Placeholder scan:** every code step contains actual Rust (fields, `fresh` inits, the accrual `match`, the T2 `if`, the predicate, `marker_less_recheck`, the relaxed gate, the K branch, the three resets) and actual test bodies with concrete asserts. No "TBD"/"add validation"/"similar to Task N". `dispose_plan` / `nudged_then_awaiting` helpers are given in full at first use.

**3. Type/threshold-name consistency:** field names `stale_plan_streak` / `marker_less_rechecks` are used identically in `job.rs`, `marker.rs`, `drive.rs`, `stops.rs`, and the `ledger_u64("stale_plan_streak"|"marker_less_rechecks")` scenario reads. Const names `DEFAULT_STALE_PLAN_STALL` (marker.rs, T2=6) and `MARKER_LESS_RECHECK_MAX` (drive.rs, K=3) match their `use super::super::…` imports in the tests. `StopKind::WorkerStuck` (confirmed a real variant, `pmstate/mod.rs:105`) and `park_stuck_kind` (confirmed `pub(super)`, already called from `drive.rs:558`) are correct. Scenario const `D_T2` reconciled 5→6 to equal `DEFAULT_STALE_PLAN_STALL`; `D_T1`/`D_K` unchanged and consistent with the loop walk-throughs.

Fix applied inline during review: the original `s_d8` loop `1..=(D_K+1)` would leave `last == Escalated` (the extra post-park tick routes to `on_blocked`); reconciled to `1..=D_K` in Task 5 Step 3. The two `[BASE]` controls D invalidates are removed in Task 5 Step 2 (found by tracing the accrual against `s_d1_base`/`s_d7_base`).
