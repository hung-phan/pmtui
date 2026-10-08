# Conversation Fork Design

## Problem

A user often wants to explore a different direction without losing the current
conversation. Starting a blank session throws away useful context; resuming the
same conversation mutates the original history. Claude and Codex both expose a
real fork operation, but pmtui has no safe, visible way to use it.

Forking must not introduce a second work-item concept. In pmtui, a session is the
thing a human chats with, watches, pauses, puts on Autopilot, and removes. A fork
is therefore another normal session in the same project, not a task attached to
a session.

## Product Contract

- `f` forks the selected session. The keybar exposes the same action as a
  clickable `Fork` chip when an agent-loop row is selected.
- The source must have a known conversation identity. A never-started session is
  not forkable and the refusal tells the user to send its first message. That
  includes a Claude session whose minted id has no transcript on disk yet.
- If the source terminal is live, it must be idle and unattached. Idle means two
  Idle captures one dashboard tick apart with an unchanged transcript and, for a
  Claude source on Autopilot, no outstanding hook-reported turn, the same merge
  the dashboard row applies. The hook turn is counted from pmd's last nudge, so a
  Standard source relies on the two captures alone; a silent Claude tool call
  that leaves the prompt unchanged across both is not detected there. Fork never
  snapshots a response that changes between the captures and never races a human
  or pmd input delivery.
- The child receives a unique `<source>-fork` session name, with the normal
  numeric suffix if needed. It uses the same project directory, worker engine,
  worker model, goal, directive, and decider configuration.
- The child always starts on Standard. Fork creates context, not autonomous
  authority; the human may turn on Autopilot with the existing `m` action.
- The fork starts immediately in its own persistent `pm-...` terminal and becomes
  the selected row. `Enter`, `s`, pause, restart, remove, and Autopilot then work
  exactly as they do for every other session.
- The source registry entry, terminal, durable session content, and conversation
  history are never modified. Fork may create the ordinary empty `input.lock`
  artifact used by every pane-input critical section.
- Fork does not create or switch git branches or worktrees. Both conversations
  see the same project directory, matching the engines' native fork behavior.

## Engine Mapping

Claude launches the equivalent of:

```text
claude --resume <source-id> --fork-session
```

An injected `SessionStart` hook atomically writes Claude's actual child session
ID to the destination session state. The row is not enabled until pmtui reads a
valid UUID that differs from the source.

Codex launches the equivalent of:

```text
codex fork <source-id>
```

Codex chooses the child thread ID. pmtui reuses its exact live-process rollout
probe to capture that ID and seed the child registry entry. pmtui never guesses
from the newest rollout or project directory alone. While `codex fork` starts it
holds the source rollout open to copy its history, so a probe that still sees the
source ID keeps polling until the bounded wait ends.

A live Codex source is identified with the same exact probe, under the source
input lock. A Standard Codex session records no ID when pmtui starts it, so the
live ID is what makes it forkable. A saved ID that disagrees with the live
conversation refuses, as restart does. A probe that proves no conversation falls
back to the saved ID.

## Ordering And Failure Rules

1. Re-read the source registry entry and session-owned state.
2. Acquire the source input lock, then recheck any live pane is unattached and
   idle.
3. Reserve the child session ID and append a disabled destination row so pmd
   cannot drive it during setup.
4. Seed the child's human-owned files.
5. Acquire the child driver lock and launch the native fork command.
6. Capture and validate the actual child identity, allowing normal CLI startup
   latency within a bounded wait. A child whose session ends or whose pane dies
   stops the wait at once. The source input lock stays held until this step
   ends, because the child's identity is the earliest evidence that the engine
   has read the source history.
7. Atomically store the child identity and enable the row.
8. Select the child and report that it is ready.

A failure after step 3 stops the child terminal, then removes the staged row and
the state directory this fork reserved, so no half-created row looks like a
paused session. If the terminal cannot be stopped, the disabled row is kept so
the live process stays visible. Enter/resume, restart, Autopilot, fork, and
Message refuse any fork row with no captured conversation and ask for it to be
deleted, rather than starting a blank conversation under the fork label or typing
into an unverified child.

No failure path terminates, pauses, retimes, or writes into the source session.

## Board Integration

The Board is a second view over these same sessions, not a parallel task
database. Its Paused, Needs You, Pending, Autopilot, and Working lanes derive
from live session and mode state (`docs/SPEC.md` section 3). Creating a card
creates a normal session; forking a card creates a normal forked session;
opening a card selects that session's ordinary workbench. This keeps the Board
useful for ad hoc work without forcing users to understand a task-versus-session
distinction.

## Acceptance

- Claude and Codex fork argv are exact and covered independently.
- Forking leaves the source entry, terminal, and durable content unchanged.
- The child is Standard, selected, running, and has copied goal/directive data.
- A busy, attached, missing-identity, corrupt-config, failed-launch, or failed-save
  path refuses without claiming success.
- A fresh Codex fork identity is captured from the exact child process and
  survives pmtui restart through the registry.
- Keyboard and keybar click invoke the same `f` handler.
- Real tmux acceptance proves both engine launch shapes with inert stubs and no
  real model calls, including a live idle source that stays running and
  unattached, a Standard Codex source with no saved ID, and a source still
  streaming when `f` is pressed, which the dashboard's own idle gate refuses.
- The under-lock recheck, which refuses a source that starts changing after the
  dashboard last saw it idle, is covered by unit tests over scripted pane
  captures and hook state, not by the real tmux suite.
