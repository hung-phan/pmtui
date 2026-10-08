# Milestone 5 — Native Coordinator Harness (agent-manager *is* the harness)

**Date:** 2026-08-12
**Status:** DRAFT for user review.
**Supersedes:** `2026-08-12-m5-real-coordinator-design.md` (the "projection shim that
drives the installed skill" approach). That draft is withdrawn — see §1.

Grounded in a parallel mapping of the installed `project-manager` skill; the raw
analysis is preserved at `docs/superpowers/research/2026-08-12-pm-skill-porting-blueprint.json`
and `…-pm-skill-area-maps.json`.

## 1. The pivot

The earlier M5 draft made `pmd` an *external supervisor* that spawned the installed
`project-manager` skill once per step and projected its `state.json` into `pmd`'s
contract files. The owner has redirected:

- **No dependency on the skill at runtime.** agent-manager should *be* the harness.
- **No API key.** The harness never calls a model API directly; it shells out to the
  already-authenticated `claude` / `codex` CLIs.
- **Own the loop in Rust, borrow the playbook.** The Rust harness owns the phase
  state machine, stop detection, tier gating, and `.project-state/` recording. The
  per-phase *prompts* are lifted from the skill into this repo as text. Zero runtime
  coupling to the installed skill.
- **The loop runs on the CLI.** Each phase is one authenticated `claude`/`codex`
  session that runs its own agentic loop to completion, then hands control back to
  Rust. Rust owns phase-to-phase transitions; the CLI owns the work within a phase.

So agent-manager keeps its whole substrate (daemon sweep, per-project scheduler,
tmux `Driver` + done-signal, atomic file state, `flock` lease, tier `policy`) and
adds a **phase machine** on top. A `mode: auto` project is driven through the skill's
full phase sequence — by Rust, not by the skill.

## 2. Goals / Non-goals

**v1 Goals**
- The **full** phase machine (`intake → research → design → plan → implement →
  review → cr → confirming → done`) implemented natively, driving a real
  single-package **GitHub** project end-to-end.
- **Worker-proposes / harness-disposes:** each phase is a detached, authenticated
  `claude -p` / `codex exec` session; the harness verifies the result against
  repository evidence and alone writes control state.
- Autonomy tiers demonstrably change behavior (autopilot auto-flows a Medium
  ambiguity that guardian escalates), with an always-hard safety floor.
- **Intake via pmtui** (an interview form); **stops/answers via terminal + files**
  (pmtui + the existing notifier + `answers.json`). No Slack in v1.
- The daemon stays non-blocking and crash-recoverable; the reference coordinator and
  the M1/M3/M4 contracts keep working (reference stays the integration fixture).

**Non-goals for v1 (additive later slices, see §12)**
- Slack comms / expert outreach (terminal + file only in v1).
- Async/detached `codex` workers (the only source of posture `monitoring`).
- monorepo / multi-package review isolation (GitHub single-package first).
- Full 3-hash checkpoint sealing (v1 relies on atomic-write + one-writer + the
  operations idempotency journal; sealing is a later hardening slice).
- Adversarial-verify *depth* in review (v1 does the repo-evidence gate + a single
  refutation pass on risky surface; the full N-lens panel comes later).

## 3. Architecture

Per sweep tick, per `mode: auto` project, the harness:

1. **Reads posture + phase** from `state.json`. `phase` (the *what*: intake…done) and
   `run.posture` (the *scheduling stance*: `working`/`monitoring`/`needs_you`/`done`)
   are **orthogonal**. Both carry a "done" token; only `confirming → accept` flips
   both. Modeling them as one field is the single biggest correctness risk (§14).
2. **Assembles the phase prompt** = borrowed playbook text (compiled in via
   `include_str!`) + injected context (state facts, `CURRENT.md`-linked file
   pointers, acceptance criteria, binding decisions, the `principles.md` block) + a
   strict REQUIRED-OUTPUT contract (§6).
3. **Dispatches one detached worker** via the *existing* `tmux.rs`
   spawn/observe/done-signal substrate — the sweep never blocks on it. The pane runs
   the authenticated CLI (exact flags in §6); the worker runs its own loop, writes
   artifacts + `steps/<seq>.result.json`, and exits.
4. **Verifies and disposes.** On the done-signal the harness parses `WorkerResult`,
   **verifies against repository evidence** (real commit, tests added+pass, diff
   scope) — a self-claim / exit 0 is never acceptance — validates the proposed
   transition against an allowlist (hard-rejects `cr → done`), applies tier/stop
   gating (§7), applies integrity (§9), and atomically writes the new state. Workers
   write only artifacts + the result descriptor; **never** control state.
5. **Advances at most one transition**, raises a stop, or waits, and the sweep moves
   to the next project.

This reuses the scheduler's double-spawn guard (keyed off in-memory `RunState`),
exponential backoff, and `restore_from_disk` crash recovery essentially unchanged —
`RunState` gains a `Phase` and spawns a phase worker instead of the reference
coordinator.

## 4. The phase machine

`enum Phase { Intake, Research, Design, Plan, Implement, Review, Cr, Confirming, Done }`
(serde lowercase; `Cr → "cr"`). Transitions are an explicit allowlist
`can_transition(from, to, &Guards) -> bool`; `cr → done` is hard-rejected.

| Phase | Advances when | To | Stop kinds |
|---|---|---|---|
| **Intake** | config validated + `brief.md` written + approvals recorded + echoed back; first-start only | Research | ambiguity |
| **Research** | no topics outstanding (each unknown answered with sources) | Design | capability |
| **Design** | `design.md` landed + linked in `CURRENT.md`; gate auto-waived | Plan | ambiguity, publish, stuck |
| **Plan** | `plan.md` landed AND `task_cursors == []` (implement owns reservation); gate auto-waived | Implement | ambiguity, publish, stuck |
| **Implement** | slice complete (every task accepted + digested) → Review; recovery edge → Plan | Review, Plan | worker_stuck, ambiguity, capability |
| **Review** | verification PASS with evidence → Cr; at `attempts ≥ stuck_threshold` raise `stuck` **without leaving** Review | Cr | stuck, capability, worker_stuck |
| **Cr** | plan exhausted (`task_cursors` empty, no slice pending) AND all `crs[]` terminal → Confirming; else another slice → Implement | Implement, Confirming | publish, merge, ambiguity, worker_stuck, stuck |
| **Confirming** | `[A]` accept → Done; `[B]` new direction → Research; `[C]`/silence → stay; fire-condition false on entry self-corrects back | Done, Research, Cr, Implement, Plan | confirm_done |
| **Done** | never self-advances; a fresh user ask reopens → Research | Research | — |

Guards are named predicates: `topics_remain`, `slice_boundary_reached`,
`plan_exhausted`, `all_crs_terminal`, `another_slice_remains`, `confirm_decision`,
`attempts >= stuck_threshold`, etc. **Confirming recomputes its fire-condition on
entry first** — this is the safety net against a premature "done" with open reviews.

## 5. State model (`state.json`)

New/extended structs (serde `deny_unknown_fields`, plus a `validate()` reproducing the
if/then invariants). Written **only** by the harness through `state::write_json_atomic`.

- `phase: Phase` — the *what*. Enforce `phase == Done ⇔ posture == Done`.
- `run: Run { active, posture, owner, wake_condition, next_check, updated_at,
  session_id, continuations, max_continuations }`, `enum Posture {Working, Monitoring,
  NeedsYou, Done}`. Supersedes today's `Step.status` (which already mirrors posture).
  Invariants: `active ⇒ owner/wake/session set`; `monitoring/needs_you ⇒ next_check`;
  `working ⇒ next_check == None`; `needs_you.owner` must be an `open_stops[].id`;
  `posture Done ⇒ active == false` and all handles null.
- `task_cursors: Vec<TaskId>` (unique) — reservation queue of `plan.md` task ids.
  `[]` at plan→implement handoff and at done; len 1 = sequential; len ≥ 2 = one
  Parallel-with group. **Completion is proven by a digest, never a bool.**
- `crs: Vec<Cr{ id: Option<NonEmpty>, slice_id, kind: CrKind{Monorepo,Github}, branch,
  base, status: CrStatus{Draft,InReview,ChangesRequested,Approved,Landed,Abandoned},
  tasks, packages, last_polled, poll_cadence }>` — "open" = status ∉ {Landed,
  Abandoned}. (v1 exercises the `Github` path only; the enum is present for the later
  monorepo slice.)
- `open_stops: Vec<OpenStop{ id, kind: StopKind, channel, context_ref,
  authorized_responders, message_id, first_posted, last_polled, last_seen_reply_ts,
  status: {AwaitingReply, Held} }>`. `enum StopKind {Publish, Merge, ConfirmDone,
  Ambiguity, Stuck, ExpertNeeded, WorkerStuck, Capability}`. Removed (not flagged) on
  resolution.
- `review_state: Option<{ slice_id, signature, attempts (≥1), updated_at }>` — the
  **single** verification-retry counter shared by Review *and* Cr fix loops; keyed on
  `(slice_id, signature)`; a new slice/signature resets to 1; `stuck` fires at
  `attempts ≥ config.stuck_threshold`.
- `operations: Vec<Operation{ id, key, status, started_at, … }>` — idempotency journal
  (§9). `toolchain/capabilities` — populated by detection after intake (v1: minimal —
  git/gh/claude/codex/tmux presence).

`config.json` keeps `autonomy` (tier) and gains `stuck_threshold`,
`coordinator_lease_s` (≥ `step_timeout_s + 60`), and comms/heartbeat knobs (defaulted;
comms unused in v1).

## 6. Worker contract (worker-proposes / harness-disposes)

Per phase-unit tick the harness:

1. **Builds the prompt**: borrowed playbook (`include_str!`) + injected context +
   REQUIRED-OUTPUT contract (the `WorkerResult` schema below).
2. **Resolves the route** from plan annotations + capabilities: `claude` default,
   `codex` for mechanical tasks; `Package:` → worker cwd; `Parallel-with` → the worker
   fans out internally (per the loop-owner decision, a phase session runs its own
   sub-agent loop). v1 = blocking sessions only.
3. **Spawns detached** via the existing wrapper, extended to run:
   `env -u CLAUDECODE claude -p --permission-mode <plan|acceptEdits>
   --no-session-persistence --output-format stream-json --verbose
   --forward-subagent-text [--add-dir …] -- <prompt>` (or `codex -a never
   -s workspace-write exec --ephemeral [--add-dir …] -- <prompt>`), with the
   stream-json trace → `steps/<seq>.log`, exit code → done-signal, and the worker
   instructed to write its machine result to `steps/<seq>.result.json` as its final
   action. Bounded by GNU `timeout` (< renewed lease).
4. **Parses** `WorkerResult { proposed_transition: Option<Phase>, stay: bool,
   stops: Vec<StopDraft{kind,question,options,context_ref,risk_class}>,
   artifacts_written: Vec<Path>, operations: Vec<OpIntent>,
   digest: Option<CompletionDigest{task_ids,commit_ref,tests,files}>, notes }`.
5. **Verifies, does not trust**:
   - **Code phases:** repository evidence is the acceptance gate — git diff scope,
     changed files within the task, required tests added + passing, commit exists.
     Exit 0 / self-claim is never acceptance.
   - **claude code workers:** the stream-json trace must prove an ordered chain — a
     successful `superpowers:test-driven-development` Skill result in a strictly
     earlier assistant record than a successful Agent/Workflow result, matched by
     `tool_use_id` (`name == req || name.ends_with("__"+req)`). Read-only routes
     (permission-mode `plan`) drop the Skill requirement. `codex`/async: repo evidence
     only, no trace validator.
6. **Disposes:** validate the proposed transition against `can_transition` (hard-reject
   `cr → done`), apply integrity (operations reconcile), and commit all control state
   itself. A missing/unparseable result, failed proof, timeout (exit 124/137), or repo
   contradiction ⇒ **keep the cursor and raise `worker_stuck`** (posture `NeedsYou`).
   A proposed illegal transition ⇒ `ambiguity`/`stuck` stop — never silent obey.

## 7. Tiers & stops

Keep the 3-tier model (`Autopilot|Standard|Guardian`) and the existing `policy.rs`,
layering the skill's stop semantics as the safety floor:

- `effective_risk(stop)` = **Hard** if `kind ∈ ALWAYS_HARD_KINDS` (`publish`, `merge`,
  `land`, `deploy`, `confirm_done`, credentials, payment, destructive, **plus newly
  added `stuck` and `capability`**), else the coordinator-labelled `risk_class`.
- `decide(tier, risk)`: **Hard ⇒ escalate at every tier** (incl. autopilot);
  Autopilot auto-flows Medium + Low; Standard/Guardian escalate Medium, auto-flow Low.
- **Auto-flow** = the harness synthesizes an `auto` answer into `answers.json`
  (`answered_by="auto_flow"`), records a source-attributed `decisions.md` note, and the
  next worker consumes it — reusing today's `on_needs_you`/`stops_requiring_human`.
- **Phase gates are always auto-waived at every tier** (design/plan approval is never a
  stop). Tier gating governs only the soft stops (`ambiguity`, `worker_stuck`,
  `expert_needed`). The daemon re-derives the hard floor as belt-and-suspenders even if
  a worker mislabels `risk_class`.
- A stop only sets posture `NeedsYou` after all independent runnable work is exhausted;
  independent work keeps posture `Working`.

Stop → risk table: `publish`/`merge`/`confirm_done`/`stuck`/`capability` = **Hard**;
`ambiguity`/`worker_stuck`/`expert_needed` = **Medium**. (Deliberate divergence from
the skill's single-tier always-halt — see §14. `stuck`+`capability` are Hard precisely
so autopilot can never auto-ship unverified work or proceed without a needed tool.)

**v1 answering:** stops render in pmtui + the existing notifier; the human answers via
the TUI (writing `answers.json`). `confirm_done`/`publish`/`merge` never infer
acceptance from silence.

## 8. Intake (the one non-code phase)

Intake is a harness-owned interactive interview, **run in pmtui** (an extension of the
`n` create flow): goal, package scope, comms destinations, cadences, and explicit
external-action approvals. Borrow the skill's interview table (prompt → field →
default) and the never-blanket-approval question verbatim as UI text; reuse
`config.schema.json`/`state.schema.json` shapes as serde structs. On completion the
harness writes `config.json` + `brief.md` + `approvals[]`, echoes the config back, and
transitions to Research. Intake never re-enters.

