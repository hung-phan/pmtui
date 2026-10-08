# Agent-Loop / Session-Heartbeat Design

**Status:** direction approved; **S0 GREEN** (claude resume-by-id verified 2026-08-13) — building.
**Date:** 2026-08-13. **Author:** derived from a code-grounded design analysis + user decisions.

## Goal

Add a reliable **heartbeat/callback** to the harness that re-invokes a **resumable agent**
to "keep working the goal" until it is **blocked or a decision is needed**. The worker agent
(`claude`/`codex`) self-manages Slack (and all domain work) via its **own MCP**; the harness
implements no Slack. "Similar to `/project-manager`": the harness is the scheduling/liveness
engine, the agent is the worker.

## S0 result (2026-08-13) — GREEN

- **claude:** `claude -p --session-id <uuid> -- "…BANANA-4287…"` then `claude -p --resume
  <uuid> -- "what codeword?"` → returned `BANANA-4287`. Resume-by-chosen-id round-trips FULL
  context, headless, **without** `--no-session-persistence`. Load-bearing fact confirmed.
- **codex:** `codex exec resume [SESSION_ID] [PROMPT]` confirmed (UUID or thread name; `--last`
  fallback). Resume-by-id supported.
- **Still to verify at first live loop (non-blocking for S1–S3):** capturing codex's assigned
  id from the `--json` stream; whether the agent's Slack MCP loads in headless `-p`/`exec`.

## Settled requirements (user, 2026-08-13)

- **Resume the SAME conversation every wake; it is always persistent** (never torn down after
  a chat). → resume by explicit conversation id, not `--continue`.
- **Per-session heartbeat; multiple sessions may share one folder**, each with its own
  heartbeat. → a session manager keyed by session id, not by folder.
- **Slack is the agent's tool on wake**; harness posts nothing. Test channel `C0BLHUTDHUZ`
  is safe.
- **Done = human confirms or closes/deletes the session; the agent NEVER self-declares done.**
- Constraints: no model API key (drive the authenticated CLIs); no runtime skill dependency;
  small testable slices (SDD + unit/integration + clippy + fmt).

## Base decision

A new free-form **"agent-loop" run mode** (`Mode::AgentLoop`) driven by a new `JobScheduler`,
reusing pmd's sweep-as-heartbeat + `flock` lease + crash-recovery + per-unit timeout +
escalation/`answers.json` + tier policy — and **NOT** the phase machine's repo-evidence gates
(a free-form job has no fixed graph and often no git evidence). Rejected: a degenerate single
"work" phase (fights `dispose`'s gates); a detached long-runner (no re-invocation = not a
heartbeat, loses the timeout safety-net).

## Session manager (identity keyed by session, not folder)

pmd already keys its runner map by `id` (`daemon.rs`), and two `ProjectEntry` rows may share a
`root` with different `id`. Only three things force one-session-per-folder today: pmtui's
create-time root dedup, the singleton per-root `.project-state/` files, and the per-root lease.
Fix exactly those:

1. `registry::Mode::AgentLoop` (third variant); routed in `Runner::build` to `Driven::Job`.
2. `ProjectEntry` gains `#[serde(default)]` `conversation_id: Option<String>` (the resume
   anchor) + `cadence_s: Option<u64>` (per-session heartbeat interval).
3. `ProjectPaths::for_session(root, id)` re-bases state under `<root>/.project-state/sessions/
   <sanitize(id)>` — every existing path helper resolves under the per-session dir with no
   scheduler changes. The lease becomes per-session automatically (closes the two-drivers-in-
   one-tree hole). The worker's cwd stays the real `root`.
4. Per-session ledger `AgentLoopState` at `sessions/<id>/…/state.json`:
   `{ created_at, conversation_id, engine, cadence_s, run: JobRun, open_stops, last_status,
   continuations, updated_at }`. `JobRun` mirrors `RunState` but has **no `Done`** variant
   (done is human-only). In-flight handle + seq reuse `driver.json` verbatim.
5. tmux name derived: `pmj-{sanitize(id)}-{hash8(id\0root)}-{seq}` — unique ids ⇒ no collision
   even in one folder.
