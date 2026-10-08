# M5 Harness — Slice 1: State + Phase Machine Core — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure, exhaustively-tested foundation of the native coordinator harness — the `Phase` state machine (with a legality/guard oracle) and the full `state.json` model with a `validate()` invariant checker — with no workers or I/O loop yet.

**Architecture:** Two new modules on the existing `agent-manager` crate: `phase.rs` (the `Phase` enum + `can_transition` allowlist) and `pmstate.rs` (the `ProjectState` ledger written to a new `.project-state/state.json`, orthogonal `phase` vs `posture`, `validate()`). Reuses `state::write_json_atomic`/`read_json_opt` and `clock::Epoch`. This is spec §12 slice 1 of `docs/superpowers/specs/2026-08-12-m5-native-harness-design.md`.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, `anyhow`, the existing `agent-manager` crate (`state.rs`, `clock.rs`).

## Global Constraints

- Rust; the crate already builds with `cargo build`; keep `cargo clippy --all-targets` and `cargo fmt --check` clean (project convention — every task ends green).
- Reuse `state::write_json_atomic`, `state::read_json_opt`, and `clock::Epoch` — do not add a second persistence path.
- All new `state.json` structs use `#[serde(deny_unknown_fields)]` (worker-proposes/harness-disposes: unknown keys are a bug, not silently dropped).
- `phase` (Intake…Done) and `run.posture` (Working/Monitoring/NeedsYou/Done) are **orthogonal**. The invariant is exactly `phase == Done ⇔ posture == Done`; nothing else couples them. Never model them as one field.
- `cr → done` is an illegal transition and MUST be rejected by `can_transition`.
- This slice adds **no** worker dispatch, no daemon wiring, no tmux, no CLI calls. Pure types + logic + unit tests.
- Enum serde uses `rename_all = "snake_case"` (matches existing `Tier`/`RiskClass`/`StepStatus`).

---

### Task 1: `Phase` enum + `can_transition` legality/guard oracle

**Files:**
- Create: `src/phase.rs`
- Modify: `src/lib.rs:10-19` (add `pub mod phase;`)

**Interfaces:**
- Produces:
  - `enum Phase { Intake, Research, Design, Plan, Implement, Review, Cr, Confirming, Done }` (serde snake_case; `Cr → "cr"`).
  - `enum ConfirmDecision { Accept, NewDirection, NotYet }` (serde snake_case).
  - `struct Guards { topics_remain, slice_boundary_reached, recovery_needs_plan, verification_passed, attempts_exhausted, plan_exhausted, all_crs_terminal, another_slice_remains, fire_condition_holds: bool, confirm: Option<ConfirmDecision> }` — all `bool` except `confirm`; derives `Default`.
  - `fn can_transition(from: Phase, to: Phase, g: &Guards) -> bool`.

- [ ] **Step 1: Write the failing tests**

