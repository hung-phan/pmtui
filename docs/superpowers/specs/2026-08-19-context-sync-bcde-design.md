<!-- Design doc generated 2026-08-19 via a grounded design+adversarial-critique workflow (context-sync-bcde-design). Each milestone was critiqued and revised; verdicts + fixes folded in below. Pending human sign-off on the open decisions before implementation. -->

# Context-Sync Mechanism — Consolidated Design (Milestones B, C, D, E)

## North star

Today pmd drives one persistent interactive agent per session and, on a cadence, nudges it with a `## What to do now` block. M86 made that block a **strict verbatim echo** of the agent's own `last_status` + `last_plan`, pinned by a firewall test (`the_nudge_is_a_pure_function_of_agent_authored_inputs`, `src/job_engine/tests/nudge.rs:648`). The context-sync milestones exist so the **deciding agent** — the pmd/autopilot side — can actually *run the project autonomously*: it needs a durable, machine-readable record of what has happened (Milestone **B**), that record projected into the supervisor's low-stakes auto-approval decision (Milestone **C**), two new objective stall detectors so "alive but not advancing" and "finished but silent" no longer hide until the 30-minute busy stall (Milestone **D**), and finally a pmd-authored, situation-aware `## What to do now` composed by a bounded LLM narrator (Milestone **E**).

**E deliberately reverses M86's verbatim-echo firewall for the worker nudge.** The user chose this knowingly. The M86 echo is not deleted — it becomes the mandatory, byte-exact **fallback** on every disabled/error/timeout/latched/unavailable path, and the firewall's *spirit* (only agent-authored inputs + a whitelist of objective counters may shape agent-facing text) is preserved by a new narrator-input firewall test rather than a byte-frozen output.

Everything below keeps `policy::decide_kind` (`src/job_engine/policy.rs:66`) **byte-for-byte pure**: no milestone threads ledger prose or counters into the deterministic policy.

---

## Build order (and why)

**B → D → C → E**, landed in series, each rebased on the prior.

1. **B first.** It is the foundation and the largest edit to `AgentLoopState`, `AgentLoopState::fresh` (`job.rs:440`), the round-trip test (`job.rs:548`), and the dispose seam (`marker.rs:120`) — the same three sites C, D, and E also touch. Landing it first means the others rebase onto a settled struct.
2. **D second.** D adds the dedicated `stale_plan_streak` counter and the marker-less-recheck counter that **E consumes**. Its `job.rs` edits (one `u32` field, `fresh`, round-trip) are small and rebase cleanly on B. D is independent of C.
3. **C third.** C's `project_situation` reads B's counters/slice for richness (soft dependency — C is buildable degraded without B, reading only the pre-existing `last_status`/`last_plan`/`continuations`/`open_stops`). Landing after B lets C read the real fields; landing after D avoids a third concurrent `job.rs`/`fresh` rebase.
4. **E last.** E is the biggest, riskiest change; it consumes D's stall counter and (optionally) B's counters as narrator inputs, and it reverses the M86 echo firewall. It should land only once B/C/D are green and the objective inputs it surfaces are stable.

The only hard code dependency is **C needs B's field names** to compile its rich projection; the rest are soft (degrade to `0`/empty) or pure merge-ordering to avoid textual conflicts on `job.rs` (`fresh` + round-trip) and `job_engine/mod.rs` (`JobScheduler` struct + `new()`).

---

## Milestone B — the decider-lane ledger

### Goal
Extend the single-writer `state.json` (`AgentLoopState`) with monotonic digest counters, a bounded/coalesced `decisions` slice, and a current `situation` snapshot; plus two side files written on the same dispose path — an append-only, size-rotated `raw.jsonl` (~2 MB) for machine full-fidelity, and a sparse, source-attributed `decisions.md` for humans. No separate `digest.json`. "The write IS the compaction."

### Approach — corrected to a single accepted-bump seam
The original design placed `record_decision` + `persist_decision_files` inside each of the four dispose arms, *before the existing `save_ledger`*. **The critique correctly identified two paths that silently skip all B writes**, so the design is revised:

> **Collapse the unconditional part of B into ONE helper invoked at the TOP of `dispose_report`, immediately after the accepted-bump is confirmed and `busy_since` is cleared (`marker.rs:109-110`), operating on `next` before any WakeState branch.**

This realizes B's own thesis ("there is no separate step that could silently never run") and resolves both criticals at once:

- **CRITICAL-1 (auto-flow+supervisor path skipped) — RESOLVED.** The Blocked-auto-flow arm's primary sub-path returns `Ok(Some(tick))` at `marker.rs:313` *before* the arm-local `save_ledger` (`marker.rs:334`), with the real save happening inside `spawn_advice` on a `parked = next.clone()` (`supervisor.rs:300-303`). By bumping `digest.disposed`, setting `situation`, and appending the `raw.jsonl` line on `next` at the top of `dispose_report` — before any branch — the clone carries them and the auto-approved decision is recorded.
- **CRITICAL-2 (blocked-with-no-stop → `park_stuck` at `marker.rs:209-215`) — RESOLVED.** That accepted bump never reaches the four arms, but it *does* pass the top-of-`dispose_report` seam, so it now bumps `disposed`, updates `situation`, and emits a raw line.

**Per-arm refinement stays where it belongs.** The *kind-specific* fields — the per-kind counter (`working`/`monitoring`/`auto_flow`/`escalated`/`stalled`), the summary one-liner, `stop_ids`, and the sparse `decisions.md` line — are still set in each arm, since only the arm knows the outcome. The invariant `digest.disposed == number of accepted bumps` is now structurally guaranteed by the single seam.

**Non-dispose park paths stay out of `disposed`.** `park_stuck`/`park_stuck_kind` are also reached from non-accepted-bump sources — `lenient_working` (`marker.rs:356`), `max_wakes` (`nudge.rs`), budget backstop. Those are **not** accepted bumps and MUST NOT bump `digest.disposed` (Missing-1). They may still record a `Stalled` DecisionRecord with `seq: None` (see below), but `disposed` counts accepted bumps only.

