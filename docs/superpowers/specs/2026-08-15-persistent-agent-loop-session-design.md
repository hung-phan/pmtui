# Persistent Interactive-Session Model for `Mode::AgentLoop` — Design

> **Status:** APPROVED by the user (2026-08-15) to build all 4 slices via SDD.
> Supersedes the ephemeral per-wake `claude -p` model for `Mode::AgentLoop` (see the COURSE CORRECTION in `.project-state/decisions.md`). `Mode::Auto` (the phase machine) is untouched.
> Authored from an adversarial design workflow (agent-deck mechanics @ `/tmp/agent-deck-2182616` + our internals). File:line citations are as-of the workflow run; verify before editing.

---

# DESIGN: Persistent Interactive-Session Model for `Mode::AgentLoop`

Status: DRAFT for human review — no code yet. Supersedes the "poll STAYS / ephemeral per-wake worker" ruling (`decisions.md:130-147`) and re-adopts the previously-offered-but-unchosen "wake drives the live session in-place (send-keys)" model (`decisions.md:261`, `handoff.md:13`).

---

## 1. Goal & the core insight

**One conversation = one live `claude`.** Today `Mode::AgentLoop` spawns a fresh headless `claude -p --output-format stream-json` worker per cadence tick (`job_engine.rs:399-412`), which exits after one step and writes a terminal `result.json`. Between wakes there is nothing running, nothing to attach to; the dashboard shows "no wake running right now — runs every 300s." The user has repeatedly called this unusable and now explicitly wants agent-deck's model.

**The insight that reconciles "live UI you can chat with" and "wake never stops":** these are not in tension once the agent is a *single long-lived interactive process*. The harness stops *being* the agent's execution (spawn/exit) and becomes only its **metronome and observer** — exactly agent-deck's split. Concretely:

