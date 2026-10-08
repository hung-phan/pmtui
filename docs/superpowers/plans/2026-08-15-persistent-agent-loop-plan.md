# Persistent Agent-Loop Session — Implementation Plan

> Executes the approved design `docs/superpowers/specs/2026-08-15-persistent-agent-loop-session-design.md`.
> Method: superpowers:subagent-driven-development — fresh implementer per task → task review → whole-branch
> review per slice → merge `--no-ff`. One branch per slice; tasks sequential on it (never parallel implementers
> in one tree). `main` stays green after every slice merge.

**Goal:** Move `Mode::AgentLoop` from the ephemeral per-wake `claude -p` model to ONE persistent interactive
`claude` session per session, driven by `tmux send-keys` nudges, that Enter attaches to. `Mode::Auto` untouched.

## Global constraints (bind every task)
- **Single-writer ledger:** only the daemon writes `state.json`; pmtui writes only the registry + `chat.json`.
  Record the live-session handle in `driver.json` BEFORE flipping the ledger `run`; on a ledger-write failure,
  `terminate` the just-launched session (mirror the existing spawn write-ordering).
- **Preserve escalation-by-scope verbatim:** the `WakeReport`/`StopDraft{kind,risk_class}` schema + `policy::decide_kind` + `JobRun::Blocked{stop_ids,since}` + the stale-answer guard are the invariant; only the *transport* changes.
- **Never nudge into a busy/attached pane:** nudge precondition = `cadence due AND !chat_lock::is_active AND !has_clients AND pane-idle`. A deferred tick consumes NO state (no run mutation, no answer consumed, budgets/window preserved).
- **Harness implements no Slack:** the nudge text keeps the "use YOUR OWN tools; the harness sends no messages" line; the agent owns comms via its own MCP.
- **Distinct `pmloop-<id>-<hash8>` session name** (NOT `pmchat-`), so the chat orphan-reaper can never kill the persistent agent. On the shared private `-L <socket>` server.
- **`ECC_GATEGUARD=off`** must be exported into the persistent session's env (else the agent stalls on the hook).
- Tests: `cargo test` / `cargo clippy --all-targets` / `cargo fmt --all -- --check` green each task. FakeDriver for unit tests; a live smoke test per slice.

---

## Slice 1 — Persistent spawn + Enter-attach + idle nudge (branch `m6-persistent-slice1`)
Delivers user requirements (1) Enter opens the live claude UI and (2) the wake never stops. Design §2, §3, §5, §8.

### Task 1.1 — tmux primitives: `send_keys` + pane-activity classifier
- **Files:** `src/tmux.rs` (Driver trait + TmuxDriver + FakeDriver).
- **Add** `fn send_keys(&self, session: &str, text: &str) -> Result<()>` to `Driver` (trait, ~:39) + impls.
  Sequence (agent-deck-proven): literal `send-keys -l -- <text>` (use `load-buffer`+`paste-buffer` for text with
  newlines or > ~1KB), then a bracketed-paste guard delay, then a SEPARATE `send-keys Enter`. FakeDriver records
  sent text (a `Vec` behind its interior mutability) for assertions.
- **Add** a PURE classifier `pub fn classify_pane(capture: &str) -> PaneActivity` (new enum `Busy`/`Idle`) using
  agent-deck's Claude patterns: `Busy` if the tail contains `esc to interrupt` / `ctrl+c to interrupt` or a
  Braille/asterisk spinner glyph; `Idle` if the last non-empty line is a bare prompt (`>`/`❯`). Reuse the existing
  `capture_tail` (:56) to get input; the classifier itself is pure and unit-tested.
- **Tests:** send_keys issues the literal-then-Enter sequence (FakeDriver records it); multiline routes via
  paste-buffer; `classify_pane` fixtures for busy (interrupt line, spinner) and idle (bare prompt) and unknown.
- **No behavior wiring** (job_engine/pmtui untouched this task).

### Task 1.2 — persistent lifecycle in `job_engine` + `daemon`
- **Files:** `src/job_engine.rs`, `src/daemon.rs`, `src/job.rs`/`src/state.rs` (JobRun handle), `src/worker.rs` (nudge-text builder if needed).
- Spawn-once: on first drive of an AgentLoop session with no live `pmloop-` session, `launch_interactive` the
  persistent claude (`pmloop-` name, cwd = root, env exports `ECC_GATEGUARD=off` + identity, pinned
  `--session-id <uuid>`; NO `-p`, NO stream-json). Write `driver.json` BEFORE the ledger flip; terminate on
  ledger-write failure.
- `JobScheduler::tick` arms change meaning (design §2.2): `Idle`/`Monitoring{until}` due → NUDGE (gated:
  `!chat_lock::is_active && !has_clients && classify_pane==Idle`) via `send_keys(wake_prompt/pending_context)`, then
  park `Monitoring{until: now+cadence}`; `Running`/nudge-in-flight → observe session liveness; `Blocked` unchanged.
  A deferred tick consumes no state.
- `JobRun::Running{seq,session,deadline}` collapses to a session-scoped handle (stable `pmloop-` name +
  nudge-in-flight marker); `cadence_s` reinterpreted as the nudge interval. Keep `Idle`/`Monitoring`/`Blocked`.
- `restore_from_disk`: on upgrade, terminate any stray `pmj-<seq>` worker, preserve `conversation_id`/`open_stops`/`Blocked`, and (re)launch the persistent session resuming the SAME conversation id.
- Retire the ephemeral `pmj-` spawn path for AgentLoop (keep it for `Mode::Auto`).
- **Tests:** FakeDriver — spawn-once idempotency; nudge fires only when idle+unattached+unlocked; deferred tick
  mutates nothing; restore_from_disk relaunches + preserves state.

