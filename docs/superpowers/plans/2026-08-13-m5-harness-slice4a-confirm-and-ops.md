# M5 Harness Slice 4a — Confirm-Answer Path + Idempotency Journal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the native loop's terminus — a human's `[A]/[B]/[C]` answer to the `confirm_done` stop is applied by the harness (Confirming→Done / Confirming→Research / stay), so a native project can actually reach **Done** — and add `ops.rs`, the idempotency journal that makes external mutations happen at most once across crashes.

**Architecture:** Two independent, fully-testable additions on top of the shipped Slice 3 phase engine. Neither needs `gh` (the GitHub CR phase is Slice 4b, deferred until `gh` is available). The confirm-answer path plugs into the EXISTING escalation-resume seam in `PhaseScheduler::on_escalated`: today a resolved `confirm_done` stop wrongly re-dispatches another Confirming worker (which just re-parks, so Done is unreachable); instead, when the resolved park is the Confirming `confirm_done` stop, the harness maps the human's answer to a `ConfirmDecision` and applies it directly via a new `apply_confirm` (no worker). `ops.rs` is standalone infra (`Operation` records already exist in `pmstate::ProjectState.operations`).

**Tech Stack:** Rust 2024; `serde`/`serde_json`; `anyhow`; the shipped `phase`/`pmstate`/`phase_engine`/`state` modules.

## Global Constraints

- **Harness-disposes / validate-before-save.** Any `ProjectState` the confirm path produces MUST pass `ProjectState::validate()` before `pmstate::save`. `Confirming→Done` sets `phase==Done` AND `posture==Done`, `active==false`, all handles null, `next_check==None`, stops cleared (the `phase==Done ⇔ posture==Done` invariant).
- **`confirm_done` is human-gated and hard.** A worker never self-accepts. Only a human `Answer` on the `confirm_done` stop (recorded at/after the park `since`) supplies the decision. Silence / `[C] not yet` never advances.
- **Reuse the existing recency guard.** The confirm answer counts only if `answered_at >= since` (same stale-answer protection `on_escalated` already applies).
- **`ops.rs` monotonicity.** An operation goes `pending → {completed, failed}` and never backward; `pending` is persisted BEFORE the (future) network call; a key resolves the same intent to one record (reconcile-before-retry). Pure logic + serde; no network in this slice.
- **Back-compat.** All shipped tests stay green; the non-Confirming escalation-resume behavior (ambiguity/worker_stuck → re-dispatch same phase with answers as prompt `extra`) is unchanged.

---

## File Structure

- Modify `src/phase.rs` — add `ConfirmDecision::from_answer(&str) -> Option<ConfirmDecision>` (parse `[A]/[B]/[C]` / accept/new-direction/not-yet).
- Modify `src/phase_engine.rs` — add `apply_confirm(cur, decision, now) -> Disposition`; branch `on_escalated` for a resolved Confirming `confirm_done` stop.
- Create `src/ops.rs` — `Operation` journal helpers (key derivation, begin/complete/fail, reconcile lookup). Modify `src/lib.rs` to register it.

---

### Task 1: Confirm-answer path — a native project can reach Done

**Files:**
- Modify: `src/phase.rs` (`ConfirmDecision::from_answer`)
- Modify: `src/phase_engine.rs` (`apply_confirm` + `on_escalated` branch + tests)

**Interfaces:**
- Produces:
  - `impl ConfirmDecision { pub fn from_answer(s: &str) -> Option<ConfirmDecision> }` — case-insensitive, trims; maps `a`/`accept`/`[a]`/`yes` → `Accept`, `b`/`new direction`/`new_direction`/`[b]` → `NewDirection`, `c`/`not yet`/`not_yet`/`[c]` → `NotYet`; anything else → `None`.
  - `fn apply_confirm(cur: &ProjectState, decision: ConfirmDecision, now: Epoch) -> Disposition` (module-private; unit-tested).
- Consumes: `phase::{Phase, ConfirmDecision, Guards, can_transition}`, `pmstate::*`, the existing `finalize`/side-effect helpers.

**`apply_confirm` behavior:**
- Build `Guards { plan_exhausted: cur.task_cursors.is_empty(), all_crs_terminal: cur.crs.iter().all(|c| !c.is_open()), fire_condition_holds: <plan_exhausted && all_crs_terminal>, confirm: Some(decision), ..Default::default() }`.
- `Accept`: if `can_transition(Confirming, Done, &g)` → build `next` = Confirming→Done via the existing `(Confirming,Done)` side effect (posture Done, active=false, null handles, clear open_stops + review_state, next_check=None); `Outcome::Done`. (If not legal — e.g. fire-condition false — fall through to re-park rather than forcing Done.)
- `NewDirection`: `can_transition(Confirming, Research, &g)` → Confirming→Research reopen (fresh working state; clear stops/review_state/task_cursors/crs as the existing reopen side effect does); `Outcome::Advanced(Research)`.
- `NotYet`: no transition — keep phase Confirming, keep the `confirm_done` stop OPEN and posture `NeedsYou` (re-park); `Outcome::Escalated([confirm_done_id])`.
- Always route the built `next` through `finalize` (validate-or-fail-safe), so an invalid state is never returned.

**`on_escalated` branch (the wiring):**
When `!still_blocking` (every blocking stop answered) AND `state.phase == Phase::Confirming` AND the answered set includes an open `confirm_done` stop:
- Find the latest `Answer` (by `answered_at`, `>= since`) whose `stop_id` is that confirm_done stop.
- `ConfirmDecision::from_answer(&answer.answer)`:
  - `Some(decision)` → `let disp = apply_confirm(state, decision, now); self.apply_disposition(disp, now)` (NO worker spawn).
  - `None` (unparseable answer) → treat as still parked: return `PhaseTick::Escalated(stop_ids)` and DO NOT resolve/spawn (an ambiguous confirm answer must not silently advance or spin a worker).