### Data / signature changes (`src/job.rs`)
`AgentLoopState` (struct at `job.rs:52`, `#[serde(deny_unknown_fields)]`) gains three `#[serde(default)]` fields so old ledgers and the minimal-load test (`job.rs:551`) still parse:

```rust
#[serde(default, skip_serializing_if = "DecisionCounters::is_zero")]
pub digest: DecisionCounters,
#[serde(default, deserialize_with = "de_decisions_lenient", skip_serializing_if = "Vec::is_empty")]
pub decisions: Vec<DecisionRecord>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub situation: Option<LedgerSituation>,
```

New types (all `#[serde(deny_unknown_fields)]`):
- `DecisionCounters { disposed, working, monitoring, auto_flow, escalated, stalled: u64 }` — monotonic, saturating, `is_zero()` for skip-serialize; additive future counters via all-`#[serde(default)]`.
- `DecisionKind { Working, Monitoring, AutoFlow, Escalated, Stalled }` (`rename_all="snake_case"`).
- `DecisionRecord { seq: Option<u64>, at: Epoch, count: u32 (default one_count), kind: DecisionKind, summary: Option<String>, stop_ids: Vec<String> }` — **`seq` is `Option<u64>`** (Missing-1) so the malformed-marker `Stalled` path (no parsed `report.seq`) is representable; `None` means "not an accepted bump."
- `LedgerSituation { state: WakeState, status: Option<String>, open_stops: Vec<String>, seq: u64, at: Epoch }`.

Methods/consts:
- `pub fn record_decision(&mut self, now, entry, situation)` — bumps `disposed` + per-kind, **coalesces on `(kind, summary, stop_ids)`** (Missing-2: divergent `stop_ids` create a new entry — never a row with a bumped seq but stale ids), drains oldest past `DECISIONS_SLICE_MAX`, sets `self.situation`. Caps summary via `cap_detail` (`job.rs:229`) before compare, exactly as `record_event` (`job.rs:372-391`).
- `fn de_decisions_lenient` mirroring `de_events_lenient` (`job.rs:247`) — one bad entry is dropped, never fails the load.
- `pub const DECISIONS_SLICE_MAX: usize = 64`.
- `fresh` (`job.rs:440`) initializes `digest=Default`, `decisions=vec![]`, `situation=None`.

### Side files
- `src/state/paths.rs`: `raw_jsonl()` → `<state_dir>/raw.jsonl`, `raw_jsonl_rotated()` → `.1`; `decisions()` (`paths.rs:76`) reused.
- `src/job_engine/mod.rs`: `RAW_JSONL_MAX_BYTES: u64 = 2*1024*1024`; `fn append_raw(&self, line, max_bytes)` — `create_dir_all`, size-check-then-rename-then-append (the `append_decision_note` style, `scheduler.rs:350-364`; no append-atomic primitive exists per `atomic.rs`); `fn persist_decision_files(&self, entry)` — always a full JSONL line, and for **notable** kinds a `- {epoch} pmd {kind}: {summary}` line on `decisions.md`.
- **Error isolation:** both side-file writers run *after* `save_ledger` returns and are called `if let Err(e) = … { eprintln!(…) }` — a side-file failure is logged and swallowed; the operational `state.json` write already happened.

### decisions.md notability & rotation (Important-2 — RESOLVED)
`AutoFlow` (auto-approval) is the **common** decider action in an autopilot session, so marking it "notable" would make the one unbounded human file grow fastest. Revised:
- **Notable set for `decisions.md`: `Escalated`, `Stalled` only.** `AutoFlow` and plain `Working`/`Monitoring` go to `raw.jsonl` full-fidelity, not the human file.
- **`decisions.md` gets the same single-generation rotation** as `raw.jsonl` (a `decisions.md.1` sibling) so even the sparse file cannot grow unbounded over a multi-day session. (Confirm with human — see Decisions.)

### Files touched
`src/job.rs`, `src/state/paths.rs`, `src/job_engine/marker.rs`, `src/job_engine/mod.rs`, `src/job_engine/tests/marker.rs`, `src/job_engine/tests/mod.rs`.

### Test strategy (non-vacuous; critique gaps closed)
- `digest_counters_are_monotonic_and_never_double_count` — dispose N mixed-state bumps; assert `disposed==N`; **now includes a blocked-no-stop bump AND an auto-flow bump that successfully spawns a consult** (FakeDriver `spawn_step` Ok + consultable brief) and asserts both increment `disposed` and emit a `raw.jsonl` line (Important-1 — the exact paths the original suite could not see). Sabotage: re-observe a stale `seq <= last_marker_seq` and assert `disposed` unchanged (pins the `marker.rs:102` anti-replay guard).
- `decisions_slice_is_bounded_and_coalesces` — 3 identical `(kind,summary,stop_ids)` → one entry `count==3`; **two AutoFlow with different `stop_ids` → two entries** (Missing-2). Push `>MAX` distinct → `len==MAX`, newest survives.
- `ledger_round_trips_full_and_minimal` — full case with all three fields; minimal case asserts `is_zero()`/empty/`None`; fresh-JSON contains none of `digest`/`decisions`/`situation`.
- `a_bogus_decision_is_dropped_and_operational_ledger_loads` — inject a bogus decision object; ledger still `.expect()`-loads with run/cadence/watermarks intact.
- `raw_jsonl_appends_one_line_per_disposed_decision` + `raw_jsonl_rotates_past_the_ceiling` (tiny injected ceiling, sub-ms).
- `decisions_md_is_sparse_and_source_attributed` — one Working (absent) + one Blocked-escalating (`pmd escalated:` present); **plus one AutoFlow asserted ABSENT from `decisions.md` but PRESENT in `raw.jsonl`** (pins the revised notable set).
- `situation_reflects_last_disposed_report` — including the blocked-no-stop path (Missing-3: situation must not go stale on the park path, now guaranteed by the top-of-dispose seam).
- `side_file_error_never_fails_the_state_json_write` — make `raw_jsonl()` a directory; dispose still `Ok`, `disposed==1`.
- `the_nudge_is_unaffected_by_the_decider_ledger` — extends the firewall test; populate B fields, assert nudge bytes byte-identical.