Create `src/phase.rs` with the type stubs and this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn guards() -> Guards {
        Guards::default()
    }

    #[test]
    fn phase_serializes_snake_case_with_cr() {
        assert_eq!(serde_json::to_string(&Phase::Cr).unwrap(), "\"cr\"");
        assert_eq!(serde_json::to_string(&Phase::Confirming).unwrap(), "\"confirming\"");
        let back: Phase = serde_json::from_str("\"cr\"").unwrap();
        assert_eq!(back, Phase::Cr);
    }

    #[test]
    fn cr_to_done_is_always_rejected() {
        // The load-bearing safety reject: a project can never declare itself done
        // straight from code review, even with every guard satisfied.
        let mut g = guards();
        g.plan_exhausted = true;
        g.all_crs_terminal = true;
        assert!(!can_transition(Phase::Cr, Phase::Done, &g));
    }

    #[test]
    fn linear_early_transitions_are_legal() {
        assert!(can_transition(Phase::Intake, Phase::Research, &guards()));
        assert!(can_transition(Phase::Design, Phase::Plan, &guards()));
        assert!(can_transition(Phase::Plan, Phase::Implement, &guards()));
    }

    #[test]
    fn research_to_design_requires_no_topics_remaining() {
        let mut g = guards();
        g.topics_remain = true;
        assert!(!can_transition(Phase::Research, Phase::Design, &g));
        g.topics_remain = false;
        assert!(can_transition(Phase::Research, Phase::Design, &g));
    }

    #[test]
    fn implement_advances_or_recovers() {
        let mut g = guards();
        g.slice_boundary_reached = true;
        assert!(can_transition(Phase::Implement, Phase::Review, &g));
        let mut g = guards();
        g.recovery_needs_plan = true;
        assert!(can_transition(Phase::Implement, Phase::Plan, &g));
        // Neither guard: no legal transition out of implement.
        assert!(!can_transition(Phase::Implement, Phase::Review, &guards()));
    }

    #[test]
    fn review_to_cr_requires_verification_pass() {
        let mut g = guards();
        g.verification_passed = true;
        assert!(can_transition(Phase::Review, Phase::Cr, &g));
        assert!(!can_transition(Phase::Review, Phase::Cr, &guards()));
    }

    #[test]
    fn cr_advances_to_confirming_only_when_plan_exhausted_and_all_terminal() {
        let mut g = guards();
        g.plan_exhausted = true;
        g.all_crs_terminal = true;
        assert!(can_transition(Phase::Cr, Phase::Confirming, &g));
        g.all_crs_terminal = false;
        assert!(!can_transition(Phase::Cr, Phase::Confirming, &g));
        // Another slice remains -> back to implement.
        let mut g = guards();
        g.another_slice_remains = true;
        assert!(can_transition(Phase::Cr, Phase::Implement, &g));
    }

    #[test]
    fn confirming_branches_on_decision_and_self_corrects() {
        let mut g = guards();
        g.confirm = Some(ConfirmDecision::Accept);
        assert!(can_transition(Phase::Confirming, Phase::Done, &g));
        let mut g = guards();
        g.confirm = Some(ConfirmDecision::NewDirection);
        assert!(can_transition(Phase::Confirming, Phase::Research, &g));
        // Fire-condition false on entry -> self-correct back to an earlier phase.
        let mut g = guards();
        g.fire_condition_holds = false;
        assert!(can_transition(Phase::Confirming, Phase::Cr, &g));
        assert!(can_transition(Phase::Confirming, Phase::Implement, &g));
        // NotYet / silence keeps it in confirming (no transition).
        let mut g = guards();
        g.fire_condition_holds = true;
        g.confirm = Some(ConfirmDecision::NotYet);
        assert!(!can_transition(Phase::Confirming, Phase::Done, &g));
    }

    #[test]
    fn done_reopens_only_to_research() {
        assert!(can_transition(Phase::Done, Phase::Research, &guards()));
        assert!(!can_transition(Phase::Done, Phase::Implement, &guards()));
    }

    #[test]
    fn unlisted_transitions_are_illegal() {
        assert!(!can_transition(Phase::Intake, Phase::Plan, &guards()));
        assert!(!can_transition(Phase::Research, Phase::Implement, &guards()));
        assert!(!can_transition(Phase::Design, Phase::Design, &guards()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib phase:: 2>&1 | tail -20`
Expected: compile error / FAIL (`can_transition` unimplemented).

- [ ] **Step 3: Write the implementation**

At the top of `src/phase.rs`:

```rust
//! The harness phase machine (spec §4). `Phase` is the *what* of work; it is
//! orthogonal to `run.posture` (the scheduling stance in `pmstate`). Transitions
//! are an explicit allowlist gated by named guard predicates; `cr → done` is
//! rejected so a project can never declare itself done with open review.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Intake,
    Research,
    Design,
    Plan,
    Implement,
    Review,
    Cr,
    Confirming,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmDecision {
    Accept,
    NewDirection,
    NotYet,
}

/// Named guard predicates the phase engine computes from the verified worker
/// result + current state. `can_transition` consults only these — it never
/// reads disk — so it is a pure, exhaustively-testable oracle.
#[derive(Debug, Clone, Default)]
pub struct Guards {
    pub topics_remain: bool,
    pub slice_boundary_reached: bool,
    pub recovery_needs_plan: bool,
    pub verification_passed: bool,
    pub attempts_exhausted: bool,
    pub plan_exhausted: bool,
    pub all_crs_terminal: bool,
    pub another_slice_remains: bool,
    pub fire_condition_holds: bool,
    pub confirm: Option<ConfirmDecision>,
}

/// Is `from → to` a legal transition given `g`? The allowlist from spec §4.
/// Any pair not listed (including `cr → done`) is illegal.
pub fn can_transition(from: Phase, to: Phase, g: &Guards) -> bool {
    use Phase::*;
    match (from, to) {
        (Intake, Research) => true,
        (Research, Design) => !g.topics_remain,
        (Design, Plan) => true,
        (Plan, Implement) => true,
        (Implement, Review) => g.slice_boundary_reached,
        (Implement, Plan) => g.recovery_needs_plan,
        (Review, Cr) => g.verification_passed,
        (Cr, Implement) => g.another_slice_remains,
        (Cr, Confirming) => g.plan_exhausted && g.all_crs_terminal,
        (Confirming, Done) => matches!(g.confirm, Some(ConfirmDecision::Accept)),
        (Confirming, Research) => matches!(g.confirm, Some(ConfirmDecision::NewDirection)),
        (Confirming, Cr) | (Confirming, Implement) | (Confirming, Plan) => !g.fire_condition_holds,
        (Done, Research) => true,
        // Explicit hard reject (also caught by the catch-all; kept for clarity).
        (Cr, Done) => false,
        _ => false,
    }
}
```

Add `pub mod phase;` to `src/lib.rs` in the module list (match existing alphabetical ordering).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib phase:: 2>&1 | tail -20`
Expected: PASS (all phase tests).

- [ ] **Step 5: Commit**

```bash
git add src/phase.rs src/lib.rs
git commit -m "feat(harness): Phase enum + can_transition allowlist (cr->done rejected)"
```

---

### Task 2: `Posture` + `Run` scheduling struct

**Files:**
- Create: `src/pmstate.rs`
- Modify: `src/lib.rs` (add `pub mod pmstate;`)

**Interfaces:**
- Consumes: `crate::clock::Epoch`, `crate::phase::Phase`.
- Produces:
  - `enum Posture { Working, Monitoring, NeedsYou, Done }` (serde snake_case).
  - `struct Run { active: bool, posture: Posture, owner: Option<String>, wake_condition: Option<String>, next_check: Option<Epoch>, updated_at: Epoch, session_id: Option<String>, continuations: u32, max_continuations: u32 }` (serde `deny_unknown_fields`; optionals + counts `#[serde(default)]`).

- [ ] **Step 1: Write the failing test**

Create `src/pmstate.rs` with the type stubs and this test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posture_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Posture::NeedsYou).unwrap(), "\"needs_you\"");
        let back: Posture = serde_json::from_str("\"monitoring\"").unwrap();
        assert_eq!(back, Posture::Monitoring);
    }

    #[test]
    fn run_round_trips_and_defaults_optionals() {
        let json = r#"{ "active": false, "posture": "working", "updated_at": 100 }"#;
        let r: Run = serde_json::from_str(json).unwrap();
        assert_eq!(r.posture, Posture::Working);
        assert!(r.owner.is_none() && r.session_id.is_none());
        assert_eq!(r.continuations, 0);
        let back: Run = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn run_rejects_unknown_fields() {
        let json = r#"{ "active": false, "posture": "working", "updated_at": 1, "bogus": 5 }"#;
        assert!(serde_json::from_str::<Run>(json).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib pmstate:: 2>&1 | tail -20`
Expected: compile error / FAIL.

- [ ] **Step 3: Write the implementation**

At the top of `src/pmstate.rs`:

```rust
//! The harness ledger — `.project-state/state.json`. Written ONLY by the harness
//! (worker-proposes/harness-disposes). `phase` (the *what*) and `run.posture`
//! (the *scheduling stance*) are orthogonal; see `validate()` for the invariants.

use serde::{Deserialize, Serialize};

use crate::clock::Epoch;
use crate::phase::Phase;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Posture {
    Working,
    Monitoring,
    NeedsYou,
    Done,
}

/// Scheduling stance. Supersedes the reference model's `Step.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub active: bool,
    pub posture: Posture,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub wake_condition: Option<String>,
    #[serde(default)]
    pub next_check: Option<Epoch>,
    pub updated_at: Epoch,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub continuations: u32,
    /// 0 = unlimited.
    #[serde(default)]
    pub max_continuations: u32,
}
```

Add `pub mod pmstate;` to `src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib pmstate:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/pmstate.rs src/lib.rs
git commit -m "feat(harness): Posture + Run scheduling struct in pmstate"
```

---

### Task 3: The rest of the ledger structs + `ProjectState`

**Files:**
- Modify: `src/pmstate.rs`

**Interfaces:**
- Produces:
  - `enum CrKind { Brazil, Github }`, `enum CrStatus { Draft, InReview, ChangesRequested, Approved, Landed, Abandoned }` (serde snake_case).
  - `struct Cr { id: Option<String>, slice_id: String, kind: CrKind, branch: String, base: String, status: CrStatus, tasks: Vec<String>, packages: Vec<String>, last_polled: Option<Epoch>, poll_cadence_s: Option<u64> }`; `impl Cr { fn is_open(&self) -> bool }` (`status ∉ {Landed, Abandoned}`).
  - `enum StopKind { Publish, Merge, ConfirmDone, Ambiguity, Stuck, ExpertNeeded, WorkerStuck, Capability }`, `enum StopStatus { AwaitingReply, Held }` (serde snake_case).
  - `struct OpenStop { id: String, kind: StopKind, channel: Option<String>, context_ref: Option<String>, authorized_responders: Vec<String>, message_id: Option<String>, first_posted: Epoch, last_polled: Option<Epoch>, last_seen_reply_ts: Option<Epoch>, status: StopStatus }`.
  - `struct ReviewState { slice_id: String, signature: String, attempts: u32, updated_at: Epoch }`.
  - `enum OpStatus { Pending, Completed, Failed }`; `struct Operation { id: String, key: String, status: OpStatus, started_at: Epoch }`.
  - `struct Toolchain { detected_at: Epoch, git: bool, gh: bool, claude: bool, codex: bool, tmux: bool }`.
  - `struct ProjectState { phase: Phase, run: Run, task_cursors: Vec<String>, crs: Vec<Cr>, open_stops: Vec<OpenStop>, review_state: Option<ReviewState>, operations: Vec<Operation>, toolchain: Option<Toolchain> }` (serde `deny_unknown_fields`; collections + optionals `#[serde(default)]`).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/pmstate.rs`:

```rust
    fn minimal_state_json() -> &'static str {
        r#"{ "phase": "implement",
             "run": { "active": true, "posture": "working", "owner": "run-1",
                      "wake_condition": "work", "session_id": "s1", "updated_at": 10 } }"#
    }

    #[test]
    fn project_state_round_trips_with_defaults() {
        let s: ProjectState = serde_json::from_str(minimal_state_json()).unwrap();
        assert_eq!(s.phase, Phase::Implement);
        assert!(s.task_cursors.is_empty() && s.crs.is_empty() && s.open_stops.is_empty());
        assert!(s.review_state.is_none() && s.toolchain.is_none());
        let back: ProjectState =
            serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn project_state_rejects_unknown_fields() {
        let json = r#"{ "phase": "intake",
            "run": { "active": false, "posture": "working", "updated_at": 1 },
            "surprise": true }"#;
        assert!(serde_json::from_str::<ProjectState>(json).is_err());
    }

    #[test]
    fn cr_open_excludes_terminal_states() {
        let mk = |st: CrStatus| Cr {
            id: None, slice_id: "s1".into(), kind: CrKind::Github, branch: "b".into(),
            base: "main".into(), status: st, tasks: vec![], packages: vec![],
            last_polled: None, poll_cadence_s: None,
        };
        assert!(mk(CrStatus::Draft).is_open());
        assert!(mk(CrStatus::InReview).is_open());
        assert!(!mk(CrStatus::Landed).is_open());
        assert!(!mk(CrStatus::Abandoned).is_open());
    }

    #[test]
    fn stop_kind_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&StopKind::ConfirmDone).unwrap(), "\"confirm_done\"");
        assert_eq!(serde_json::to_string(&StopKind::WorkerStuck).unwrap(), "\"worker_stuck\"");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib pmstate:: 2>&1 | tail -20`
Expected: compile error / FAIL.

- [ ] **Step 3: Write the implementation**

Add to `src/pmstate.rs` (after `Run`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrKind {
    Brazil,
    Github,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrStatus {
    Draft,
    InReview,
    ChangesRequested,
    Approved,
    Landed,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cr {
    #[serde(default)]
    pub id: Option<String>,
    pub slice_id: String,
    pub kind: CrKind,
    pub branch: String,
    pub base: String,
    pub status: CrStatus,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub last_polled: Option<Epoch>,
    #[serde(default)]
    pub poll_cadence_s: Option<u64>,
}

impl Cr {
    /// A review unit still needing attention (not yet landed or abandoned).
    pub fn is_open(&self) -> bool {
        !matches!(self.status, CrStatus::Landed | CrStatus::Abandoned)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopKind {
    Publish,
    Merge,
    ConfirmDone,
    Ambiguity,
    Stuck,
    ExpertNeeded,
    WorkerStuck,
    Capability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopStatus {
    AwaitingReply,
    Held,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenStop {
    pub id: String,
    pub kind: StopKind,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub context_ref: Option<String>,
    #[serde(default)]
    pub authorized_responders: Vec<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    pub first_posted: Epoch,
    #[serde(default)]
    pub last_polled: Option<Epoch>,
    #[serde(default)]
    pub last_seen_reply_ts: Option<Epoch>,
    pub status: StopStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewState {
    pub slice_id: String,
    pub signature: String,
    pub attempts: u32,
    pub updated_at: Epoch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpStatus {
    Pending,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub id: String,
    pub key: String,
    pub status: OpStatus,
    pub started_at: Epoch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Toolchain {
    pub detected_at: Epoch,
    #[serde(default)]
    pub git: bool,
    #[serde(default)]
    pub gh: bool,
    #[serde(default)]
    pub claude: bool,
    #[serde(default)]
    pub codex: bool,
    #[serde(default)]
    pub tmux: bool,
}

/// The full harness ledger persisted to `.project-state/state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectState {
    pub phase: Phase,
    pub run: Run,
    #[serde(default)]
    pub task_cursors: Vec<String>,
    #[serde(default)]
    pub crs: Vec<Cr>,
    #[serde(default)]
    pub open_stops: Vec<OpenStop>,
    #[serde(default)]
    pub review_state: Option<ReviewState>,
    #[serde(default)]
    pub operations: Vec<Operation>,
    #[serde(default)]
    pub toolchain: Option<Toolchain>,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib pmstate:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/pmstate.rs
git commit -m "feat(harness): full state.json ledger structs (crs, stops, ops, review_state)"
```

---

### Task 4: `ProjectState::validate()` + `fresh()`

**Files:**
- Modify: `src/pmstate.rs`

**Interfaces:**
- Produces:
  - `impl ProjectState { fn fresh(now: Epoch) -> Self }` — a first-start ledger: `phase: Intake`, `run { active: false, posture: Working, updated_at: now, .. }`, all collections empty.
  - `impl ProjectState { fn validate(&self) -> Result<(), Vec<String>> }` — returns every violated invariant (empty `Err` never returned; `Ok(())` when clean).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/pmstate.rs`:

```rust
    fn valid_working() -> ProjectState {
        let mut s = ProjectState::fresh(10);
        s.phase = Phase::Implement;
        s.run.active = true;
        s.run.owner = Some("run-1".into());
        s.run.wake_condition = Some("work".into());
        s.run.session_id = Some("s1".into());
        s
    }

    #[test]
    fn fresh_is_valid() {
        assert_eq!(ProjectState::fresh(5).validate(), Ok(()));
    }

    #[test]
    fn phase_done_iff_posture_done() {
        let mut s = valid_working();
        s.phase = Phase::Done; // posture still Working -> violation
        assert!(s.validate().is_err());
        s.run.posture = Posture::Done;
        s.run.active = false;
        s.run.owner = None;
        s.run.wake_condition = None;
        s.run.session_id = None;
        s.run.next_check = None;
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn active_requires_owner_wake_session() {
        let mut s = valid_working();
        s.run.owner = None;
        assert!(s.validate().is_err());
    }

    #[test]
    fn monitoring_and_needs_you_require_next_check_working_forbids_it() {
        let mut s = valid_working();
        s.run.posture = Posture::Working;
        s.run.next_check = Some(50); // working must not carry next_check
        assert!(s.validate().is_err());

        let mut s = valid_working();
        s.run.posture = Posture::Monitoring;
        s.run.next_check = None; // monitoring must carry next_check
        assert!(s.validate().is_err());
    }

    #[test]
    fn needs_you_owner_must_reference_an_open_stop() {
        let mut s = valid_working();
        s.run.posture = Posture::NeedsYou;
        s.run.next_check = Some(50);
        s.run.owner = Some("missing".into());
        assert!(s.validate().is_err(), "owner not among open_stops");
        s.open_stops.push(OpenStop {
            id: "missing".into(), kind: StopKind::Ambiguity, channel: None, context_ref: None,
            authorized_responders: vec![], message_id: None, first_posted: 1, last_polled: None,
            last_seen_reply_ts: None, status: StopStatus::AwaitingReply,
        });
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn task_cursors_must_be_unique() {
        let mut s = valid_working();
        s.task_cursors = vec!["t1".into(), "t1".into()];
        assert!(s.validate().is_err());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib pmstate:: 2>&1 | tail -20`
Expected: compile error / FAIL.

- [ ] **Step 3: Write the implementation**

Add to `src/pmstate.rs` (an `impl ProjectState` block). Put the `use std::collections::HashSet;` with the other imports at the top of the file:

```rust
use std::collections::HashSet;

impl ProjectState {
    /// A first-start ledger: intake, idle, no work yet.
    pub fn fresh(now: Epoch) -> Self {
        ProjectState {
            phase: Phase::Intake,
            run: Run {
                active: false,
                posture: Posture::Working,
                owner: None,
                wake_condition: None,
                next_check: None,
                updated_at: now,
                session_id: None,
                continuations: 0,
                max_continuations: 0,
            },
            task_cursors: Vec::new(),
            crs: Vec::new(),
            open_stops: Vec::new(),
            review_state: None,
            operations: Vec::new(),
            toolchain: None,
        }
    }

    /// Check the ledger invariants (spec §5). Collects every violation.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        let phase_done = self.phase == Phase::Done;
        let posture_done = self.run.posture == Posture::Done;
        if phase_done != posture_done {
            errs.push(format!(
                "phase==Done must iff posture==Done (phase_done={phase_done}, posture_done={posture_done})"
            ));
        }
        if posture_done && self.run.active {
            errs.push("posture Done requires run.active == false".into());
        }
        if self.run.active
            && (self.run.owner.is_none()
                || self.run.wake_condition.is_none()
                || self.run.session_id.is_none())
        {
            errs.push("active run requires owner, wake_condition, and session_id".into());
        }
        match self.run.posture {
            Posture::Working | Posture::Done => {
                if self.run.next_check.is_some() {
                    errs.push(format!("{:?} must not carry next_check", self.run.posture));
                }
            }
            Posture::Monitoring | Posture::NeedsYou => {
                if self.run.next_check.is_none() {
                    errs.push(format!("{:?} requires next_check", self.run.posture));
                }
            }
        }
        if self.run.posture == Posture::NeedsYou {
            let owner_ok = self
                .run
                .owner
                .as_deref()
                .is_some_and(|o| self.open_stops.iter().any(|s| s.id == o));
            if !owner_ok {
                errs.push("posture NeedsYou requires run.owner to be an open_stops id".into());
            }
        }
        let mut seen = HashSet::new();
        for c in &self.task_cursors {
            if !seen.insert(c) {
                errs.push(format!("duplicate task cursor: {c}"));
            }
        }
        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib pmstate:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/pmstate.rs
git commit -m "feat(harness): ProjectState::fresh + validate (phase/posture orthogonality invariants)"
```

---

### Task 5: `Config` additions + `ProjectPaths::pmstate()` + load/save

**Files:**
- Modify: `src/state.rs:65-79` (Config), `src/state.rs:164-205` (ProjectPaths impl)
- Modify: `src/pmstate.rs` (load/save helpers)

**Interfaces:**
- Consumes: `state::write_json_atomic`, `state::read_json_opt`, `ProjectPaths`.
- Produces:
  - `Config` gains `stuck_threshold: u32` (default 3) and `coordinator_lease_s: u64` (default 1860), both `#[serde(default = ...)]`; `impl Config { fn validate(&self) -> Result<(), String> }` requiring `coordinator_lease_s >= step_timeout_s + 60`.
  - `ProjectPaths::pmstate(&self) -> PathBuf` → `.project-state/state.json`.
  - `pmstate::load(paths: &ProjectPaths) -> anyhow::Result<Option<ProjectState>>` and `pmstate::save(paths: &ProjectPaths, s: &ProjectState) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/state.rs`:

```rust
    #[test]
    fn config_defaults_new_harness_fields() {
        let c: Config = serde_json::from_str(r#"{ "autonomy": "standard" }"#).unwrap();
        assert_eq!(c.stuck_threshold, 3);
        assert_eq!(c.coordinator_lease_s, 1860);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn config_validate_requires_lease_over_timeout() {
        let c = Config { autonomy: Tier::Standard, step_timeout_s: 1800, max_failures: 3,
            stuck_threshold: 3, coordinator_lease_s: 1800 };
        assert!(c.validate().is_err(), "lease must be >= timeout + 60");
    }

    #[test]
    fn pmstate_path_is_state_json() {
        let p = ProjectPaths::new("/tmp/proj");
        assert!(p.pmstate().ends_with(".project-state/state.json"));
    }
```

Add to the `tests` module in `src/pmstate.rs`:

```rust
    #[test]
    fn load_save_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::state::ProjectPaths::new(dir.path());
        assert!(load(&paths).unwrap().is_none());
        let s = ProjectState::fresh(42);
        save(&paths, &s).unwrap();
        assert_eq!(load(&paths).unwrap().unwrap(), s);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib 2>&1 | tail -25`
Expected: compile error / FAIL (`stuck_threshold`/`pmstate()`/`load` missing).

- [ ] **Step 3: Write the implementation**

In `src/state.rs`, extend `Config` (keep the existing `step_timeout_s`/`max_failures` fields and their default fns) and add the validator:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub autonomy: Tier,
    #[serde(default = "default_step_timeout_s")]
    pub step_timeout_s: u64,
    #[serde(default = "default_max_failures")]
    pub max_failures: u32,
    #[serde(default = "default_stuck_threshold")]
    pub stuck_threshold: u32,
    #[serde(default = "default_coordinator_lease_s")]
    pub coordinator_lease_s: u64,
}
fn default_stuck_threshold() -> u32 {
    3
}
fn default_coordinator_lease_s() -> u64 {
    1860
}

impl Config {
    /// The renewed lease must outlast a step's hard timeout by a margin, so a
    /// killed pass can't hold a stale lease past the next spawn (spec §9).
    pub fn validate(&self) -> Result<(), String> {
        if self.coordinator_lease_s < self.step_timeout_s + 60 {
            return Err(format!(
                "coordinator_lease_s ({}) must be >= step_timeout_s + 60 ({})",
                self.coordinator_lease_s,
                self.step_timeout_s + 60
            ));
        }
        Ok(())
    }
}
```

Add to `impl ProjectPaths` in `src/state.rs`:

```rust
    /// `state.json` — the harness ledger (spec §5), written only by the harness.
    pub fn pmstate(&self) -> PathBuf {
        self.state_dir().join("state.json")
    }
```

Add to `src/pmstate.rs` (free functions, after the structs). Put the imports with the file's other `use` lines:

```rust
use anyhow::Result;

use crate::state::{self, ProjectPaths};

/// Load the harness ledger, or `None` if the project has none yet.
pub fn load(paths: &ProjectPaths) -> Result<Option<ProjectState>> {
    state::read_json_opt(&paths.pmstate())
}

/// Persist the harness ledger atomically (the harness is the sole writer).
pub fn save(paths: &ProjectPaths, s: &ProjectState) -> Result<()> {
    state::write_json_atomic(&paths.pmstate(), s)
}
```

> Fix fallout: any existing `Config { .. }` struct literal (tests in `scheduler.rs`, `daemon.rs`, integration tests) now needs the two new fields. Grep: `grep -rn "Config {" src/ tests/` and add `stuck_threshold: 3, coordinator_lease_s: 1860,` to each literal. (Deserialized `Config`s are unaffected — the new fields default.)

- [ ] **Step 4: Run the full test suite + lints**

Run: `cargo test 2>&1 | tail -20 && cargo clippy --all-targets 2>&1 | tail -5 && cargo fmt --check && echo OK`
Expected: all tests PASS, clippy clean, fmt clean, `OK` printed. (Fix any `Config { .. }` literals the compiler flags.)

- [ ] **Step 5: Commit**

```bash
git add src/state.rs src/pmstate.rs
git commit -m "feat(harness): Config lease/stuck knobs + state.json path + pmstate load/save"
```

---

## Self-Review

**1. Spec coverage (slice 1 = spec §12.1 "State + phase machine core"):**
- Phase enum + transitions + `cr→done` reject → Task 1. ✅ (spec §4)
- `phase`/`posture` orthogonality → Task 2 + Task 4 invariant. ✅ (spec §3, §5, §14 risk 1)
- Full `state.json` model (task_cursors, crs, open_stops, review_state, operations, toolchain) → Task 3. ✅ (spec §5)
- `validate()` invariants → Task 4. ✅ (spec §5)
- Config `stuck_threshold`/`coordinator_lease_s` + `≥ timeout+60` → Task 5. ✅ (spec §5, §9)
- `state.json` path + load/save on the existing atomic substrate → Task 5. ✅ (spec §9)
- Out of slice 1 (later slices, correctly absent here): worker dispatch, prompt assembly, verify.rs, ops journal logic, tiers/stops wiring, phase_engine, daemon integration, intake UI. Tracked in spec §12.2–§12.5.

**2. Placeholder scan:** No TBD/TODO; every step has real code or a concrete command. ✅

**3. Type consistency:** `Phase`/`ConfirmDecision`/`Guards` (Task 1) reused by name in later slices; `Posture`/`Run` (Task 2) consumed by `ProjectState` (Task 3) and `validate`/`fresh` (Task 4); `Config` fields + `pmstate()` + `load`/`save` (Task 5) match the signatures declared in their Interfaces blocks. `poll_cadence_s`/`coordinator_lease_s`/`stuck_threshold` named consistently throughout. ✅
