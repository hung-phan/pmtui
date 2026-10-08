# Pmtui Spawn Command Design (v1)

## Status

Proposed v1. It replaces the first draft (`5a49b74`, written before PR #39). That
draft had a separate request journal in `<registry>.spawn-requests/`, a pmd
owner-record readiness protocol, and a CLI that wrote the registry itself. Claude
and Codex reviewed it against Orca's orchestration references
(`stablyai/orca@d0db35c`; `skill-guides/orchestration` is unchanged on Orca `main`
as of 2026-09-29) and against `main@4f25157`. The user approved the change to a
dashboard broker (option B) on 2026-09-29.

## Problem

An agent working inside a pmtui session sometimes wants to hand an independent
piece of its work to a separate agent. Today only a human can do that, by pressing
`n` and retyping the task.

The agent cannot write the registry itself either. An Autopilot Codex worker runs
with `--sandbox workspace-write --ask-for-approval never`
(`src/worker/launch.rs:291`): `$HOME` is read-only and it cannot ask for approval,
so `~/.config/pmd/registry.json` is out of reach. Only the session's workspace and
`/tmp` are writable.

## Outcome

An agent in a pmtui session runs one command:

```text
"$PMTUI_BIN" spawn --title "Fix flaky fork test" --message "<self-contained task spec>" --json
```

1. The command writes a request file into the calling session's own state
   directory, which every sandbox can write.
2. The running dashboard picks it up and creates the child with interactive New's
   seed, command-building and launch primitives.
3. The dashboard writes a receipt, which the command prints.

The child is an ordinary Standard session. It shows on the Task board with its
title and a `from <parent>` link, and its agent starts with the Message as its
first turn. From then on a human drives it like any Standard session. If no
dashboard is running, the request waits and the command returns `queued`. That is
acceptable because a Standard child needs a human anyway.

Success means the child exists and its agent was launched with the Message. It
does not mean the task was done.

## Decisions

| Decision | Choice | Source |
|---|---|---|
| Caller | An agent inside a pmtui-managed session, any engine or tier | user |
| Mechanism | Request file, with the dashboard as broker (option B) | user, recommended by Claude and Codex |
| Nesting | Depth 1: a spawned session cannot spawn (a guardrail, not a security boundary) | user |
| Tier | Children are Standard only in v1 | Claude + Codex, pending user review |
| Fan-out | At most 5 existing rows per parent (spawned children plus their forks) | proposed, pending user review |
| Registry writer | Only the spawn broker: the dashboard holding its singleton and the registry-keyed spawn-broker lease; no cross-process registry lock in v1 | Claude + Codex; lease added in review |
| Placement | The exact folder; no Git, branch or worktree behavior | first draft, kept |
| Entities | No new Task/Run/Dispatch entity; a child is an ordinary session row | first draft, kept |

## Command

```text
pmtui spawn --message <text>
            [--title <text>] [--name <display-name>]
            [--dir <existing-directory>] [--agent claude|codex] [--model <id>]
            [--request-id <uuid>] [--wait <seconds>] [--json]
```

- `--message` is required. It is the child's one-time initial Message, passed in
  the fresh launch argv exactly as interactive New does. Restart and resume never
  replay it.
- `--title` becomes the row's `task_title`. When absent, it is the Message's first
  non-empty line (`intent_title`). Either way it must be 120 characters or fewer
  after trimming (longer derived titles are cut to 120) and contain no control
  bytes.
- `--name` is optional display metadata, validated by `normalize_display_name`.
- Defaults are resolved by the dashboard from the parent row:
  - `--dir` defaults to the parent's root;
  - `--agent` defaults to the parent's engine;
  - `--model` defaults to the parent's `worker_model`, but only when the agent is
    the parent's engine.
- `--dir` must already exist. v1 has no `--create-dir`. After canonicalizing, the
  dashboard accepts only a directory inside the parent's canonical root subtree, or
  one equal to the canonical root of an existing registry row. Anything else, such
  as `$HOME`, is `dir_not_allowed`: the agent names the directory, and the
  dashboard writes session state into it.
- `--request-id` names the operation. When absent, the command generates one and
  prints it. Rerunning with the same id never creates a second child.
- `--wait` bounds how long the command waits for a final receipt: default 20,
  maximum 120, and `0` returns right after the request is written.

The command itself only validates syntax, reads its environment, writes the
request file, and reads the receipt. It never reads or writes the registry, never
calls tmux, and never takes the dashboard singleton. Parsing happens before any
terminal setup.

## Session Identity

Every managed launch passes a typed `ManagedEnv` to
`Driver::launch_interactive`, which applies it with `tmux new-session -e
KEY=VALUE` (tmux 3.0+). Every pmtui and pmd start goes through that call. The
driver cannot infer these values: `TmuxDriver` knows only its tmux binary and
socket, and `JobScheduler` has no pmtui path. Callers therefore supply them
explicitly. `-e` needs tmux 3.0 or later; `pmd doctor` checks the installed
version (`tmux.managed_env`) with its existing `tmux -V` probe, and the README
names the requirement.

| Variable | Value |
|---|---|
| `PMTUI_SESSION` | the stable session id |
| `PMTUI_STATE_DIR` | the session's canonical state directory (`.project-state/sessions/<id>-<fnv8>`) |
| `PMTUI_BIN` | canonical `pmtui` executable (pmd passes its sibling `pmtui`, or omits the variable when that file does not exist) |

The variables are routing, not authentication. A session started before this
change gets them at its next launch (Enter after pause, or `r`). Without
`PMTUI_SESSION` and `PMTUI_STATE_DIR`, the command returns `not_in_a_session`.

## Request And Receipt Files

Both files live in `<PMTUI_STATE_DIR>/spawn-requests/`, which the command creates
with mode `0700`.

| File | Written by | Mutability |
|---|---|---|
| `<request_id>.request.json` | the command (agent side) | published once without overwriting: write a temp file, then `hard_link` it to the final name (fails if the name exists), then remove the temp file; never changed |
| `<request_id>.receipt.json` | the dashboard only | replaced atomically as the request advances |

Request:

```json
{
  "schema_version": 1,
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "parent_session": "service",
  "created_at": "2026-09-29T05:40:00Z",
  "args": { "message": "…", "title": null, "name": null, "dir": null, "agent": null, "model": null }
}
```

- Publishing first checks whether the final request file already exists, before
  it stages the temp file, so a full disk cannot mask an existing request. If the
  name exists, the command reads that file and compares
  `(parent_session, request_id, args_hash)`. If they match it waits for the
  receipt again; otherwise it returns `request_conflict`, writes nothing, and
  prints the existing receipt, if any, in its JSON. `args_hash` is the hash of
  the normalized `args`.
- Before publishing, the command reads the receipt, which outlives its request.
  A receipt whose `args_hash` differs is `request_conflict` even when the request
  file was cleaned up. A final receipt whose request file is gone is returned as
  the answer, and nothing is published again.
- The dashboard handles bad entries in two ways:
  - It skips and logs any entry whose filename is not a lowercase UUID followed by
    `.request.json`. Such a name has no trustworthy id to write a receipt for.
  - It writes an `invalid_request` receipt for a validly named entry that fails
    any of these: a regular file, not a symlink; at most 64 KiB; a valid schema;
    `parent_session` equal to the id that owns the directory.
- It processes at most 16 unfinished requests per parent directory and leaves the
  rest queued.
- Discovery keeps its per-refresh cost bounded. It remembers every
  `(requests_dir, id)` whose receipt it found final and skips it without opening
  anything; such a listed request file is only removed once past retention. For
  any other entry it checks the receipt's finality before parsing the request.
  It opens at most 64 entries per parent per refresh, and logs once when capped.

Receipt (`state` is one of `claimed`, `staged`, `launching`, `ready`,
`needs_attention`, `failed`, `outcome_unknown`):

```json
{
  "schema_version": 1,
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "state": "ready",
  "claimed_by": "pid:4242",
  "session": {
    "id": "service-2",
    "title": "Fix flaky fork test",
    "display_name": null,
    "root": "/workspace/service",
    "agent": "codex",
    "model": null,
    "spawned_by": "service",
    "tmux_session": "pm-service-2-1a2b3c4d",
    "state_dir": "/workspace/service/.project-state/sessions/service-2-1a2b3c4d"
  },
  "launch_state": "started",
  "error": null,
  "next_action": null,
  "args_hash": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
  "updated_at": "2026-09-29T05:40:04Z"
}
```

- Error codes:
  - command side: `not_in_a_session`, `invalid_argument`, `request_conflict`,
    `request_unwritable`;
  - dashboard side: `parent_not_found`, `nested_spawn_refused`,
    `child_limit_reached`, `dir_not_found`, `dir_not_allowed`, `message_too_long`,
    `invalid_request`, `registry_unreadable`, `launch_failed`, `readiness_unknown`.
- `next_action` is `null` or `{ "kind": "wait | attach", "argv": [...] }`, with an
  exact executable and target.
- `args_hash` is the hash of the arguments the request id was answered for
  (serde default `null` for older receipts and for an `invalid_request`, whose
  content could not be read). A `request_conflict` found by the pre-check keeps the
  hash of the row the id already created.
- `claimed_by` is `pid:<pid>`, the broker dashboard's process id.
- The Message is never echoed.

Retention: the dashboard removes only a request file whose receipt has been final
for more than 7 days. Receipts are kept for good as small tombstones. A row alone
cannot protect a replay, because a human may delete the child's row; the tombstone
still answers the id with its final receipt and its `args_hash`.

## What The Command Prints

The command prints the receipt, or a synthesized one when there is none yet.
Its `state` is the receipt state, with two additions:

- `queued`: the request is durable but no dashboard has claimed it before the
  deadline;
- `usage_error`: argument errors.

A `claimed`, `staged` or `launching` receipt at the deadline is printed as
`in_progress`.

Exit codes:

- `0` for `ready`;
- `3` for `queued`, `in_progress` and `needs_attention` (not failures; the
  receipt says what to do);
- `1` for `failed` and `outcome_unknown`;
- `2` for `usage_error`.

With `--json`, every outcome is a JSON document on stdout. Without it, one line
is rendered from the same data, such as
`spawned service-2 "Fix flaky fork test" (codex, from service): ready`.

## Dashboard Broker

Only one dashboard per registry brokers requests. The dashboard singleton is per
`(registry, socket)`, so two dashboards on one registry under different sockets would both hold
one. The broker therefore also needs a lease keyed by the registry alone: a flock on
`<registry>.spawn-broker.lock`, taken through the `lease` module. A dashboard discovers and steps
requests only while it holds both its singleton and that lease. It tries the lease at startup and
on every later frame until it succeeds, then holds it for its life. Until then the broker stays
idle, and the status log notes once that another dashboard is brokering spawn requests.

Discovery runs in `refresh()`: it lists `spawn-requests/` for every registry row.
A request from a session that was removed from the registry is therefore never
processed, and a staged child of such a session is finished by recovery (below).
Processing runs as a non-blocking state machine stepped from `finish_frame`, so no
wait freezes the dashboard. All registry mutations stay on the dashboard's single
event-loop thread. For each request:

The broker reuses interactive New's seed, command-building, `driver.lock` and
launch primitives. It does not use New's in-memory retry path.

1. **Pre-check.** Look for a registry row whose `spawned_by` equals the request's
   `parent_session` and whose `launch` matches `(request_id, args_hash)`.
   - If its `launch.outcome` is final, rewrite the receipt from the row and stop.
     This keeps replay safe after the receipt was cleaned up.
   - If the row is unfinished, continue through the recovery table below; never
     stop early.
   - A row with the same request id but a different hash is a `request_conflict`.
2. **Claim.** Write a `claimed` receipt before any side effect.
3. **Validate.**
   - The parent row exists.
   - The parent has no `spawned_by` (depth 1).
   - Fewer than 5 existing rows name the parent in `spawned_by`.
   - The directory exists (`dir_not_found`) and, canonicalized, is inside the
     parent's canonical root or equals an existing row's canonical root
     (`dir_not_allowed`).
   - Title and name are valid.
   - The exact launch command fits `tmux::LAUNCH_COMMAND_MAX_BYTES`.

   On any failure, write a `failed` receipt.
4. **Stage.**
   - Reserve the id and seed `config.json`, `control.json` and `brief.md`.
   - Re-run the pre-check against the registry the append loads. If a row names the
     request by then, release the reservation and resume that row from the
     pre-check, so a stale job never stages a duplicate.
   - Otherwise append the row with `enabled: false`, `spawned_by`, `task_title`,
     `initial_prompt`, and `launch = { request_id, args_hash, state: pending }`.
   - Write a `staged` receipt.
5. **Launch.**
   - Set `launch.state = attempted` (and a `launching` receipt). For Claude, the
     same registry update persists the minted `conversation_id`, so recovery and
     every ambiguous path keep the conversation the launch created; nothing seeds
     it later.
   - Launch under the per-session `driver.lock`, like interactive New.
   - Map the typed outcome (below).
6. **Finish.**
   - `started`: set `enabled: true`, then check readiness across frames for up to
     3 seconds. Once a first live observation is recorded, the deadline becomes
     `max(deadline, first_ok_ms + 1000)`, so a stalled frame cannot report a false
     `outcome_unknown`.
   - Only promotion out of `attempted` (at launch or recovery) enables the row.
     Recording the final outcome never flips `enabled`, so a human's pause during
     readiness stands.
   - Proven `failed_before_start`:
     - first persist `launch.state = failed_before_start`;
     - revalidate the row id, request id, hash and state;
     - remove the staged row and its exact reserved directory, as a failed fork
       does (`app/forking.rs:547`);
     - write a `failed` receipt.

     Nothing ran, but a retry would fail the same way, so the agent tells the human.
   - Ambiguous: enable the row so a human can see it, keep `attempted`, and write
     `outcome_unknown` with an attach or inspect action. The Message is never sent
     again.

   Every final receipt state is also recorded as `launch.outcome` on the row
   (`ready`, `needs_attention` or `outcome_unknown`; a removed row needs none).

Readiness means the exact tmux session and a live, not-dead pane in two
observations at least 100 ms apart, with no recognized dialog (`tmux::dialog`):

- no dialog: `ready`;
- a trust or setup dialog: `needs_attention` with the attach argv. It is never
  approved automatically;
- probe errors or the deadline: `outcome_unknown`.

A `dashboard spawned <child> from <parent>` status line reports each new child.

### Staged Rows

A row with `launch.state` of `pending` or `attempted` that is still disabled is
staged. Enter, `r`, `m`, `s`, `p` and fork refuse it with
"still being created by a spawn request", generalizing `refuse_incomplete_fork`.
As for an incomplete fork, no keybar or Task board chip offers a refused key:
the chips derive from the same `start_refusal` arms through
`ProjectView.spawn_staged`, so `d` Delete and `R` Rename remain. The empty
preview and the composer shelf name the spawn request instead of `m` or Message.

### Crash And Takeover Recovery

The spawn-broker lease guarantees one broker per registry at a time. Takeover
waits for the old owner to release, and the lease frees when the old process
exits, so a `claimed`, `staged` or `launching` receipt found when a dashboard
becomes the broker belongs to a dead or replaced broker. The new broker recovers
it before processing new requests:

| Registry row for the request id | Action |
|---|---|
| none, receipt `claimed` with no session or launch state (or no receipt) | nothing was staged; restart at Validate (a reserved empty directory may remain) |
| none, receipt past `claimed` or naming a session or launch state | the request staged a row that was removed; never re-claim it. `attempted` or `started`: `outcome_unknown`. `failed_before_start`, `pending` or none: `failed` ("its staged row was removed") |
| `pending` | not launched; continue at Launch |
| `failed_before_start` | proven nothing ran; revalidate, remove the row and its directory, write `failed` |
| `attempted`, terminal alive | promote to `started`, enable, check readiness |
| `attempted`, terminal absent | `outcome_unknown`; enable the row; never resend |
| `started` | finish readiness and the receipt |

On the same first step, the broker also scans the registry for `is_staged_spawn()`
rows that no job resumed: their request file is gone, their parent is no longer
in the registry, or their receipt is already final. Without this they would be
stuck, because every start path refuses a staged row.

| Staged row with no job | Action |
|---|---|
| `pending`, no request | discard the row and its directory; nothing launched it |
| `pending`, request still waiting to be read | leave it for discovery |
| `attempted`, terminal alive | promote to `started`, enable, check readiness |
| `attempted`, terminal absent | enable with `outcome_unknown`; never relaunch |
| `failed_before_start` | discard |

Receipts for these go to the parent's requests directory when the parent is
still registered, and nowhere otherwise.

## Launch Outcomes And Launch State

`launch_interactive` returns a typed outcome instead of a string error.

| Outcome | Meaning | Launch state |
|---|---|---|
| `Started` | new session created with our argv | `started` |
| `NotOnPath` | engine binary missing; tmux never invoked. Behind an `env` prefix (`env -u NAME … NAME=VALUE … <bin>`) the payload binary is checked, not `env` | `failed_before_start` |
| `CommandTooLong` | quoted command over budget; tmux never invoked | `failed_before_start` |
| `AlreadyAlive` | a session with that name existed; our argv was not used | ambiguous (`attempted`) |
| `NewSessionFailed` | tmux exited non-zero | ambiguous (`attempted`): the session may have briefly existed |
| `ExitedAfterStart` | created, then died | ambiguous (`attempted`): the agent may have read the Message |

`ProjectEntry` gains two fields, both serde defaults:

- `spawned_by: Option<String>`;
- `launch: Option<LaunchRecord { request_id, args_hash, state, outcome }>`:
  - `args_hash` is the hash of the normalized request args. It is fixed at
    staging, so later renames or model changes never break replay matching.
  - `outcome` is the final broker result (`ready`, `needs_attention` or
    `outcome_unknown`), or `None` while unfinished.

Only the spawn broker writes `launch` in v1. Interactive New and its dashboard
`initial_message_retries` map are unchanged.

## Parent Link, Forks And Limits

- A fork copies `spawned_by` from its source, so forking a child cannot get
  around the depth rule. Forking a child is refused once the parent has 5
  existing children, because the fork would count as a sixth.
- A fork of a parent is a new root.
- Deleting a parent leaves its children, whose link then shows the raw id.
- Depth 1 is a product guardrail. Sibling sessions share one writable workspace,
  so a child could write into its parent's request directory. The result is still
  a sibling under the same parent's cap, never a grandchild.

## Dashboard Rendering

- The Task card body already prefers `task_title` (`app/refresh.rs:433`).
- The card context line and the preview head gain a lineage fact:
  - `forked from X` wins when a row has both links;
  - otherwise `from X` for `spawned_by`, with X the parent's display label (or
    the raw id once the parent is gone).

  The Session row gets no lineage slot; its fixed-width head stays unchanged.