6. Relax pmtui root-dedup for AgentLoop only; keep the interactive-over-Auto and
   second-agent-in-Auto guards.

`Mode::Auto`/`Interactive` keep the singleton per-root files unchanged; only AgentLoop uses the
`sessions/<id>/` subtree.

## Resume mechanism (resume by id)

- **claude (VERIFIED S0):** mint a uuid, pin it on the first wake: `claude -p --session-id
  <uuid> --permission-mode acceptEdits … -- <bootstrap>`; **drop** `--no-session-persistence`
  (worker.rs adds it today — with it nothing persists). Resume: `claude -p --resume <uuid> …
  -- <wake>`. Never `--fork-session`. `conversation_id` is known at create (we choose it).
- **codex:** no caller-chosen id. First wake `codex exec --json … -- <bootstrap>`, then
  **capture** the assigned session id from the `--json` stream and store it. Resume:
  `codex exec resume <id> --json -- <wake>`. Until captured, `conversation_id` is None.
- Interactive attach (pmtui `build_attach`) also switches to resume-by-id (from the stored
  `conversation_id`) instead of `--continue`/`has_prior_session`.
- Implementation: `worker::build_command` gains `enum Resume { Fresh{session_id: Option<String>},
  Continue(String) }`. Preserve the `--`-separator + trailing-positional-prompt discipline.

## Heartbeat mechanism (two decoupled clocks)

- **Liveness heartbeat** = pmd's 500ms sweep — only observes done-signals + timers, zero token
  cost.