## 9. Integrity (what must be preserved)

- **Atomic durable writes** — reuse `state::write_json_atomic` (temp+fsync+rename+dir
  fsync); one writer (the harness) per control file.
- **Worker-proposes / harness-disposes** — workers write only artifacts + `result.json`;
  illegal control states are unrepresentable from a worker.
- **Transition allowlist + guards** — `can_transition`; hard-coded `cr → done` reject.
- **Operations idempotency journal** (`ops.rs`) — every external mutation (branch,
  push, draft-CR, publish, merge, post) happens at most once across crashes. Keyed by a
  deterministic unique key (e.g. `draft-cr:<review-unit>:<slice_id>`, review-unit =
  sorted, %-encoded packages joined `+`); persist `pending` **before** the network
  call; reconcile-before-retry; monotonic `pending → {completed,failed}`.
- **Repository-evidence acceptance** (`verify.rs`) — inspect real diff/tests/commit;
  identical gate for blocking and (later) async returns.
- **Claude trace proof** — as in §6.
- **Coordinator lease + identity** — reuse `lease.rs` flock; extend the owner record
  with `(run_id, session_id)`; renew before dispatch with `coordinator_lease_s ≥
  timeout + 60`.
- **Never-blanket-approval denylist** — enforced in the approval-grant code path, not
  just the prompt.