- A staged row renders as `starting…` in the slot where a paused row shows
  `paused`.
- Column placement is unchanged. A child launched with a Message is Working until
  its first turn ends.

## Agent Skill

`skills/pmtui-spawn/SKILL.md` is embedded like `agent-manager-worker`. A new shared
pre-launch installer writes it to `<root>/.agents/skills/pmtui-spawn/SKILL.md` and
the Claude link `.claude/skills/pmtui-spawn`. The installer runs for every managed
launch that sets `PMTUI_BIN`, from pmtui (Standard, Enter, restart, fork) and from
pmd. Today only pmd's worker path installs a skill (`src/job_engine/session.rs:157`).

Procedure, in imperative steps:

1. Spawn only independent work that can proceed without you. Do the rest
   yourself.
2. Write the Message as a self-contained spec: target, change, constraints,
   ownership (what the child may edit), and observable acceptance.
3. Generate one request id. Run
   `"$PMTUI_BIN" spawn --request-id <id> --title … --message … --json`.
4. Parse the JSON on stdout even when the exit code is nonzero. Read `state`:
   - `ready`: report the child id to the human;
   - `queued` or `in_progress`: tell the human the task is waiting for the
     dashboard. To check again, rerun with the same `--request-id`; never make
     a new id;
   - `needs_attention`: tell the human the child needs them at its terminal;
   - `failed`: act on `error.code`:
     - `request_conflict`: rerun with the original arguments under the same id to
       check it; never create a new id for the same task;
     - `request_unwritable`: rerun the same command later;
     - `nested_spawn_refused`, `child_limit_reached`, `dir_not_allowed`: stop and
       ask the human;
     - `invalid_argument`, `dir_not_found`, `message_too_long`, `invalid_request`:
       fix the argument, then retry with a new id (nothing ran);
     - any other code: stop and tell the human;
   - `outcome_unknown`: stop and tell the human; never spawn a replacement.
