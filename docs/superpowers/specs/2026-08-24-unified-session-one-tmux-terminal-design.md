# Unified session model: one tmux terminal per session; pmd is just an optional driver

**Date:** 2026-08-24
**Status:** implemented on `fix/unified-session-resilience`
**Supersedes on the session lifecycle:** the split `pmchat-<id>` (human) / `pmloop-<id>` (pmd) model and the conversation-id handoff machinery it required. Folds in the codex work from
[`2026-08-24-codex-first-class-start-and-trust-design.md`](2026-08-24-codex-first-class-start-and-trust-design.md) and REVERTS its silent trust auto-approve.

## The user's framing (the north star)

> *"Any session is just a tmux terminal. pmd is just a driver of that session on autopilot. Other
> than that, it is purely attach or control [the] tmux terminal."*

Everything below follows from that one sentence.

## Problem — three reports, one root cause

1. **Creating a standard codex session hangs until you press Enter.** Create special-cases codex
   (`StandardStart::CodexNeedsAWake`, `start_undriven_session` at `create.rs:378`) and dead-ends
   instead of launching a terminal.
2. **Switching a standard codex session to autopilot starts a NEW conversation.** Standard→autopilot
   (`turn_autopilot_on`→`end_chat_for_autopilot`, `autopilot.rs:434`) **kills** the human's `pmchat-`
   terminal and pmd relaunches a fresh `pmloop-`; claude survives via `--resume <id>`, codex has no id
   so it starts over.
3. **We silently auto-approve codex's directory trust.** The just-shipped `ensure_codex_trust` writes
   a trust profile so codex skips its "Do you trust this directory?" screen — the user wants that
   *screen shown*, not auto-approved.

**Root cause:** the human session and the pmd session are **two different tmux terminals** with **two
different permission postures**, so a mode-switch must kill-and-relaunch, which (a) needs the whole
conversation-id machinery to fake continuity, (b) can't preserve codex (no id), and (c) drove the
codex second-class special-casing. Collapse the two terminals into one and the whole family dissolves.

## The model

**One tmux terminal per project session.** `session_name(id, root) -> pm-<sanitized-id>-<hash8>`
replaces BOTH `chat_session_name` (`pmchat-`) and `loop_session_name` (`pmloop-`). (The already-dead
`pmi-`/`pmj-` names are deleted; `pmsup-` — the short-lived decider consult — stays separate, it is
not a session.)