- **Phase-aware schema validation** (`validate.rs`) — serde `deny_unknown_fields` + the
  extra ledger checks (id uniqueness, CR branch-chain, terminal-CR cursor null, etc.).

**Deferred to a later slice:** the full 3-hash checkpoint seal
(control/material/journal). v1's atomic-write + one-writer + operations journal already
cover crash safety for the GitHub single-package path; sealing is added when async /
multi-writer surface appears.

## 10. Borrowed playbook (hybrid: own the loop, borrow the brain)

Prompts are compiled in via `include_str!` from `src/playbook/*.md` (copied from the
skill, drift-frozen; a header records the source commit). **Zero runtime dependency.**

- **Verbatim / load-bearing:** plan's *vertical-slices* + *program-design-before-code*
  blocks; confirming's `[A]/[B]/[C]` semantics + required recap contents;
  `principles.md` (injected into **every** worker prompt); the research
  `{summary,findings[],sources[]}` and review `VERDICT` schemas; the operation-key
  formats; the review-unit derivation.
- **Adapt:** intake interview flow (harness-driven, not a worker); design's
  brainstorming brain; implement's 6-section worker-prompt structure + the
  TDD-first-action instruction; cr's babysit polling (reimplemented in Rust).

## 11. Rust module map

New: `phase.rs` (enum + `can_transition`), `phase_engine.rs` (per-phase glue, advances
≤1 unit), `worker.rs` (route + CLI command + trace validator + `WorkerResult` parse),
`prompt.rs` (assembly + `include_str!`), `verify.rs` (repo-evidence + risky-surface),
`stops.rs` (grows `policy.rs`), `ops.rs`, `validate.rs`, `due.rs` (cadence authority);
`checkpoint.rs`/`comms.rs` stubbed for later slices.