5. A spawned session cannot spawn. At 5 children, stop and ask the human.

`doctor` checks the installed skill as it checks the worker skill.

## Writer Ownership

| File | Sole writer |
|---|---|
| `spawn-requests/*.request.json` | the calling agent, through `pmtui spawn` |
| `spawn-requests/*.receipt.json` | pmtui dashboard |
| registry, child `config.json`, `control.json`, `brief.md` | pmtui dashboard |
| skill files | the launching process (pmtui or pmd) |

pmd ignores spawn files and remains the sole writer of `state.json`. The broker
never writes `state.json`, `needs-you.json`, `checkpoint.json`, `stops.json`,
`answers.json`, `directive.md` or provider transcripts.

AGENTS.md invariants gain one line: "An agent requests a session only through
`spawn-requests/`; the dashboard is the only process that turns a request into a
registry row."

## Implementation Slices

1. **Typed launch outcome and `ManagedEnv`:** `launch_interactive` returns the
   outcome enum and applies the env; pmtui and pmd pass their values; `PMTUI_BIN`
   resolution.
2. **Registry fields:** `spawned_by`, `LaunchRecord`, staged-row refusal, and fork
   copying `spawned_by` under the cap.
3. **Command:** the `spawn` subcommand (action enum before terminal setup),
   argument validation, request file, receipt wait, and output.