- **Work cadence** (token cost) = the ledger's `next_check` timer (reuse `RunState::Monitoring
  {until}`). Between wakes the session parks `Monitoring{until = now + cadence_s}`; a sweep
  re-invokes only when `now >= until`.

A single wake: take per-session lease → if due and not blocked, spawn ONE detached worker
(resume-by-id; cwd = real root) with a fixed wake-prompt ("continue the goal; do what's
pending; use your own Slack MCP; write your machine report to `stop.json`; NEVER declare
done — a human closes it") + a hard deadline → park `Monitoring`. Later sweep observes the
done-signal and parses a `WakeReport { state: working|monitoring|blocked, stops[], next_check_s,
status, conversation_id }`:
- `working` → park `Monitoring{now + cadence_s}` (capture codex id on the create wake).
- `monitoring` → park `Monitoring{now + next_check_s}` (agent self-schedules a longer nap).
- `blocked` → route each stop through the existing `policy::decide_kind`/tier oracle; escalating
  stops park `NeedsYou` + surface via escalation/`answers.json`; auto-flowable ones get an
  `auto_flow` answer + immediate re-invoke.
- missing/unparseable report → lenient: treat as `working`, bump `continuations`; at
  `stuck_threshold` raise a bounded `Stuck` escalation.
- non-zero/timeout/orphan → reuse failure→backoff→`Stuck`.

Human answers a blocked stop in pmtui → `answers.json` → next wake feeds it into the wake-prompt
and resumes the SAME conversation. Human closes/deletes (pmtui `d`) → the ONLY "done": drop the
row, kill the in-flight worker, retain per-session state. `restore_from_disk` rebuilds `Running`
from per-session `driver.json` + ledger (no double-spawn), resumes `Blocked` without re-notify.

## Borrowed from prior art

- **lavish:** separate a fast liveness heartbeat from the slow work cadence; wake-path safety
  (re-invoke only through a tracked facility whose completion the same driver observes — pmd's
  sweep + done-signal IS that callback); durable state keyed by a canonical session key; classify
  events WAKE (blocked/timeout/stall) vs ACCUMULATE (plain "working").
- **ralph-loop:** the registry entry + per-session dir ARE the loop switch; deleting them is the
  universal human-only off/done — no agent-writable "done". Re-feed an unchanged wake-prompt and
  rely on the resumed conversation + on-disk artifacts as cross-wake memory.
- **ecc continuous-agent-loop / autonomous-loops:** three bounded budgets (max wakes, max
  wall-clock, per-wake timeout) that ESCALATE ("still running — close?") rather than auto-
  terminate; no-progress stall detection; an explicit working→monitoring→blocked→closed state
  machine.

## Slice plan

- **S0 — CLI resume spike. DONE, GREEN** (see S0 result above).
- **S1 — resume-aware argv builders (pure).** `worker::build_command` `Resume` param; pmtui
  `build_attach` by-id. Unit-tested via argv asserts. Files: `worker.rs`, `bin/pmtui.rs`.
- **S2 — per-session identity, paths, registry.** `Mode::AgentLoop`, `conversation_id`+
  `cadence_s`, `ProjectPaths::for_session`, `pmj-` name, `AgentLoopState`+`WakeReport`, relaxed
  pmtui dedup. Files: `registry.rs`, `state.rs`, `tmux.rs`, `job.rs` (new), `bin/pmtui.rs`.
- **S3 — `JobScheduler` core over FakeDriver.** `Driven::Job`; tick/observe/dispose per the
  heartbeat mechanism; `restore_from_disk`. Files: `job.rs`, `daemon.rs`.
- **S4 — FIRST DEMOABLE END-TO-END LOOP.** pmtui create kind=AgentLoop → real claude/codex
  resume worker on a short cadence → watch create→work→park→re-invoke→status→blocked-surface.
  Also verifies the two S0 follow-ups (codex id-capture, MCP-in-headless). Files: `bin/pmtui.rs`,
  `daemon.rs`, `bin/pmd.rs`.
- **S5 — answer round-trip, human-close-as-done, budgets, hardening.**

## Decided defaults (assumptions; user may override)

- **Cadence default 5 min**, per-session configurable in the create form; a `monitoring` agent
  may self-schedule a longer `next_check`.
- **Blocked→escalate reuses the existing tier policy** (autopilot auto-flows low/medium;
  publish/merge/deploy/credentials/confirm always hard-stop).
- **Budgets escalate, never auto-terminate** ("still running after N — close?"). Per-wake
  timeout = `step_timeout_s`.
- **Close mid-wake kills the pane** (like interactive delete) and retains per-session state.
- **Multi-per-folder day-1 for claude** (session-id works); **codex gated to one-per-folder**
  until `--json` id-capture is proven (else `resume --last` is ambiguous in a shared folder).
- **Wake-prompt is harness-owned** (a fixed template + the session goal).

## Top risks

- ~~Resume-by-id for detached non-interactive CLIs unproven~~ → **claude proven in S0**; codex
  resume-by-id subcommand confirmed, id-capture still to prove at S4.
- **codex has no caller-chosen id** → must capture from `--json`; failure makes codex + multi-
  per-folder ambiguous (hence codex gated to one-per-folder until proven).
- Relaxing root-dedup + per-session lease are load-bearing; a bug re-introduces two drivers in
  one tree.
- Two sessions in one folder **edit the same working tree** concurrently (git races) — may need
  a git worktree per session, or make it the human's responsibility.
- **Cadence vs token cost**; the resumed conversation grows unboundedly across wakes (context
  exhaustion, depends on CLI auto-compaction).
- Unbounded session lifetime (human-only done) → needs the escalating budgets.
- The agent's **Slack MCP may not load in headless `-p`/`exec`** — verify at S4.

## Addendum (2026-08-13) — "tier is the dial" mode model (user-confirmed)

The user clarified the intended product semantics, which supersede the earlier
mode framing. Decisions (confirmed via two explicit choices):

- **There is ONE self-driving engine — this heartbeat loop.** What the user calls
  "Interactive" and "Autonomous" are NOT two engines; they are the SAME loop at
  different **tiers**. The tier is the "how much does it check with me?" dial.
  - **Interactive** = loop at **Standard** tier (default): auto-resolves routine
    work, surfaces every important decision (ambiguity / publish / merge /
    confirm-done). This is the `/project-manager` experience — collaborative,
    human-in-the-loop, resumes the same conversation, reaches the human via its
    own Slack MCP + escalated stops answered in pmtui (`a`).
  - **Autonomous** = loop at **Autopilot** tier (default): auto-flows almost
    everything, only hard-stops for irreversible actions (publish/merge/
    credentials) or a genuine dead-end; otherwise runs until the human intercepts.
  - The tier stays **user-adjustable** in the create form (e.g. Interactive →
    Guardian to check in more), so it is a spectrum, not two fixed points. The
    kind choice just sets the default tier.
- **The raw hand-driven terminal (old `Mode::Interactive`, attach-and-type) is
  RETIRED from the create form.** The word "Interactive" now means the
  collaborative loop. (User chose "retire it" over keeping a separate "Terminal"
  kind.)
- **The native phase-machine (`Mode::Auto`, M5 slices 1–4a) is displaced from the
  "Autonomous" slot** (that slot is now the loop @ autopilot). DEFAULT: shelve it
  from the create form but keep the code intact (dormant, revivable later as a
  separate "Project" mode). Not removed. (Coordinator default, pending any user
  override.)

**Implication for implementation:** both create-form kinds seed `Mode::AgentLoop`
sessions (the S4 seeding path) differing only in the seeded `config.autonomy`
tier; the tier→escalate/auto-flow wiring already exists (`policy::decide_kind`,
consumed only by the driven schedulers). The work is a pmtui create-form +
labeling refactor (drop the raw-terminal creation path; two kinds → loop with
tier defaults; dashboard/preview labels read naturally for a loop session), NOT
new scheduler machinery. The S2 "one-tree invariant" simplifies: form-created
sessions are all agent-loop, so multiple-per-folder is the norm.

### Follow-up (2026-08-13, user) — collapse "kind" and "tier" into ONE dial

Shipping S6 left the create form with BOTH a "kind" (Interactive/Autonomous) and
a "tier" (Guardian/Standard/Autopilot). The user found this confusing: they are
the SAME axis. The kind was only a preset that set the tier's default and carried
no other behavior (both kinds seed an identical `Mode::AgentLoop` session), so
"Autonomous + Standard" was a contradictory/redundant combination.

DECISION (user-confirmed): **drop the "kind" entirely; the create form has a
single "Autonomy" dial.** "Interactive" and "Autonomous" were just stops on it;
they are no longer separate controls. Every created session is `Mode::AgentLoop`;
the dial IS `config.autonomy`. Implemented in S7 (pmtui-only: `ProjectKind`
removed, `submit_create` a single path seeding `Mode::AgentLoop @ form.tier`).

**Follow-up (S8, user) — remove Guardian.** The dial shipped with three levels
(Guardian/Standard/Autopilot), but `policy::decide` treats Guardian and Standard
IDENTICALLY (only Autopilot special-cases Medium risk), so Guardian was dead
weight. `Tier::Guardian` removed; the dial is now two levels: **Standard**
(collaborative — auto-resolves routine work, surfaces important decisions; the
default) and **Autopilot** (hands-off — auto-resolves nearly everything, runs
until you intercept). Legacy `config.json` with `autonomy: guardian` still loads
(graceful `#[serde(alias)]` → Standard, behaviorally identical).