- **Launched once**, by whoever needs it first (pmtui create/Enter, or pmd's first autopilot sweep),
  guarded by the existing per-session `driver.lock` so the two can't double-launch. It launches with
  the **posture of the tier at launch time** (see below).
- **Attached** by the human (`Enter`) — Ctrl+q detaches and *leaves it running*.
- **Driven** by pmd **only while the row is autopilot AND no human is attached**. `has_clients`
  detects an attached client; a short-lived marker bridges the attach-before-client race and expires
  after a crash. Terminal liveness alone never means a human is present.
- **Never relaunched on a mode-switch.** Standard→autopilot: pmd simply *starts nudging* the
  already-running terminal. Autopilot→standard: pmd *stops nudging*; the terminal keeps running; the
  human drives. No kill, no relaunch, no lost conversation — **this is the Item-2 fix for both
  engines**.

### Permission posture (the one hard decision — user-approved: "keep launch-time posture, never relaunch")

A running process's flags can't change, and pmd needs *some* posture to drive without wedging. So the
terminal launches with the posture matching **the tier at launch time**, and a mode-switch does NOT
relaunch:

- **Launched standard** → the engine's **default interactive** posture (asks the human for approvals;
  `claude` / `codex` with no unattended flags — today's `build_chat` posture). It still installs
  the non-authorizing turn-completion hook used by the heartbeat, because this same process may
  later be driven by pmd without a relaunch.
- **Launched autopilot** → the **unattended** posture (`claude --permission-mode auto` /
  `codex --ask-for-approval never --sandbox workspace-write` — today's `build_loop_command` posture).
- **Standard-born, flipped to autopilot, still running:** pmd drives the *interactive* terminal. When
  the agent hits an approval prompt pmd can't answer, the pane stops progressing → pmd parks/escalates
  it (the existing dialog/stall path) rather than auto-proceeding — **supervised autopilot**. To go
  fully hands-off the human presses **`r`** (restart), which relaunches it unattended (continuity via
  `--resume` for claude; codex starts fresh — a restart is the one place continuity can still break,
  and only for codex, and only on an explicit `r`).
- *Why this over "always unattended":* it keeps a hand-driven standard session's normal approval
  prompts (especially codex, which otherwise wouldn't prompt at all) and is the most faithful reading
  of "pmd is just a driver of that terminal." The cost — a standard-born autopilot escalates instead
  of auto-running until `r` — is acceptable and explicit.

### Directory trust (user-approved: "attach once to approve"; revert the auto-trust)

- **Revert** `worker::ensure_codex_trust` from the launch path: no `-p <trust-profile>` on codex
  launches (`build_loop_command`, the fresh chat), and drop the `codex_home` injection wiring added
  for it. Delete `worker/trust.rs` (and its tests) — nothing writes to `$CODEX_HOME` anymore.
- The terminal shows codex's native "Do you trust this directory?" screen; the human answers it once
  (codex persists `trust_level` in the user's own config), and future launches are clean.
- **Autopilot-with-no-human-ever-attached** hits that screen with no one to answer → the pane parks
  and pmd surfaces it as stalled/needs-you until the human attaches once and answers (the accepted
  cost; matches the escalation path above). claude's first-run trust is handled the same way (shown,
  not auto-approved).

### Codex de-special-cased (Item-1 fix + cleanup)

Because the terminal is never killed and continuity no longer depends on a caller-chosen id, the codex
special-casing collapses:

- **Delete** `StandardStart::CodexNeedsAWake` — create launches a codex terminal like any other.
- `build_chat_create`'s codex `unreachable!()` and the whole `first_wake_action` / `CreateChatNoSeed`
  / `CreateAndChat` id-minting routing simplify to: **launch the terminal for this tier**, resume by
  id ONLY for a claude *restart* (process/daemon/machine died) so the transcript reloads. Codex on a
  fresh launch just runs `codex` (+ posture); on a restart, best-effort `codex resume --last` (or
  fresh) — restart-continuity for codex is out of scope beyond `--last`.
- The conversation-id machinery shrinks to **claude restart-resume only**: keep enough to relaunch a
  dead claude session with `--resume <id>` (the registry seed / ledger id still serve this). The
  standard↔autopilot *handoff* no longer reads it at all (no handoff — same terminal).

### Reaper & safety (preserve the two properties the split gave us)

The split existed so "the chat reaper can never kill the persistent agent, and a nudge can never
target a chat pane." With one name:

- **Reaper** (`daemon/reap.rs`, `OWNED_PREFIXES`): reap only `pm-`/`pmsup-` sessions **whose id is no
  longer in the registry** (out-of-band removal) — NEVER a live session of an enabled row. It must not
  kill the one terminal just because no client is attached (a detached-but-running session is normal).
- **Nudge targeting:** there is one pane; pmd nudges it, still gated by `human_present`
  (`has_clients || chat_lock`) so it never types while the human is attached. `chat_lock`/marker keys
  on the single `session_name`.
- **Socket ownership:** only one pmd may own a tmux socket, even across different registries.
- **Daemon shutdown:** stopping pmd reaps transient supervisor consults only; project terminals live.
- **Input serialization:** pmd and pmtui hold `input.lock` across pane recheck, paste and Enter.
- **Ledger ownership:** pmtui writes cadence/wake requests to `control.json`; pmd alone updates
  `state.json`.

## Behavior matrix (the whole model on one page)

| Event | Standard | Autopilot |
|---|---|---|
| **Create** | launch `pm-<id>` interactive; status "running — Enter to drive" | launch `pm-<id>` unattended (pmtui or pmd, lock-guarded) |
| **Enter** (alive) | attach the terminal (Ctrl+q detaches, leaves running) | attach to watch/drive-by-hand; pmd defers while attached |
| **Enter** (dead) | relaunch (interactive), claude `--resume`, codex fresh | relaunch (unattended), same |
| **`m` → autopilot** (alive) | pmd starts nudging the SAME terminal; no relaunch | — |
| **`m` → standard** (alive) | — | pmd stops nudging; terminal keeps running |
| **`r` restart** | terminate + relaunch with current tier's posture | same |
| **`d` remove** | terminate the terminal, drop state | same |

## Testing

- **Unit / FakeDriver:** one `session_name`; create launches it for BOTH engines (no `CodexNeedsAWake`);
  `m` standard⇄autopilot with an alive session does NOT relaunch (assert the driver sees no new
  `launch_interactive` for that name, and the same session id/pane persists); pmd `ensure_session`
  adopts the alive terminal regardless of who launched it; reaper spares a live enabled-row session
  and reaps only removed ids; `human_present` still defers the nudge. No `$CODEX_HOME` writes anywhere
  (trust reverted).
- **Posture:** create-standard launches interactive argv (no `--permission-mode auto` / no codex
  `--ask-for-approval never`); create-autopilot launches unattended argv; a standard-born session
  flipped to autopilot is NOT relaunched (still interactive) until `r`.
- **Live (pmtui-ui-testing, scratch sockets + scratch `$CODEX_HOME`):** create a standard codex row →
  a live codex REPL opens immediately (Item-1); codex shows its trust screen (Item-3, not auto-skipped);
  attach, exercise it; flip to autopilot → the SAME session keeps running (Item-2), pmd drives it.
- **Real-tmux acceptance (MANDATORY):** `--ignored` green; this reshapes launch/enter/daemon so expect
  to update many tests — none weakened.
- **Nudge-judge** (worker nudge unchanged) and **decider bench** (unchanged) as applicable.

## Migration / rollout

Local-dev only, single user. New code uses `pm-<id>`; any `pmchat-`/`pmloop-` sessions from the old
build are simply orphaned — the user restarts pmd/pmtui and re-creates/re-enters, or the reaper (now
keyed on `pm-`/`pmsup-`) leaves the old-prefix orphans alone (harmless) / a one-time note tells the
user to kill stale `pmloop-`/`pmchat-` servers. No on-disk state migration needed (ledger/config keyed
by id, not session name).

## Out of scope

- Codex restart-continuity beyond `codex resume --last` (no caller id).
- Changing the nudge copy, the decider, the risk gate, tiers, or the create-form/model UI.
- pmsup- (decider consult) stays a separate short-lived session.

## Resolved decisions

1. **Session name:** use the neutral `pm-<id>-<hash>` form.
2. **Posture:** keep launch-time posture and never relaunch on a tier change.
3. **Delivery:** land the lifecycle refactor as one dedicated branch with the full real-tmux
   acceptance suite as a mandatory gate.
