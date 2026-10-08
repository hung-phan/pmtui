# Chattable-on-Create Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** A freshly-created agent-loop session is chattable *immediately* — the human's first interactive chat itself creates the conversation, so they never wait for a poll wake (works even if `pmd` is not running). The poll then resumes what the human started.

**Why:** Today `AgentLoopState.conversation_id` is minted only on the first poll wake, so a never-woken session routes to `WaitingFirstWake` and cannot be chatted (user: *"i cannot attach to it first time"*).

**Design authority:** the `chattable-on-create-design` workflow synthesis (2026-08-14). Spike-proven live: interactive `claude --session-id <new-uuid>` CREATES a chattable REPL; headless `claude -p --resume <same-uuid>` then RESUMES it.

## Global Constraints

- **SINGLE-WRITER LEDGER (hard):** only the daemon writes `state.json` (`AgentLoopState`). pmtui writes only the **registry** and the **chat marker** (`chat.json`) — NEVER `state.json`.
- **FENCE = the per-session `driver.lock` flock** (NOT the chat marker). The marker fences the daemon's spawn *decision* but leaves a check-then-act gap (between `chat_lock::is_active` and the ledger write at the end of `spawn`) during which pmtui and the daemon could each mint a *different* id → a silent two-conversation FORK. The `driver.lock` (`lease::try_acquire`, `flock(LOCK_EX)`, released on `Drop`) makes creation *mutually exclusive by construction*: pmtui may CREATE only while it exclusively holds the lock; the daemon reaches `spawn()` only while IT holds the lock. flock ⇒ at most one holder ⇒ no fork.
- **TWO IDS:** `ledger.conversation_id` (state.json — the daemon's sole authority) and `registry.conversation_id` (pmtui-owned, a first-spawn SEED). pmtui keys Enter on `effective_id = ledger.cid.or(registry.seed)`. The daemon keys create-vs-resume on `ledger.cid`, consulting the seed only when `ledger.cid` is `None` on the first spawn.
- **DOUBLE-CREATE prevented** structurally by the exclusive lease; **double-RESUME** prevented by the existing chat marker (unchanged).
- **codex is EXCLUDED from immediate-create in v1** (no caller-chosen id; interactive codex exposes no `--json` to capture the minted id → seeding would fork). codex uses the armed-auto-open fallback (chat as soon as the daemon's first wake makes it chattable; requires `pmd`), honestly messaged. Offline codex create is deferred (future S4).
- Reuse `mint_uuid_v4` for the claude uuid on both sides (make it `pub`) so the id format stays byte-compatible with `claude --session-id` across the two binaries.
- Tests must not need a TTY or a real CLI (except the noted codex live-proof, which is deferred). New logic is unit-testable over `FakeDriver`/`ProjectPaths`.

---

### Task 1 (S1): daemon adopt arm + de-poison

**Files:**
- `src/job_engine.rs` — `JobScheduler`: add `registry_seed: Option<String>` + `adopted_unverified: bool` + `seed_discarded: bool` fields (init `None`/`false`); `pub fn set_registry_seed(&mut self, seed: Option<&str>)`; make `mint_uuid_v4` `pub`; the adopt arm in `spawn()`'s resume match (~lines 291-303); de-poison in the wake-failure path.
- `src/daemon.rs` — `Runner::build` passes `p.conversation_id` into `JobScheduler::new` (~line 86); `reconcile_one` calls `s.set_registry_seed(p.conversation_id.as_deref())` each sweep BEFORE the `Driven::Job` tick (~line 287-290), so a seed written to the registry AFTER the runner was built reaches the live scheduler.

**Interfaces / mechanics:**
- `set_registry_seed(&mut self, seed: Option<&str>)` stores the latest registry seed on the scheduler.
- **Adopt arm** — new resume-match order in `spawn()`:
  ```
  (Some(id), _)                       => Resume::Continue(id)                 // ledger cid wins (unchanged)
  (None, _) if seed.is_some()
             && !self.seed_discarded  => { next.conversation_id = Some(seed); // ADOPT (engine-agnostic RESUME)
                                           self.adopted_unverified = true;
                                           Resume::Continue(seed) }
  (None, Engine::Claude)              => { mint uuid; next.conversation_id = Some(id);
                                           self.adopted_unverified = false;
                                           Resume::Fresh { session_id: Some(id) } }  // CREATE (unchanged)
  (None, Engine::Codex)               => Resume::Fresh { session_id: None }          // (unchanged)
  ```
  The adopted id reaches `state.json` only via the existing atomic `job::save` at the end of `spawn` — the harness stays the sole ledger writer.
- **De-poison** (self-heal an empty/never-persisted seed, WITHOUT losing a real conversation): the failure path (`on_worker_result` non-clean exit / `after_failure`) checks: if `self.adopted_unverified` AND the worker's `step_log(seq)` contains the marker string **`"No conversation found with session ID"`**, then DISCARD the seed — set `self.seed_discarded = true`, clear the ledger (`conversation_id = None`, persisted), and park a short backoff `Monitoring`. The next `spawn` sees `ledger.cid == None` and (because `seed_discarded`) SKIPS the adopt arm → mints a fresh id and CREATEs (`Fresh{Some}`), persisting the new cid; future wakes `Continue` it. This bounds a poisoned seed to one failed wake + one backoff and cannot loop (post-discard the ledger holds a real minted id).
  - **Gate on the log signal, not a blind first-failure** — a transient failure (network/tool hiccup) of a *legitimately-persisted* adopted seed must NOT discard the human's real conversation. Only the "No conversation found…" signal triggers the discard; any other failure takes the normal backoff/Stuck path unchanged.
  - Clear `adopted_unverified` on the FIRST SUCCESSFUL wake off an adopted seed (so a later unrelated failure never triggers de-poison).

- [ ] **Step 1:** add the fields + `set_registry_seed` + make `mint_uuid_v4` pub.
- [ ] **Step 2:** the adopt arm in `spawn`.
- [ ] **Step 3:** wire `Runner::build` (pass `p.conversation_id`) + `reconcile_one` (`set_registry_seed` each sweep before tick) in `daemon.rs`.
- [ ] **Step 4:** the de-poison in the failure path, gated on the `"No conversation found with session ID"` step_log signal.
- [ ] **Step 5 — tests (FakeDriver):**
  - seed `Some(U)` + Idle ledger ⇒ first `Spawned` uses `Resume::Continue(U)` and persists `U` to the ledger (adopt, not mint).
  - seed `None` ⇒ unchanged: claude mints+`Fresh{Some}`, codex `Fresh{None}`.
  - ledger `cid=Some(X)` ⇒ seed ignored (`Continue(X)`).
  - **de-poison:** adopted-seed wake fails with a step_log containing "No conversation found with session ID" ⇒ next spawn MINTS a fresh id + `Fresh`-creates + persists it, does NOT re-adopt the seed, and does NOT loop.
  - **no false de-poison:** adopted-seed wake fails WITHOUT the not-found signal (generic failure) ⇒ seed is NOT discarded (normal backoff; ledger keeps the adopted id).
  - adopted-seed wake SUCCEEDS ⇒ later wakes `Continue` the same id; a subsequent failure does NOT de-poison.
- [ ] **Step 6:** full suite + clippy + fmt green; commit.

**Ships standalone:** a manually-seeded `registry.conversation_id` is resumed by the poll. No pmtui change, no single-writer relaxation.

---

### Task 2 (S2): pmtui lease-fenced create-and-chat (claude) + armed auto-open + honest copy

**Files:** `src/bin/pmtui.rs` (`request_attach` AgentLoop branch + `agent_loop_enter`; new `build_chat_create`; `App.pending_first_chat`; the refresh-loop drain; `WaitingFirstWake` copy). Consumes `crate::lease`, `crate::chat_lock`, `job_engine::mint_uuid_v4` (pub from S1).

**Mechanics:**
- `effective_id(ledger) = ledger.conversation_id.or(registry.conversation_id)` — compute in `request_attach` and feed into the pure router.
- `build_chat_create(engine, id) -> Vec<String>` — the CREATE sibling of `build_chat`: claude → `["env","-u","CLAUDECODE","claude","--session-id",<id>]` (NO `-p`, NO `--resume`). codex → not used in v1 (never create codex here).
- **`request_attach` AgentLoop routing** (`agent_loop_enter` stays pure for the live/parked cases; the lease dance is impure in `request_attach`):
  1. live wake pane alive ⇒ Watch (unchanged).
  2. `effective_id` Some AND not Running ⇒ Chat(effective_id) via the existing marker+TOCTOU `--resume` path (unchanged).
  3. `effective_id` None:
     - **claude:** `lease::try_acquire(entry_state_paths(e).daemon_dir().join("driver.lock"))`:
       - `Ok(Some(lease))` (pmd is NOT driving this session) ⇒ **create-and-chat**: mint `U = mint_uuid_v4()`; write `registry.conversation_id = U` (pmtui saves the registry — it owns it); `chat_lock::mark(pid, now)`; launch interactive `build_chat_create(Claude, U)` **holding the lease for the whole REPL**; a drop-guard clears the marker AND drops the `ProjectLease` on every exit path (kernel-released on crash). pmtui NEVER writes `state.json`.
       - `Ok(None)` (pmd holds the lease — it is driving) ⇒ **ARM auto-open** (do NOT mint): `pending_first_chat = Some(session id)`; status "creating on the first wake — you'll drop in automatically".
     - **codex:** never mint. ARM auto-open with status "codex: the conversation is created after the first wake completes" (requires pmd). If pmd is NOT running (the lease is FREE — `try_acquire` returns `Some`; release it immediately since we won't create), status "no daemon is driving this session — start pmd for the first codex wake".
- **Armed auto-open** (`App.pending_first_chat: Option<String>`): each `refresh`, re-read the ledger; when it is cleanly chattable (`conversation_id` Some AND `run` is `Monitoring`/`Idle` — NOT `Running`, NOT `Blocked`) ⇒ populate `pending_chat` (the existing `chat()` path) and disarm. If `run` becomes `Blocked` ⇒ DISARM and surface the answer/decision prompt (do NOT yank the human into a REPL on a parked-blocked session; a codex capture-failure `Capability` stop must route here, never leave them "waiting"). Disarm on navigation / Esc.
- **Honest `WaitingFirstWake` copy** — replace the single message with state-specific copy: (a) claude, lease free, pmd down → "no daemon is driving — start pmd, or press Enter to chat now" (Enter triggers create-and-chat); (b) claude, lease held → "creating on the first wake — you'll drop in automatically"; (c) codex + pmd → "codex: the conversation is created after the first wake completes"; (d) codex, no pmd → "start pmd for the first codex wake".

**Ordering note (determinism):** for a session pmtui *itself just created via the create form*, immediate create-and-chat is only deterministic if pmtui acquires the lease BEFORE publishing the registry row (so the daemon can't grab it first). v1 does NOT change `submit_create`'s ordering — Enter-on-a-never-woken-row uses `try_acquire` and correctly degrades to armed-auto-open when pmd already holds the lease (no fork either way, thanks to the flock). A deterministic create-form "create & chat" affordance (acquire-before-publish) is a **future** refinement.

- [ ] **Step 1:** `build_chat_create` (+ unit test: claude argv shape, no `-p`/`--resume`; codex path unused).
- [ ] **Step 2:** `effective_id` + `agent_loop_enter` precedence (unit-test: ledger cid beats registry seed; a second Enter after a seed-only state yields Chat(seed), never a re-mint).
- [ ] **Step 3:** the `request_attach` lease dance (create-and-chat vs arm) + `pending_first_chat` + the create REPL launcher with the drop-guard that releases BOTH the marker and the `ProjectLease`.
- [ ] **Step 4:** armed auto-open drain in `refresh` + Blocked→answer routing + disarm-on-nav; honest copy (unit-test the copy/route selection per `(engine, lease-free?, run-state)`).
- [ ] **Step 5:** full suite + clippy + fmt green; commit.

**Manual acceptance after merge (supervised):** with pmd DOWN, create a session, Enter ⇒ interactive `claude --session-id` REPL opens immediately, chat, exit; then start pmd ⇒ its first wake ADOPTS the registry seed and resumes the human's conversation. With pmd UP, Enter on a fresh row ⇒ arms, then auto-opens the instant the daemon's wake parks it.

---

### Task 3 (S4, DEFERRED — do NOT build now): codex offline/immediate create

Best-effort `~/.codex/sessions` rollout snapshot-diff under the same lease+marker fence to capture the interactive codex id into `registry.conversation_id` (the S1 adopt arm then resumes it). Fragile (path/format assumptions, unrelated host codex processes); requires a live proof before merge; armed-auto-open remains the guaranteed fallback. Not required for the headline. Left unbuilt.

## Self-Review

- **Spec coverage:** S1 = daemon adopt+de-poison (the resume side); S2 = pmtui create-and-chat + arm + copy (the human side). Fork prevented by the flock; single-writer held (pmtui writes only registry+chat.json). Codex honestly scoped. ✓
- **De-poison safety:** gated on the real "No conversation found…" log signal, so a transient failure never discards a real conversation. ✓
- **Type consistency:** `effective_id = ledger.or(registry)` used everywhere pmtui decides create-vs-resume; `mint_uuid_v4` shared (pub); adopt arm sets the same fields the mint arm does. ✓
