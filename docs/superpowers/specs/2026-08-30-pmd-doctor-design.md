# Read-Only `pmd doctor`

## Purpose

`pmd doctor` answers whether the local supervisor installation and registered sessions are
coherent without entering daemon control flow. It is safe to run while pmd, pmtui, workers,
deciders, and human tmux clients are active.

## Command

```text
pmd doctor [--registry <path>] [--socket <name>] [--json]
pmd --version
```

Text is the interactive default. JSON is a stable automation surface.

Exit status:

- `0`: no failed checks; warnings are allowed;
- `1`: at least one failed check;
- `2`: invalid CLI usage.

## Check Model

Every check has a stable identifier, one status, a summary, optional detail, and optional manual
remediation. Remediation is guidance only; doctor never performs it.

- `PASS`: expected state is present and valid;
- `WARN`: degraded, optional, stale, disabled, or not-yet-started state;
- `FAIL`: required state is corrupt, inconsistent, or unavailable;
- `SKIP`: a prerequisite failed, so no claim is made.

The first version checks:

- pmd version;
- registry readability plus blank IDs, duplicate IDs, and non-absolute roots;
- enabled and disabled root availability;
- config, control, ledger, and driver JSON, including config validation and registry/ledger engine
  agreement;
- required Autopilot goals and bounded human cadence;
- worker markers, optional checkpoints, and human answer inboxes, including special-file and size
  rejection for worker-facing runtime JSON;
- orphan session-state directories;
- canonical worker skill freshness;
- worker and configured decider executables by executable PATH metadata only;
- `notify-send` availability without sending a notification;
- tmux version and a read-only session inventory on the selected socket;
- missing expected project terminals, unregistered `pm-*` terminals, active `pmsup-*` terminals
  correlated with durable consult ownership on enabled Autopilot rows, and stale or
  mode-inconsistent decider terminals.

Missing ledgers and driver records are valid before first launch. A missing config is a warning
because the session cannot be driven yet. A missing enabled root is a failure; a missing disabled
root is a warning.

## Read-Only Boundary

Doctor is a dedicated CLI action and never calls daemon startup or sweep code. It performs only:

- file metadata and byte reads;
- executable metadata lookup on `PATH`;
- `tmux -V`;
- `tmux -L <socket> list-sessions`; and
- `tmux -L <socket> display-message` for the `pane_dead` state of an expected decider.

It never acquires a daemon, socket, chat, input, or coordinator lock. It never creates or removes a
stop file, rewrites JSON, refreshes a skill, starts or kills tmux, sends keys, emits a desktop
notification, or launches Claude or Codex.

## JSON Contract

```json
{
  "schema_version": 2,
  "pmd_version": "0.1.0",
  "status": "warn",
  "registry": "/path/registry.json",
  "socket": "pmd",
  "checks": [
    {
      "id": "registry",
      "status": "warn",
      "summary": "registry does not exist",
      "detail": "/path/registry.json",
      "remediation": "create or import a session from pmtui"
    }
  ],
  "summary": {
    "pass": 4,
    "warn": 1,
    "fail": 0,
    "skip": 0
  }
}
```

New checks may be added without changing `schema_version`. Schema version 2 adds the optional
`remediation` field. Renaming fields, status values, or changing their meaning requires another
schema version change.

## Acceptance

Unit fixtures cover healthy, absent, malformed, inconsistent, stale, and unavailable inputs. The
real-tmux acceptance creates registered, unregistered, and stale-supervisor sessions, then runs
both text and JSON doctor modes. Before and after snapshots require:

- identical file trees, bytes, inode, size, mode, and modification time;
- identical pane PIDs, commands, dead/alive state, start commands, clients, and captured content;
- all sessions still alive;
- no invocation of a fake `notify-send`.
