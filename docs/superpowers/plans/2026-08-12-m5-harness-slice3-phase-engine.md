# M5 Harness Slice 3 — Phase Engine + Full Loop (terminal/file comms) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the full native phase machine (intake→…→review reachable now; cr/confirming exercised by unit tests) so a `mode:auto`, coordinator-less project is driven by Rust through real detached `claude`/`codex` phase workers — prompt assembled from the borrowed playbook, each result verified against repository evidence, tier-gated stops raised to the human via pmtui + notifier, and control state written only by the harness.

**Architecture:** The existing `Scheduler` (reference coordinator over `step.json`) is untouched — it stays the M1/M3/M4 integration fixture. A NEW `PhaseScheduler` (in `phase_engine.rs`) drives native-harness projects over `state.json` (`ProjectState`). The daemon routes each `Auto` project by a single rule: **empty `coordinator_cmd` ⇒ `PhaseScheduler`; non-empty ⇒ reference `Scheduler`.** The engine reuses the whole substrate: `worker::build_command`, `tmux::Driver::spawn_step`/`observe` (detached, non-blocking, done-signal), `verify::gather_repo_facts`/`accept_code_phase`, `pmstate` load/save (atomic), `policy` tier decisions, and `lease`. Intake is a pmtui-owned interview (extends the `n` create flow) that writes `config.json` + `brief.md` + a fresh `ProjectState` advanced to `Research`.

**Tech Stack:** Rust 2024 edition; `serde`/`serde_json`; `anyhow`; `ratatui`/`crossterm` (pmtui); tmux CLI substrate; real `claude`/`codex` CLIs (no API key).

## Global Constraints

