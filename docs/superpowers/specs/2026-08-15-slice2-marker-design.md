# Slice 2 — Marker-file blocked/completion detection (`needs-you.json`) — Design

> Executes Slice 2 of `docs/superpowers/specs/2026-08-15-persistent-agent-loop-session-design.md` (§4b, §6).
> Authored from a design workflow over the POST-Slice-1 code. **Coordinator adjudication of the §7 open questions:**
> - OQ1 (seam): ACCEPT the mtime-fast-path in the `Monitoring`-not-due arm (a blocked marker parks within one sweep; bounded to one `stat`).
> - OQ2 (agent `seq`): ACCEPT the monotonic guard as-is.
> - OQ3 (write/read race): INCLUDE the hardening — prompt the agent to write atomically (tmp+rename), AND on a parse error whose mtime is very fresh, treat as "not ready" and recheck at `BUSY_RECHECK_S` WITHOUT bumping `continuations`; only count malformed after it stays unparseable across a recheck.
> - OQ4 (config threading): ACCEPT (un-discard `config`, thread `&Config` into `drive`/`observe_marker`).
> - OQ5 (de-poison): a bad `--resume` launch never writes a marker, so de-poison is NOT solved here — DEFER to Slice 3 (launch/stall backstop). (Corrects the earlier 'Slice 2' note.)
> - OQ6 (codex id capture): INCLUDE persisting `report.conversation_id` into the ledger when it is currently `None` (harmless for claude; closes deferred I2 plumbing-side for codex) + tell codex in the nudge prompt to report its conversation id in the first marker. codex actually populating it stays best-effort.

---

# Slice 2 Task Plan — Marker-file blocked/monitoring/working detection (`needs-you.json`)

## Goal & non-goals

