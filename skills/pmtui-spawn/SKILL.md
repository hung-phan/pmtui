---
name: pmtui-spawn
description: Dispatch one independent part of your task to a child job. A child job is one headless agent run that does the task, reports a result and exits. Invoke when another agent could do that part without you. Dispatch it with one complete Message, poll for its result, and act on the JSON.
---

# Spawn a child job

A child is a JOB. It is one agent run. It does the task, it reports a result, and it exits. It is
not a session you talk to. You cannot message it, and it cannot ask you anything.

1. Spawn only independent work that can proceed without you. Do the rest yourself.
2. Write the Message as a complete spec. Name the target. Name the change. Name the constraints.
   Name the ownership, which is the set of files the child may edit. Name the observable acceptance.
   The child is told the Message and nothing else.
3. Make one request id, and keep it. Every later check uses that same id. The cancel uses it too.

   ```bash
   id=$(cat /proc/sys/kernel/random/uuid 2>/dev/null || uuidgen | tr 'A-F' 'a-f')
   "$PMTUI_BIN" spawn --request-id "$id" --title "<short title>" --message "<Target / Change / Constraints / Ownership / Acceptance>" --wait 120 --json
   ```

   `--wait` sets how long to wait for the child's RESULT. The limit is 120 seconds. A short task
   returns its result inside this one call.

   TO SPAWN SEVERAL: dispatch them all first, then ask once. Each command waits for its own child.
   So one command after another puts independent children in series.

   ```bash
   for task in "<task 1>" "<task 2>" "<task 3>"; do
     id=$(cat /proc/sys/kernel/random/uuid 2>/dev/null || uuidgen | tr 'A-F' 'a-f')
     "$PMTUI_BIN" spawn --request-id "$id" --title "<short title>" --message "$task" --wait 0 --json &
   done
   wait
   ```

   `--wait 0` dispatches and returns at once. Read the outcome of all of them with step 5. In a git
   repository each child gets its own worktree and its own branch, so children cannot overwrite
   each other. Name the ownership even so. Two children sent to the same file both commit, and then
   a person must choose between them.

4. Parse the JSON on stdout even when the exit code is nonzero. `--wait` waited for the child's
   RESULT, so `state` is usually the answer:
   - `done`: report `result.summary` to the human. In a git repository the child worked in its own
     worktree. `work.branch` and `work.commit` name its commit, `work.touched` lists what it
     changed, and `work.uncommitted` reports changes it never committed. That commit is NOT in the
     human's checkout. Say the branch is ready, and let the human apply it with `a` in the
     dashboard.
   - `needs_human`: relay `result.summary` as the child's question. Do not re-dispatch it.
   - `ended_without_result`: the run crashed, or something killed it. Tell the human, and name
     `error.message`. A new attempt needs a new id.
   - `cancelled`: something stopped it on purpose. Do not retry it in silence.
   - `in_progress` or `queued`: the child still works. Go to step 5.
   - `needs_attention`: tell the human that the child needs a person at its terminal.
   - `failed`: act on `error.code`:
     - `request_conflict`: this id already names a request with other arguments. Rerun the
       command with the original arguments and the same `--request-id` to check on it. Never make
       a new id for the same task.
     - `request_unwritable`: rerun the same command, with the same `--request-id`, later.
     - `nested_spawn_refused`, `child_limit_reached` or `dir_not_allowed`: stop and ask the human.
     - `invalid_argument`, `dir_not_found`, `message_too_long` or `invalid_request`: nothing ran.
       Fix the argument, then retry with a new id.
     - `job_failed`: the child ran, and it reports that the task failed. Read `result.summary`,
       then decide. Any retry needs a new id.
     - Any other code: stop and tell the human the code and its message.
   - `outcome_unknown`: stop and tell the human. Never spawn a replacement.
5. Use this step only for `in_progress` or `queued`. The child still works, and you cannot know for
   how long. Do your own work. At your next pause, ask about every child in one call:

   ```bash
   "$PMTUI_BIN" spawn --status --json
   ```

   You get one line per child, with its state and what it reported, and no ids to remember. Read it
   until your child reaches one of the terminal states in step 4. The dispatch command also works
   again under the same `--request-id`, and it waits again. Never make a new id for the same task.
   No clock stops a child. Cancel one when you decide it is no longer worth the wait.
6. To stop a child you no longer want, cancel it by id:

   ```bash
   "$PMTUI_BIN" spawn --request-id "$id" --cancel --json
   ```

   The command reports `cancel_requested`. Keep checking as in step 5, until the receipt reads
   `cancelled`.
7. A job cannot spawn. Five children run at one time. Ask for more, and they wait their turn.