### Task 1.3 — pmtui Enter → attach the persistent session
- **Files:** `src/bin/pmtui.rs`.
- Collapse `agent_loop_enter`/`request_attach` for AgentLoop to: attach the one `pmloop-` session (reuse the
  existing suspend/restore + `attach_command` + Ctrl+q-detach machinery). Includes **Fix D** (a stale ledger
  `Running` no longer wedges Enter). If the session isn't up yet (never driven / pmd down), keep an honest
  message. Chat-marker / survive semantics reused so a human attach defers the nudge.
- The read-only full-screen view is Slice 4; `UiMode::WakeView`/`stream_json` stays for `Mode::Auto`.
- **Tests:** Enter on an AgentLoop row routes to attach-the-persistent-session (not WakeView); stale-Running no
  longer no-ops.

### Slice 1 close: whole-branch review (opus) → merge → live smoke (create session, confirm one `pmloop-` lives across Ctrl+q detach; nudge lands only when idle/unattached).

---

## Slice 2 — Marker-file blocked/completion detection (branch `m6-persistent-slice2`)
Restores escalation-by-scope. Design §4b (option 1), §6.
- Add a watched marker path `sessions/<id>/needs-you.json` with a MONOTONIC turn/seq stamp (so a stale marker
  can't re-trigger — analogous to the `answered_at >= since` guard). Nudge prompt instructs the agent to write a
  `WakeReport`/`StopDraft` JSON there at any decision point (not "as a final act").
- Re-point `observe`/dispose from done-signal/`result.json` to a marker-mtime/content bump → the UNCHANGED
  `dispose`/`on_report_blocked` pipeline (tier oracle, `JobRun::Blocked`, notify-once). Malformed → `lenient_working` + stall counter.
- Idle-at-prompt stall = backstop only (feeds a generic Stuck within `max_wall_clock_s`/`stuck_threshold`).
- Review → merge → live smoke (agent writes a `Blocked{kind:Publish}` marker → escalates once; fresh answer resumes nudges).

---

## Slice 3 — Robust busy detection + stall backstops (branch `m6-persistent-slice3`)
Design §4a (option 2). **PAUSE for user consent** before the settings.json hook injection.
- Hook-driven status: per-session, reversible injection into claude `settings.json` (`UserPromptSubmit`→running,
  `Stop`→waiting) writing a status file read with a freshness window; pane-scrape fallback when stale.
- Short-circuit auth/model-unavailable banners to error before the busy check; wire the "busy > N min → stall"
  backstop into the existing budgets.
- Review → merge → live smoke.

---

## Slice 4 — Read-only full-screen view (branch `m6-persistent-slice4`)
User requirement (3). Design §3, §8.
- Bind a key (e.g. `w`) to a read-only attach (`attach-session -r` via a new `attach_command_readonly`), or a
  `capture-pane -e` ANSI mirror. Retire/relabel `UiMode::WakeView` for AgentLoop (keep for `Mode::Auto`).
- Review → merge → live smoke (read-only key shows live UI; typing rejected; Ctrl+q returns).

---

## Task 1.4 — pmd configurable per-session env injection (user-requested; builds after the persistent path)
User: *"For pmd, expose option so i can add additional env, so i can help set any future settings."* Design in
`.project-state/decisions.md` (2026-08-15 entry).
- **`pmd --session-env KEY=VALUE`** (repeatable flag) + **`~/.config/pmd/session-env`** file (one `KEY=VALUE`/line,
  `#` comments) read at pmd startup so it survives pmtui autopilot auto-spawns.
- Merge precedence: built-in defaults (`ECC_GATEGUARD=off`) < file < CLI flag. `ECC_GATEGUARD=off` stops being
  hardcoded in `build_loop_command` and becomes a default the user can extend/override.
- Threaded pmd → JobScheduler → `worker::build_loop_command(engine, &Resume, add_dirs, &extra_env)` (and the phase
  worker path) as an env map → the launched `env K=V … claude …` prefix.
- Review → merge → smoke (a `--session-env` / file entry appears in the launched `pmloop-` session's env).

## Deferred findings (from the Slice 1 whole-branch review — rulings in `.project-state/decisions.md`)
- **I1 — dropped de-poison self-heal.** A bad/never-persisted adopted seed can poison-pause (surfaced) or, if
  `claude --resume <bad>` lands on an error prompt, `classify_pane`→Busy-forever with no backstop (silent stall).
  → **Slice 3** stall backstop closes the silent-stall; a proper seed-verify/re-mint de-poison is **Slice 2** (needs
  the marker/observation it adds).
- **I2 — codex conversation-id capture removed.** A codex `pmloop-` that DIES relaunches a brand-new conversation
  (memory lost); codex armed-open never fires. codex is best-effort this slice → **later codex-id-capture slice**.
- **Minors M1–M5 parked** (100ms send_keys sleep on the sweep hot path; ensure_session is_alive dead-on-error;
  classify_pane heuristic edges; dead-at-runtime AgentLoop `Watch`→`WakeView` → remove in Slice 4; `park_stuck_kind`
  always `Stuck`).

## Open items carried from the design (§9)
- Slice 3 `settings.json` hook injection needs explicit user consent (per-session, reversible) — PAUSE before it.
- Slice 1 merged (`68e5e96`); user said "go" (proceed to Slice 2) rather than gating on a live-test.