4. **Broker:** discovery, the state machine, validation, staging, launch,
   readiness, receipts, recovery, retention and the status line.
5. **Rendering:** the lineage fact on cards and preview, and `starting…` for staged
   rows.
6. **Skill:** the embedded skill, the shared installer, a `doctor` check, and
   `docs/SPEC.md`/`README.md`/`AGENTS.md` updates.

Every slice passes build, fmt, clippy, `cargo test`, audit and coverage. The
ignored real-tmux suite runs once, at the end, before commit.

## Acceptance

### Unit (FakeDriver, scratch registry and state dirs)

- **Command:**
  - accepts the documented flags and rejects missing `--message`, bad UUIDs,
    unknown agents and an out-of-range `--wait`;
  - returns `not_in_a_session` without the env;
  - an existing identical request waits again, and different args give
    `request_conflict`;
  - never touches the registry;
  - `--json` usage errors give exit 2;
  - `queued` gives exit 3.
- **Broker:**
  - refuses a missing parent, a parent with `spawned_by`, a sixth child, a
    symlinked or oversized request, a wrong `parent_session`, and a Message over
    budget, each with its code and without staging anything;
  - inherits dir, agent and model from the parent;
  - stores the title;
  - stages a disabled row before launching.
- **Launch outcomes:**
  - `NotOnPath` and `CommandTooLong` remove the staged row and directory;
  - `AlreadyAlive`, `NewSessionFailed` and `ExitedAfterStart` keep an enabled row
    with `outcome_unknown` and no second launch.