### Deferred / documented
- **Torn `raw.jsonl` last line (Missing-4):** no append-atomic primitive, so a crash mid-`writeln` can tear the last line; a rotation then preserves it in `.1`. Documented contract: *any* future reader of `raw.jsonl` MUST use lenient JSONL parsing and tolerate a torn final line. Reader is out of scope for B.
- **`digest.stalled` undercount (Important-3):** stalls reached via non-dispose `park_stuck` (lenient/max_wakes) are recorded as `Stalled` DecisionRecords with `seq: None` but do not bump `disposed`. This is intentional (disposed == accepted bumps); the per-kind `stalled` counter is best-effort. Flagged, not silently dropped.

---

## Milestone C — supervisor "situation" projection

### Goal
Give the supervisor consult goal-aware **context** (recent auto-decisions, agent-authored status/plan, objective counters from B) as a new fifth `Consult` field `situation`, fenced as untrusted DATA, so the low-stakes decision is grounded in the run's history — while `decide_kind` stays pure and the consult **never** degrades because of the added context.

### Approach — corrected so context is purely additive
The original design folded `situation.len()` into the same 8 KiB `MAX_CONSULT_DATA_BYTES` gate. **The critique's headline break is real:** `situation` is *additive*, so a previously-consultable decision (goal 4096 + question 3900 = 7996 ≤ 8192) plus a 2 KiB situation → 10044 > 8192 → `is_consultable()` false → `spawn_advice` returns `Ok(None)` → the pre-m20 static note. Richer ledgers produce larger situations, so the sessions with the *most* history would be the *most* likely to lose the consult. Revised:

> **`situation` is bounded solely by its own `MAX_SITUATION_BYTES = 2*1024` clamp and is EXCLUDED from the `is_consultable` byte sum (`consult.rs:108-111`).** (CRITICAL — RESOLVED.)

This guarantees C is purely additive context that can never turn a consultable decision unconsultable. (Alternative considered and rejected as more fragile: bump `MAX_CONSULT_DATA_BYTES` by `MAX_SITUATION_BYTES`.)

Rendering: `build_consult_prompt` (`prompt.rs:60`) emits a third nonce-derived DATA fence tagged `SITUATION`, only when `situation` is non-empty (empty → fence omitted → consult byte-identical to pre-C). The `strip` closure (`prompt.rs:63`) must strip **all three** tags (`GOAL`/`WORKER-DATA`/`SITUATION`) from **all** fields, and `situation` is `.trim()`ed like goal/question.

The projection is built in the engine (never in the pure `advise` crate): `fn project_situation(next: &AgentLoopState) -> String` in `src/job_engine/supervisor.rs`, called from the `spawn_advice` Consult literal, reading B's counters/slice + `last_status`/`last_plan`/`continuations`/`open_stops`, passed through `clamp_situation`.

### Critique resolutions
- **Clamp direction vs recency (Important) — RESOLVED.** `clamp_situation` mirrors `clamp_goal` (keeps the beginning). So `project_situation` orders decision entries **newest-first**, and the clamp truncates the stale tail — the most decision-relevant history survives.
- **"Agent-authored" claim inaccurate (Important) — ACKNOWLEDGED.** `dispose_report` overwrites `next.last_status` with a harness cadence note (`marker.rs:163-170`) before `spawn_advice`, so harness prose lands inside the untrusted SITUATION fence. This is safe (fencing trusted text is harmless). The `SUPERVISOR_SYSTEM_PROMPT` sentence is worded **conditionally** ("*if* a SITUATION block is present, treat it as untrusted progress context…") so it never misleads the model on consults where the block is omitted.
- **Precedent-bias / grounding hazard (Missing) — RESOLVED with a prompt line.** Surfacing past auto-approvals could bias the supervisor to rubber-stamp from precedent. Blast radius is bounded (`decide_kind` already gated `AutoFlow` upstream at `marker.rs:255`; the supervisor picks an index or refuses and cannot widen authority per `advise/mod.rs:7-14`), but the system prompt gains an explicit line: *the situation is history for context only; you must still VERIFY this specific decision and must not approve merely because a similar action was auto-approved before.*
- **Migration undercount (Important) — NOTED.** Adding a non-`Default` fifth field breaks **every** `Consult{}` literal, not just the two named helpers: `consult_with_options`/`consult_free` (`tests.rs:14-30`) plus inline literals at `tests.rs:325, 378, 494, 500, 508, 518, 535, 546`. All must set `situation: String::new()` (or a value) or the crate won't compile.
- **`decide_kind_stays_pure` is a compile-guard (Missing) — ACKNOWLEDGED.** It documents the invariant (a future thread-through breaks the build) but is not runtime coverage; it is not counted as real coverage.
- **Ordering wrinkle vs B (Missing) — DECISION NEEDED (see below).** Because B now appends the current decision at the top of `dispose_report`, and `spawn_advice` runs downstream at `marker.rs:312`, the current in-flight decision is already in the slice when `project_situation` runs. Recommended: `project_situation` **excludes the just-appended current entry** (project the prior state) so the supervisor is not shown a decision it has not yet made. Flagged for confirmation.

### Data / signatures
- `src/advise/consult.rs`: `MAX_SITUATION_BYTES: usize = 2*1024`; `pub fn clamp_situation(&str) -> String`; `Consult` gains `pub situation: String`; **`is_consultable` unchanged** (situation excluded).
- `src/advise/mod.rs`: re-export `clamp_situation` (+ `MAX_SITUATION_BYTES` if the projection helper needs it).
- `src/advise/prompt.rs`: `SITUATION` fence + strip + one conditional system-prompt sentence + the verify-don't-defer line.
- `src/job_engine/supervisor.rs`: `fn project_situation(next: &AgentLoopState) -> String`.
- **No persisted field** — situation is projected fresh at spawn, in-memory only; no serde migration, no torn-write exposure. Keep it that way.

### Files touched
`src/advise/consult.rs`, `src/advise/prompt.rs`, `src/advise/mod.rs`, `src/job_engine/supervisor.rs`, `src/advise/tests.rs`, `src/job_engine/tests/` (supervisor/marker module).

