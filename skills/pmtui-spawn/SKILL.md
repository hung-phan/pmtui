---
name: pmtui-spawn
description: Hand an independent part of your task to a child job — one headless agent run that does the task, reports a result and exits. Invoke when a separate agent could do that part without you, to dispatch it with one self-contained Message, poll its result, and act on the JSON.
---

# Spawn a child job

A child is a JOB: one agent run that does the task, reports a result and exits. It is not a session
you talk to. You cannot message it, and it cannot ask you anything.

1. Spawn only independent work that can proceed without you. Do the rest yourself.
2. Write the Message as a self-contained spec. Name the target, the change, the constraints, the
   ownership (what the child may edit), and the observable acceptance. It is the only thing the
   child is told.
3. Generate one request id and keep it. Every later check and the cancel use that same id.

   ```bash
   id=$(cat /proc/sys/kernel/random/uuid 2>/dev/null || uuidgen | tr 'A-F' 'a-f')
   "$PMTUI_BIN" spawn --request-id "$id" --title "<short title>" --message "<Target / Change / Constraints / Ownership / Acceptance>" --wait 120 --json
   ```

   `--wait` is how long to wait for the child's RESULT, up to 120 seconds. A short task comes back
   answered in this one call.

   SPAWNING SEVERAL: dispatch them all first, then ask once. Each command waits, so running them one
   after another makes children that could have run together run in series.

   ```bash
   for task in "<task 1>" "<task 2>" "<task 3>"; do
     id=$(cat /proc/sys/kernel/random/uuid 2>/dev/null || uuidgen | tr 'A-F' 'a-f')
     "$PMTUI_BIN" spawn --request-id "$id" --title "<short title>" --message "$task" --wait 0 --json &
   done
   wait
   ```

   `--wait 0` dispatches without waiting. Read what became of all of them with step 5. In a git
   repository each child gets its own worktree and branch, so children do not overwrite each other;
   name the ownership anyway, since two children solving the same file both commit and a person then
   has to pick.

4. Parse the JSON on stdout even when the exit code is nonzero. `--wait` already waited for the
   child's RESULT, so `state` is usually the answer:
   - `done`: report `result.summary` to the human. In a git repository the child worked in its own
     worktree: `work.branch` and `work.commit` say where its commit is, `work.touched` what it
     changed, and `work.uncommitted` that it left changes it never committed. The commit is NOT in
     the human's checkout — say it is ready and let them apply it with `a` in the dashboard.
   - `needs_human`: relay `result.summary` as the child's question. Do not re-dispatch it.
   - `ended_without_result`: the run crashed or was killed. Tell the human, naming `error.message`.
     A fresh attempt needs a new id.
   - `cancelled`: it was stopped deliberately. Do not retry it silently.
   - `in_progress` or `queued`: it is still working. Go to step 5.
   - `needs_attention`: tell the human the child needs them at its terminal.
   - `failed`: act on `error.code`:
     - `request_conflict`: this id already names a request with other arguments. Rerun the
       command with the original arguments and the same `--request-id` to check on it. Never make
       a new id for the same task.
     - `request_unwritable`: rerun the same command, with the same `--request-id`, later.
     - `nested_spawn_refused`, `child_limit_reached` or `dir_not_allowed`: stop and ask the human.
     - `invalid_argument`, `dir_not_found`, `message_too_long` or `invalid_request`: nothing ran.
       Fix the argument, then retry with a new id.
     - `job_failed`: the child ran and reported that the task failed. Read `result.summary`, then
       decide. Any retry needs a new id.
     - Any other code: stop and tell the human the code and its message.
   - `outcome_unknown`: stop and tell the human. Never spawn a replacement.
5. Only for `in_progress` or `queued`: the child is still working, and you cannot know for how long.
   Do your own work. When you next come up for air, ask about every child in one call:

   ```bash
   "$PMTUI_BIN" spawn --status --json
   ```

   One line per child — its state and what it reported — with no ids to remember. Read it until the
   child you care about is one of the terminal states in step 4. Rerunning the dispatch command with
   the same `--request-id` also works and waits again; never make a new id for the same task.
   Nothing stops a child on a clock: if you decide one is no longer worth waiting for, cancel it.
6. To stop a child you no longer want, cancel it by id:

   ```bash
   "$PMTUI_BIN" spawn --request-id "$id" --cancel --json
   ```

   That reports `cancel_requested`; keep checking as in step 5 until the receipt is `cancelled`.
7. A job cannot spawn. Five children run at once; ask for more and they wait their turn.
