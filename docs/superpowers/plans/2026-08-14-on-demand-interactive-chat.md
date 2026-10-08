# On-Demand Interactive Chat Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a human attach to a live interactive `claude`/`codex` REPL on an agent-loop session's conversation on demand, while the ephemeral poll keeps doing autonomous work — pausing the poll's wakes only while the human is attached.

**Architecture:** The poll (`JobScheduler` ephemeral per-wake headless workers) is UNCHANGED. We add (1) a per-session **chat marker** file the daemon consults before spawning a wake, and (2) a pmtui launcher that writes the marker, runs a real interactive `claude --resume <id>` / `codex resume <id>` in the foreground (suspend/restore, like `$EDITOR`), and clears the marker on exit. Because both the poll's headless wakes and the human's REPL resume the SAME `conversation_id`, context is one continuous thread. Mutual exclusion on that shared id is the only new invariant.

**Tech Stack:** Rust (existing crate `pmd`/`pmtui`), serde JSON, existing `tmux`/`ProjectPaths`/`JobScheduler` substrate. Linux-only (matches the rest of the harness).

**Design authority:** `docs/superpowers/specs/2026-08-13-agent-loop-session-heartbeat-design.md` — the 2026-08-14 addendum ("on-demand interactive chat; the poll STAYS"). Spiked green live (both engines' interactive resume-by-id confirmed).

## Global Constraints

- **The poll model does not change.** No new scheduler machinery, no killing/replacing the ephemeral per-wake `JobScheduler`. The only scheduler edit is a pre-spawn guard.
- **Single host.** pmd (daemon) and pmtui run on the same machine and share the filesystem + tmux socket, so pid-liveness via `/proc/<pid>` and a shared marker file are valid coordination primitives.
- **Marker path is per-session.** It MUST live under the per-session state subtree so multiple sessions in one folder don't collide. Use `ProjectPaths` (which already routes `sessions/<seg>/` for agent-loop sessions).
- **`is_active` is a pure predicate — no side effects.** A dead/stale marker is simply ignored (returns false) and overwritten by the next `mark`; nothing unlinks it mid-check. `clear` (called by pmtui on exit) is the normal removal path and is idempotent.
- **Interactive launch mirrors the poll's argv shape but WITHOUT `-p`/`exec`:** claude → `env -u CLAUDECODE claude --resume <id>`; codex → `codex resume <id>`. (The poll worker uses `claude -p --resume <id>` / `codex exec resume <id>`; the interactive REPL drops `-p`/`exec`.)
- **Tests must not require a TTY or a real CLI.** All new logic is unit-testable over the existing `FakeDriver`/`ProjectPaths` fixtures. The one tty-owning function (`chat()`) is left untested exactly like the existing `attach`/`watch`/`edit_brief`.
- **Cadence/budget/backoff behavior is untouched** — deferring a wake while chat is active does not consume the wake/wall-clock budget and does not advance `self.run`.

---

### Task 1 (CHAT-A): chat marker module + `ProjectPaths::chat_lock` + daemon interlock

**Files:**
- Create: `src/chat_lock.rs`
- Modify: `src/lib.rs` (add `pub mod chat_lock;` in alphabetical position — between `job_engine` and `lease`, i.e. after line `pub mod job_engine;`)
- Modify: `src/state.rs` (add `ProjectPaths::chat_lock()` next to `daemon_dir()` at ~line 274)
- Modify: `src/job_engine.rs` (add the pre-spawn interlock in `tick()`, ~lines 202-219; add tests near the existing scheduler tests)

**Interfaces:**
- Produces (`src/chat_lock.rs`):
  - `pub const CHAT_STALE_S: i64` — staleness cap (use `12 * 3600`).
  - `pub struct ChatMarker { pub pid: u32, pub since: crate::clock::Epoch }` — `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]` + `#[serde(deny_unknown_fields)]`. (`Epoch` is the crate's epoch-seconds type, `i64`.)
  - `pub fn mark(paths: &ProjectPaths, pid: u32, now: Epoch) -> anyhow::Result<()>` — create `paths.daemon_dir()` if missing, then `state::write_json_atomic(&paths.chat_lock(), &ChatMarker { pid, since: now })`.
  - `pub fn clear(paths: &ProjectPaths)` — `let _ = std::fs::remove_file(paths.chat_lock());` (idempotent; ignore missing).
  - `pub fn is_active(paths: &ProjectPaths, now: Epoch) -> bool` — PURE read: `state::read_json_opt::<ChatMarker>(&paths.chat_lock())` → `Some(m)` returns `pid_alive(m.pid) && (now - m.since) < CHAT_STALE_S`; `None`/error returns `false`. No writes, no unlink.
  - private `fn pid_alive(pid: u32) -> bool { std::path::Path::new(&format!("/proc/{pid}")).exists() }`.
- Produces (`src/state.rs`): `pub fn chat_lock(&self) -> PathBuf { self.daemon_dir().join("chat.json") }`.
- Consumes: `crate::clock::Epoch`, `crate::state::{self, ProjectPaths}` in `chat_lock.rs`; `crate::chat_lock` in `job_engine.rs`.

**Interlock logic in `JobScheduler::tick` (`src/job_engine.rs`):** guard the two spawn-eligible branches so a live chat marker DEFERS the wake without spawning, mutating `self.run`, or consuming budget. Re-checked each sweep, so the poll resumes promptly when the human detaches.

```rust
match self.run.clone() {
    JobRun::Idle => {
        if crate::chat_lock::is_active(&self.paths, now) {
            // Human is chatting this conversation; do NOT spawn a wake (a second
            // resume of the same id would collide). Stay Idle; re-check next sweep.
            return Ok(JobTick::Monitoring { until: now });
        }
        self.spawn(driver, now, &ledger, &config, "")
    }
    JobRun::Monitoring { until } => {
        if now >= until {
            if crate::chat_lock::is_active(&self.paths, now) {
                return Ok(JobTick::Monitoring { until });
            }
            self.spawn(driver, now, &ledger, &config, "")
        } else {
            Ok(JobTick::Monitoring { until })
        }
    }
    JobRun::Running { seq, session, deadline } =>
        self.on_running(driver, now, &ledger, &config, seq, &session, deadline),
    JobRun::Blocked { stop_ids, since } =>
        self.on_blocked(driver, now, &ledger, &config, &stop_ids, since),
}
```

Note: the `Idle` deferred arm returns `Monitoring { until: now }` as a benign "parked, will re-check" signal but does NOT set `self.run` (it stays `Idle`, so the next sweep re-matches `Idle` and re-checks). Add a short comment saying so.

- [ ] **Step 1: `ProjectPaths::chat_lock()`** in `src/state.rs` (+ a unit test asserting it is `daemon_dir().join("chat.json")` and that a `for_session` path lands under `sessions/<seg>/.daemon/chat.json`).
- [ ] **Step 2: `src/chat_lock.rs`** with the API above; `pub mod chat_lock;` in `src/lib.rs`.
- [ ] **Step 3: chat_lock unit tests** (no TTY, no CLI):
  - `mark_then_is_active_true_for_live_pid`: `mark(paths, std::process::id(), now)` (this test process is alive) → `is_active(paths, now)` is `true`.
  - `absent_marker_is_not_active`: fresh paths, no file → `false`.
  - `dead_pid_is_not_active`: `mark(paths, 4_000_000_000, now)` (a pid that cannot exist) → `false`.
  - `stale_marker_is_not_active`: `mark(paths, std::process::id(), now)` then check `is_active(paths, now + CHAT_STALE_S + 1)` → `false`.
  - `clear_removes_marker`: `mark` → `clear` → `is_active` false AND `paths.chat_lock()` no longer exists; `clear` again is a no-op (no panic).
- [ ] **Step 4: interlock in `tick()`** as above.
- [ ] **Step 5: interlock tests** in `src/job_engine.rs`, reusing the existing test fixture (the `fx` builder + `ledger(&fx)` helper used by e.g. `fresh_idle_session_spawns_the_create_wake`):
  - `chat_active_defers_the_idle_wake`: on a fresh Idle session, `chat_lock::mark(&fx.paths, std::process::id(), now)`, then `tick()` returns `JobTick::Monitoring { .. }` and the ledger run stays `Idle` (no `Running`, no `conversation_id` minted). Then `chat_lock::clear(&fx.paths)` and `tick()` again → `JobTick::Spawned { .. }` (the wake finally launches).
  - `chat_active_defers_a_due_monitoring_wake`: drive the session to `Monitoring { until }` with `until <= now`, mark chat active → `tick()` returns `Monitoring` and does NOT spawn; clear → next `tick()` spawns.
  - `dead_pid_marker_does_not_block_spawn`: on a fresh Idle session, `mark(&fx.paths, 4_000_000_000, now)` → `tick()` still `Spawned` (the dead marker is ignored).
  - Assert "did not spawn" the same way the existing tests observe spawns (via the returned `JobTick` and the ledger's `run`/`conversation_id`); mirror the closest existing test rather than inventing a new probe.
- [ ] **Step 6:** `cargo test`, `cargo clippy --all-targets`, `cargo fmt --check` all green; commit.

**Out of scope for CHAT-A:** any pmtui change. This slice is daemon/core only and is fully unit-testable.

---

### Task 2 (CHAT-B): pmtui interactive-chat launcher + Enter routing

**Files:**
- Modify: `src/bin/pmtui.rs` (`request_attach` AgentLoop branch ~lines 541-561; add a `pending_chat` field to `App` next to `pending_watch` ~line 217/232; add `build_chat`, a `ChatReq` struct, a `chat()` fn, and the run-loop drain next to the `pending_watch` drain ~lines 1621-1633)

**Interfaces:**
- Consumes: `crate::chat_lock` (from CHAT-A), `AgentLoopState.conversation_id` (already read via `entry_state_paths(e)` → `job::load`), `AgentLoopState.run` (to detect an in-flight wake for the TOCTOU re-check), the existing `Engine`, `SystemClock`, and the suspend/restore idiom from `edit_brief`/`attach`.
- Produces:
  - `struct ChatReq { session_paths: ProjectPaths, argv: Vec<String>, label: String }` (or reuse fields sufficient to `mark`/`clear` + spawn). Keep whatever the `chat()` fn needs.
  - `fn build_chat(engine: Engine, conversation_id: &str) -> Vec<String>` — argv for the interactive REPL:
    - `Engine::Claude` → `["env", "-u", "CLAUDECODE", "claude", "--resume", <id>]`
    - `Engine::Codex` → `["codex", "resume", <id>]`
  - `App.pending_chat: Option<ChatReq>`.
  - `fn chat(terminal, req: &ChatReq) -> Result<()>` — mirrors `edit_brief`'s EXACT suspend→run→restore sequence: `chat_lock::mark(&req.session_paths, std::process::id(), SystemClock.now())`; `disable_raw_mode()`; `LeaveAlternateScreen`; run `Command::new(&argv[0]).args(&argv[1..]).status()` (foreground, inherits the tty); ALWAYS `chat_lock::clear(&req.session_paths)` afterward (even on error — clear before returning on every path); `enable_raw_mode()`; `EnterAlternateScreen`; `terminal.clear()`. A non-zero engine exit (e.g. the user `/exit`s) is a clean end, NOT an error, since interactive engines can exit non-zero on Ctrl+C — treat any spawn that RAN as success; only a spawn *failure* (engine not found) is an `Err`.

**`request_attach` AgentLoop routing (replace the current watch-only branch):**
1. Read the per-session ledger: `job::load(&entry_state_paths(e))`.
2. If a wake is live (the existing `watch_pane(driver)` returns `Some(pane)` AND `is_alive(pane)`), keep the CURRENT behavior: watch it (`pending_watch = Some(pane)`), status "watching …".
3. Else if the ledger has `conversation_id = Some(id)` AND `run` is not `Running`: set `pending_chat = Some(build_chat(...))` with `session_paths = entry_state_paths(e)`; status e.g. "chatting {id} — exit the REPL (Ctrl+D / /exit) to return".
4. Else if `conversation_id` is `None`: status "{id}: waiting for first wake — the conversation is created on the first heartbeat; Enter to chat once it has woken · a answer · d close".
5. Else (a wake is Running but no live pane found — rare race): keep the existing `no_wake_status`.

**TOCTOU guard in `chat()` (or just before launching):** after `chat_lock::mark`, re-read the ledger `run`; if it became `Running` (a wake beat us), `chat_lock::clear` and return an `Err`/status "a wake just started — try again in a moment" WITHOUT launching. (This makes the cross-process race benign; the daemon's CHAT-A guard handles the other ordering.)

- [ ] **Step 1: `build_chat`** + unit tests for both engines' argv (claude has `env -u CLAUDECODE … --resume <id>` and NO `-p`; codex has `resume <id>` and NO `exec`).
- [ ] **Step 2: `request_attach` routing** as above; extract the pure decision (given `live_pane: Option<&str>`, `conversation_id: Option<&str>`, `run_is_running: bool`) into a small helper returning an enum (`Watch(pane)` | `Chat(id)` | `WaitingFirstWake` | `NoWake`) and unit-test THAT (the tmux/tty parts stay in `request_attach`).
- [ ] **Step 3: `pending_chat` field + run-loop drain** next to the `pending_watch` drain; on `chat()` `Ok`/`Err` set an appropriate status.
- [ ] **Step 4: `chat()` fn** (tty-owning; not unit-tested, like `attach`/`edit_brief`) with the guaranteed `chat_lock::clear` on all paths + the TOCTOU re-check.
- [ ] **Step 5:** `cargo test`, `cargo clippy --all-targets`, `cargo fmt --check` green; commit.

**Manual (not automated) acceptance after merge:** with pmd running and an agent-loop session that has woken at least once, Enter on the parked row drops into a real claude/codex REPL on its conversation; typing works; exiting returns to the dashboard; the daemon skipped wakes while attached and resumes after exit.

---

### Task 3 (CHAT-C, optional polish): dashboard "paused for chat" indicator

**Files:** Modify `src/bin/pmtui.rs` (the AgentLoop row renderer + keybar copy).

**Interfaces:** Consumes `chat_lock::is_active(&entry_state_paths(e), SystemClock.now())` (pure predicate — safe to call during render).

- [ ] **Step 1:** when rendering an AgentLoop row whose `chat_lock::is_active` is true, show a "paused for chat" glyph/label so the human understands why the poll is idle.
- [ ] **Step 2:** update the keybar/help copy so Enter reads as "watch/chat" for agent-loop rows.
- [ ] **Step 3:** `cargo test` / clippy / fmt green; commit.

Decide whether to build CHAT-C after A+B land; it is cosmetic and can be deferred.

## Self-Review

- **Spec coverage:** CHAT-A = interlock + marker (spec "Interlock", "Availability"); CHAT-B = launcher + routing (spec "On-demand chat", "Enter is context-sensitive"); CHAT-C = "paused for chat" indicator. TOCTOU handled in both A (daemon guard) and B (re-check). ✓
- **Type consistency:** `ChatMarker { pid: u32, since: Epoch }` used identically by `mark`/`is_active`; `build_chat(engine, conversation_id) -> Vec<String>` matches `worker::build_command`'s argv style but drops `-p`/`exec`. ✓
- **No placeholders:** every step has the concrete API, argv, and test cases. ✓