- The agent runs as ONE persistent interactive `claude` inside a detached tmux session, born once at session create, living for the session's lifetime.
- The heartbeat/cadence is no longer "spawn a process" — it is a `send-keys` **nudge** typed into that live session (the `/loop`/ralph-loop Stop-hook style the user cited, `decisions.md:116-118`).
- **Enter in pmtui attaches** to that live session (agent-deck's `tmux attach-session`); **Ctrl+q detaches** and it keeps running (we already ship this: commit `9efa7f6`, `tmux.rs:349`); a **separate key** shows a read-only full-screen view.

The three hard user requirements map cleanly: (1) Enter always opens the live claude UI → attach the one session; (2) the wake must not stop the agent → nudge is a keystroke into the running REPL, never a kill/respawn; (3) read-only full-screen on another key → `attach-session -r` or a `capture-pane -e` mirror.

The escalation/ledger machinery is **transport-agnostic** and survives almost verbatim (see §6). The re-architecture is deliberately narrow: swap the *execution transport* (spawn→nudge) and the *completion signal* (process-exit+result.json → observed idle/marker), and preserve everything else.

---

## 2. Architecture — the persistent-session lifecycle

### 2.1 Spawn ONE long-lived interactive claude (create-once)

Mirror agent-deck's `tmux new-session -d` (agentdeck study §1a) using the primitive pmtui **already** uses for the human chat REPL: `TmuxDriver::launch_interactive(session, cwd, argv)` (`tmux.rs:292`), which is idempotent-by-name, checks `binary_on_path`, re-checks liveness so a crash-on-start surfaces, and calls `ensure_detach_key()`.

- **Naming:** use the **no-seq** family. Recommend a new prefix `pmloop-<id>-<hash8>` via a copy of `chat_session_name` (`tmux.rs:218`), namespaced separately from ad-hoc `pmchat-`. (Alternative: promote `pmchat-` to be the loop session itself — pmtui-tmux study §3 notes it is already 90% of what we need. See §9 open question.) Do **not** use the seq family (`pmj-`, `tmux.rs:206`) — a persistent session is one stable name, not a per-wake spawn.
- **Socket:** the shared private `-L <socket>` server (default `"pmd"`, `pmtui.rs:59-65`). pmd, pmtui attach, and the chat_lock liveness probe MUST share it or the fork hole reopens (`chat_lock.rs:15-17`).
- **cwd:** the session's work_dir (registry root), same as today.
- **env:** the launch command must `export` the session's identity and **`ECC_GATEGUARD=off`** (the worker stalls on the ECC GateGuard hook otherwise — MEMORY). Follow agent-deck's `bashExportPrefix` pattern (agentdeck study §1d, `instance.go:1352-1359`): `export` so vars survive an exit-to-shell fallback. Deliver the command as separate argv tokens (`bash -c <cmd>`) so tmux `execvp`s claude directly as pane leader rather than wrapping in `$SHELL -c` (agentdeck study §1b).
- **argv:** interactive claude (NO `-p`, NO `--output-format stream-json`) — the full-screen alt-screen UI, resuming a pinned `--session-id <uuid>` minted once at create. This is exactly `build_chat_create` today (`pmtui.rs:1403`).

**Write ordering (preserve the single-writer invariant, our-agentloop study §4):** record the live session handle in `driver.json` *before* flipping the ledger `run`, and if the ledger write fails, `terminate` the just-launched session so no live process exists that the ledger doesn't know about (mirror `job_engine.rs:411-426`).

### 2.2 The daemon's role — metronome + observer, no external OS timer

agent-deck needs a launchd/systemd timer to run its heartbeat script (agentdeck study §6a) because it has no long-running supervisor. **We already have one:** pmd's 500ms liveness sweep (`bin/pmd.rs:146-155`) + the cadence clock in `JobRun::Monitoring{until}` (`job.rs:38`). So pmd's sweep *is* the heartbeat driver — no new OS timer. Per sweep, `reconcile_one` still acquires the per-session `driver.lock` lease (`daemon.rs:271-293`) and calls `JobScheduler::tick`.

`JobScheduler::tick` (`job_engine.rs:220-284`) keeps its shape but its arms change meaning:
- `Idle` / `Monitoring{until}` due → **nudge** (was: spawn).
- `Running` → **observe liveness + idle/marker** (was: observe done-signal).
- `Blocked` → unchanged (wait for fresh human answer, `job_engine.rs:771-823`).

### 2.3 Heartbeat becomes a send-keys nudge on cadence

When a sweep finds the cadence due (`now >= until`) AND the agent is idle-at-prompt AND no human is attached AND no chat_lock defer, the harness types the nudge:

1. Build the nudge text = the existing `wake_prompt(...)` (`job_engine.rs:1034-1089`) or `answers_extra`/`pending_context` (`job_engine.rs:1094-1115`) — reused verbatim, including the "use YOUR OWN tools / the harness sends no messages" instruction (`job_engine.rs:1050-1052`) so the agent keeps owning Slack (our-state study §4).
2. Deliver it via a NEW `send_keys` primitive following agent-deck's proven sequence (agentdeck study §2a): `send-keys -l -- <text>` (literal; `load-buffer`+`paste-buffer` for large/multiline), then **sleep 100ms** (bracketed-paste guard), then a separate `send-keys Enter`. The 100ms gap is load-bearing (`tmux.go:5367-5372`).
3. Park `run = Monitoring{until: now + cadence}` (`cadence_s`, reinterpreted as the nudge interval).

This is the "ralph loop": every cadence tick, if the agent has gone quiet, poke it to keep working the goal. The agent never dies.

---

## 3. Components / files to change

| File | Change | Reused / kept |
|---|---|---|
| **`tmux.rs`** | **ADD `send_keys(session, text)`** to the `Driver` trait + `TmuxDriver` impl (biggest missing primitive — grep-confirmed none exist). Sequence: literal `send-keys -l --` (or `load-buffer`/`paste-buffer` for big text), 100ms sleep, separate `Enter`. **ADD `attach_command_readonly`** = `attach-session -r` (one-line variant of `tmux.rs:378`). Optionally extend `capture_tail` (`tmux.rs:453`) with `-e` (keep ANSI) + widened `-S` for a mirror view. **ADD an idle/busy probe** (`capture-pane` scrape — see §4). | `launch_interactive` (`tmux.rs:292`), `ensure_detach_key` (`tmux.rs:349`), `attach_command` (`tmux.rs:378`), `is_alive`/`has_clients`/`session_created`/`terminate`, `sanitize_name`/`hash8` all reused verbatim. |
| **`worker.rs`** | The ephemeral `claude -p … stream-json … -- <prompt>` argv builder (`worker.rs:126-209`) is **no longer used by AgentLoop**. Add/keep an interactive argv builder (essentially `build_chat_create`, already in pmtui `pmtui.rs:1403`) with `ECC_GATEGUARD=off` in env. `PermissionMode::Auto` rationale (`worker.rs:23`) still applies. | `worker.rs` stays for `Mode::Auto` phase steps. |
| **`job_engine.rs`** | Replace the launch triplet (`job_engine.rs:399-412`) with create-once `launch_interactive`. Replace the two cadence-due `spawn` sites (`job_engine.rs:247`, `270`) and the unblock `spawn` (`job_engine.rs:821`) with `send_keys` nudge. Replace `on_running`'s done-signal `observe` (`job_engine.rs:442-518`) with an idle/marker detector. Replace `on_worker_result`'s `parse_report(step_result(seq))` (`job_engine.rs:539`) with parse of a **watched marker file** (§4). **Delete** the per-wake `seq` counter, resume-anchoring/adopt/de-poison block (`job_engine.rs:355-389`, `825-876`), `mint_uuid_v4` per-wake (moves to create), `time_out`/`step_timeout_s`. | `park_monitoring`, `on_report_blocked` (`job_engine.rs:600-713`), `lenient_working`, `after_failure` backoff, budget gates (`job_engine.rs:304-327`), stall ceiling, `restore_from_disk` (`job_engine.rs:178-216`, adapted to "re-attach live session, don't relaunch") all kept. |
| **`daemon.rs`** | Routing (`daemon.rs:85-96`) and lease (`daemon.rs:271-293`) unchanged. Escalation re-emit + `notified` dedup (`daemon.rs:386-419`) unchanged. | Verbatim. |
| **`state.rs` / `job.rs`** | Prune per-wake artifacts: `steps/<seq>.{done,log,result.json}` (`state.rs:262-272`), `driver.json` per-wake `step_id`/`deadline` semantics. Change `JobRun::Running{seq,session,deadline}` → session-scoped handle (see §6). Add a watched marker path (e.g. `sessions/<id>/needs-you.json`). | `AgentLoopState`, `WakeReport`/`WakeState`/`StopDraft`, `open_stops`, `Blocked{stop_ids,since}`, atomic writes (`state.rs:351-377`) kept. |
| **`pmtui.rs`** | Enter routing **collapses** (§ below). `Watch`/`Chat`/`CreateAndChat`/re-attach all become "attach to the one session." The lease dance (`pmtui.rs:929-1000`), `create_chat` lease-through-REPL fence, `reattach`/`run_is_running` guards, ledger-vs-registry `effective_id` reconciliation (`pmtui.rs:876-897`) become largely dead. Add a key for the read-only view. | Suspend/restore block (`pmtui.rs:2628-2633`), `ChatGuard`/`CreateGuard` never-kill-on-return (`pmtui.rs:2697-2707`), chat_lock defer (`chat_lock.rs`) kept. |
| **`policy.rs`** | **Zero changes.** `decide_kind`/`effective_risk_kind`/`ALWAYS_HARD_KINDS` (`policy.rs:51-78`) are pure policy over stops, transport-independent. | Verbatim. |

**Enter routing simplification (pmtui-tmux study §4).** The pure decision fn `agent_loop_enter(...)` (`pmtui.rs:1186-1206`) exists to arbitrate two racing conversation origins (headless poll worker vs human REPL). With ONE shared session that fork surface disappears, so the four-way router collapses to: *ensure the session exists (`launch_interactive`, idempotent) → `attach_command`.* What survives: the chat_lock defer (still tells the loop "a human is typing, hold your nudge") and a "hasn't been launched yet" first-time branch.

**What happens to the just-shipped `UiMode::WakeView` + `stream_json` parser (pmtui-tmux study §5).** Be honest: this work is **demoted, not deleted**. The stream-json log source dries up for AgentLoop because the interactive session emits an ANSI TUI, not newline-JSON. Options:
- **Keep** `stream_json::render_transcript` + `UiMode::WakeView` for `Mode::Auto` phase steps and any remaining headless wake (they still log stream-json) — no loss there.
- **For the AgentLoop read-only view**, replace the content pipeline with either (a) `tmux attach-session -r` (true live UI, zero parsing, reuses the entire suspend/restore + Ctrl+q machinery — cheapest) or (b) a `capture-pane -e` snapshot rendered through an ANSI-to-ratatui parser (needs a new dep like `ansi-to-tui`). The reusable shell of WakeView is the `UiMode` variant + full-screen layout + footer + keybar; the disposable part is the per-seq log-path resolution and the append-only scroll model (a repainting alt-screen has no linear scrollback — "live mirror," not "scroll a transcript").

---

## 4. Busy/idle + completion/blocked detection (the hardest part)

Two distinct questions, two distinct mechanisms. This is where we must borrow agent-deck's layered approach (agentdeck study §3) but adapt it to preserve our *richer* escalation-by-scope contract that agent-deck lacks.

### 4a. Busy (mid-response) vs idle-at-prompt — so we never nudge mid-work

We must NOT `send-keys` a nudge while the agent is mid-response (agent-deck's #1 rule; a send into a busy pane just queues the keystroke and can merge with output). Two possible signals:

- **Pane scraping (v1, no hook install).** `capture-pane -p` the last ~15 lines and apply agent-deck's proven Claude patterns (`detector.go:107-339`): BUSY if it contains `"esc to interrupt"` / `"ctrl+c to interrupt"` or a Braille/asterisk spinner; IDLE if the last line is a bare `❯`/`>` prompt. This is a new probe on `TmuxDriver`. Simple, no claude-settings mutation, works today.
- **Hook-driven status (v2, authoritative).** Mirror agent-deck exactly: inject a hook handler into claude's `settings.json` for `UserPromptSubmit`(→running) and `Stop`(→waiting) (agentdeck study §3a, `claude_hooks.go:39-66`); the hook writes a status file the harness reads with a freshness window (`StatusIsBusy = running|starting`). This is a true turn-edge, strictly better than scraping, and is the same signal we'd use for blocked detection (4b). Fall back to pane scraping when the hook is stale (agent-deck's exact layering).

**Nudge gate (agent-deck's `--defer-if-busy`, `deferbusy.go:49-85`):** only nudge when status is idle/waiting; if busy, re-park `Monitoring{until}` and try next cadence. **Failure modes:** a stale spinner glyph masking a wedged session (agent-deck short-circuits model-unavailable/auth-401 banners to error *before* the busy check, `substate.go`; we should add a bounded "busy for > N minutes → treat as stall" backstop tied to our existing `max_wall_clock_s`/`stuck_threshold` budgets, `job_engine.rs:304-327`).

### 4b. Completion / blocked / needs-human — WITHOUT a clean `-p` exit + result.json

This is the crux. A persistent REPL idling at a prompt never writes a done-signal, so today's `observe()` would see `Running` forever (our-state study Q1). We need a "a decision point was reached" edge that still carries `{kind, risk_class, question, context_ref}` so `policy::decide_kind` runs **unchanged**. Ranked (our-state study Q1):

1. **Marker file the agent writes (STRONGLY PREFERRED).** Keep the exact `WakeReport`/`StopDraft` JSON contract (`job.rs:118-148`, `deny_unknown_fields`), but the nudge prompt instructs the agent to write it to a **watched, stable path** (e.g. `sessions/<id>/needs-you.json`) *whenever it wants a decision* — not "as its final act before exiting." The harness watches mtime/content and, on a bump, feeds it straight into the unchanged `dispose`/`on_report_blocked` pipeline (`job_engine.rs:532-590`). This preserves escalation-by-scope verbatim (tier oracle, `JobRun::Blocked`, stale-answer guard, notify-once dedup) and matches our single-writer-files discipline. **This is the natural pairing for the send-keys model.**
   - *Failure modes:* agent forgets to write it (→ falls back to idle-at-prompt backstop, below); agent writes malformed JSON (→ `lenient_working`, bump stall counter, `job_engine.rs:718-740`); marker is stale from a previous turn (→ need a monotonic turn/seq stamp inside the marker so an old file can't re-trigger, analogous to the `answered_at >= since` stale-answer guard, `job_engine.rs:783-791`).
2. **Sentinel line in the transcript** (`<<PM_STOP {json}>>` scraped from `capture-pane`). Weaker: free-form output is lossy/racy, the model may reword it. Use only if (1) proves unreliable.
3. **Idle-at-prompt heuristic (backstop ONLY).** If the pane goes quiet at a `❯` with a visible question, we know it's *stuck* but NOT its `kind`/`risk_class`, so it can only produce a generic `Ambiguity`/`WorkerStuck` (floors to Medium) or `Stuck`. This loses scope fidelity — reserve it strictly as the stall backstop (the persistent analogue of `max_wakes`/`max_wall_clock_s`, `job_engine.rs:41-51`), never the primary blocked detector.

**Design ruling to carry forward:** detection mechanism is swappable; the `WakeReport`/`StopDraft` *schema* and the tier oracle are the invariant. Adopt (1) as primary, (3) as backstop.

**"Working" vs "Monitoring" completion.** When the marker says `Working`/`Monitoring`, park on cadence/`next_check_s` (`job_engine.rs:577-586`) exactly as today. The only change is the *trigger* (observed marker bump vs process exit).

---

## 5. Human-attached coexistence

When a human is attached and typing, the heartbeat MUST NOT `send-keys` into their input line (it would merge the nudge with their draft — agent-deck's `GuardComposerDraft` collision, `guard.go:165-270`). We already have the machinery:

- **`chat_lock::is_active`** (`chat_lock.rs:121-151`) is the primary gate, consulted at the three defer sites (`job_engine.rs:234`, `251`, `802`). It is re-keyed on tmux liveness (survives detach), fail-safe to DEFER, mark-before-launch, and never reaps an attached session (our-agentloop study §4). In the persistent model these three sites become "don't NUDGE" sites (instead of "don't spawn"). **Deferring must still not consume state** — no nudge, no `self.run` mutation, no answer consumed, budgets preserved (`job_engine.rs:242-246`).
- **`has_clients`** (`tmux.rs:484`) is the second gate: if a client is attached RIGHT NOW, defer the nudge even if the chat marker is somehow absent. agent-deck gates its wake nudge on idle for the same reason (`inbox_nudge.go`).
- **Busy gate (§4a):** even with no human, only nudge at an idle prompt.

Combined nudge precondition: `cadence due AND !chat_lock::is_active AND !has_clients AND idle-at-prompt`. The `Monitoring`-due defer additionally pauses the wall-clock window (`window_start = now`, `job_engine.rs:262-268`) so a long human chat can't trip a spurious "stuck after Nh."

Note: with ONE shared session, "human attaches" and "harness nudges" target the *same* tmux session — so the `has_clients`/chat_lock gates are exactly the arbitration we need, and the whole fork-guard apparatus that existed for two racing sessions (pmtui-tmux study §4) becomes unnecessary.

---

## 6. Ledger / escalation preservation

Escalation-by-scope is a transport-independent seam (our-state study, summary): `StopDraft{kind,risk_class}` → `decide_kind(tier,·)` → `JobRun::Blocked{stop_ids,since}` + `OpenStop`s, unblocked only by a fresh `Answer` past `since`. **Preserved verbatim.**

`AgentLoopState` (`job.rs:50-98`) field fates:
- **Keep:** `created_at`/`updated_at`, `engine`, `conversation_id` (becomes *more* central — the id the live session lives on), `open_stops`, `last_status`, `pending_context` (now `send-keys`'d, not templated into `-p`), `continuations` (re-sourced: consecutive nudges with no marker progress).
- **Reinterpret:** `cadence_s` → nudge/heartbeat interval (not spawn interval). `run: JobRun` — keep the enum, change one variant.

`JobRun` (`job.rs:26-41`):
- `Idle`, `Monitoring{until}` (until = "next nudge due"), `Blocked{stop_ids,since}` — **kept** (`Blocked` fully unchanged, stale-answer guard keys on `since`).
- `Running{seq,session,deadline}` — **the variant that changes.** `seq`/`session=pmj-<seq>`/`deadline` are per-wake-spawn artifacts. Collapse to a session-scoped handle: the stable session name + a "nudge-in-flight" marker; per-wake `deadline` → session-liveness + idle-timeout check. Crash-recovery handle stays in `driver.json` but keyed on "is the one tmux session alive?" not "which numbered wake."

`WakeReport`/`WakeState` (`job.rs:118-148`): `state{Working|Monitoring|Blocked}`, `stops`, `next_check_s`, `status`, `conversation_id` (codex-capture backstop) — **all kept**; only the *transport* (marker file vs terminal result.json) changes.

**Dropped** (per-wake machinery): `steps/<seq>.{done,log,result.json}`, the `seq` counter, per-wake backoff/`Observation::Completed{exit_code}`, resume-anchoring/adopt/de-poison, per-wake `deadline`/`time_out`. The deliberate exclusion of a machine `Done` (`job.rs:22`, done is human-only) is orthogonal and stays.

**Slack:** unchanged — the harness implements no Slack (`registry.rs:27-28`, `decisions.md:104-106`); the agent owns comms via its own MCP. The only requirement: the nudge text must keep the "use your own tools; the harness sends no messages" line (`job_engine.rs:1050-1052`).

---

## 7. Migration / coexistence

- **`Mode::Auto` (phase machine) is UNTOUCHED.** It still uses ephemeral `pmd-`/`pmp-` steps (`scheduler.rs`/`phase_engine.rs`) and still logs stream-json into WakeView. This design touches only `Mode::AgentLoop`.
- **Replace `Mode::AgentLoop` wholesale, or add a new mode?** Recommend: keep the `Mode::AgentLoop` enum name (registry entries don't change) but replace its `Driven::Job(JobScheduler)` execution with the persistent path. This avoids a config migration. Alternative: add `Mode::AgentLoopLive` alongside and route existing sessions to the old path — more code, dual maintenance, but lets us A/B. Recommend the wholesale replace since the user has firmly chosen this model and the ephemeral path's just-shipped niceties (WakeView) are being demoted anyway.
- **Back-compat for existing sessions on disk:** an existing `state.json` with `run = Running{seq=…}` or a live `pmj-<seq>` worker must be handled on first sweep after upgrade. `restore_from_disk` (`job_engine.rs:178-216`) should: terminate any stray `pmj-` worker, treat the session as "needs (re)launch of the persistent session," and preserve `conversation_id`/`open_stops`/`Blocked`. Since `conversation_id` is already persisted, the new persistent session resumes the SAME conversation — no history loss.
- **`stream_json` parser + `UiMode::WakeView`:** kept for `Mode::Auto`; AgentLoop's live view switches to `attach -r` (v1) or `capture-pane -e` mirror (later).

---

## 8. Incremental slice plan

Each slice independently testable (FakeDriver unit tests + a live smoke test).

**Slice 1 — Persistent spawn + Enter-attach + naive idle nudge (shippable, usable).**
- Add `send_keys` to `Driver` trait + `TmuxDriver` (send-keys -l / 100ms / Enter) with FakeDriver test.
- AgentLoop `spawn` → create-once `launch_interactive` (name `pmloop-`, `ECC_GATEGUARD=off` env, pinned session-id). Write-ordering preserved.
- Enter routing collapses to attach-the-one-session. Ctrl+q detach already works.
- Cadence nudge fires the `wake_prompt` via `send_keys`, gated ONLY on pane-scrape idle (§4a option 1) + `has_clients` + chat_lock.
- No blocked detection yet (agent still uses its own Slack MCP to reach a human; harness just keeps nudging). This alone delivers requirements (1) live UI on Enter and (2) wake never stops.
- *Test:* create session, confirm one `pmloop-` session lives across detach; confirm nudge lands only when idle/unattached; confirm chat_lock defers.

**Slice 2 — Marker-file blocked/completion detection (restores escalation-by-scope).**
- Add watched marker path + a monotonic turn stamp. Nudge prompt updated to write `WakeReport` JSON to it at decision points.
- `on_running`/`on_worker_result` re-pointed from done-signal/`result.json` to marker bump → unchanged `dispose`/`on_report_blocked`.
- *Test:* agent writes a `Blocked{kind:Publish}` marker → `JobRun::Blocked`, escalation notified once; fresh answer unblocks and nudges resume. Reuse existing policy/escalation tests unchanged.

**Slice 3 — Robust busy detection + stall backstops.**
- Add hook-driven status (agent-deck-style `settings.json` injection, freshness window) with pane-scrape fallback; short-circuit auth/model-unavailable banners to error.
- Wire the idle-at-prompt stall backstop into `max_wall_clock_s`/`stuck_threshold`.
- *Test:* nudge never fires mid-response; wedged session escalates as Stuck within budget.

**Slice 4 — Read-only full-screen view (requirement 3).**
- v1: bind a key to `attach -r` (`attach_command_readonly`). Later: `capture-pane -e` + ANSI render if a non-attach mirror is wanted.
- Retire/relabel `UiMode::WakeView` for AgentLoop; keep it for `Mode::Auto`.
- *Test:* read-only key shows live UI; typing is rejected; Ctrl+q returns.

---

## 9. Risks & open questions FOR THE HUMAN

1. **Busy-detection approach (biggest risk).** Start with pane scraping (Slice 1, no claude-settings mutation) and add the hook-driven status later (Slice 3)? Or invest in the claude `settings.json` hook injection up front for a true turn-edge (agent-deck's authoritative signal)? Hook injection mutates the user's claude config and must be per-session-scoped and reversible — confirm this is acceptable.
2. **How is "blocked/needs-human" signaled?** Recommend the **marker file** the agent writes (`needs-you.json`, preserves scope fidelity) over transcript-sentinel or idle-heuristic. Confirm we may change the wake prompt so the agent writes its `WakeReport` mid-session (not "as final act"). This is the linchpin of preserving escalation-by-scope.
3. **Fully replace ephemeral AgentLoop, or run both?** Recommend wholesale replace (keep the `Mode::AgentLoop` name). Confirm we may retire the ephemeral `pmj-` path for AgentLoop and **demote** the just-shipped `UiMode::WakeView` + `stream_json` parser to `Mode::Auto`-only. (Honest cost: that recently-shipped read-only transcript view stops applying to AgentLoop; for AgentLoop it becomes `attach -r`.)
4. **Session name:** new `pmloop-` prefix, or promote the existing `pmchat-` (`tmux.rs:218`) to BE the loop session (it's already no-seq, survives-detach, poll-deferring — pmtui-tmux study §3)? Promoting `pmchat-` means human-chat and the loop are literally the same session (simplest, matches "one conversation = one live claude" most purely); a separate `pmloop-` keeps them namespaced. Recommend **promote `pmchat-`** for maximal simplicity unless there's a reason to keep ad-hoc chat separate.

**Honest cost summary.** Reused nearly verbatim: policy/escalation, ledger + atomic writes, chat_lock, budgets/stall ceiling, crash recovery shape, ops journal, lease, the entire attach/detach/Ctrl+q machinery (already shipped). Genuinely new: the `send_keys` primitive (~small), busy/idle detection (medium — the real engineering), the marker-file blocked contract + watcher (medium), Enter-routing simplification (net deletion — the fork-guard apparatus largely disappears). Superseded/demoted: the ephemeral `pmj-` per-wake path and, for AgentLoop, the `stream_json`/`WakeView` transcript view. This reverses the "poll STAYS" correction (`decisions.md:130-147`) — record a new dated decision citing and superseding it, and re-adopting the "wake drives the live session in-place (send-keys)" model (`decisions.md:261`, `handoff.md:13`), per the ledger's own reversal convention.