Restore "the agent can signal blocked / monitoring / working" without reviving the ephemeral `pmj-` process. The persistent `pmloop-<id>` agent **overwrites** a single stable marker (`needs-you.json`) with a `WakeReport` JSON carrying a **monotonic `seq`** at any decision point. On each drive tick the harness stats+reads the marker, and on a `seq` bump feeds the parsed report into the **surviving** escalation machinery (`policy::decide_kind`, `open_stop`, `JobRun::Blocked`, `on_blocked`'s stale-answer guard, daemon notify-once). `Mode::Auto` is untouched. Slice 3's hook-driven idle detection is **out of scope** (backstop-only remains the budget/stall path in `nudge`).

---

## 1. Marker contract

### 1.1 Path — one accessor on `ProjectPaths`
Add beside `step_result` (`src/state.rs:270`) and `chat_lock` (`src/state.rs:283`):

```rust
/// The persistent agent's decision marker (design §4b option 1). At ANY decision
/// point the agent OVERWRITES this with a `WakeReport`/`StopDraft` JSON carrying a
/// monotonic `seq`. The harness only STATS + READS it — the agent is the SOLE
/// writer (single-writer-per-file discipline, inverse of the ledger). This is the
/// design's `sessions/<id>/needs-you.json`, a sibling of `state.json`/`brief.md`.
pub fn needs_you(&self) -> PathBuf {
    self.state_dir().join("needs-you.json")
}
```
No change to `state_dir()`/`for_session` — per-session namespacing (`src/state.rs:212-228`) already isolates two loop sessions sharing a project folder. The harness MUST never `write` this path (only `read`/`metadata`), or the single-writer invariant breaks.

### 1.2 JSON schema — reuse `WakeReport`, add `seq`
Reuse `WakeReport` verbatim (`src/job.rs:118-136`: `state`, `stops`, `next_check_s`, `status`, `conversation_id`) — the schema is the invariant per design §6. Add one field:

```rust
// in WakeReport (src/job.rs:120-136)
/// Monotonic decision stamp. The agent sets this STRICTLY GREATER than any it has
/// written before (simplest reliable source: current Unix seconds). The harness
/// ignores a marker whose `seq <= last_marker_seq` — the marker analogue of the
/// `answered_at >= since` stale-answer guard (job_engine.rs:511-515).
#[serde(default)]
pub seq: u64,
```
`#[serde(default)]` keeps old JSON loadable despite `#[serde(deny_unknown_fields)]` (deny rejects *extra*, not *missing* keys — proven by the round-trip test at `src/job.rs:156-168`).

### 1.3 The last-seen watermark — persisted on the ledger
Add to `AgentLoopState` (`src/job.rs:52-80`):

```rust
/// Highest marker `seq` the harness has already disposed. Monotonic anti-replay
/// guard for `needs-you.json` (the marker analogue of `Blocked.since`). Persisted
/// so the guard survives a daemon restart (an in-memory-only mtime would re-dispose
/// the last marker after a restart).
#[serde(default)]
pub last_marker_seq: u64,
```
**Also update `AgentLoopState::fresh` (`src/job.rs:82-97`)** to init `last_marker_seq: 0`, and the two full round-trip test fixtures (`src/job.rs:180`-ish and the full-case JSON) — otherwise the struct literal won't compile.

### 1.4 How the harness knows a marker is NEW (the freshness rule)
Two-stage, cheap-first:
1. **mtime pre-check (cheap):** `fs::metadata(needs_you()).modified()`. If absent → no marker → fall through (failure mode A). If mtime unchanged since the last stat → skip entirely (no read, no parse) — matters on the pmd sweep.
2. **`seq` authoritative guard:** if mtime advanced, `parse_report(needs_you())`, then act **only if `report.seq > ledger.last_marker_seq`**. `<=` (stale or re-saved-without-bump) → ignore, consume nothing (failure modes C/D). mtime alone is insufficient across restart/clock-skew; the persisted `seq` is the durable guard.

The mtime "last stat" can be an **in-memory field on `JobScheduler`** (`Option<SystemTime>`, mirroring `wakes`/`window_start` at `src/job_engine.rs:109-121`) — a lost mtime after restart just forces one extra read+parse that the `seq` guard then no-ops, so it need not persist. The **`seq`** watermark MUST persist (on the ledger, §1.3).

---

## 2. Dispose logic to (re)build

Slice 1 deleted `on_worker_result`/`on_report_blocked`/`lenient_working`/`park_monitoring`. Rebuild them as **one method** on `JobScheduler`, ported from `phase_engine::dispose`'s stop block (`src/phase_engine.rs:158-187`), reusing the still-present helpers rather than reinventing.

### `fn observe_marker(&mut self, now, ledger, config) -> Result<Option<JobTick>>`
Returns `Some(tick)` when a bump was disposed into a terminal-for-this-tick decision; `None` when the caller should fall through to the normal `match self.run` path.

```
stat needs_you(); if absent OR mtime unchanged  -> return Ok(None)        // mode A
record mtime seen (in-memory)
report = match parse_report(needs_you()) {
    Ok(r) if r.seq > ledger.last_marker_seq => r,
    Ok(_)  => return Ok(None),                                            // modes C/D: stale/equal seq
    Err(_) => return Ok(Some(self.lenient_working(now, ledger)?)),        // mode B: malformed
};
// valid bump: persist watermark + status, then branch on state
```

Then branch on `report.state`:

- **`WakeState::Working`** → progress: build `next = ledger.clone()`, set `last_marker_seq = report.seq`, `last_status = report.status`, `continuations = 0`, `updated_at = now`; `job::save`; return `Ok(None)` so `drive` falls through to the normal classify+`nudge` heartbeat (`src/job_engine.rs:296-315`). (Do **not** reset `wakes`/`window_start` — a marker bump is not a human touch.)

- **`WakeState::Monitoring`** → self-scheduled nap: `until = now + report.next_check_s.unwrap_or(cadence_s|DEFAULT_CADENCE_S)`; persist `last_marker_seq`, `last_status`, `continuations = 0`, `run = Monitoring { until }`; mirror `self.run`; return `Ok(Some(JobTick::Monitoring { until }))` — **skips the nudge** this tick.

- **`WakeState::Blocked`** → port `phase_engine.rs:161-187` almost verbatim, substituting the loop's helpers:
  ```
  let mut escalating: Vec<OpenStop> = vec![];
  let mut auto_ids: Vec<String> = vec![];
  for (i, draft) in report.stops.iter().enumerate() {
      let id = format!("stop-{}-{}-{}", self.project_id, i, now);
      let open = open_stop(id, draft.kind, draft.context_ref.clone(), now); // job_engine.rs:777
      match policy::decide_kind(config.autonomy, draft.kind, draft.risk_class) { // policy.rs:66
          Decision::Escalate => escalating.push(open),
          Decision::AutoFlow => auto_ids.push(open.id),
      }
  }
  ```
  - **Escalate branch** (any `escalating`): mirror `park_stuck_kind`'s persistence (`src/job_engine.rs:623-637`) but with the real drafted stops: `next.open_stops = escalating`; `next.run = Blocked { stop_ids, since: now }`; `last_marker_seq`, `last_status`, `continuations = 0`; `job::save`; mirror `self.run`; return `Ok(Some(JobTick::Escalated(ids)))` → daemon notifies once (`src/daemon.rs:390-401`). Co-drafted auto-flow stops are dropped (matches `phase_engine.rs:170-174`).
  - **All auto-flow, no escalation** (`escalating.empty()`): set `next.pending_context = Some(<auto-approved note listing auto_ids>)`, `continuations = 0`, `last_marker_seq`, `last_status`, `run = Monitoring { now + cadence }`; `job::save`; return `Ok(None)` so `drive` falls through and `nudge` delivers the `pending_context` and clears it (`src/job_engine.rs:453-457, 472`). (Consistent with §3 auto-flow carry.)
  - **Blocked with an empty `stops` vec**: no routable stop → `Ok(Some(self.park_stuck(now, ledger, "agent reported blocked without a stop".into())?))` (`src/job_engine.rs:602`).

### `fn lenient_working(&mut self, now, ledger) -> Result<JobTick>`
Missing-parse (mode B). **Record the mtime as seen** (already done by caller) so the same bad file is not re-counted every 500ms sweep; bump `continuations += 1` **once for this bump**; if `continuations >= config.stuck_threshold` (`src/state.rs:80-81`, default 3) → `park_stuck(now, ledger, "agent's decision marker is unparseable".into())`; else `job::save` the bumped `continuations` and return a short recheck `Monitoring { now + BUSY_RECHECK_S }` so the malformed file is re-observed promptly. Never a silent stop (honors `parse_report`'s doc, `src/job.rs:113-117`).

**Surviving helpers reused (no reimplementation):** `open_stop` (`job_engine.rs:777`), `park_stuck`/`park_stuck_kind` (`job_engine.rs:602-638`), `policy::decide_kind` (`policy.rs:66`), `on_blocked`/`answers_extra`/`resume_with_answer` (unchanged — they unblock the marker-parked `Blocked` identically because we set `stop_ids`=OpenStop ids and `since = now`, exactly as `park_stuck_kind`). **Re-imported into `job_engine.rs`:** `crate::policy::{self, Decision}` (dropped in Slice 1).

---

## 3. Where it plugs into `tick`

Insert `observe_marker` **at the top of `drive`, in the `AlreadyUp` fall-through — between `EnsureOutcome::AlreadyUp => {}` (`src/job_engine.rs:293`) and the `capture_tail`+classify (`src/job_engine.rs:296`)**, i.e. as "step 2.5". Rationale, tied to existing early-returns:

- The **human-present defer** (`src/job_engine.rs:269-281`) already returned before this point → a marker is never disposed while a human is chatting/attached... **except** we want a `Blocked` marker to *park* even during human presence (parking is a state write, no `send_keys`, safe). Design §4 says "process the marker always; defer only the keystroke." **Decision:** run `observe_marker` at step 2.5 (after `ensure_session`, before classify). The human-present gate at `drive`'s top still prevents any *nudge*; and `on_blocked` (`src/job_engine.rs:523-532`) already re-defers the resume-nudge when a human is present. This keeps the seam simple and never types into an occupied pane. (Parking `Blocked` a tick later, after the human detaches, is acceptable and matches the "defer consumes no state" rule.)
- The **`JustLaunched`** cold-start path (`src/job_engine.rs:285-292`) returns before step 2.5 → we never read a marker a still-booting agent hasn't written yet (cold-start grace preserved).
- Reaching step 2.5 means the session is confirmed alive, so the marker (the agent's latest self-assessment) legitimately decides whether to park `Blocked`/`Monitoring` or fall through to the heartbeat `nudge` (`src/job_engine.rs:300`).

**Un-discard config:** `tick` currently reads into `_config` (`src/job_engine.rs:214`). Change to `let config: Config = ...` and thread `config.autonomy` / `config.stuck_threshold` through `drive` → `observe_marker`/`lenient_working` (add a `config: &Config` param to `drive`, or read config inside `observe_marker`). Prefer threading `&Config` from `tick` into `drive` to avoid a second disk read.

**Cadence-gate tension (the key structural decision):** `tick` only calls `drive` when `Monitoring` is due (`src/job_engine.rs:227-233`), so a mid-cadence `blocked` marker would wait up to the full 300s cadence. **Resolution (chosen): after a successful `nudge`, keep the existing full work cadence, BUT the daemon's 500ms sweep is not the gate — the gate is `Monitoring.until`.** To poll the marker promptly without a second read site, **shorten the post-nudge park to an "observe cadence"** is rejected (it would spam nudges). Instead adopt the design's primary recommendation: **read the marker in `tick` before the `match self.run`, i.e. also in the `Monitoring`-not-due branch** — but scoped to a *stat-only* fast path (mtime unchanged → immediate `Ok(Monitoring{until})`, no behavior change). Concretely: at `tick` top, if `mtime` advanced since last stat AND session is `Monitoring{until}` not-yet-due, call `drive` early (which runs step 2.5). This makes a fresh `blocked` marker park within one sweep. Implementable as: in the `JobRun::Monitoring { until }` arm (`src/job_engine.rs:227-233`), replace `else { Ok(Monitoring) }` with `else if self.marker_mtime_advanced() { self.drive(...) } else { Ok(Monitoring{until}) }`. The mtime pre-check keeps this O(1) when nothing changed. **This is the single most important seam decision — call it out for the coordinator (see §7).**

---

## 4. Nudge-prompt change

`loop_nudge_prompt(brief, extra)` → `loop_nudge_prompt(brief, extra, marker_path: &Path)` (`src/job_engine.rs:721`). Sole caller is `nudge` (`src/job_engine.rs:458`); pass `self.paths.needs_you()` (absolute — unambiguous regardless of agent cwd).

Keep all three framing blocks verbatim. Insert a new section **before** the `## Pending context` append (`src/job_engine.rs:449-452`):

```
## Signal a decision point (write your machine report)

Whenever you reach a decision point — you need a human decision, you're now waiting
on something, or you just made progress — OVERWRITE this file with ONE JSON object
(this is how the harness sees your state; it does not read your chat):

  {MARKER_PATH}

Schema (write at ANY decision point, not only when you're about to stop):
  {
    "seq": <integer, STRICTLY GREATER than every seq you wrote before — use the
            current Unix time in seconds>,
    "state": "working" | "monitoring" | "blocked",
    "status": "<one-line human-facing status>",
    "next_check_s": <optional; for "monitoring", seconds to nap>,
    "stops": [                        // only for "blocked"
      { "kind": "<publish|merge|confirm_done|ambiguity|stuck|expert_needed|worker_stuck|capability>",
        "risk_class": "<low|medium|hard>",
        "question": "<what you need decided>",
        "options": ["..."],
        "context_ref": "<optional pointer>" }
    ]
  }

Rules:
- Bump `seq` every write; the harness ignores any marker whose seq is not higher
  than the last it processed.
- "blocked" means you cannot proceed without a human decision. The harness routes
  each stop through its risk policy and pages the human — you STILL reach out via
  your OWN Slack MCP as well.
- Writing the marker never stops you. Keep working after you write it.
```
`{MARKER_PATH}` interpolated from the arg. `kind` values are the real `StopKind` snake_case variants (`src/pmstate.rs:92-101`); `risk_class` values match `RiskClass` serde (`low|medium|hard`, `src/state.rs:42-48`).

**Flip the test** at `src/job_engine.rs:1546` (`loop_nudge_prompt_keeps_framing_and_drops_the_report_instruction`): rename to `..._writes_the_marker_instruction`; keep the four framing asserts and the `Pending context` asserts; **invert** the two negative asserts to positive — assert the prompt contains the marker path, `"seq"`, `working`, `monitoring`, `blocked`. Update the call sites to pass a marker path arg.

---

## 5. Task breakdown (SDD, each FakeDriver-testable)

Two cohesive tasks. Task 1 is pure schema/path/prompt plumbing (no behavior change to the drive path yet — safe to land alone, all existing tests green after the one flipped test). Task 2 wires the disposer and seam.

### Task 1 — Marker schema, path, and nudge-prompt instruction
- **Files:** `src/state.rs` (add `needs_you()`), `src/job.rs` (add `WakeReport.seq` + `AgentLoopState.last_marker_seq`, both `#[serde(default)]`; update `fresh()` + round-trip fixtures), `src/job_engine.rs` (`loop_nudge_prompt` signature + marker section; update the one caller in `nudge`; flip the framing test).
- **Interfaces produced:** `ProjectPaths::needs_you() -> PathBuf`; `WakeReport.seq: u64`; `AgentLoopState.last_marker_seq: u64`; `loop_nudge_prompt(brief, extra, &Path)`.
- **Acceptance:** `cargo test` green; new `job.rs` round-trip asserts a `WakeReport{seq,…}` and an `AgentLoopState{last_marker_seq}` serialize/parse and that old JSON (missing both fields) still loads with defaults; flipped prompt test passes; the marker section contains the absolute `needs_you()` path, `"seq"`, and the three states.

### Task 2 — `observe_marker` disposer + `tick`/`drive` seam
- **Files:** `src/job_engine.rs` only (plus re-import `policy::{self, Decision}`).
- **Interfaces produced:** `JobScheduler::observe_marker(now, &AgentLoopState, &Config) -> Result<Option<JobTick>>`; `JobScheduler::lenient_working(now, &AgentLoopState, &Config) -> Result<JobTick>`; in-memory `last_marker_mtime: Option<SystemTime>` field on `JobScheduler`; `config` un-discarded in `tick` and threaded into `drive`; step-2.5 call in `drive` + the mtime-fast-path in the `Monitoring`-not-due arm.
- **Acceptance:** all §6 FakeDriver tests pass; existing drive/nudge/on_blocked tests stay green (no-marker path is byte-identical to today — `observe_marker` returns `None` when absent, and `drive` falls through unchanged).

*(If the coordinator prefers three tasks, split Task 2 into 2a "Blocked/escalate + auto-flow disposer" and 2b "Working/Monitoring/lenient + mtime fast-path seam". They share the same file so a worktree-serial order is required — never two implementers in one tree, per memory.)*

---

## 6. Tests (FakeDriver, in `src/job_engine.rs` tests + `src/job.rs` round-trip)

Use the existing FakeDriver + `fx` harness (as at `src/job_engine.rs:1031`, `1421`). Session `AlreadyUp`, pane `Idle`, no human present, unless noted.

1. **Fresh `Blocked` marker (hard) ⇒ escalate once.** Write `needs-you.json` with `{seq:100, state:"blocked", stops:[{kind:"publish", risk_class:"low", ...}]}`. `tick` → `JobTick::Escalated([id])`; ledger `run == Blocked{stop_ids:[id], since:now}`, `open_stops` has that stop, `last_marker_seq == 100`. A **second** `tick` with the same file → re-emits `Escalated` (daemon dedups) and does **not** re-append stops.
2. **Stale/equal stamp ⇒ ignored.** Ledger `last_marker_seq = 100`; write marker `{seq:100, state:"blocked", …}` (mtime advanced). `tick` → falls through to normal `Idle`→`nudge` (`Monitoring{now+cadence}`); ledger `run` not `Blocked`, `continuations` unchanged, no answer consumed. Also `{seq:50}` ⇒ same ignore.
3. **Malformed ⇒ lenient working + stall.** Write `needs-you.json` with `{ not json` / unknown key. `tick` → `continuations == 1`, `Monitoring{now+BUSY_RECHECK_S}`, no `Blocked`. Repeat to `continuations == stuck_threshold(3)` ⇒ `JobTick::Stuck(...)`, `run == Blocked`. Confirm the **same** bad file isn't double-counted within one sweep (mtime unchanged → skip).
4. **`Monitoring{next_check_s}` ⇒ park, no nudge.** `{seq:5, state:"monitoring", next_check_s: 900}`. `tick` → `Monitoring{now+900}`; `send_keys` NOT called (assert FakeDriver recorded zero nudges); `continuations == 0`.
5. **`Working` ⇒ keep nudging on cadence.** `{seq:7, state:"working", status:"indexing"}`. `tick` → `send_keys` called once (heartbeat); `Monitoring{now+cadence}`; `last_status == "indexing"`, `continuations == 0`, `last_marker_seq == 7`.
6. **Auto-flow low-stakes ⇒ pending_context + keep nudging.** Tier `Autopilot`; `{seq:9, state:"blocked", stops:[{kind:"ambiguity", risk_class:"low"}]}` → `decide_kind` = AutoFlow. `tick` → no `Blocked`; `pending_context` set; next `tick` `nudge` delivers it and clears it; `run` stays `Monitoring`.
7. **Answer past `since` resumes a marker-parked Blocked.** After test 1 parks `Blocked{since:S}`, write `answers.json` with an `Answer{stop_id, answered_at: S+1}`; `tick` (no human) → `on_blocked` resumes via `resume_with_answer` → `nudge` with `answers_extra`; `run` back to `Monitoring`, `open_stops` cleared, `wakes`/`window_start` reset (`src/job_engine.rs:533-543`). Confirms the marker-fed `Blocked` unblocks identically to `park_stuck`.
8. **Human present + fresh Blocked marker.** `chat_lock` active OR `has_clients` true; fresh `blocked` marker. `tick` → `drive`'s human-present gate returns `Monitoring` **before** step 2.5, so **no** `send_keys`; the `Blocked` is parked on the next tick after detach (assert no nudge fired; document the one-tick defer).
9. **`job.rs` round-trip (Task 1):** `WakeReport{seq}` and `AgentLoopState{last_marker_seq}` serialize/parse; old JSON without the fields loads with `seq=0`/`last_marker_seq=0`.

---

## 7. Risks / open questions for the coordinator

1. **The cadence-gate seam (highest risk).** Where to poll the marker so a mid-cadence `blocked` is seen promptly: option (a) mtime-fast-path in the `Monitoring`-not-due arm of `tick` (§3, recommended — bounded to one stat), vs option (b) short post-nudge "observe cadence" (risks nudge spam), vs (c) accept up-to-cadence latency (simplest, but a blocked agent waits ~300s for the human page). The plan chose (a); confirm this is acceptable given daemon sweeps every 500ms and `metadata()` is cheap.
2. **Agent-generated `seq` correctness.** The prompt tells the agent to use "current Unix seconds, strictly greater than before." A confused agent could reuse or lower `seq` (→ its next real decision is silently ignored — failure mode D by design). Mitigation options: (i) accept it (monotonic guard is the whole point), (ii) have the nudge tell the agent the *next* stamp to use (`> last_marker_seq`) — but that couples the prompt to ledger state and the agent may still not comply. Recommend (i) for Slice 2; note it.
3. **Write/read race (agent writing while harness reads).** The agent may be mid-write when the harness reads → partial JSON → `parse_report` `Err` → `lenient_working` bumps `continuations` spuriously. Mitigations: (i) instruct the agent to write atomically (tmp + rename) — but we can't enforce it in the agent's own tooling; (ii) on a parse error whose mtime is very recent (< N ms), treat as "not yet ready", re-check at `BUSY_RECHECK_S` **without** bumping `continuations`, and only count it as malformed after it stays unparseable across a recheck. Recommend (ii) as a small hardening; flag for the coordinator whether it's in Slice 2 scope or deferred.
4. **`config` threading.** `tick` must stop discarding config (`src/job_engine.rs:214`, `_config` → `config`) and thread `&Config` into `drive`/`observe_marker`. Low risk but touches the `drive` signature and all its call sites (`src/job_engine.rs:226, 232, 242`).
5. **De-poison self-heal still absent.** Slice 1 dropped the `seed_discarded` gate in `resolve_conversation_id`. Not in Slice 2 scope, but if a poisoned adopted seed exists, a `Blocked` marker can't fix it — confirm that remains a separate slice.
6. **`conversation_id` capture on the create wake.** `WakeReport.conversation_id` (codex id capture) is carried in the schema but the plan's disposer doesn't consume it. Confirm whether Slice 2 should persist `report.conversation_id` into the ledger when present (one extra line in the `Working`/`Monitoring` branch) or defer to a later slice.

### Files touched (Slice 2 surface)
- `src/state.rs` — `ProjectPaths::needs_you()`.
- `src/job.rs` — `WakeReport.seq`, `AgentLoopState.last_marker_seq` (both `#[serde(default)]`), `fresh()` init, round-trip fixtures/tests.
- `src/job_engine.rs` — `loop_nudge_prompt` marker section + `&Path` arg; re-import `policy::{self, Decision}`; new `observe_marker` + `lenient_working`; in-memory `last_marker_mtime`; un-discard `config` and thread into `drive`; step-2.5 seam + `Monitoring`-not-due mtime fast-path; flip the framing test; add §6 tests.
- **No change** to `policy.rs`, `daemon.rs` (notify-once/`notified` dedup reused as-is), or `on_blocked`'s stale-answer guard / `resume_with_answer`.