### Test strategy
- `situation_is_NOT_counted_in_the_consult_budget` — a consult whose goal+question is already at the limit stays `is_consultable()==true` after adding a 2 KiB situation (pins the revised gate). Paired with a control where the *goal itself* over-budget still degrades, proving the gate still works for the fields it governs.
- `clamp_situation_truncates_on_a_char_boundary_and_announces` — `"é".repeat(MAX)` clamps, announces, no panic; in-budget passes byte-for-byte; **newest-first ordering asserted** (a recent sentinel survives, an old one is dropped).
- `the_prompt_fences_the_situation_as_untrusted_data` — forged `SITUATION` fence in situation stripped (count==2); **also a forged SITUATION fence embedded in goal/question is stripped**.
- `an_empty_situation_omits_the_block_but_keeps_goal_and_worker_data`.
- `project_situation_reads_the_ledger_and_clamps` — fresh → `""`; rich ledger → contains `last_plan` sentinel, clamped `<= MAX`; **excludes the current in-flight decision** (asserts the just-disposed entry's marker is absent, per the ordering decision).
- `spawn_advice_puts_the_projected_situation_in_the_consult` — FakeDriver records `spawn_step` argv (no send_keys/idle path changes, so real-tmux not required); assert sentinel inside the SITUATION fence when populated and absent when thin.
- `an_over_budget_situation_does_not_disable_the_consult` — the inverse of the old (now-removed) degrade: a giant projection is clamped to 2 KiB and the consult **still spawns**; the static-note degrade is reached only when goal/question themselves exceed budget.

---

## Milestone D — FO-1 plan-staleness detector + FO-2 marker-less-finish recheck

### Goal
Catch two stall modes the existing counters miss, without letting the harness interpret prose: (FO-1) an agent that bumps a fresh marker seq every wake but keeps restating the same plan; (FO-2) an agent that finished a turn and went idle but never wrote its marker.

### FO-1 approach — corrected semantics, shared escalation, stronger signal
- **CRITICAL-2 (off-by-one / vacuous test) — RESOLVED by pinning semantics.** `stale_plan_streak: u32` counts **consecutive repeats**. The first plan report goes through the adopt branch (streak stays 0); each subsequent report whose plan is trim-equal to the stored plan increments the streak. So a fresh ledger needs `DEFAULT_STALE_PLAN_STALL + 1` same-plan reports to escalate. The threshold test **seeds `last_plan` explicitly** so it is non-vacuous, and asserts the pre-threshold tick nudged (`JobTick::Monitoring`, `sent_keys` grew) while the threshold tick returned `Stuck` with no keystroke — proving it is the *count* that triggers.
- **Important-2 (weak proxy) — RESOLVED.** The streak increments only when **both `last_status` AND `next_step` are byte-identical** (still pure equality, no prose interpretation). A healthy long-grind agent that keeps a stable `next_step` ("keep running the suite until green") but reports changing `last_status` will *not* accrue the streak. This matches "alive but not advancing" far better than `next_step` alone.
- **Important-1 / Missing (Monitoring never escalates) — RESOLVED.** The streak accrues in the pre-match keep-prior block (`marker.rs:142-144`) for Working/Monitoring/Blocked alike, and the **threshold check is placed in the shared pre-match location** (before the WakeState match) rather than only the Working arm — so a self-napping Monitoring agent restating the same plan still escalates via `park_stuck_kind(now, &next, StopKind::WorkerStuck, reason)` (`stops.rs:284`), the same backstop the busy-stall/malformed-marker/dead-pane escalations use. This is not a new terminal path and bypasses `decide_kind` exactly as those do.
- **Missing (trim both sides / store trimmed) — RESOLVED.** Comparison trims both sides and keep-prior stores the **trimmed** plan, so trailing-newline/whitespace drift cannot spuriously reset the streak.
- Resets: `on_blocked` (`stops.rs:66-77`, beside `continuations = 0`) so answering "keep going" clears it; and `retime` (`job.rs:425`). Both are fail-safe (can only delay, never fire early).

### FO-2 approach — corrected to preserve a fast backstop
- **CRITICAL-1 (FO-2 silently removes the 30-min busy-stall backstop) — RESOLVED.** As originally written, the relaxed gate (`awaiting_report() && !turn_finished_since_nudge()`) falls through to `nudge()`, which clears `busy_since` (`nudge.rs:139`), so `park_stuck` after `DEFAULT_STALL_BUSY_S=1800s` can never fire; the only surviving bound is `max_wakes` (~7 days at a 5-min cadence). Fix: add a **bounded marker-less-recheck counter** — each time FO-2 relaxes the hold and re-nudges a turn-finished-but-marker-less agent, increment it; after **K consecutive** marker-less rechecks, escalate `WorkerStuck` via the same `park_stuck_kind` machinery. This restores a fast, bounded backstop **and** yields an objective "finished N turns without reporting" counter that E can surface. The counter resets on any accepted marker bump and on `on_blocked`.
- `turn_finished_since_nudge(&self) -> bool` beside `turn_in_progress` (`drive.rs:244`): true iff the turn-signal file exists AND size > `turns_at_nudge` (a `Some` baseline). Refines the first gate in `idle_observed` (`drive.rs:303`) to `if base.awaiting_report() && !self.turn_finished_since_nudge()`. With no hook/baseline it is false → byte-identical to today.
- **Important-3 / Missing (content-stability is the sole remaining guard) — RESOLVED with a test.** Once the hold is relaxed, `turn_in_progress()` is false by construction, so the fingerprint + idle-confirmation gates are the only guard against typing into a working agent. Add a test that a **still-changing transcript re-arms** (does not nudge) on the relaxed path, and confirm the `human_present` gate covers the shared-hooked-process concern.
- **Missing (attach/detach + `turns_at_nudge`) — HANDLED.** `turns_at_nudge` deliberately survives attach/detach (`mod.rs:503`). Add a test for attach-drive-several-turns-then-detach: assert FO-2 does not immediately re-nudge a legitimately-stale marker against a pre-attach baseline (confirm intended, or re-baseline on detach).
- FO-2 lives only in `idle_observed` (full drive path), never `drive_marker_only` — the I1 cadence guard is untouched.

### Data / signatures
- `src/job.rs`: `#[serde(default)] pub stale_plan_streak: u32` (after `continuations`, plain default like the sibling counter) + a `#[serde(default)]` marker-less-recheck counter; init in `fresh`; reset in `retime`; extend the full/minimal round-trip test.
- `src/job_engine/marker.rs`: staleness accounting at the keep-prior block; threshold check in the shared pre-match location; `DEFAULT_STALE_PLAN_STALL` and marker-less `K` consts.
- `src/job_engine/drive.rs`: `fn turn_finished_since_nudge(&self) -> bool`; refined `awaiting_report` gate; marker-less-recheck counter increment + escalation.
- `src/job_engine/stops.rs`: reset both counters in `on_blocked`.
- No `Config`-schema change (consts, matching `DEFAULT_STALL_BUSY_S`) — unless the human wants `DEFAULT_STALE_PLAN_STALL` as a per-session `Config` field (see Decisions).

### Test strategy (non-vacuous)
`restated_plan_bumps_dedicated_streak_not_continuations`; `changing_the_plan_resets_the_streak`; `a_terse_bump_neither_bumps_nor_resets`; `same_plan_but_changed_status_does_not_accrue` (pins the both-fields rule); `repeated_plan_past_threshold_escalates_worker_stuck` (seeds `last_plan`, asserts pre-threshold nudge + threshold `Stuck`); `monitoring_state_plan_staleness_also_escalates` (Important-1); `human_answer_resets_plan_staleness`; round-trip. FO-2: `a_completed_turn_rechecks_a_marker_less_finish`; `no_turn_signal_leaves_the_awaiting_report_hold_intact`; `marker_less_recheck_is_rate_limited_to_one_per_completed_turn`; `marker_less_recheck_escalates_after_K` (pins the restored fast backstop — CRITICAL-1); `still_changing_transcript_re_arms_on_relaxed_path` (Important-3); `attach_then_detach_does_not_spuriously_renudge` (Missing); `turn_signal_size_zero_edge`.

### Deferred / documented
- **Exact trim-equality only (open):** semantically-reworded identical plans evade FO-1. This is a deliberate floor (semantic similarity would require interpreting prose, violating the purity rule). Documented.
- **`max_wakes==0` unbounded re-nudge:** the marker-less-recheck counter now bounds this independent of `max_wakes`, so the original risk is closed.

---

## Milestone E — pmd-composed dynamic "## What to do now" via a bounded LLM narrator

### Goal
Replace the static verbatim echo (`nudge.rs:256-277`) with pmd-authored, situation-aware prose generated from agent-authored inputs + a whitelist of objective counters, grounded **only** in the ledger (never the codebase), degrading to **exactly** the M86 echo bytes on any failure, using its **own** slot/latch/seq/binary probe (never sharing `self.advise`), and bounding cost.

### What the critique confirmed is right (keep intact)
The **slot/latch/seq/binary-cache isolation** from `self.advise`, and making `narrate_step` return `Result<()>` so it can never park or win the tick, cleanly eliminate the priority inversion the task warned about — a flaky narrator can neither evict a consult nor latch it off, and a consult in flight never blocks a narration. This design is preserved.

### CRITICAL fixes

**CRITICAL-1 — efficacy/staleness (the central flaw).** As designed, `narrate_step` runs only on the due `drive()` path; mid-cadence marker bumps route to `drive_marker_only`, which *still* disposes the marker and rewrites `last_status`/`last_plan`. So by the next `drive()` the signature has advanced, the in-flight narration was spawned for the old signature, and `current_narration(sig)` returns `None` → echo. Fresh prose lands only when two consecutive cadences share an identical signature — i.e. a quiescent session where the echo already shows the same lines. **Resolved by two changes, chosen explicitly:**
1. **Key the cache on a coarse *situation class***, not the verbatim latest line: `{ blocked/working/monitoring, human_answer_pending, stall bucket, elapsed bucket }`. Minor `last_status` churn no longer invalidates a completed narration; the prose describes the situation *class*, which is what "what to do now" guidance needs.
2. **Reap on the fast path too.** `drive_marker_only` gains a cheap, non-nudging reap (a `stat` of the done-signal + read-if-complete into the cache). Spawning stays on the due path only (to honor I1 and avoid `claude` on the fast sweep), but a completed narration is picked up promptly instead of a full cadence later.

**CRITICAL-2 — cache-key bug.** If `wakes`/fine `elapsed` are in the signature, the sig changes every nudge → cache never hits, reaped prose never matches, and a fresh narration spawns every cadence (the exact "call the LLM every wake" we must avoid). **Resolved:** `narrate_sig` is computed over the coarse situation class above and **excludes `wakes` and fine-grained time**. This is consistent with CRITICAL-1's coarse-class key.

### IMPORTANT fixes

- **Grounding not structurally enforced (Important-1) — RESOLVED.** `worker::build_supervisor_command` hardwires `--permission-mode plan`, which *allows* reads/greps (the consult deliberately kept that). For the narrator that is pure downside — it lets the model read the tree and manufacture task facts. **Do not reuse the consult's read-enabled argv.** Extend the command builder with a **no-tools / tool-denylist mode** for the narrator so the codebase is structurally unreachable (requirement (a)), not merely forbidden by prompt.
- **No guard against invented/contradicting guidance (Important-2) — RESOLVED as far as feasible.** The narrator output is injected as authoritative imperative text into a live agent; hallucinated file names or a "you appear done, wrap up" line would contradict the retained "You do not decide when done" section. Mitigations, all adopted: (a) keep the echo's **reminder-framing preamble** ("this is a reminder, not a new instruction; you hold full context, re-derive if things changed", `nudge.rs:260-261`) wrapping the prose; (b) `validate_narration` **rejects prose asserting completion** (done/complete/finished-style claims) and rejects new imperative task specifics beyond the whitelisted inputs; (c) on any rejection, degrade to the echo. Grounding for invented facts remains partly prompt-enforced — flagged in Risks.
- **No hard bound on total narrations (Important-3) — RESOLVED.** Add a **per-session narration budget** (a count cap) in addition to `NARRATE_MIN_INTERVAL_S`, gate spawns to when a nudge is actually imminent (idle-gate armed), and pass a default `--max-budget-usd` to the narrator command (the consult's builder defaults to `None`).

### MISSING fixes
- **Placement before dead-pane escalation (Missing-1) — RESOLVED.** Move `narrate_step` to run **after** `dead_pane_escalation` (`drive.rs:192`) and gate spawning on the pane being idle/alive, so it never spawns `claude` for a session that is about to go `Blocked`.
- **Fast-path human-attach hole (Missing-2) — RESOLVED.** Add `abandon_narration` to `drive_marker_only`'s own `human_present` gate (`drive.rs:433-446`), not just `drive()`'s.
- **Post-restart stale done-signal/log collision (Missing-3) — RESOLVED.** `spawn_narration` mirrors the consult's `remove_file` of stale `narrate-<seq>.done`/`.log` before spawning (`supervisor.rs:256-257`). The per-narration nonce already makes `validate_narration` reject a stale reply (degrade to echo, no spurious escalation), but the wasted spawn is avoided.
- **Firewall test vacuity (Missing-4) — RESOLVED.** The **load-bearing** test is `narrator_input_firewall` (only whitelisted inputs reach `build_narrate_prompt`; a never-passed marker-seq token is absent). Additionally assert at the delivery seam that with a cached prose present, the delivered nudge equals `loop_nudge_prompt(agent-inputs, Some(prose), marker)` and contains **no raw ledger counter**.
- **Signature hash collision (Missing-5) — RESOLVED.** Store the situation-class **key value** (not just a `u64`) alongside the cached prose and compare the actual key before delivering — never deliver on a bare `u64` match.

### Shape (unchanged where sound)
Detached, best-effort headless `claude -p` (same substrate, but **narrator-specific no-tools command**), tee'd log + atomic done-signal. `narrate_step` returns `Result<()>` — never parks, never types, never escalates: REAP (observe in-flight, on success store `narrate_last=(class_key, prose)` + `note_success`; on refusal/nonzero/orphan/deadline count toward `narrate_health` and **drop silently** — no escalation, no `pending_context`) then MAYBE-SPAWN (if not in flight, not latched, binary present, inputs non-empty, situation class differs from cached, interval elapsed, per-session budget not exhausted, pane idle/alive).

`loop_nudge_prompt` gains `whatnow: Option<&str>`: `None` → the M86 echo verbatim (the fallback); `Some(prose)` → reminder-preamble + prose, with the four operating-guardrail bullets, the "You do not decide when done" section, the goal, and the "Signal a decision point" schema all emitted **verbatim regardless** (see Decisions on whether the bullets are replaced or retained — recommended **retained**).

### Data / signatures
- `src/advise/narrate.rs` (NEW pure module): `NARRATE_SYSTEM_PROMPT`, `NARRATE_SCHEMA`, `MAX_NARRATION_BYTES`, `NarrateInputs { goal, last_status, last_plan, human_answer_pending, elapsed_bucket, stall, marker_less_streak, .. }` (**`wakes` excluded from the signature even if surfaced**), `build_narrate_prompt(nonce, &NarrateInputs)`, `validate_narration(nonce, raw) -> Result<String, NarrateRefusal>` (nonce + control-byte + size + non-empty + **done-claim rejection**), `NarrateRefusal`.
- `src/job_engine/narrate.rs` (NEW): `NarrateInFlight`, `narrate_step`, `spawn_narration` (with stale-file cleanup), `abandon_narration`, `narrator_binary_present`, `current_narration(class_key) -> Option<&str>`, `narrate_class(base, inputs)` (coarse), `NARRATE_*` consts (tighter timeouts than the consult, e.g. ~25s shell / ~30s reap; a per-session budget cap; a cheaper/faster model option).
- `src/job_engine/mod.rs`: 7 in-memory `JobScheduler` fields (`narrate_enabled`, `narrate`, `narrate_seq`, `narrate_binary`, `narrate_health`, `narrate_last: Option<(ClassKey, String)>`, `narrate_at`) + a per-session narration count; `set_narrator_enabled` test hook; **not serialized**, no `restore_from_disk` change.
- `src/job_engine/nudge.rs`: `loop_nudge_prompt(brief, extra, last_status, last_plan, whatnow: Option<&str>, marker_path) -> String`; `nudge()` computes the class key + `current_narration` and passes it.
- `src/job_engine/drive.rs`: `narrate_step` **after** `dead_pane_escalation`; `abandon_narration` in **both** `human_present` gates; **not** spawning in `drive_marker_only` (but reaping there).
- `src/state/paths.rs`: `narrate_done_signal(seq)`, `narrate_log(seq)`.
- `src/tmux/session_names.rs`: `narrator_session_name(id, root, seq)` → `pmnar-…` (distinct prefix from `pmsup-`).

### Files touched
`src/advise/narrate.rs` (new), `src/advise/mod.rs`, `src/job_engine/narrate.rs` (new), `src/job_engine/mod.rs`, `src/job_engine/nudge.rs`, `src/job_engine/drive.rs`, `src/state/paths.rs`, `src/tmux/session_names.rs`, `src/job_engine/tests/nudge.rs`, `src/advise/tests`, `jokes-acceptance-test.sh` (or sibling).

### Test strategy (non-vacuous)
`loop_nudge_prompt_narration_none_is_the_m86_echo` (byte-identical) + the `Some(...)` control; `loop_nudge_prompt_keeps_guardrails_under_narration`; reworked `the_nudge_is_a_pure_function_of_its_inputs` (determinism + closure, narrator disabled) **backed by** the load-bearing `narrator_input_firewall`; `validate_narration_rejects_control_bytes_bad_nonce_and_done_claims`; `narrator_disabled_and_latched_degrade_to_echo` (cached prose present, must not leak); `narration_timeout_never_escalates` (asserts run NOT Blocked, tick NOT `Stuck`); `narration_success_then_cached_reuse` across the **coarse class** (minor status churn → still a cache hit; class change → fresh spawn); `fast_path_reaps_but_never_spawns_or_nudges`; `narrator_uses_a_no_tools_command` (argv assertion — grounding structurally enforced); `slot_isolation_no_priority_inversion`; `human_present_abandons_narration_on_both_paths`; `per_session_narration_budget_caps_spawns`; `collision_key_compared_not_just_hash`. Plus the mandated **real-tmux acceptance** (per the locked memory): a headless narration actually replaces the whatnow block, and `PM_NARRATOR=off` / binary-absent falls back to the M86 echo — asserting CLI/pane output, not exit codes.

---

## Risks & guardrails (E-heavy)

**Milestone E (highest concern):**
- **Authoritative-text hazard.** Narrator prose is typed into a live autonomous agent as guidance. Guardrails: reminder-framing preamble, retained "You do not decide when done" section, `validate_narration` done-claim rejection, control-byte rejection, and hard degrade to echo on any doubt. Residual: invented *task facts* (file names, commands) are only prompt-forbidden even with a no-tools command — a determined model could still fabricate from the fenced inputs. This is the milestone's headline residual risk; the no-tools command removes the *codebase-read* vector but not fabrication. Flagged.
- **Efficacy vs cost tension.** Coarse-class caching + fast-path reaping makes fresh prose actually land during active work; the per-session budget + min-interval + single slot + idle-armed spawn gate cost. A pathological status-flapping agent is bounded by the class key (flaps within a class don't spawn) and the budget cap.
- **Priority inversion — closed by design.** Separate slot/latch/seq, `Result<()>` return; a narrator can never park the tick, evict a consult, or escalate.
- **Model-pin silent fallback.** `build_supervisor_command` silently falls to ambient opus on a bad model id; a `PM_NARRATOR_MODEL` override must be a full id. Guardrail: validate/log the id; default to the known-good `SUPERVISOR_MODEL` if unset.
- **Firewall reversal.** M86's byte-exact echo firewall is intentionally relaxed. Guardrail: the echo is the mandatory fallback (never deleted), and the narrator-input firewall test pins that only whitelisted inputs shape agent-facing text.

**Milestone B:**
- `raw.jsonl` append is not crash-atomic (torn last line possible); mitigated by machine-only use + lenient reads + documented reader contract.
- Digest counters make old-binary forward-compat one-way once anything disposes — identical to the existing events trade-off (`job.rs:150-154`); no new class of risk.
- `digest.stalled` best-effort undercount on non-dispose park paths — documented, `disposed` remains exact.

**Milestone C:**
- Situation excluded from the consult gate → context can never disable a consult (the critical fix). Residual: a very long clamped situation is still 2 KiB of prompt the model pays attention to — bounded and fenced.
- Precedent bias mitigated by the explicit "verify, don't defer to history" system-prompt line; authority cannot widen (upstream `decide_kind` gate).

**Milestone D:**
- FO-1 false positives on legitimate long-grinds — mitigated by the both-fields (`last_status`+`next_step`) rule, a conservative threshold, human-dismissable escalation, and reset-on-advancement.
- FO-2's fast backstop restored by the bounded marker-less-recheck counter (fixes the multi-day escalation regression).
- `retime` resetting the streak wipes a genuine accumulating signal on a human cadence tweak — fail-safe direction (delays, never early-fires); flagged as a judgment call.

---

## Decisions still needed from the human

**Milestone B**
1. Confirm the ~2 MB `raw.jsonl` ceiling and single rotated generation (~4 MB/session total).
2. Confirm the `decisions.md` notable set = **Escalated + Stalled only** (AutoFlow/Working/Monitoring → `raw.jsonl` only), and confirm `decisions.md` should get single-generation rotation (this doc proposes yes).
3. Confirm digest counters are **lifetime-monotonic** (never reset by `on_blocked`/`retime`) vs "since last human touch."
4. Confirm the `decisions.md` line format `- {epoch_seconds} pmd {kind}: {summary}` (no human-date dep exists) and the exact `raw.jsonl` schema (full `WakeReport` per line vs `DecisionRecord` only — this doc assumes full-fidelity machine JSONL).

**Milestone C**
5. **Thin-ledger semantics (spec-vs-design conflict):** the spec says "degrade to a static note when the ledger is thin/absent"; this design instead lets a thin ledger project an empty situation and the consult **still runs** on goal+question (forcing a note would regress today's behavior). Confirm the design's semantics or revert to thin→note.
6. Confirm `MAX_SITUATION_BYTES = 2 KiB` and that it is excluded from the consult budget gate.
7. Confirm the **current in-flight decision is excluded** from its own projected situation (recommended), given B now appends it before `spawn_advice`.

**Milestone D**
8. `DEFAULT_STALE_PLAN_STALL` value and whether it is a const or a per-session `Config` field (this doc leans Config field for tunability); the marker-less-recheck `K`.
9. Should FO-1 **escalate WorkerStuck** in D, or only maintain the counter and let E's narrator surface "you've repeated this plan N times" first (softer)?
10. Should `retime` (a human cadence edit) reset `stale_plan_streak`? (Implemented as yes.)

**Milestone E**
11. **Narrator invocation cadence & cache key:** confirm the coarse *situation-class* cache (blocked/working/monitoring + human-answer-pending + stall/elapsed buckets) with fast-path reaping and due-path spawning. This is the fix for "fresh each wake" vs "don't call the LLM every wake."
12. **Exactly what to feed the narrator:** goal, `last_status`, `last_plan`, `human_answer_pending`, D's stall counter, elapsed bucket. Is **wake-budget-remaining** in scope, or does surfacing it risk time-pressure nagging that changes agent behavior? (Even if surfaced, it stays **out of the cache signature**.)
13. **Guardrail bullets:** fully replace the four operating bullets with narrator prose (task's literal wording) or **retain them as static guardrails beneath the prose** (recommended — they encode "the harness sends no messages for you," which an LLM must not drop)?
14. **Acceptable degrade behavior & containment strictness:** confirm the echo fallback on every failure, and the `validate_narration` done-claim rejection (how aggressively to reject "wrap up / looks complete" prose).
15. **Model & budget:** reuse `SUPERVISOR_MODEL` or pin a cheaper/faster model for cosmetic prose; the per-session narration count cap and default `--max-budget-usd`.
16. Should the narrator run at all on an empty first wake (no `last_status`/`last_plan`)? (Default: skip → echo.)

---

## LOCKED DECISIONS (2026-08-19, human sign-off)

Three forks were the human's call; the rest adopt the design's recommended defaults.

**Forks answered by the human:**
- **E-13 (guardrail bullets): REPLACE.** When the narrator succeeds, its composed prose is the SOLE `## What to do now` guidance — the four static operating bullets are OMITTED. Mitigation for the accepted risk: `NARRATE_SYSTEM_PROMPT` MUST require the narrator to always convey the non-negotiable operating constraints — (1) keep working the goal, (2) use YOUR OWN tools incl. Slack MCP because the harness sends no messages for you, (3) poll what you can yourself, (4) reach humans via your own tools. The separate "You do not decide when the project is finished" section is still emitted verbatim regardless. On EVERY fallback (whatnow=None), `loop_nudge_prompt` returns the M86 echo WITH the bullets.
- **D-9 (FO-1 stall action): NUDGE SMARTER, THEN ESCALATE.** FO-1 uses two thresholds on `stale_plan_streak`: **T1 (lower)** flags the stall into E's `NarrateInputs` so the narrator surfaces "you've restated this plan N times without progress — are you stuck?"; **T2 (higher)** escalates `WorkerStuck` via `park_stuck_kind`. FO-2's marker-less-recheck escalation remains its own bounded backstop.
- **E-12 (time pressure): NO COUNTDOWN.** `NarrateInputs` includes coarse elapsed/stall buckets but EXCLUDES wake-budget-remaining — no "N wakes left" nagging.

**Defaults adopted (human deferred to the design):**
- B-1 `raw.jsonl` ~2 MB + one rotated generation (~4 MB/session). B-2 `decisions.md` notable = Escalated + Stalled only, also single-rotation. B-3 counters lifetime-monotonic (not reset by on_blocked/retime). B-4 `raw.jsonl` = full-fidelity machine JSONL, `decisions.md` line `- {epoch} pmd {kind}: {summary}`.
- C-5 thin ledger → empty situation, consult STILL runs (do not regress to a forced static note). C-6 `MAX_SITUATION_BYTES = 2 KiB`, EXCLUDED from the consult budget gate. C-7 the current in-flight decision is excluded from its own projected situation.
- D-8 `DEFAULT_STALE_PLAN_STALL` + marker-less `K` as consts initially (Config field later if wanted); T1/T2 concrete values chosen in the D plan. D-10 `retime` resets `stale_plan_streak` (fail-safe).
- E-11 coarse situation-class cache key + fast-path reaping + due-path spawning. E-14 echo fallback on every failure; `validate_narration` rejects done-claims + control bytes. E-15 reuse `SUPERVISOR_MODEL`, per-session narration budget cap + default `--max-budget-usd`. E-16 skip narration on an empty first wake (→ echo).

**Build order: B → D → C → E**, each its own plan + branch, merged green before the next.

---

## MILESTONE E — REDESIGNED (2026-08-19, human): skills + deterministic signal-flags (LLM narrator DROPPED)

The human proposed shipping agent-manager's OWN skills for reliability, and chose **both a worker and a
decider skill** + a **deterministic signal-flag** nudge. This **supersedes the LLM-narrator E section above** —
that whole apparatus (`claude -p` narrator, own slot/latch/seq, `validate_narration`, cache-key/situation-class,
the authoritative-prose hazard) is **REMOVED**. E is now simpler, deterministic, testable, zero-LLM-cost.

**E now = three parts (its own design pass + sign-off REQUIRED before building; build LAST, after B/D/C):**
1. **`agent-manager-worker` skill** — the STABLE loop protocol currently crammed into `loop_nudge_prompt`'s
   static sections: the marker/WakeReport JSON schema + rules, "you own comms / the harness sends no messages
   for you", "you do NOT decide when the project is finished (write blocked+confirm_done)", "pick up your own
   plan". Versioned/reviewed text, installed where a claude worker discovers it; the nudge references it so the
   agent (re-)invokes it each wake (surviving in-REPL compaction, which is invisible to pmd).
2. **`agent-manager-decider` skill** — the supervisor-consult reasoning protocol (auto-approve / hold /
   escalate; how to read C's `situation`; verify-don't-defer-to-precedent). Delivered as the consult's
   instructions. Complements C.
3. **Signal-flag nudge** — rewrite `loop_nudge_prompt` to: the goal + a trigger ("continue per your
   agent-manager worker skill") + a deterministic **"Since last wake"** block of FIXED lines toggled by
   objective signals: D's `stale_plan_streak >= T1` → "you've restated the same plan N× without the marker
   advancing — if stuck, write a blocked/stuck marker"; human-answer-arrived → "a human answer landed, handle
   Pending context first"; elapsed bucket. The big static protocol sections MOVE OUT of the nudge into the
   worker skill. pmd only flips fixed lines on/off from counters — no per-wake generation.

**Cross-engine:** skills are Claude Code; **codex** workers get the same worker-skill text via `AGENTS.md` /
the launch prompt. **Fallback / degrade:** if skill delivery can't be guaranteed for a session, the nudge
degrades to carrying a compact protocol pointer (and the M86 echo path remains available) so a skill-less
worker is never left without the rules.

**Unchanged by this pivot:** B (ledger), D (counters — T1 now sets a deterministic nudge flag instead of a
narrator input; T2 still escalates WorkerStuck), C (situation projection — now feeds the decider skill). The
narrator-specific parts of the E section above (paths.rs `narrate_*`, `pmnar-` sessions, `advise/narrate.rs`,
the `narrate_*` scheduler fields) are OBSOLETE and will NOT be built.
