# Worker Lane + Evaluation Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the heartbeat nudge echo the agent's OWN last status + a new agent-authored `next_step` verbatim (a "briefing off your own plan," not pmd re-teaching), while every other part of the prompt stays byte-stable — and stand up the evaluation bar (purity/firewall, next_step echo, bounded-ledger-over-many-ticks, restart survival, real-tmux end-to-end) that tells us the change is good.

**Architecture:** Add `next_step: Option<String>` to `WakeReport` (agent-authored) and mirror it into `AgentLoopState.last_plan` in the disposer with keep-prior-when-omitted. Thread `last_status` + `last_plan` from `nudge()` into `loop_nudge_prompt`, which PREPENDS a small echo block into ONLY the `## What to do now` section when either is present (static bullets stay). Everything else in the prompt stays static. The nudge remains a pure function of `(brief, pending_context, last_status, last_plan, marker_path)` — never of any ledger/counter/events/decisions field — pinned by a firewall test.

**Tech Stack:** Rust 2024; serde/serde_json; `cargo test` (unit/render over `FakeDriver`+`FakeClock`); real-tmux `tests/integration/` target (`#[ignore]`d acceptance).

## Global Constraints

- `WakeReport` (src/job.rs:476-517) and `AgentLoopState` (src/job.rs:50-158) both carry `#[serde(deny_unknown_fields)]`. Every NEW field MUST be `#[serde(default)]` and MUST be added to `AgentLoopState::fresh()` (src/job.rs:436-455) and to the full-struct-literal in `agent_loop_state_round_trips_full_and_minimal` (src/job.rs:562-598) or the crate will not compile / the round-trip test will fail.
- The static prompt prose pinned by `loop_nudge_prompt_writes_the_marker_instruction` (src/job_engine/tests/nudge.rs:498) and `loop_nudge_prompt_sanctions_confirm_done_without_allowing_unilateral_completion` (src/job_engine/tests/nudge.rs:520) and `loop_nudge_prompt_with_no_goal_forbids_inventing_one` (nudge.rs:551) MUST keep passing. The static `## What to do now` bullets ("Slack MCP", "harness sends no messages") and every `## You do not decide when the project is finished` / `## Signal a decision point` line stay verbatim — the echo is ADDITIVE (prepended within the section), never a replacement.
- The nudge text must be a pure function of AGENT-authored inputs only: `brief` + `pending_context` + `last_status` + `last_plan` (+ the static marker path). NO harness/ledger/counter/events/decisions/situation content may enter the nudge. This is the firewall (Task 6).
- Run commands: `cargo test` (unit+render+fake integration), `cargo clippy --all-targets` (must be clean), `cargo build`. Real-tmux acceptance: `cargo test --test integration -- --ignored --test-threads=1`. Plain `cargo`, no brazil wrapper. Run all with `ECC_GATEGUARD=off` in the env.
- Commit after every task (frequent commits). Branch off `main` before Task 1 (do not commit to `main` directly).

---

## File Structure

