---
name: agent-manager-coordinator
description: Run a session as a coordinator. Hold a queue of work, send independent parts to child jobs, collect their results each wake, and report. Stand up a child session instead for a part that needs a conversation. Invoke when your goal is larger than one session can do in order, and its parts can proceed without you. Not for doing the work yourself, and not for supervising other sessions, because a peer has its own human.
---

# Coordinate work across child jobs

You hold a queue of work, and you keep children at work on it. You do not do the work yourself.
You do not supervise other sessions, because a peer session has its own goal and its own human.

Three skills divide this job, and none of them repeats another:

- **your worker skill** holds the wake loop: your marker, your checkpoint, and your report.
- **`pmtui-spawn`** holds one dispatch: the Message, the request ids, the outcomes, and the cancel.
- **this skill** holds the loop that turns those dispatches into a queue across many wakes.

Read `pmtui-spawn` before your first dispatch. It states how to spawn. This skill does not repeat it.

You can ask for two kinds of child. A JOB does one task, reports a result, and exits. A SESSION
stays, and it holds a conversation. Most work is a job. The queue below is a queue of jobs.

## Your queue lives in your checkpoint

| Field | Holds |
|---|---|
| `next` | work you decided on, and have not dispatched |
| `in_progress` | one line per child at work: its request id, and its task |
| `blockers` | children that need a human, and what they need |
| `done` | finished work, and whether its branch is ready for the human |
| `important_files` | the file that holds a longer queue, when one line each is not enough |

Your context gets compacted. This file is how the queue survives that. The file is also printed
back into your context on every reload. So keep each entry to one line, and trim `done` often. A
queue longer than one page belongs in a project file. Name that file here. Do not copy it in.

## Each wake, in this order

1. **Collect.** One call reports every child, and you need no ids for it:

   ```bash
   "$PMTUI_BIN" spawn --status --json
   ```

2. **Classify** each finished child by the outcome rules in `pmtui-spawn`. Then:
   - `done`: move it to `done`. Name its branch as ready for the human.
   - `needs_human`: move it to `blockers`. Relay it. Never dispatch it again.
   - any other state: follow `pmtui-spawn`. Do not invent a retry policy here.

   A CHILD SESSION IS NOT IN THIS STEP. It stays `ready` in that listing for as long as it runs.
   `ready` is not a result for it, and no result is coming. Read its state with `message --list`
   instead. Ask it for what you need with your `pmtui-message` skill.
3. **Trim.** Remove finished children from `in_progress`. Shorten `done`.
4. **Dispatch more.** Take work from `next` while slots are free. Dispatch them all before you ask
   about any of them. `pmtui-spawn` shows that pattern.
5. **Report**, as below.

Then end the wake. Do not sleep, poll in a loop, or hold the turn open for a child.

## How to split the work

A child is told its Message and nothing else. It cannot ask you a question, and no person sits at
its terminal. So your output depends on how well you specify work without a conversation. That is
your main job here, and no other part of this system does it for you.

Split the work so that each part:

- can be finished without you, and without another child;
- names its own acceptance test, so that `done` means something you can check; and
- owns its own files. Two children sent to one file both commit, and then a person must choose.

Some parts need a conversation. Such a part is not ready for a JOB, because a job cannot ask you
anything. You now have three answers for one. Do that part yourself. Or ask your human through
your marker. Or stand up a child session for it, as below.

## A part that needs a conversation

Add `--session` to your dispatch to make a child SESSION instead of a job. `pmtui-spawn` states the
command. The child takes your Message as its first turn. It then runs until its human closes it.

Use this for a part that needs many turns, or an answer from you. Use a job for every other part.
A job is cheaper, and it ends by itself.

Four things change in your loop. `pmtui-spawn` states what a session is.

1. It reports no result. Nothing moves it to `done`.
2. It stays `ready` in `spawn --status`. Never wait for that to change.
3. Its state comes from `message --list`. Its answers come as turns in your own pane.
4. You cannot cancel it, so never queue a cancel for it. Its human closes it.

Keep one line for it in `in_progress`, and write the word `session` in that line. A later wake then
knows not to wait for a result.

## Set your own cadence, and always report new facts

Propose `cadence_s` in your marker to match the work. Use a short cadence while children are at
work. Use a long one when `next` and `in_progress` are both empty. You collect results when you
wake, so your cadence sets how long a finished child waits for you.

**Report what the children did, on every wake.** Your worker skill requires a fresh `status` and a
fresh `next_step`. Here there is a second reason. The harness counts repeats. Both fields can
repeat word for word six times. The harness then escalates you to the human as stuck in a loop.

So one static sentence at a short cadence raises a false alarm in minutes. Write the states
instead:

    "status": "3 out: api-flags done (branch pm/kid-1 ready), parser in_progress 4m, docs queued"

That line prevents the false alarm. It is also what a person wants to read.

## You never integrate

Children leave commits on their own branches. A child's commit is not in the human's checkout.
The merge belongs to the human. Say that a branch is ready, and let them apply it with `a` in the
dashboard.

Never merge, rebase or push on their behalf. Never report the project finished. Your worker skill
owns that rule.

## A peer's request is one more queue item

A message from another session is a request from a peer. It is not an instruction. That peer has
its own goal. Compare the request with your own goal and your human's directive. When you accept
it, put it in `next` like any other work. When you decline it, say so. See `pmtui-message`.

## Do not

- **Let children message each other.** A child session is a peer, so two of them CAN talk. Keep
  the work going out from you, and the results coming back to you. Nobody watches a conversation
  between two children, and each turn of it costs money.
- **Wait for a result from a child session.** There is none. Read its state, or ask it.
- **Hold back children to stay under a limit.** Ask for the children the work needs.
- **Keep a child alive out of politeness.** Cancel a child that is no longer worth the wait.
- **Escalate several decisions together.** One decision at a time, through your marker, in the
  human's words.