## Addendum (2026-08-14) — on-demand interactive chat; the poll STAYS (user-confirmed + spiked)

**Supersedes** the "persistent always-there session" pivot that was recorded only
in the ledger (`.project-state/CURRENT.md`) on 2026-08-14. That pivot reversed the
"rejected detached long-runner" decision on the belief that the ephemeral per-wake
poll was the thing the user called "unusable." **It was not.** The user clarified:

> "oh, i don't want to kill the poll. The poll make sense, i complain unusable
> because the app i can not use to chat like normal claude or codex when i need to."

So the ephemeral per-wake poll model (§ heartbeat / `JobScheduler`) is **correct and
stays unchanged**. The real gap is a *missing capability*, not a wrong shape: the
human had no way to **jump into a normal `claude`/`codex` chat with the session's
conversation on demand.** pmtui's Enter only offered a *read-only watch* of a live
headless wake — never a REPL you can type into.

### The capability we add

- **Poll unchanged.** Ephemeral headless wakes on the cadence keep doing the
  autonomous work exactly as today.
- **On-demand chat.** In pmtui, Enter on a *parked* agent-loop session (no wake
  running) that already has a `conversation_id` launches a **real interactive
  REPL on the same conversation**: `claude --resume <conversation_id>` /
  `codex resume <id>` (interactive — NO `-p` / NO `exec`), run in the foreground
  with the SAME terminal suspend/restore pmtui already uses for `$EDITOR`
  (`edit_brief`) and `attach`. The human chats like normal claude/codex; on exit
  the poll resumes. Because both the poll's headless wakes and the human's chat
  resume the **same conversation id**, context is one continuous thread — the
  human's guidance flows into the next wake and vice-versa.