- **Recovery:** each row in the recovery table resumes as documented, and none
  resends the Message.
- **Replay:** the same request id replayed, including after cleanup, gives one row
  and one launch.
- **Rows and forks:**
  - staged rows refuse Enter, `r`, `m`, `s`, `p` and fork;
  - a fork of a child copies `spawned_by` and is refused at the cap.
- **Non-blocking:** the broker never blocks a frame (FakeDriver timing assertion).
- **Allowlist:** each writer writes only its allowlisted files.

### Real tmux (run once, at the end)

- A stub agent inside a real `pm-` terminal, using only its inherited env, runs
  `"$PMTUI_BIN" spawn …` while a real dashboard is open.
  - Exactly one child terminal starts in the right folder with the Message.
  - The command prints `ready`.
  - The Task board shows the title and `from <parent>` without a restart.
- With the same stub run while no dashboard is open, the command returns `queued`
  (exit 3). Starting a dashboard creates the child.
- The same stub run under Codex's `workspace-write` sandbox profile (read-only
  `$HOME`), through the `codex sandbox … -- <command>` subcommand, which calls no
  model, can still write its request. The helper checks the installed Codex
  version and skips with a visible reason only when Codex or its sandbox facility
  is missing. An installed but broken sandbox fails the test.
- A child that tries to spawn gets `nested_spawn_refused`.
- Killing the dashboard mid-request and starting a new one recovers the request
  without a second terminal or Message.

## Deferred

- Autopilot children.
- A read-only `pmtui status --json` so a parent can check on its children.
- Child-to-parent messages, completion reports, and Orca-style ask/reply.
- `--create-dir`, and spawning from outside a pmtui session.
- A cross-process registry lock (needed only if a second registry writer
  appears).
- Moving interactive New onto the persisted launch state.
- Non-blocking fork identity wait (known follow-up from #39).
