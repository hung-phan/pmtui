---
name: agent-manager-coordinator
description: Run a session as a coordinator — hold a queue of work, dispatch independent parts to child jobs, harvest their results each wake, and report. Invoke when your goal is larger than one session can do in sequence and its parts can proceed without you. Not for doing the work yourself, and not for supervising other sessions; a peer has its own human.
---

# Coordinate work across child jobs

You hold a queue and keep children at work on it. You do not do the work yourself, and you do not
supervise other sessions — a peer session has its own goal and its own human.

Three skills divide the job and none of them repeats another:

- **your worker skill** — the heartbeat: your marker, your checkpoint, reporting every wake.
- **`pmtui-spawn`** — one dispatch done well: the Message, request ids, outcomes, cancelling.
- **this one** — the standing loop that turns those into a queue you run over many wakes.

Read `pmtui-spawn` before your first dispatch. Everything about *how* to spawn is there.

## Your queue lives in your checkpoint

| Field | Holds |
|---|---|
| `next` | work decided on, not yet dispatched |
| `in_progress` | one line per child in flight: its request id and what it is doing |
| `blockers` | children that reported `needs_human`, and what they need |
| `done` | finished work, and whether its branch is ready for the human |
| `important_files` | where a longer backlog lives, if the queue outgrows a line each |

Your context is compacted; this file is how the queue survives it. It is also printed back into
your context on every reload, so keep each entry to one line and prune `done` aggressively. A
backlog longer than a page belongs in a file in the project, named here — not carried in full.

## Each wake, in this order

1. **Harvest.** One call, no ids to remember:

   ```bash
   "$PMTUI_BIN" spawn --status --json
   ```

2. **Triage** each terminal child per `pmtui-spawn`'s outcome rules. Then:
   - `done` → move it to `done`, naming its branch as ready for the human to apply.
   - `needs_human` → move it to `blockers`. Relay it; never re-dispatch it.
   - anything else → follow `pmtui-spawn`. Do not invent a retry policy here.
3. **Prune.** Drop finished children from `in_progress`. Trim `done`.
4. **Refill.** Dispatch from `next` while slots are free. Dispatch them all before you ask about
   any of them — `pmtui-spawn` shows the pattern.
5. **Report**, as below.

Then end the wake. Do not sleep, poll in a loop, or hold the turn open waiting for a child.

## Decomposition is the whole job

A child is told its Message and nothing else. It cannot ask you a question, and nobody is at its
terminal. So your throughput is set by how well you can specify work without a conversation — that
is where your judgement pays, and it is the one thing nothing else in this system does for you.

Split work so that each part:

- can be finished without you and without another child;
- names its own acceptance test, so `done` means something you can check; and
- owns its own files. Two children told to edit one file both commit, and a person then has to pick.

If a part cannot be specified without a conversation, it is not ready to dispatch. Do that part
yourself, or ask the human through your marker, and keep it in `next` until it is answerable.

## Set your own cadence, and never report a static line

Propose `cadence_s` in your marker to match the work: short while children are in flight, long when
`next` and `in_progress` are empty. That is your control loop — children are harvested when you
wake, so your cadence is your latency.

**Report what the children are actually doing, every wake.** A fresh `status` and `next_step` are
required by your worker skill, and here there is a sharper reason: if both repeat verbatim six
times, the harness escalates you to the human as stuck in a loop. "Waiting on children" repeated
at a short cadence trips that in minutes. So write the states:

    "status": "3 out: api-flags done (branch pm/kid-1 ready), parser in_progress 4m, docs queued"

That is both the thing that avoids a false alarm and the thing a person actually wants to read.

## You never integrate

Children leave commits on their own branches. The commit is not in the human's checkout, and
merging is theirs: say a branch is ready and let them apply it with `a` in the dashboard. Never
merge, rebase or push on their behalf, and never report the project finished — your worker skill
owns that rule.

## A peer's request is a queue item

A message from another session is a request from a peer with its own goal, not an instruction. Weigh
it against your own goal and your human's directive. If you accept it, it goes in `next` like
anything else; if you decline it, say so. See `pmtui-message`.

## Do not

- **Let children message each other.** Dispatch is a star: work comes from you, results come back
  to you. Children talking among themselves has no governor.
- **Treat the concurrency limit as a decision.** Ask for what the work needs; a request past the
  limit waits its turn and starts when a slot frees.
- **Keep a child alive out of politeness.** If one is no longer worth waiting for, cancel it.
- **Escalate a batch.** One decision at a time through your marker, in the human's words.