- **Worker-proposes / harness-disposes.** A worker writes ONLY artifacts + `steps/<seq>.result.json`. The harness alone writes `state.json` (and `answers.json`/`decisions.md`). Never trust a self-claim or exit 0: a code phase advances only on repository evidence (`verify::accept_code_phase`).
- **`phase` ⟂ `posture`.** `ProjectState.phase` (the *what*) and `run.posture` (the *scheduling stance*) are orthogonal; the ONLY coupling is the invariant `phase == Done ⇔ posture == Done`. Every state the engine writes MUST pass `ProjectState::validate()` — call it before every `pmstate::save` and treat a validation error as an engine bug (return `Err`, do not persist).
- **`cr → done` is hard-rejected.** All phase advances go through `phase::can_transition`; an illegal proposed transition is NEVER silently obeyed — it becomes a `worker_stuck` (or `ambiguity`) stop.
- **Hard-stop safety floor at every tier.** Reuse `policy::decide`. `StopKind ∈ {Publish, Merge, ConfirmDone, Stuck, Capability}` is forced to `RiskClass::Hard` (escalates at every tier, incl. autopilot); `{Ambiguity, WorkerStuck, ExpertNeeded}` default to `Medium`. Auto-flow synthesizes an `answered_by="auto_flow"` answer + a source-attributed `decisions.md` note.
- **Non-blocking daemon.** The engine dispatches via the detached tmux substrate and returns immediately; a sweep NEVER blocks on a worker. Reuse the double-spawn guard (spawn at most once per tick, keyed off in-memory `RunState`), timeout enforcement, and crash-recovery-from-disk patterns from `Scheduler`.
- **Back-compat.** All existing tests and the reference coordinator keep passing. The dispatch rule (empty `coordinator_cmd` ⇒ phase engine) leaves every current fixture (all supply a `coordinator_cmd`) on the existing `Scheduler`.
- **Deferred to later slices (note, don't build):** the stream-json trace-chain proof (§6) — v1 uses repository evidence as the code-phase acceptance gate; the `gh` CR phase (Slice 4, and `gh` is absent here); async/`monitoring` workers; checkpoint sealing. Prompts are drift-frozen copies; each borrowed file records its source path in a header comment.

---

## File Structure

- Create `src/playbook/` — drift-frozen copies of the skill's per-phase prompt text + `principles.md`, each with a provenance header. `include_str!`ed by `prompt.rs`.
- Create `src/prompt.rs` — prompt assembly (playbook + injected context + REQUIRED-OUTPUT contract) and `permission_for(phase)`.
- Create `src/phase_engine.rs` — `dispose` (pure transition core) + `PhaseScheduler` (stateful tmux driver).
- Modify `src/policy.rs` — add `StopKind`-aware `effective_risk_kind` + `decide_kind`.
- Modify `src/state.rs` — add `ProjectPaths::brief()` + `ProjectPaths::decisions()`.
- Modify `src/daemon.rs` — route native projects to `PhaseScheduler`.
- Modify `src/bin/pmtui.rs` (+ helpers) — intake interview in the create flow.
- Modify `src/lib.rs` — register `prompt`, `phase_engine` modules.
- Test: `tests/integration.rs` — phase-worker-over-tmux loop (fake worker + `#[ignore]` real claude).

---

### Task 1: Playbook fragments + `prompt.rs` (prompt assembly)

**Files:**
- Create: `src/playbook/principles.md`, `src/playbook/intake.md`, `src/playbook/research.md`, `src/playbook/design.md`, `src/playbook/plan.md`, `src/playbook/implement.md`, `src/playbook/review.md`, `src/playbook/cr.md`, `src/playbook/confirming.md`
- Create: `src/prompt.rs`
- Modify: `src/state.rs` (add `brief()` + `decisions()` accessors)
- Modify: `src/lib.rs` (add `pub mod prompt;`)

**Interfaces:**
- Produces:
  - `pub struct PromptContext<'a> { pub phase: Phase, pub project_root: &'a Path, pub result_path: &'a Path, pub brief: &'a str, pub acceptance: &'a str, pub decisions: &'a str, pub task_cursors: &'a [String], pub extra: &'a str }`
  - `pub fn assemble_prompt(ctx: &PromptContext) -> String`
  - `pub fn permission_for(phase: Phase) -> worker::PermissionMode`
  - `pub const RESULT_CONTRACT: &str` (the REQUIRED-OUTPUT block text)
  - `ProjectPaths::brief() -> PathBuf` (= `state_dir().join("brief.md")`), `ProjectPaths::decisions() -> PathBuf` (= `state_dir().join("decisions.md")`)
- Consumes: `phase::Phase`, `worker::PermissionMode`.

**Borrowed text:** copy the body of each skill file into the matching `src/playbook/*.md`, prefixed with a provenance header line:
`<!-- Borrowed (drift-frozen) from ~/.claude/skills/project-manager/references/… — do not edit to track upstream; re-copy deliberately. -->`
Sources (read and copy verbatim, trimming only skill-navigation cruft like "Next: read …"):
- `principles.md` ← `references/principles.md`
- `intake.md` ← `references/phases/intake.md`; `research.md` ← `references/phases/research.md`; `design.md` ← `references/phases/design.md`; `plan.md` ← `references/phases/plan.md`; `implement.md` ← `references/phases/implement.md`; `review.md` ← `references/phases/review.md`; `cr.md` ← `references/phases/cr.md`; `confirming.md` ← `references/phases/confirming.md`

**Design notes:**
- `assemble_prompt` output order: (1) `principles.md` (always); (2) the phase playbook fragment; (3) an injected "## Current project facts" block built from `ctx` (phase name, project root, brief excerpt, acceptance criteria, binding decisions, task cursors, `extra`); (4) `RESULT_CONTRACT` — a strict instruction ending: *"As your FINAL action, use the Write tool to create `<result_path>` containing EXACTLY one JSON object matching this schema and nothing else:"* followed by the `WorkerResult` shape (`proposed_transition`, `stay`, `stops[]`, `artifacts_written[]`, `digest`, `notes`) and the legal `proposed_transition` values for THIS phase (derive from `can_transition`'s outgoing edges).
- `permission_for`: `Research | Design | Plan | Intake | Confirming => Plan`; `Implement | Review | Cr => AcceptEdits`. (Read-only phases run in `plan` permission.)
- Use `include_str!("playbook/<file>.md")` — a `fn playbook(phase: Phase) -> &'static str` match.

**Steps:**
- [ ] **Step 1:** Add `brief()` + `decisions()` to `ProjectPaths` (mirror `answers()` at `src/state.rs:212`), with a unit test asserting the suffixes.
- [ ] **Step 2:** Create the nine `src/playbook/*.md` files by copying the skill sources with the provenance header.
- [ ] **Step 3:** Write the failing test in `src/prompt.rs` (`#[cfg(test)] mod tests`): `assemble_prompt` for `Phase::Research` contains the principles marker, a research-specific phrase, the project root, the result path, and the literal substring `"proposed_transition"`; `permission_for(Research) == Plan` and `permission_for(Implement) == AcceptEdits`.
- [ ] **Step 4:** Run it, verify it fails to compile / fails.
- [ ] **Step 5:** Implement `PromptContext`, `playbook`, `permission_for`, `RESULT_CONTRACT`, `assemble_prompt`. Register `pub mod prompt;` in `lib.rs` (alphabetical: after `policy`, before `registry`).
- [ ] **Step 6:** `cargo test --lib prompt`, `cargo clippy --all-targets`, `cargo fmt --all`. All green.
- [ ] **Step 7:** Commit: `feat(harness): prompt assembly + borrowed playbook (slice 3 task 1)`.

---

### Task 2: `policy` StopKind mapping + `phase_engine::dispose` (pure transition core)

**Files:**
- Modify: `src/policy.rs` (add `effective_risk_kind`, `decide_kind`)
- Create: `src/phase_engine.rs` (the `dispose` half only; `PhaseScheduler` is Task 3)
- Modify: `src/lib.rs` (add `pub mod phase_engine;`)

**Interfaces:**
- Produces (policy):
  - `pub fn effective_risk_kind(kind: StopKind, labelled: RiskClass) -> RiskClass` — forces `Hard` for `Publish|Merge|ConfirmDone|Stuck|Capability`; else returns `labelled`.
  - `pub fn decide_kind(tier: Tier, kind: StopKind, labelled: RiskClass) -> Decision` (= `decide(tier, effective_risk_kind(kind, labelled))`).
- Produces (phase_engine):
  - `pub enum Outcome { Advanced(Phase), Stayed, Escalated(Vec<String>), AutoFlowed(Vec<String>), WorkerStuck(String), Done }`
  - `pub struct Disposition { pub next: ProjectState, pub outcome: Outcome, pub auto_answers: Vec<String> }` (`auto_answers` = stop ids the caller must write `auto_flow` answers + decisions notes for)
  - `pub fn dispose(cur: &ProjectState, config: &Config, session_id: &str, result: &WorkerResult, repo: Option<&RepoFacts>, now: Epoch) -> Disposition`
- Consumes: `phase::{Phase, Guards, ConfirmDecision, can_transition}`, `pmstate::*`, `worker::{WorkerResult, StopDraft}`, `verify::{RepoFacts, accept_code_phase, Acceptance}`, `policy`, `state::{Config, RiskClass, Tier}`.

**`dispose` algorithm (this is the correctness heart — implement exactly):**
1. Start from `next = cur.clone()`; set `next.run.updated_at = now`, `next.run.session_id = Some(session_id.into())`.
2. **Stops first.** Map each `StopDraft` → `OpenStop` (`id = format!("stop-{phase}-{i}-{now}")`, `kind`, `context_ref`, `first_posted = now`, `status = AwaitingReply`, other fields default/empty). Partition by `policy::decide_kind(config.autonomy, kind, risk_class)`:
   - Any `Escalate` stop ⇒ **escalate**: `next.open_stops = <the escalating stops>`; `next.run.posture = NeedsYou`; `next.run.owner = Some(<first escalating stop id>)`; `next.run.next_check = Some(now)`; `next.run.active = false`, `wake_condition=Some("human")`; keep `next.phase = cur.phase`. Return `Outcome::Escalated(ids)`. (Independent runnable work is out of MVP scope — a stop parks the project.)
   - All `AutoFlow` ⇒ record their ids in `auto_answers` and DROP them (do not persist as open); continue to step 3 as if no stops. Set `Outcome::AutoFlowed` only if no transition happens in step 3 (else the transition outcome wins; still return the auto_answers).
3. **Transition or stay.** If `result.proposed_transition` is `Some(to)`:
   - Build `Guards` from `cur` + `result` + `repo` (see guard table below).
   - **Code-phase evidence gate:** if `cur.phase ∈ {Implement, Review, Cr}` and the target requires forward code (`Implement→Review`, `Review→Cr`), require `repo` present AND `accept_code_phase(repo, result.digest.as_ref()) == Accepted`; if rejected ⇒ `worker_stuck` (step 5) with the rejection reason — do NOT advance.
   - If `can_transition(cur.phase, to, &guards)` ⇒ set `next.phase = to`; apply per-phase side effects (below); `next.run.posture = Working`, `active=true`, `owner=Some(session_id)`, `wake_condition=Some("work")`, `next_check=None`. Return `Outcome::Advanced(to)` (or `Done` if `to == Done`).
   - Else ⇒ `worker_stuck` (step 5): `format!("illegal transition {:?}→{:?} proposed", cur.phase, to)`.
4. **Stay** (`proposed_transition == None` or `result.stay`): keep `next.phase`; `posture = Working`, `active=true`, `owner=Some(session_id)`, `wake_condition=Some("work")`. If in `Review|Cr`, bump `review_state` attempts (see below); if attempts reach `config.stuck_threshold` raise a `Stuck` stop (hard ⇒ escalate). Return `Outcome::Stayed` (with `auto_answers` if any).
5. **worker_stuck helper:** push an `OpenStop{ kind: WorkerStuck, … }`, `posture=NeedsYou`, `owner=<that id>`, `next_check=Some(now)`, `active=false`; keep phase. Return `Outcome::WorkerStuck(reason)`.
6. **Always** `next.validate()`; on `Err(e)`: return a `WorkerStuck(format!("engine produced invalid state: {e:?}"))` built from `cur` with a `WorkerStuck` stop (fail safe-loud, never persist invalid state). The caller re-checks `validate()` before save.

**Guard derivation (MVP):**
- `topics_remain = result.artifacts_written.is_empty()` (research honors `→Design` only once a research artifact exists).
- `slice_boundary_reached = repo-accepted && cur.task_cursors.is_empty()` (implement finished the slice's tasks).
- `recovery_needs_plan = matches!(result.proposed_transition, Some(Plan)) && cur.phase == Implement`.
- `verification_passed = repo present && accept_code_phase == Accepted` (review).
- `plan_exhausted = cur.task_cursors.is_empty()`; `all_crs_terminal = cur.crs.iter().all(|c| !c.is_open())`.
- `another_slice_remains = !cur.task_cursors.is_empty()`.
- `confirm`: confirming acceptance is human-gated — a worker never self-accepts. For MVP set `confirm = None` unless a human `confirm_done` answer is already recorded (out of the worker's control); so `Confirming→Done` from a worker result alone stays. (The confirm-answer path lands with the CR slice.)
- `fire_condition_holds = plan_exhausted && all_crs_terminal` (confirming self-corrects back when false).
- `attempts_exhausted = cur.review_state.map(|r| r.attempts >= config.stuck_threshold).unwrap_or(false)`.

**Per-phase side effects on advance:**
- `Plan → Implement`: task-cursor population from the plan worker is a Slice-4 refinement; keep `cur.task_cursors` (implement drains them). Note the seam in a code comment.
- `Implement → Review`: on accept, if `cur.task_cursors` non-empty, drain the first (a completed task); create/reset `review_state` for the slice signature.
- `Confirming → Done`: posture Done, active=false, null all handles, clear stops, `next_check=None`.
- `Done → Research` (reopen): fresh working state.

**Steps:**
- [ ] **Step 1:** `policy`: write failing tests for `effective_risk_kind` (`Stuck`/`Capability` → Hard; `Ambiguity` Medium passthrough) and `decide_kind` (autopilot escalates `Stuck`, auto-flows `Ambiguity` Medium). Implement. Green.
- [ ] **Step 2:** Create `phase_engine.rs` with `Outcome`, `Disposition`, and a `dispose` stub returning `Stayed`; register module in `lib.rs` (after `phase`, before `pmstate`).
- [ ] **Step 3:** Write failing unit tests (table-driven) for `dispose`, one per row of the transition table PLUS the safety rows:
  - `intake`→`research` advances (artifact present).
  - `research` stays when no artifact written; advances to `design` when `research.md` written.
  - code phase `implement→review` REJECTED (no repo forward move) ⇒ `WorkerStuck`; ACCEPTED (forward move) ⇒ `Advanced(Review)`.
  - illegal proposed transition (`intake→plan`) ⇒ `WorkerStuck`, never advances.
  - `cr→done` proposed ⇒ `WorkerStuck` (hard reject), phase unchanged.
  - hard stop (`publish`) at autopilot ⇒ `Escalated`, posture `NeedsYou`, owner set.
  - medium `ambiguity` at autopilot ⇒ `AutoFlowed` (id in `auto_answers`, not persisted as open); at standard ⇒ `Escalated`.
  - every returned `next` passes `validate()`.
- [ ] **Step 4:** Run, verify fail.
- [ ] **Step 5:** Implement `dispose` per the algorithm. Iterate to green.
- [ ] **Step 6:** `cargo test --lib`, `clippy`, `fmt`. Green.
- [ ] **Step 7:** Commit: `feat(harness): dispose transition core + StopKind risk mapping (slice 3 task 2)`.

---

### Task 3: `PhaseScheduler` (stateful tmux driver) + integration test

**Files:**
- Modify: `src/phase_engine.rs` (add `PhaseScheduler`, `PhaseTick`)
- Modify: `tests/integration.rs` (fake-worker loop over real tmux; `#[ignore]` real-claude smoke)

**Interfaces:**
- Produces:
  - `pub enum PhaseTick { WaitingForIntake, Spawned{seq:u64}, Running, Advanced(Phase), Stayed, Escalated(Vec<String>), AutoFlowed(Vec<String>), Stuck(String), Done }`
  - `pub struct PhaseScheduler { … }` with `pub fn new(project_id, paths, engine: Engine) -> Self` and `pub fn tick(&mut self, driver: &dyn Driver, clock: &dyn Clock) -> Result<PhaseTick>`; `pub fn abort(&mut self, driver, clock)`.
- Consumes: everything from Tasks 1–2 + `worker::build_command`, `tmux::{Driver, observe, StepHandle}`, `verify::{gather_repo_facts, head_commit}`, `pmstate::{load, save}`, `state::{append_answer, Answer, Config}`, `clock::{Clock, Epoch}`.

**`tick` behavior (mirror `Scheduler`'s RunState shape; reuse its guards):**
- Load `Config` (error ⇒ propagate, daemon poisons). Load `ProjectState` via `pmstate::load`.
  - `None` ⇒ `Ok(PhaseTick::WaitingForIntake)` (intake is pmtui's job; the engine never runs intake).
- Internal `RunState`: `Idle | Running{seq, phase, handle, deadline, head_before: Option<String>} | Escalated{stop_ids, since} | Done`. `restore_from_disk` rebuilds from `ProjectState.run` + `driver.json` (posture `NeedsYou`→Escalated using `open_stops` ids and `run.updated_at` as `since`; `Done`→Done; else Idle).
- `Idle`/working: if `state.phase == Done` ⇒ Done. Else **spawn a phase worker**:
  - `head_before = Some(verify::head_commit(root))` when `permission_for(phase) == AcceptEdits` and root is a git repo (else `None`).
  - Build `PromptContext` from disk (`brief.md`, `decisions.md`, acceptance from config/brief, `task_cursors`), `assemble_prompt`, then `worker::build_command(engine, &prompt, permission_for(phase), &[root])`.
  - `driver.spawn_step(&session, root, &argv, &done, &log)`; write `driver.json` (reuse `DriverState`); set `RunState::Running`; mark `state.run.active=true, session_id`, save (validate first). Return `Spawned`.
- `Running`: `observe`; timeout past deadline ⇒ terminate + `worker_stuck` (write state NeedsYou, return `Stuck`). On `Completed{0}`: gather `repo = head_before.map(|h| gather_repo_facts(root, &h, result.digest.commit_ref))`; `parse_result` — a missing/unparseable result ⇒ `worker_stuck`. Call `dispose(&state, &config, session_id, &result, repo.as_ref(), now)`; for each `auto_answers` id write an `auto_flow` `Answer` + append a `decisions.md` note; `state = disposition.next`; `validate()`; `save`. Map `Outcome`→`PhaseTick`. On non-zero exit ⇒ `worker_stuck` immediately (MVP: no retry storm).
- `Escalated`: same as `Scheduler::on_escalated` but over `open_stops` ids + `answers.json` filtered by `since`; when unblocked, drive the next worker (re-dispatch same phase, consuming the answer via prompt `extra`).

**Steps:**
- [ ] **Step 1:** Implement `PhaseScheduler` + `restore_from_disk` + `tick`. Add `#[cfg(test)]` unit tests over `FakeDriver`/`FakeClock`: `WaitingForIntake` when no state; spawns a worker when state present & working; does-not-double-spawn; `Done` when phase Done.
- [ ] **Step 2:** Run unit tests; green.
- [ ] **Step 3:** Add integration test `phase_worker_loop_advances_via_tmux` (real tmux, fake `sh` worker): seed `config.json` + a `ProjectState` at `phase=Research, posture=Working`; the fake worker writes `steps/<seq>.result.json` = `{"proposed_transition":"design","artifacts_written":[".project-state/research.md"]}`; drive `PhaseScheduler::tick` in a bounded poll loop over a private `-L` socket; assert `state.json` advanced to `design`. Skip gracefully if tmux absent. Kill the private server in teardown.
- [ ] **Step 4:** Add `#[ignore]` `real_claude_phase_worker_advances_via_tmux`: same but `build_command(Engine::Claude, …)` with a prompt instructing the worker to write the result JSON; assert advance. Self-skip if `claude`/tmux unavailable.
- [ ] **Step 5:** `cargo test` (lib + integration, non-ignored) + `clippy` + `fmt`. Green.
- [ ] **Step 6:** Run the fake-worker integration test to confirm it passes over real tmux.
- [ ] **Step 7:** Commit: `feat(harness): PhaseScheduler drives phases over tmux (slice 3 task 3)`.

---

### Task 4: Daemon dispatch — route native projects to `PhaseScheduler`

**Files:**
- Modify: `src/daemon.rs`

**Interfaces:**
- Consumes: `phase_engine::{PhaseScheduler, PhaseTick}`, `registry::{Engine, Mode}`.

**Design:** the `Runner` gains an enum driver:
```
enum Driven { Reference(Scheduler), Native(PhaseScheduler) }
```
`Runner::build(p)`: if `p.coordinator_cmd.is_empty()` (native) ⇒ `Driven::Native(PhaseScheduler::new(p.id, p.paths(), p.engine.unwrap_or(Engine::Claude)))`; else `Driven::Reference(Scheduler::new(…))`. `reconcile_one` drives whichever and maps its tick outcome to notifications:
- Native: `PhaseTick::Escalated(ids)` ⇒ read `open_stops`, convert to old `Stop`s, `notifier.notify(Escalation::for_stops(id, &stops))`; `Stuck(reason)` ⇒ `Escalation::stuck`; `Done` ⇒ `r.done = true`; `WaitingForIntake` ⇒ no-op (not done, not driven).
- `disk_done` for native projects checks `pmstate::load(...).phase == Done` (add a native branch).
- The rebuild trigger (`config_changed`) must also fire when a project flips native↔reference (coordinator_cmd emptiness changes) — it already compares `coordinator_cmd`, so this is covered.

**Steps:**
- [ ] **Step 1:** Write a failing daemon test `native_project_is_driven_by_phase_engine`: a `mode:Auto`, empty-`coordinator_cmd` project with a seeded `ProjectState(phase=Research, working)` + valid config; one `sweep` over `FakeDriver` spawns exactly one worker (assert `driver.spawn_count()==1`). And `native_without_state_is_not_done_and_not_spawned` (WaitingForIntake ⇒ `spawn_count()==0`, `!all_enabled_done`).
- [ ] **Step 2:** Run, verify fail.
- [ ] **Step 3:** Implement the `Driven` enum + routing + native `disk_done`. Keep every existing daemon test green (they all supply `coordinator_cmd` ⇒ Reference).
- [ ] **Step 4:** `cargo test`, `clippy`, `fmt`. Green.
- [ ] **Step 5:** Commit: `feat(harness): daemon routes coordinator-less projects to the phase engine (slice 3 task 4)`.

---

### Task 5: Intake interview in pmtui

**Files:**
- Modify: `src/bin/pmtui.rs` (extend the `n` create flow; helpers as needed)

**Design:** Extend the existing create form into a short interview that collects: **goal/brief** (multiline → `brief.md`), **tier** (autopilot/standard/guardian), **engine** (claude/codex), and an **external-action approvals** acknowledgement (borrow the never-blanket-approval question text as a yes/no the human must actively answer). On submit, the flow:
1. writes `config.json` (`Config{ autonomy, step_timeout_s, max_failures, stuck_threshold, coordinator_lease_s }` defaults + chosen tier),
2. writes `brief.md`,
3. writes a fresh `ProjectState::fresh(now)` then advances it to `Research` working (the intake→research transition, harness-owned) via a small helper `pmstate` write (validate first),
4. registers a `ProjectEntry{ mode: Auto, coordinator_cmd: vec![], engine: Some(chosen) }` so pmd's phase engine picks it up.

Scope guard (YAGNI): reuse the current create-form widgets/scaffolding; do NOT build a multi-screen wizard — a single form with the added fields is sufficient for v1. Comms destinations/cadences are v1-defaulted (terminal+file), so do not prompt for them.

**Steps:**
- [ ] **Step 1:** Read the current `n`/create flow + `CreateForm` in `pmtui.rs`; identify the submit path that writes the registry entry.
- [ ] **Step 2:** Add the interview fields (goal, tier, engine, approval ack) to `CreateForm` with defaults; render them; validate that goal is non-empty and the approval was actively acknowledged before submit is allowed.
- [ ] **Step 3:** On submit, write `config.json` + `brief.md` + the advanced `ProjectState` + the native `ProjectEntry`. Factor the state-writing into a testable free function `intake_finalize(paths, tier, engine, brief, now) -> Result<()>` and unit-test it (asserts config/brief/state.json land, `state.json` validates and is at `phase=research`).
- [ ] **Step 4:** `cargo test`, `clippy`, `fmt`. Green. Manually sanity-check the form renders (build `pmtui`, note it compiles; a full TTY test is out of scope — the finalize function carries the logic under test).
- [ ] **Step 5:** Commit: `feat(pmtui): intake interview creates a native harness project (slice 3 task 5)`.

---

## Self-Review

- **Spec coverage:** §12.3 = phase_engine wiring (Tasks 2–3), stops in pmtui+notifier + tier auto-flow/escalate (Tasks 2,4,5), intake in pmtui (Task 5), drives a native project (Tasks 3–4). §4 machine reused via `can_transition`. §6 worker contract: build_command (done Slice 2) + prompt (Task 1) + repo-evidence verify (done Slice 2, wired Task 2–3). §7 tiers via `policy` (Task 2). §5 state written only by harness, always `validate()`d.
- **Deliberately deferred (flagged in Global Constraints):** stream-json trace-chain proof; `gh` CR phase (Slice 4); task-cursor population from the plan worker (Slice 4 refinement); async/monitoring. These are noted in code comments where the seam is.
- **Type consistency:** `PermissionMode`/`Engine`/`WorkerResult`/`RepoFacts`/`ProjectState`/`Guards`/`can_transition` signatures match the shipped Slice 1–2 code read during planning.
- **Risk focus (spec §14):** phase⟂posture + `validate()` before every save (Global Constraints); `cr→done` reject (Task 2 test); premature-confirming fire-condition recompute (Task 2 guard); no-self-claim repo-evidence gate (Task 2–3); non-blocking detached substrate (Task 3).