Reused ~verbatim: `scheduler.rs` (extend `RunState` with `Phase`), `tmux.rs` (extend
the wrapper to emit stream-json + result file), `state.rs`, `lease.rs`, `daemon.rs`,
`policy.rs`, `escalation.rs`, `registry.rs`, `clock.rs`, `view.rs`/`pmtui.rs`.

## 12. Build sequencing (MVP depth first, all phases)

1. **State + phase machine core.** `Phase`, `Posture`, the extended `state.json`
   model, `can_transition` + guards, `validate()`. Pure, exhaustively unit-tested. No
   workers yet.
2. **Phase worker substrate.** Extend the tmux wrapper (stream-json + `result.json`);
   `worker.rs` command construction + `WorkerResult` parse + trace validator;
   `verify.rs` repo-evidence gate. Prove one blocking `claude -p` phase runs, is
   verified, and advances.
3. **The full loop, terminal/file comms.** `phase_engine` wiring all phases; stops in
   pmtui + notifier; auto-flow/escalate via tiers; intake interview in pmtui. Drives a
   real single-package GitHub project intake→done.
4. **Idempotency + GitHub CR.** `ops.rs` journal; `cr` phase over `gh` (draft →
   publish hard stop → merge hard stop); review-unit keys.
5. **Integration test.** A gated end-to-end run over a minimal real GitHub project,
   self-skipping when `claude`/`gh`/`tmux` are unavailable; assert repo-evidence
   gating, one tier-gated fork both ways, and no `cr → done`.