- **src/job.rs** — add `WakeReport.next_step` (agent-authored) and `AgentLoopState.last_plan` (harness mirror); update `fresh()`; extend inline tests.
- **src/job_engine/marker.rs** — in `dispose_report`, before the `match report.state`, mirror `next_step` → `last_plan` with keep-prior (beside the existing `next.last_status = report.status.clone();` at line 137).
- **src/job_engine/nudge.rs** — change `loop_nudge_prompt` signature + prepend the echo block into `## What to do now`; add the `next_step` line to the machine-report schema instruction so the agent knows to write it; update the sole call site in `nudge()`.
- **src/job_engine/tests/nudge.rs** — echo test, purity/firewall test, static-prose-unchanged (existing tests updated for the new signature), bounded-over-many-ticks eval, restart-survival eval.
- **tests/integration/** — an `#[ignore]`d acceptance test proving the nudge echoes `next_step` end-to-end (extend the marker-writing claude stub).

Each task ends with an independently testable deliverable + commit.

---

### Task 1: Add `next_step` to `WakeReport` (agent-authored field)

**Files:**
- Modify: `src/job.rs:476-517` (WakeReport struct)
- Test: `src/job.rs` inline `mod tests` (~741-794)

**Interfaces:**
- Produces: `WakeReport.next_step: Option<String>` — the agent's one-line intended next action, `#[serde(default)]` so old markers still parse and a terse `{state:"working"}` bump is unchanged.

- [ ] **Step 1: Write the failing test** — extend the marker-parse test in `src/job.rs` tests:

```rust
#[test]
fn wake_report_parses_next_step() {
    // A working bump that carries the agent's own next step.
    let r: WakeReport = serde_json::from_str(
        r#"{"state":"working","seq":7,"status":"tests running","next_step":"wire the alert if green"}"#,
    )
    .unwrap();
    assert_eq!(r.next_step.as_deref(), Some("wire the alert if green"));
    // A terse bump with no next_step still parses, defaulting to None.
    let terse: WakeReport = serde_json::from_str(r#"{"state":"working"}"#).unwrap();
    assert_eq!(terse.next_step, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ECC_GATEGUARD=off cargo test --lib wake_report_parses_next_step`
Expected: FAIL — `unknown field 'next_step'` (deny_unknown_fields rejects it until declared).

- [ ] **Step 3: Add the field** — in `src/job.rs` WakeReport (after `status`, before `conversation_id`):

```rust
    #[serde(default)]
    pub status: Option<String>,
    /// The agent's own one-line plan for its next wake. Echoed VERBATIM back into
    /// the nudge's "What to do now" — a briefing off the agent's own words, never
    /// pmd re-teaching. Optional: a terse bump omits it and behaves exactly as before.
    #[serde(default)]
    pub next_step: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test --lib wake_report_parses_next_step`
Expected: PASS. Also run `cargo test --lib wake_report` to confirm `wake_report_rejects_unknown_fields` and `wake_report_parses_each_state_and_minimal` still pass.

- [ ] **Step 5: Commit**

```bash
git add src/job.rs
git commit -m "feat(job): add agent-authored WakeReport.next_step field"
```

---

### Task 2: Add `last_plan` to `AgentLoopState` (harness mirror)

**Files:**
- Modify: `src/job.rs:50-158` (struct), `src/job.rs:436-455` (`fresh()`)
- Test: `src/job.rs` inline `agent_loop_state_round_trips_full_and_minimal` (~537-606)

**Interfaces:**
- Consumes: nothing new.
- Produces: `AgentLoopState.last_plan: Option<String>` — the last non-empty `next_step` the agent reported, mirrored by the disposer (Task 3), read by the nudge (Task 4). `#[serde(default)]`.

- [ ] **Step 1: Write the failing test** — extend the FULL-struct-literal round-trip test to set and assert `last_plan`. In `agent_loop_state_round_trips_full_and_minimal`, add `last_plan: Some("wire the alert".into()),` to the constructed literal and assert it survives the round-trip:

```rust
    // ... inside the fully-populated AgentLoopState { ... } literal:
    last_status: Some("tests running".into()),
    last_plan: Some("wire the alert".into()),   // NEW
    // ... after the round-trip assert_eq!(parsed, original) already present, add:
    assert_eq!(parsed.last_plan.as_deref(), Some("wire the alert"));
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ECC_GATEGUARD=off cargo test --lib agent_loop_state_round_trips_full_and_minimal`
Expected: FAIL — struct has no field `last_plan` (compile error).

- [ ] **Step 3: Add the field** — in `AgentLoopState` (after `last_status`, before `continuations`):

```rust
    #[serde(default)]
    pub last_status: Option<String>,
    /// The agent's last reported `next_step` (mirror of WakeReport.next_step),
    /// kept-prior when a report omits it. Echoed verbatim into the nudge.
    #[serde(default)]
    pub last_plan: Option<String>,
    #[serde(default)]
    pub continuations: u32,
```

And in `fresh()` (src/job.rs:436-455), add `last_plan: None,` beside `last_status: None,`.

- [ ] **Step 4: Run test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test --lib agent_loop_state_round_trips_full_and_minimal`
Expected: PASS. Also `cargo test --lib old_json_missing_marker_fields_loads_with_defaults` (a ledger without `last_plan` must still load, defaulting to None).

- [ ] **Step 5: Commit**

```bash
git add src/job.rs
git commit -m "feat(job): add AgentLoopState.last_plan mirror field"
```

---

### Task 3: Disposer mirrors `next_step` → `last_plan` (keep-prior)

**Files:**
- Modify: `src/job_engine/marker.rs:120-144` (`dispose_report`, the cross-cutting mutations before the `match report.state`)
- Test: `src/job_engine/tests/marker.rs`

**Interfaces:**
- Consumes: `WakeReport.next_step` (Task 1), `AgentLoopState.last_plan` (Task 2).
- Produces: after any disposed marker, `ledger.last_plan == report.next_step` when the report carried one, else the PRIOR `last_plan` (keep-prior). Applies to every WakeState arm because it is set before the match, exactly like `last_status`.

- [ ] **Step 1: Write the failing test** — in `src/job_engine/tests/marker.rs`, using the existing `marker_fx`/`write_marker`/`backdate_marker`/`ledger` helpers (src/job_engine/tests/mod.rs):

```rust
#[test]
fn next_step_is_mirrored_and_kept_when_a_later_report_omits_it() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_s| {});
    // First report carries a next_step.
    write_marker(&fx, r#"{"state":"working","seq":10,"status":"a","next_step":"wire the alert"}"#);
    backdate_marker(&fx, 2);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).last_plan.as_deref(), Some("wire the alert"));
    // Second report omits next_step -> the prior plan is KEPT, not cleared.
    write_marker(&fx, r#"{"state":"working","seq":11,"status":"b"}"#);
    backdate_marker(&fx, 1);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).last_plan.as_deref(), Some("wire the alert"));
    assert_eq!(ledger(&fx).last_status.as_deref(), Some("b"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ECC_GATEGUARD=off cargo test next_step_is_mirrored_and_kept_when_a_later_report_omits_it`
Expected: FAIL — `last_plan` is `None` after the first tick (disposer does not set it yet).

- [ ] **Step 3: Implement the mirror** — in `dispose_report`, next to the existing `next.last_status = report.status.clone();` (marker.rs:137):

```rust
    next.last_marker_seq = report.seq;
    next.last_status = report.status.clone();
    // Keep-prior: only overwrite the plan when THIS report restated it, so an agent
    // that reports progress without repeating its plan does not lose its orientation.
    if let Some(step) = report.next_step.clone() {
        next.last_plan = Some(step);
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test next_step_is_mirrored_and_kept_when_a_later_report_omits_it`
Expected: PASS. Run `cargo test marker` to confirm the disposer suite is green.

- [ ] **Step 5: Commit**

```bash
git add src/job_engine/marker.rs src/job_engine/tests/marker.rs
git commit -m "feat(marker): mirror next_step into last_plan, keep-prior when omitted"
```

---

### Task 4: `loop_nudge_prompt` echoes `last_status` + `last_plan` (static fallback)

**Files:**
- Modify: `src/job_engine/nudge.rs:219` (signature) + the `## What to do now` block inside the `format!` (nudge.rs:241-248) + the call site in `nudge()` (nudge.rs:149)
- Test: `src/job_engine/tests/nudge.rs`

**Interfaces:**
- Consumes: `AgentLoopState.last_status`, `AgentLoopState.last_plan`.
- Produces: `loop_nudge_prompt(brief: &str, extra: &str, last_status: &str, last_plan: &str, marker_path: &Path) -> String`. When `last_status` or `last_plan` is non-empty, an echo block is PREPENDED inside `## What to do now`, above the (unchanged) static bullets. When both are empty (first wake / pre-upgrade), the section is byte-identical to today.

- [ ] **Step 1: Write the failing test** — in `src/job_engine/tests/nudge.rs`:

```rust
#[test]
fn loop_nudge_prompt_echoes_the_agents_own_plan_verbatim() {
    let marker = std::path::Path::new("/tmp/p/.project-state/sessions/s/needs-you.json");
    // With a status + plan, both are echoed verbatim inside "What to do now",
    // AND the static bullets remain.
    let p = loop_nudge_prompt("Ship search.", "", "tests running", "wire the alert if green", marker);
    assert!(p.contains("tests running"), "echoes last_status verbatim");
    assert!(p.contains("wire the alert if green"), "echoes last_plan verbatim");
    assert!(p.contains("your OWN plan"), "frames it as the agent's own plan, not a new order");
    assert!(p.contains("Use YOUR OWN tools"), "static operating bullets remain");
    // With NO status and NO plan (first wake), the section is the static form:
    // the echo framing is absent, the bullets are present.
    let first = loop_nudge_prompt("Ship search.", "", "", "", marker);
    assert!(!first.contains("your OWN plan"), "no echo block on the first wake");
    assert!(first.contains("Use YOUR OWN tools"), "bullets present on the first wake");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ECC_GATEGUARD=off cargo test loop_nudge_prompt_echoes_the_agents_own_plan_verbatim`
Expected: FAIL — signature mismatch (function takes 3 args, test passes 5).

- [ ] **Step 3: Change the signature + prepend the echo block.** In `src/job_engine/nudge.rs`, change the signature at line 219 to:

```rust
pub(super) fn loop_nudge_prompt(
    brief: &str,
    extra: &str,
    last_status: &str,
    last_plan: &str,
    marker_path: &Path,
) -> String {
```

Build the echo string BEFORE the `format!`, then interpolate it as a new `{whatnow_echo}` slot placed at the top of the `## What to do now` section (immediately after the `## What to do now\n\n` heading and before the existing `- Continue working the goal.` bullet). Keep the bullets exactly as-is:

```rust
    // Dynamic, agent-authored briefing prepended into "What to do now". Empty on the
    // first wake (no report yet) -> the section is byte-identical to the old static form.
    // VERBATIM: last_status / last_plan are trimmed only, never summarized or rewritten.
    let whatnow_echo = if last_status.trim().is_empty() && last_plan.trim().is_empty() {
        String::new()
    } else {
        let mut e = String::from(
            "You are picking up your OWN plan — a reminder, not a new instruction; you still \
             hold full context, so re-derive if things changed.\n\n",
        );
        if !last_status.trim().is_empty() {
            e.push_str(&format!("Last wake you reported: \"{}\"\n", last_status.trim()));
        }
        if !last_plan.trim().is_empty() {
            e.push_str(&format!("Your next step, in your words: \"{}\"\n", last_plan.trim()));
        }
        e.push('\n');
        e
    };
```

In the `format!` body, the `## What to do now` section becomes (only the `{whatnow_echo}` insertion is new; the bullets are unchanged):

```
## What to do now

{whatnow_echo}- Continue working the goal. Do whatever is pending or needs attention right now.
- Use YOUR OWN tools to do the work and to communicate — including your Slack MCP ...
```

Then update the SOLE caller in `nudge()` (nudge.rs:149):

```rust
    let brief = std::fs::read_to_string(self.paths.brief()).unwrap_or_default();
    let extra = base.pending_context.clone().unwrap_or_default();
    let last_status = base.last_status.clone().unwrap_or_default();
    let last_plan = base.last_plan.clone().unwrap_or_default();
    let text = loop_nudge_prompt(&brief, &extra, &last_status, &last_plan, &self.paths.needs_you());
```

Finally, update the THREE existing test call sites that use the 3-arg form (nudge.rs:~499, ~521, ~552) to pass `"", ""` for the two new params, e.g. `loop_nudge_prompt("Ship vector search.", "", "", "", marker)`.

- [ ] **Step 4: Run test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test loop_nudge_prompt`
Expected: PASS for the new test AND the three static-prose tests (`..._writes_the_marker_instruction`, `..._sanctions_confirm_done...`, `..._with_no_goal_forbids_inventing_one`) — the bullets and every static line are unchanged.

- [ ] **Step 5: Commit**

```bash
git add src/job_engine/nudge.rs src/job_engine/tests/nudge.rs
git commit -m "feat(nudge): echo the agent's own status+next_step in 'What to do now'"
```

---

### Task 5: Tell the agent to write `next_step` (schema instruction)

**Files:**
- Modify: `src/job_engine/nudge.rs:273-308` (the machine-report schema `push_str` block)
- Test: `src/job_engine/tests/nudge.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: the nudge's `## Signal a decision point` schema now documents `next_step`, so the agent actually emits it. Additive to the schema literal; does not disturb the pinned `"seq"`/`"state"`/`working`/`monitoring`/`blocked` substrings.

- [ ] **Step 1: Write the failing test** — in `src/job_engine/tests/nudge.rs`:

```rust
#[test]
fn loop_nudge_prompt_asks_the_agent_to_record_next_step() {
    let marker = std::path::Path::new("/tmp/p/.project-state/sessions/s/needs-you.json");
    let p = loop_nudge_prompt("Ship search.", "", "", "", marker);
    assert!(p.contains("\"next_step\""), "the schema documents the next_step field");
    assert!(p.contains("one line"), "explains it is a single line the agent will see next wake");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ECC_GATEGUARD=off cargo test loop_nudge_prompt_asks_the_agent_to_record_next_step`
Expected: FAIL — `"next_step"` absent from the schema block.

- [ ] **Step 3: Add the schema line** — inside the raw-string JSON schema in the `push_str` block (near `"status"`), add:

```
    "status": "<one-line human-facing status>",
    "next_step": "<one line: what you intend to do on your NEXT wake — you will see
                   this quoted back to you verbatim, so write it for your future self>",
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test loop_nudge_prompt`
Expected: PASS, all `loop_nudge_prompt*` tests green.

- [ ] **Step 5: Commit**

```bash
git add src/job_engine/nudge.rs src/job_engine/tests/nudge.rs
git commit -m "feat(nudge): document next_step in the machine-report schema"
```

---

### Task 6: EVALUATION — the firewall (nudge is pure over agent-authored inputs only)

**Files:**
- Test: `src/job_engine/tests/nudge.rs`

**Interfaces:**
- Consumes: `loop_nudge_prompt` (Task 4), `JobScheduler::nudge` via the `setup`/`tick_confirmed`/`sent_keys` fixture.

- [ ] **Step 1: Write the test** — the guardrail-as-test: the delivered nudge must not change when NON-agent-authored ledger fields change. In `src/job_engine/tests/nudge.rs`:

```rust
#[test]
fn the_nudge_is_a_pure_function_of_agent_authored_inputs() {
    // (a) determinism: loop_nudge_prompt is byte-stable for the same inputs.
    let m = std::path::Path::new("/tmp/p/.project-state/sessions/s/needs-you.json");
    let a = loop_nudge_prompt("g", "ctx", "st", "plan", m);
    let b = loop_nudge_prompt("g", "ctx", "st", "plan", m);
    assert_eq!(a, b, "same agent-authored inputs -> byte-identical nudge");

    // (b) closure: varying LEDGER-ONLY fields (events, continuations, watermarks)
    // must NOT change the delivered nudge. Drive two sessions whose brief +
    // pending_context + last_status + last_plan are identical but whose ledger
    // history differs, and assert the sent text is identical.
    let sent_with = |edit: &dyn Fn(&mut AgentLoopState)| -> String {
        let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
        // seed identical agent-authored state, plus a divergent ledger history.
        // (Reuse the fixture's ledger-edit path; see nudge_fires_when_idle_unattached_unlocked
        //  at nudge.rs:200 for the launch+tick_confirmed drive sequence.)
        seed_ledger(&mut fx, |s| {
            s.last_status = Some("st".into());
            s.last_plan = Some("plan".into());
            s.pending_context = Some("ctx".into());
            edit(s); // the divergence
        });
        drive_one_nudge(&mut fx);
        fx.driver.sent_keys().last().expect("a nudge was sent").1.clone()
    };
    let plain = sent_with(&|_s| {});
    let noisy = sent_with(&|s| {
        s.continuations = 7;
        s.last_marker_seq = 999;
        s.nudged_at_seq = Some(998);
        for i in 0..50 { s.record_event(i, AutopilotEventKind::Reported(Some(format!("x{i}")))); }
    });
    assert_eq!(plain, noisy, "no ledger/counter/events content leaks into the nudge");
}
```

> Implementer note: `seed_ledger` / `drive_one_nudge` are illustrative. If the fixture lacks them, inline the equivalent: `setup(...)`, write the seed fields onto the on-disk ledger (`job::save(&fx.paths, &state)` or the fixture's edit helper), run the launch tick, set the pane to `IDLE_PANE`, advance the `FakeClock`, `tick_confirmed(&mut fx)`, then read `fx.driver.sent_keys()`. Keep the two runs' agent-authored inputs byte-identical; only the `edit` differs. Model the drive/observe sequence on `nudge_fires_when_idle_unattached_unlocked` (nudge.rs:200).

- [ ] **Step 2: Run test to verify it passes** (this invariant should already hold — the test PINS it)

Run: `ECC_GATEGUARD=off cargo test the_nudge_is_a_pure_function_of_agent_authored_inputs`
Expected: PASS. If it FAILS, a ledger field is leaking into the prompt — fix `loop_nudge_prompt`/`nudge` so only `{brief, pending_context, last_status, last_plan}` reach the text, then re-run.

- [ ] **Step 3: Commit**

```bash
git add src/job_engine/tests/nudge.rs
git commit -m "test(nudge): pin the firewall — nudge is pure over agent-authored inputs"
```

---

### Task 7: EVALUATION — the ledger stays bounded over many DRIVEN ticks

**Files:**
- Test: `src/job_engine/tests/nudge.rs`

**Interfaces:**
- Consumes: the `setup`/`tick`/`report_progress`/`backdate_marker`/`ledger` fixture; `AUTOPILOT_EVENTS_MAX` (src/job.rs:193).

- [ ] **Step 1: Write the test** — proves the bound holds under real driving, not just direct `record_event` calls (the 530K-regression floor the decider ledger must later preserve):

```rust
#[test]
fn the_ledger_stays_bounded_over_hundreds_of_driven_ticks() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(60));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch
    let mut now = START + LAUNCH_GRACE_S;
    for i in 0..400i64 {
        fx.clock.set(now);
        fx.driver.set_tail(&sess, IDLE_PANE);
        // Alternate a fresh report so the awaiting-report gate reopens and nudges keep firing.
        report_progress(&fx, 1000 + i as u64, 1);
        backdate_marker(&fx, 1);
        let _ = fx.sched.tick(&fx.driver, &fx.clock);
        now += 60 + BUSY_RECHECK_S;
    }
    let led = ledger(&fx);
    assert!(led.events.len() <= AUTOPILOT_EVENTS_MAX, "events feed stays capped");
    let bytes = serde_json::to_string(&led).unwrap().len();
    assert!(bytes < 64 * 1024, "the whole ledger stays well under 64KiB, got {bytes}");
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test the_ledger_stays_bounded_over_hundreds_of_driven_ticks`
Expected: PASS. If the byte budget is exceeded, the events feed or a new field is unbounded — investigate before proceeding.

- [ ] **Step 3: Commit**

```bash
git add src/job_engine/tests/nudge.rs
git commit -m "test(nudge): prove the ledger stays bounded over 400 driven ticks"
```

---

### Task 8: EVALUATION — durable state survives a restart

**Files:**
- Test: `src/job_engine/tests/nudge.rs`

**Interfaces:**
- Consumes: `JobScheduler::new(project_id, root, session_id, engine)` (calls `restore_from_disk`, src/job_engine/mod.rs); the fixture's session id/root.

- [ ] **Step 1: Write the test** — reconstruct the scheduler mid-fixture (the exact restart pattern from supervisor.rs:497) and assert `last_plan` + the watermarks survive and behaviour resumes without a double-dispose:

```rust
#[test]
fn last_plan_and_watermarks_survive_a_restart() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_s| {});
    write_marker(&fx, r#"{"state":"working","seq":42,"status":"a","next_step":"wire the alert"}"#);
    backdate_marker(&fx, 2);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let before = ledger(&fx);
    assert_eq!(before.last_plan.as_deref(), Some("wire the alert"));

    // pmd restarts: reconstruct the scheduler -> restore_from_disk re-reads .project-state.
    fx.sched = crate::job_engine::JobScheduler::new(SESSION_ID, fx.root.clone(), SESSION_ID, Engine::Claude);
    let after = ledger(&fx);
    assert_eq!(after.last_plan.as_deref(), Some("wire the alert"), "plan persisted across restart");
    assert_eq!(after.last_marker_seq, 42, "anti-replay watermark persisted");
    // A re-observation of the SAME marker (seq 42) must not re-dispose (seq <= watermark).
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).last_marker_seq, 42, "no double-dispose after restart");
}
```

> Implementer note: match the exact `JobScheduler::new(...)` argument order/types used at `src/job_engine/tests/supervisor.rs:497`; `SESSION_ID`/`fx.root` come from `src/job_engine/tests/mod.rs`.

- [ ] **Step 2: Run test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test last_plan_and_watermarks_survive_a_restart`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/job_engine/tests/nudge.rs
git commit -m "test(nudge): prove last_plan + watermarks survive a pmd restart"
```

---

### Task 9: EVALUATION — real-tmux end-to-end nudge→report(next_step)→echo

**Files:**
- Modify: `tests/integration/stubs.rs` (extend the marker-writing claude stub to write `next_step`)
- Test: `tests/integration/job_scheduler.rs` (new `#[ignore]`d acceptance test)

**Interfaces:**
- Consumes: `nudge_counting_claude_stub`/`write_stubs` (stubs.rs), `TmuxSocket`/`wait_for_file_contents`/`tmux_available` (probe.rs), `CARGO_BIN_EXE_pmd`.

- [ ] **Step 1: Write the test** — the real-substrate proof (asserts on the pane text the stub receives, never on exit codes). Add an `#[ignore]`d test that starts a real pmd on a private socket driving a session whose stubbed `claude` (a) prints a bare idle prompt, (b) on its FIRST turn writes a `needs-you.json` with `"next_step":"REPLAY-MARKER-abc"`, (c) tees every received prompt to `@TYPED@`; then poll `@TYPED@` for the SECOND prompt (the post-report nudge) and assert it contains the marker:

```rust
#[test]
#[ignore = "real tmux: run with --ignored --test-threads=1"]
fn the_next_nudge_echoes_the_agents_reported_next_step() {
    if !tmux_available() { eprintln!("skipping: no tmux"); return; }
    // ... set up a registry + Autopilot session; put a stub `claude` on PATH via tmuxw
    // whose worker mode writes needs-you.json {"state":"working","seq":<unix_now>,
    // "next_step":"REPLAY-MARKER-abc"} on its first turn and tees each received prompt
    // to @TYPED@. Start CARGO_BIN_EXE_pmd on socket with a short --tick-ms.
    assert!(
        wait_for_file_contents(&typed_path, "REPLAY-MARKER-abc", std::time::Duration::from_secs(30)),
        "the nudge after the agent reported its next_step must echo it verbatim"
    );
    // teardown kills pmd by pid + the socket (RAII TmuxSocket).
}
```

> Implementer note: model the stub extension and the pmd-on-socket harness on `tests/integration/coordinator_loop.rs` (real pmd binary, assert on files) and `nudge_counting_claude_stub` in `stubs.rs`. Single-threaded; self-skip without tmux. Depends on Tasks 1-5, so run it last.

- [ ] **Step 2: Run**

Run: `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 the_next_nudge_echoes_the_agents_reported_next_step`
Expected: PASS after Tasks 1-5.

- [ ] **Step 3: Commit**

```bash
git add tests/integration/stubs.rs tests/integration/job_scheduler.rs
git commit -m "test(accept): real-tmux proof the nudge echoes the reported next_step"
```

---

### Task 10: Green gate — full suite + clippy, then record the milestone

- [ ] **Step 1: Full unit/render/fake suite** — Run: `ECC_GATEGUARD=off cargo test` — Expected: PASS.
- [ ] **Step 2: Lint** — Run: `ECC_GATEGUARD=off cargo clippy --all-targets` — Expected: clean.
- [ ] **Step 3: Real-tmux acceptance** — Run: `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1` — Expected: PASS (the real-run bar).
- [ ] **Step 4: Record the milestone** in `.project-state/` (CURRENT.md posture + progress.md entry + a source-attributed decisions.md entry citing this plan and the design), then merge the green branch to `main`:

```bash
git checkout main && git merge --no-ff <branch> -m "Merge: worker lane (next_step echo) + evaluation bar"
```

---

## Self-Review

**Spec coverage (against the locked design §10 / Q1-Q5):**
- Q1 worker-first → this plan IS the worker lane; decider ledger / supervisor `situation` / Q4 follow-ons are deferred to their own plans (below). ✓
- Q2 verbatim echo → Task 4 echoes `last_status`+`last_plan` trimmed-only; Task 6 pins purity. ✓
- Q3 full ledger + summarize → NOT in this plan (next milestone); Task 7 pins the existing bound as the regression floor the ledger work must preserve. ✓ (scoped out intentionally)
- next_step prerequisite (the design's blocking item) → Tasks 1, 3, 5. ✓
- Firewall as a test → Task 6. ✓
- Evaluation ("know what you do is good") → Tasks 6 (firewall), 7 (bounded), 8 (restart), 9 (real-tmux end-to-end). ✓

**Placeholder scan:** every code/test step carries real code grounded in exact signatures (WakeReport 476-517, AgentLoopState 50-158/436-455, dispose_report 136-137, loop_nudge_prompt 219/241-248/273-308, fixtures in tests/mod.rs). The `> Implementer note` blocks flag where a helper name must be confirmed against the fixture rather than invented — pointers to the exact precedent test, not placeholders for logic.

**Type consistency:** `next_step: Option<String>` (WakeReport) → mirrored to `last_plan: Option<String>` (AgentLoopState) → read as `&str` (unwrap_or_default) into `loop_nudge_prompt(brief, extra, last_status, last_plan, marker_path)`. Consistent across Tasks 1→2→3→4.

## Deferred to follow-up plans (each its own spec → plan)
- **Milestone B — Decider ledger (Q3):** `raw.jsonl` (rotated) + `decisions.md` (markdown, source-attributed) + `state.json` digest counters/decisions-slice/situation, bounded-by-construction in the `dispose_report` write path. Extends Task 7's bound proof to the new artifacts.
- **Milestone C — Equip the decider:** a clamped `situation` field in the supervisor consult (`MAX_SITUATION_BYTES`, modeled on `clamp_goal`/`build_consult_prompt` fencing).
- **Milestone D — Q4 follow-ons:** plan-staleness stall (dedicated counter) + marker-less-finish recheck (turn-signal gated).
