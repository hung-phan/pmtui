# Spawn Child Jobs Design (v2)

## Status

Proposed. A delta on `2026-09-29-pmtui-spawn-command-design.md` (v1, shipped): the
request/receipt protocol, the broker lease, staging, launch and readiness all stand.
What changes is what a child IS and what happens when its work ends.

The user asked for this after watching a child "answer the question" and then sit
there. They chose, on 2026-10-01: a child modelled on how Claude and Codex already do
multi-agent work ("the pmtui spawn uses the same idea of multi-agent, it just makes
visualization easier … adapt their approach to ours"), automatic retirement ("it is
auto retired and clean it up cleanly"), and no runtime bound in the dashboard ("don't
need to control timeout of child task, i think the parent should do that"). They then
asked for the design to be checked against how others solve this, including a local
clone of `asheshgoplani/agent-deck`. The `## Prior art` section is that check; it
changed five decisions, and two of its lessons are things that tool learned the hard
way and labelled with issue numbers.

## Problem

v1 provisions a child and stops. Every terminal receipt state is a LAUNCH outcome —
`ready`, `needs_attention`, `failed`, `outcome_unknown` — and `ready` means only
"a live terminal with no dialog, twice, 100 ms apart". Once a request is settled the
broker never opens that row again (`src/bin/pmtui/app/spawning.rs:294-298`).

Three consequences, all observed:

1. **The work's outcome has no route home.** The child is launched with
   `launch_interactive` (`app/spawning.rs:798`) and its reply lives in its own
   transcript. Receipts carry no work output, so the parent knows only `ready` plus an
   id — it cannot even report that the child finished.
2. **Nothing retires the child.** Its row stays enabled and its tmux terminal keeps an
   idle agent process alive. The only rows the broker removes are ones that provably
   never launched (`pending`, `failed_before_start`).
3. **Only files are tidied, after a week.** The request file goes `RETENTION_SECS`
   after its receipt is final (`app/spawning.rs:1321`); the receipt is kept for good as
   a tombstone.

Children therefore accumulate silently, each holding a process, until a human notices
and presses `d`.

## Outcome

Dispatch → run → result → retire. The child is a JOB with a bounded life, and its end
is a fact rather than an inference.

```text
$ "$PMTUI_BIN" spawn --request-id "$id" --title "…" --message "…" --json
{"state":"ready", "receipt":{"session":{"id":"fix-flaky-fork"}}}      # in flight

$ "$PMTUI_BIN" spawn --request-id "$id" --title "…" --message "…" --json
{"state":"done","result":{"outcome":"done","summary":"Fixed …"}}      # finished, row gone
```

## Prior art

Checked before fixing the design, because every tool in this space has already hit the
"when is it done, and who cleans up" question.

### agent-deck (`asheshgoplani/agent-deck`, read from a local clone)

The closest analogue: a Go TUI over tmux panes running unmodified vendor CLIs, with a
"conductor" supervisor and `--parent` linkage. Four findings, two of them scars.

1. **A hook is not a completion signal.** From
   `internal/session/done_sentinel.go`:

   > "Issue #1186: the conductor previously had no trustworthy 'worker finished'
   > signal because Claude's Stop hook fires at the end of every turn and is mapped to
   > the generic 'waiting' status. Completion is now asserted by the only party that
   > knows — the worker — by ending its final turn with a line of the form
   > `===AGENTDECK_DONE=== status=<ok|fail> summary=<text to end of line>`"

   This is a tool with INTERACTIVE children forced to invent a done-assertion. It is
   the strongest argument for a one-shot child: when the process exits, the task
   boundary is the process boundary and no assertion is needed.

2. **Parse discipline worth copying.** `ParseDoneSentinel` rejects anything malformed
   "rather than guessed at", and `ScanDoneSentinel` takes the LAST valid line,
   "Last-wins so a worker that retried (printing fail then ok) reports its final
   outcome."

3. **PULL, not push.** From `internal/session/transition_notifier.go`:

   > "NotifyTransition validates the event and commits it to the parent's durable
   > outbox (issue #1225: PULL, not push). A busy conductor drains it at its own turn
   > boundary, so delivery no longer depends on an idle window that never opens."

   They also carry two layers of de-duplication (a short window, plus an output-hash
   TTL per child) because they infer status by polling pane content. A job that writes
   one terminal state into a tombstoned receipt needs none of that.

4. **Archive, don't vanish.** Archiving is a timestamp on the row (`ArchivedAt`) plus a
   two-partition filter — `VisibleInstances` hides archived rows, nothing is deleted —
   and `d` deletes with a 30-second undo, transcripts preserved either way. Containers
   default to `auto_cleanup`, and `worktree cleanup` sweeps orphans. Their worker "May
   NOT decide it's done"; the manager reads receipts. A job asserting its TASK finished
   is not a machine declaring the PROJECT done, so our invariant holds.

   Notably, agent-deck never runs a headless agent: `-p` appears in its tree only as a
   statusline argument to parse. The sentinel is the price of that choice.

### Claude Code's own multi-agent model

A subagent "runs in its own context window … works independently and returns results";
only its summary crosses back. Its transcript is NOT discarded — stored per session
(`…/subagents/agent-{agentId}.jsonl`) and deleted after `cleanupPeriodDays`, 30 days by
default. The UI removes a successful subagent's row **immediately** and keeps a failed
or stopped one for 30 seconds. A subagent can be RESUMED with its full history rather
than re-run.

`claude -p` exits 0 on success and non-zero on failure; the last line of
`--output-format stream-json` is a `result` message carrying the final text, cost and
session id, and `--json-schema` makes that payload conform to a schema. For unattended
runs it documents `--permission-prompts none`: anything that would prompt is denied,
"Claude is told that nobody can approve the request and not to retry it, and the run
continues". A `-p` run waiting on background work has a 10-minute idle ceiling
(`CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`, `0` disables) — the bound lives in the
ORCHESTRATOR, which is exactly where the user put it.

### `codex exec`

`--json` is a JSONL event stream (`thread.started`, `turn.started`, `item.*`,
`turn.completed`, `turn.failed`, `error`); the agent's text arrives as a completed
`agent_message` item and `turn.completed` terminates the turn.
`-o/--output-last-message <path>` writes the final message to a file, and
`--output-schema <FILE>` requests a schema-conforming final JSON, "intended for
automated workflows that need stable fields (for example, job summaries …)".
`codex exec resume` continues a previous run.

### The ones that confirm the gap

**claude-squad** documents no idle or done detection at all — you read the preview or
diff and press `D`. **Conductor** (conductor.build) ends at "when the work is ready …
archive the workspace", user-initiated. Both are human-driven, which is the state v1
is in.

### What the research changed

| Was | Now | Why |
|---|---|---|
| The child writes `job-result.json` as its final action | The HARNESS writes the result; the prompt asks for no final step | agent-deck's sentinel exists because interactive children cannot signal; a one-shot child's `result` event / `turn.completed` is the signal, and both CLIs can emit a schema-conforming payload |
| Every terminal state retires the row | `done` and `cancelled` retire; every other outcome KEEPS the row | Claude Code removes a successful subagent's row immediately and holds a failed one; agent-deck archives rather than deletes. A failure you never saw is not cleanup |
| `--no-session-persistence` | Pin `--session-id`, and carry a resume command on a non-`done` receipt | Claude Code's subagents resume with full history; re-running a half-finished job from scratch is waste |
| Parent polls, and a pmd nudge was a likely phase 2 | Polling IS the design; no nudge | agent-deck's issue #1225: a busy supervisor has no idle window, so a durable record drained at the parent's own turn boundary beats pushing. Our receipt already is that record |
| First valid result wins | LAST valid result wins | agent-deck's `ScanDoneSentinel`: a worker that retried should report its final outcome |

## Decisions

1. **A child is a one-shot headless run, not an interactive session.** Both harnesses
   offer the primitive, this repo already builds the argv, and it removes the
   "waiting ≠ done" problem that forces other tools into sentinels.
2. **It runs in tmux, not as a dashboard-owned process.** tmux is this project's
   durability layer; a child owned by pmtui would die with a dashboard restart or a
   confirmed takeover, and a 20-minute job must not.
3. **Completion is the process exiting, and the payload comes from the harness.** No
   idle timer, no activity heuristic, no reliance on an agent remembering a final step.
4. **No clock in the broker.** The dashboard never kills on time. The parent cancels
   when it decides to give up — its own ceiling, as `claude -p` has one — and the human
   cancels with `d`.
5. **Success retires; anything else is kept to be seen.** `done` and `cancelled` drop
   the row. `needs_human`, `failed` and `ended_without_result` keep it, inert, carrying
   the outcome until a human clears it.
6. **The parent pulls.** The receipt is the durable record; there is no push.
7. **The result schema is the spawn protocol's own, and small.** The retired phase
   worker's `StopDraft`/`CompletionDigest` vocabulary is not resurrected; AGENTS.md
   forbids retaining branches for removed modes.

## The child's invocation

`worker::build_command` already produces the one-shot argv for both engines, with its
`--` separator trap verified against both real CLIs. It has no production caller — it
is left over from the retired per-wake model. This spec gives it its first one, plus
the flags the research says an unattended child needs.

```text
claude:  env -u CLAUDECODE claude -p --permission-mode acceptEdits
         --permission-prompts none --session-id <uuid>
         --output-format stream-json --verbose --forward-subagent-text
         --json-schema <result schema> [--add-dir …] -- <prompt>

codex:   codex exec -s workspace-write --skip-git-repo-check --json
         --output-schema <result schema> -o <job-result.json>
         [--add-dir …] -- <prompt>
```

- **`--permission-prompts none`** is why a job cannot hang on a prompt nobody will
  answer: the request is denied, Claude is told not to retry, and the run continues.
  Codex's `exec` is already non-interactive under `-s workspace-write`.
- **`--session-id <uuid>`** replaces `--no-session-persistence`, so a job that ends
  `needs_human` or `failed` can be continued rather than restarted. Codex has no
  caller-chosen id, so its id is captured from the `--json` stream the way the existing
  codex paths already do.
- **No `--bare`.** The child is doing project work and needs the project's `CLAUDE.md`
  and skills. Anthropic intends `--bare` to become the `-p` default, so the argv states
  what it wants rather than relying on today's default. The directory is already
  allowlisted by v1's `dir_not_allowed` validation, which is what makes loading project
  content acceptable.
- **The spawn skill is NOT installed into a child.** Nested spawn stays refused by
  construction rather than by a check.

Launched in the child's own tmux window, named exactly as today, with its combined
output tee'd to `<child state>/job.log` — the pattern the decider consult already uses
(`paths.advice_log`, the tee'd `handle.log`). The log rotates at 1 MiB keeping one
previous file, mirroring `pmtui.log`.

The prompt is the parent's Message plus a fixed harness preamble, written as imperative
steps rather than narration (house style). The preamble states the task boundary and the
outcome vocabulary; it does NOT ask the child to write a file.

### The row knows it is a job

`launch` already records what the broker owes a row. It gains `kind`: `chat` for a v1
interactive child and `job` for this one, defaulted on deserialization to `chat` so a
row written by an older build keeps its meaning. Everything that must treat a job
differently — the retirement sweep, the refusals below, the `job from <parent>` label —
reads that one field rather than inferring from `spawned_by`.

Resume, restart, Autopilot, Message and **fork** refuse a job row with `<id> is a job —
it reports its result and exits`, the same shape as v1's refusal for a staged row.
Attach is allowed: watching is the point.

## The result (harness-written)

Both CLIs can be made to produce a final payload conforming to a schema we supply, so
the structured outcome is the harness's output rather than a file an agent must remember
to write:

```json
{
  "outcome": "done",
  "summary": "Fixed the flaky fork test by waiting on the lease instead of sleeping.",
  "detail": "optional, longer prose"
}
```

- `outcome` ∈ `done` | `needs_human` | `failed`.
- codex writes it directly with `-o <path>` under `--output-schema`.
- claude's conforming payload arrives in the `result` event's `structured_output`; the
  broker reads it from the tee'd `job.log`, whose last line is that event.
- **Last valid payload wins**, after agent-deck's `ScanDoneSentinel`: a run that emitted
  an earlier result and then continued reports its final one.
- Anything malformed is REJECTED, not guessed at — their other parse rule.
- `summary` ≤ 4096 bytes, `detail` ≤ 16384 bytes, TRUNCATED with a marker rather than
  rejected: a child that did the work must not lose its report to a size rule.
- Absent, unreadable, over `MAX_REQUEST_BYTES`, or unparseable → `ended_without_result`,
  which now means the run crashed or was killed, not that an agent forgot a step. Read
  with the same hardened I/O as requests: no symlink following, no blocking on a FIFO.

The dashboard is the only writer of the receipt; it copies this payload in.

## Lifecycle

v1's launch states are unchanged: `staged` → `attempted` → `started`, with `ready`,
`needs_attention`, `failed` and `outcome_unknown` as its outcomes. `ready` keeps its
wire name and now means "launched and running", so a v1 parent's branch still reads.

The broker, on each refresh, looks at every `job` row whose receipt is not yet final:

| Observation | Receipt state | Row |
|---|---|---|
| terminal alive | `ready` (unchanged) | stays; preview tails `job.log` |
| terminal gone, result `done` | `done` + `result` | **retired** |
| terminal gone, result `needs_human` | `needs_human` + `result` + resume command | kept, inert |
| terminal gone, result `failed` | `failed` + `result` + resume command | kept, inert |
| terminal gone, no usable result | `ended_without_result` + resume command | kept, inert |
| cancel requested (parent or human) | `cancelled` | window killed, **retired** |
| launch reached `needs_attention` | unchanged | stays — a human is wanted |

A kept row's process is gone: it refuses every lifecycle key except `d` and reads
`<outcome> · from <parent>`. It exists so an outcome you have not seen cannot be swept
away — Claude Code holds a failed subagent's row for the same reason, and agent-deck
archives rather than deletes. `d` clears it.

### Retirement

The dashboard's own removal path minus the confirm gate: terminate the window (a no-op
when already dead), drop the registry row, delete the pmtui-owned `session.json`, clear
attach intent, and leave `.project-state/` and the source tree untouched
(`app/lifecycle.rs:402-436`). The status log records one line:
`retired <child> · done — <first line of summary>`.

`job.log` and the child's reserved state are swept on the existing `RETENTION_SECS`
(7 days), the way request files already are and the way Claude Code deletes subagent
transcripts after `cleanupPeriodDays`. The receipt stays for good as the tombstone that
keeps a replayed request id from relaunching.

**Rejected alternative:** agent-deck's archive-as-a-field (`ArchivedAt` plus a
visible/archived partition), which never deletes a row. Here the receipt already is the
durable record, and `registry.json` is the human's session list — accumulating retired
job rows in it would make a human-owned file grow without bound.

Guards, each a refusal rather than a race:

- Only rows whose `launch.kind` is `job`. A human's row is never touched.
- **Never while a human is attached.** Attachment is tmux clients plus the attach-intent
  marker, as everywhere else; retirement defers to a later frame.
- Never a staged row (v1's start-path refusals already cover it).
- Never a `needs_attention` launch.

Retirement frees a slot against the five-children-per-parent cap, which becomes a live
fan-out limit.

## Cancel

- **Parent:** `pmtui spawn --request-id <id> --cancel` writes `<id>.cancel` beside its
  request — the directory it already writes — and prints the receipt. The file is a
  marker, not a message: its presence is the whole signal, published with the same
  no-clobber write as a request, so a repeated cancel is idempotent. A cancel for an
  unknown or already-final request is reported, never invented.
- **Human:** `d` on a job row confirms, then kills and retires it.

Cancellation sends SIGTERM and then kills: `claude -p` exits 143 on SIGTERM after
running its `SessionEnd` hooks and terminating the process tree of any Bash command it
started, so a polite signal first is what reaps a child's own children.

There is no timeout. A runaway child burns tokens until the parent or the human stops it
— the bound belongs to the parent, as `claude -p`'s own idle ceiling is a property of
the orchestrator rather than the child.

## The parent's contract

The poll is unchanged, and by decision 6 it stays the whole mechanism: the receipt is a
durable per-request record the parent reads at its own turn boundary. The skill gains
the terminal branches:

- `done` — report the summary to the human.
- `needs_human` — relay the child's question; do not re-dispatch. The receipt carries the
  command that resumes that exact conversation.
- `failed` — act on `error.code` as v1 specifies, plus the result's summary.
- `ended_without_result` — the run crashed or was killed. Read `job.log`, then decide;
  the receipt's resume command continues the same conversation, and a fresh attempt needs
  a NEW request id.
- `cancelled` — stopped deliberately; never silently retry.

## What the human sees

A running job is a row reading `job from <parent>`, with the tail of `job.log` in the
preview — the visualization that justifies running children as rows at all. On success
the row disappears and the status log carries the outcome; on anything else the row stays
until cleared, so the thing you need to act on is the thing still on screen.

## Schema versioning

Receipt `SCHEMA_VERSION` 1 → 2 for the new states and the `result` field. v1 receipts
already on disk stay readable as tombstones, and a row with no `launch.kind` reads as
`chat`. The request schema is unchanged. Both sides ship in one binary, so a v1 reader of
a v2 receipt is out of scope.

## Verification

Unit:

- result parse for both engines: codex's `-o` file, claude's `result` event in `job.log`;
  each outcome; last-valid-wins over an earlier payload; malformed input rejected rather
  than guessed; truncation of over-long fields; absent, garbage, oversized and symlinked
  cases → `ended_without_result`.
- every row of the lifecycle table, including a `needs_attention` launch that must NOT
  retire, and a kept row that refuses every lifecycle key except `d`.
- retirement guards: human attached (defers), staged row, a row whose `launch.kind` is
  `chat`.
- cancel from both routes; `cancelled` is never retried; SIGTERM precedes the kill.
- a v1 receipt still loads, and a row with no `launch.kind` reads as `chat`.
- `job.log` rotation, and the `RETENTION_SECS` sweep of a retired child's log.

Real tmux (mandatory, the ignored suite):

- a child that completes → its row disappears and the parent's poll returns `done` with
  the summary parsed from the real CLI's output.
- a child that returns `needs_human` → the row stays, inert, and the receipt carries a
  resume command.
- a child killed mid-run → `ended_without_result`, row kept.
- cancel while running → window gone, row gone, receipt `cancelled`, and the window's
  process group gone with it.
- a human attached to a finished child → retirement waits until they detach.

Plus the full gate list in AGENTS.md, including the per-file coverage gate.

## Out of scope

- Nested spawn, or depth beyond 1.
- Interactive children (the rejected approach B). A job that needs a conversation returns
  `needs_human` and leaves a resumable conversation behind.
- Any queue beyond the existing five per parent.
- A history view of finished jobs. The receipts are the history; a reader for them is a
  later delta.
- Pushing to the parent, per decision 6.

## Risks

- **A runaway child.** Accepted, by decision 4; the parent's own bound and `d` are the
  controls. Prior art agrees the ceiling belongs to the orchestrator.
- **A child that stalls without exiting** — now much narrower: `--permission-prompts
  none` denies rather than waits, and codex's `exec` cannot prompt. What remains is a
  model loop, which the parent cancels.
- **Orphaned sidecars.** A child's MCP servers and background shells are the harness's to
  reap; SIGTERM-before-kill gives it the chance. agent-deck reports accumulating orphaned
  MCP processes as a real failure mode, so the acceptance test asserts the window's
  process group is gone after a cancel.
- **`stream-json` verbosity.** Bounded by log rotation, not by dropping output.