**Later, additive slices (post-v1):** Slack comms + expert outreach; async/detached
`codex` workers (posture `monitoring`); monorepo workspace + multi-package CR isolation;
full 3-hash checkpoint sealing; the N-lens adversarial-verify panel.

## 13. Testing

- **Unit** — the phase machine (every transition + guard + the `cr → done` reject),
  `validate()` invariants, `WorkerResult` parse + trace-proof (fixture logs), the
  stop→risk/tier decision matrix, operation-key derivation + reconcile.
- **Integration** — one gated end-to-end GitHub run (§12.5); a fault-injection test
  that kills a phase worker mid-flight and asserts the lease frees, the cursor is kept,
  and the next sweep re-spawns from intact state (no double-drive, no double external
  action).
- Reuse `FakeDriver` + `FakeClock` for deterministic phase/loop tests.

## 14. Open questions & risks

**Resolved by the owner:** MVP-depth-first across all phases; pmtui intake;
terminal+file comms in v1; GitHub single-package first; per-phase CLI session with Rust
owning transitions; no API key; no skill runtime dependency.

**Deferred design decisions (resolve when their slice starts):**
- Guardian vs Standard for soft stops (v1: identical; later — escalate Low too, or a
  per-phase confirmation?).
- Playbook drift tracking mechanism against the upstream skill.
- Async worker reaping + `monitoring` cadence when that slice lands.
- review-unit / worktree isolation details for the monorepo slice.

**Top risks (from the mapping):** (1) conflating `phase` with `posture` — keep them
distinct; (2) `cr → done` leak / premature confirming — the entry-time fire-condition
recompute + validator reject are load-bearing; (3) trusting worker self-claims — the
repo-evidence + trace proof must not be under-implemented; (4) incomplete operations
journal ⇒ double external actions after a crash; (5) worker prompt/`WorkerResult`
schema drift ⇒ wedged loop (strict schema + parse-fail → `worker_stuck`); (6) safety
regression from tiering (autopilot auto-flowing `ambiguity` picks a default fork
silently — bounded + logged); (7) a synchronous worker freezing the single-threaded
daemon — MUST use the detached-tmux substrate, never a literal synchronous `claude -p`.