- Otherwise (non-Confirming, or non-confirm_done stop): the EXISTING behavior (re-dispatch the same phase with `answers_extra` as prompt `extra`) — unchanged.

**Steps:**
- [ ] **Step 1:** Write failing tests for `ConfirmDecision::from_answer` (A/B/C, words, brackets, case, garbage→None). Implement. Green.
- [ ] **Step 2:** Write failing `apply_confirm` unit tests: Accept from a Confirming state with empty cursors/crs ⇒ `Outcome::Done`, `next.phase==Done`, `next.run.posture==Done`, `!active`, handles null, stops cleared, `validate()` Ok; NewDirection ⇒ `Advanced(Research)`, fresh working, cursors/crs/stops cleared, `validate()` Ok; NotYet ⇒ `Escalated`, phase stays Confirming, confirm_done stop still open, `validate()` Ok.
- [ ] **Step 3:** Implement `apply_confirm`. Green.
- [ ] **Step 4:** Write a failing `PhaseScheduler`-level test (FakeDriver/FakeClock): seed a Confirming project parked on a `confirm_done` stop (posture NeedsYou); write an `Answer{stop_id, answer:"A", answered_at >= since}`; one `tick` ⇒ `PhaseTick::Done`, `state.json` phase==Done, and NO worker spawned (`driver.spawn_count()==0`). Add a `"B"` variant ⇒ `Advanced(Research)` (a Research worker then spawns); a `"C"` variant ⇒ stays `Escalated`, no spawn; a garbage-answer variant ⇒ stays `Escalated`, no spawn.
- [ ] **Step 5:** Implement the `on_escalated` Confirming branch. Green. Keep every existing PhaseScheduler/daemon test green (non-Confirming resume unchanged).
- [ ] **Step 6:** `cargo test`, `cargo clippy --all-targets`, `cargo fmt --all`. Green.
- [ ] **Step 7:** Commit: `feat(harness): apply human confirm decision so a native project reaches Done (slice 4a task 1)`.

---

### Task 2: `ops.rs` — idempotency journal

**Files:**
- Create: `src/ops.rs`
- Modify: `src/lib.rs` (register `pub mod ops;` alphabetically: clock, daemon, escalation, lease, ops, phase, ...)

**Interfaces:** (operate on the EXISTING `pmstate::{Operation, OpStatus}` and `ProjectState.operations`)
- `pub fn op_key(kind: &str, parts: &[&str]) -> String` — deterministic key: `kind` + `:` + parts joined by `+`, each part `%`-encoded for `+`/`:`/`%`/space (a stable, injective encoding; document the scheme). E.g. `op_key("draft-cr", &["auth+api", "slice-1"])`.
- `pub fn find(ops: &[Operation], key: &str) -> Option<&Operation>` — the existing record for a key (reconcile-before-retry lookup).
- `pub fn begin(ops: &mut Vec<Operation>, id: &str, key: &str, now: Epoch) -> BeginOutcome` where `pub enum BeginOutcome { Fresh, AlreadyPending, AlreadyCompleted, RetryAfterFailed }` — if no record: push `Operation{ id, key, status: Pending, started_at: now }` → `Fresh`; if a record exists: return its status mapped (Pending→AlreadyPending, Completed→AlreadyCompleted, Failed→RetryAfterFailed) WITHOUT duplicating. (The caller performs the network action only on `Fresh`/`RetryAfterFailed`.)
- `pub fn complete(ops: &mut Vec<Operation>, key: &str) -> bool` / `pub fn fail(ops: &mut Vec<Operation>, key: &str) -> bool` — flip the record's status monotonically (Pending→Completed / Pending→Failed; Completed never regresses — a `complete` on Completed is a no-op success; a `fail` on Completed returns false / leaves it Completed). Return whether a live transition occurred.

**Design notes:** keys are the anti-double-action anchor; `op_key`'s encoding must be injective so two distinct intents never collide (that is why parts are `%`-encoded before joining). No network, no time source beyond the passed `now`. Pure and exhaustively unit-testable.

**Steps:**
- [ ] **Step 1:** Write failing tests: `op_key` deterministic + injective (two different part-lists never collide, incl. a part containing `+`); `begin` on a fresh vec pushes one Pending → `Fresh`; a second `begin` with the same key → `AlreadyPending`, no duplicate; `complete` flips Pending→Completed and re-`begin` → `AlreadyCompleted`; `fail` flips Pending→Failed and re-`begin` → `RetryAfterFailed`; monotonicity: `fail` after `complete` does not regress.
- [ ] **Step 2:** Run, verify fail.
- [ ] **Step 3:** Implement `ops.rs` + register the module. Green.
- [ ] **Step 4:** `cargo test`, `clippy`, `fmt`. Green.
- [ ] **Step 5:** Commit: `feat(harness): ops.rs idempotency journal (begin/complete/fail + injective keys) (slice 4a task 2)`.

---

## Self-Review

- **Spec coverage:** completes §4's Confirming→Done/Research edges (human-gated) and §9's operations idempotency journal. §12.4's `gh` CR phase is Slice 4b (deferred — `gh` absent).
- **Type consistency:** `ConfirmDecision`/`Guards`/`can_transition`/`Disposition`/`Outcome`/`Operation`/`OpStatus` match the shipped Slice 1–3 code.
- **Risk focus:** validate-before-save on the new Done state (phase⟂posture); confirm is human-only (never worker); an unparseable confirm answer never advances or spins; `ops` keys injective so no double external action later; back-compat of the non-Confirming resume path.