- **Interlock (mutual exclusion on the shared conversation id).** The poll must
  NOT spawn a headless wake while the human is mid-chat, or two processes resume
  the same conversation at once (double-resume). Mechanism: a per-session **chat
  marker** at `sessions/<seg>/.daemon/chat.json` = `{ pid, since }`. pmtui writes
  it immediately before launching the interactive REPL and clears it on exit
  (every path). `JobScheduler::tick` checks it before spawning (the `Idle` and
  due-`Monitoring` branches): a live marker **defers** the wake (stay parked);
  the session shows "paused for chat." **Self-healing:** a marker whose `pid` is
  no longer alive (`/proc/<pid>` gone) or whose `since` is older than a generous
  staleness cap is treated as released and unlinked, so a pmtui crash can never
  wedge the poll off forever.
- **TOCTOU.** pmtui writes the marker THEN re-reads the ledger `run`; if a wake
  started in the same sweep it aborts the chat ("a wake just started — try
  again"). The daemon checks the marker before spawning. The residual sub-ms
  cross-process window is benign — a second resume makes claude report the
  session busy and the wake backs off; it does not corrupt state. (Hardening to a
  true lease handshake is a noted follow-up, not v1.)
- **Availability.** `conversation_id` is set on the FIRST wake (claude mints +
  pins a uuid in `spawn`; codex is captured from the report/log). Chat is
  therefore offered once `conversation_id.is_some()`; before that pmtui says
  "waiting for first wake — the conversation is created on the first heartbeat."
- **Enter is context-sensitive:** a live wake → **watch** (existing read-only
  view of the autonomous work in flight); parked + `conversation_id` → **chat**
  (new); parked + no id yet → the waiting-for-first-wake status.

### Spike evidence (2026-08-14, live, real CLIs)

Both engines confirm interactive resume-by-id is a first-class feature, NOT gated
to `-p`/`exec`: `claude -r/--resume [value]` "Resume a conversation by session
ID"; `codex resume [SESSION_ID]` "Resume a previous **interactive** session".
End-to-end round-trip proven: (1) `claude -p --session-id <uuid> --permission-mode
auto -- "remember BANANABREAD"` (mirrors the poll's first wake) → persisted; (2)
`claude -p --resume <uuid> -- "what's the word?"` → `BANANABREAD` (the poll's
resume mechanism); (3) `claude --resume <uuid>` (interactive, in a tmux pane)
launched a live REPL that displayed **the full prior headless conversation** (both
the create turn and the resume turn) and sat at an interactive `❯` prompt in auto
mode — exactly the "attach and chat on the conversation the poll drives" design.

### Slices

- **CHAT-A — paths + daemon interlock (core, unit-tested over `FakeDriver`).**
  `ProjectPaths::chat_lock()`; chat-marker helpers (`mark`/`clear`/`is_active`
  with `/proc` pid-liveness + staleness self-heal); wire `is_active` into
  `JobScheduler::tick` so a live marker defers the spawn (stays `Monitoring`).
- **CHAT-B — pmtui interactive chat launch.** Extend `request_attach` (AgentLoop:
  live→watch, parked+id→chat, parked+no-id→waiting); `build_chat` argv for both
  engines; `pending_chat` + a `chat()` fn mirroring `edit_brief`'s foreground
  suspend/restore that writes the marker → runs the REPL → clears the marker
  (always). Pure pieces unit-tested (argv, routing decision); the tty-owning fn
  is not, like `attach`/`edit_brief`.
- **CHAT-C (optional polish) — dashboard "paused for chat" indicator + keybar
  copy** so the human sees why the poll is idle while they're attached.
