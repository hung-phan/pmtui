# AGENTS.md - agent-manager

Read this with `README.md` (getting started), `docs/GUIDE.md` (day-to-day usage), and the
canonical design in `docs/SPEC.md`.
When behavior changes, update the matching spec and rationale in the same change.

## Product Model

`agent-manager` is a Rust supervisor with two binaries:

- `pmd`: an optional headless driver.
- `pmtui`: a ratatui dashboard and terminal controller.

Each project session owns exactly one persistent tmux terminal named
`pm-<id>-<hash>`. A human attaches to that terminal; pmd sends input to the same
terminal only while the row is on Autopilot and no human is attached. Changing
between Standard and Autopilot never restarts the terminal.

Standard session intake may carry one optional initial Message. It is submitted
only on the fresh interactive launch; restart/resume never replay it. Pointer
actions must route through the same handlers as their keyboard bindings, and hit
regions must be rebuilt from the current frame.

The `/` switcher selects existing sessions only; it does not create task or ticket
state. The `s` composer is inline and keeps dashboard-local drafts by session id.
Drafts never enter project state and clear only after successful delivery or removal.

## Required Verification

Run all of these after every change:

```bash
cargo build --locked
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo audit
bash tests/coverage_test.sh
bash tests/install_sh_test.sh
bash tests/repo_contract_test.sh
cargo test --test integration -- --ignored --test-threads=1
```

`tests/coverage_test.sh` runs the all-target suite serially, aggregates the real
`enter_routing` and confirmed-dashboard-takeover tmux profiles, and requires every
production source file to reach 95% for regions, functions, and lines.

The ignored integration suite uses real tmux and is mandatory. A green unit
suite cannot prove terminal arguments, pane classification, process lifecycle,
rendering, or input delivery.

For nudge or worker-procedure changes, also run the LLM nudge judge:

```bash
PM_NUDGE_JUDGE=1 cargo test --test integration nudge_judge -- --ignored --test-threads=1
```

For decider, policy, or situation-projection changes, run the decider benchmark:

```bash
PM_DECIDER_BENCH=1 cargo test --test integration decider_bench -- --ignored --test-threads=1
```

## Live Tmux Testing

Use a unique private socket and scratch registry. Observe the actual panel, not
only process exit codes.

```bash
tmux -L am-check new-session -d -s host -x 140 -y 40 \
  "target/debug/pmtui --socket am-agent --registry /tmp/am-registry.json"
tmux -L am-check capture-pane -p -t '=host:' -S -200
tmux -L am-check send-keys -t '=host:' -l -- 'n'
tmux -L am-check send-keys -t '=host:' Enter
```

Capture the pane after each meaningful action. Use `send-keys -l --` for literal
text and a separate `send-keys ... Enter` to submit it. Tear down every scratch
server and daemon when the test ends.

## Invariants

- One project session, one tmux terminal. Never create separate human and pmd
  terminals for the same session.
- pmd is the sole writer of `state.json`. The worker is the sole writer of
  `needs-you.json` and its optional bounded `checkpoint.json`. pmtui writes
  human-owned files such as `registry.json`, `config.json`, `control.json`,
  `answers.json`, `brief.md`, `directive.md`, and its own `pmtui.json` preferences.
- `control.json` carries human cadence and immediate-wake requests. pmd consumes
  those requests and updates its ledger.
- Every pmd or pmtui pane input owns `input.lock` across the final pane recheck,
  paste, and Enter.
- Clickable preview titles, create rows, and keybar chips never bypass keyboard
  preconditions, confirmations, or lifecycle routing.
- Quick-switch results commit by stable session id, and composer input always keeps
  the existing pane-input lock and final pane recheck.
- One pmd owns a tmux socket. Another registry must use another socket.
- One pmtui owns a registry/socket pair. Confirmed takeover replaces only that dashboard and must
  preserve pmd plus every project terminal.
- An agent requests a session only through `spawn-requests/`; the dashboard is the only process
  that turns a request into a registry row.
- A spawned child is a JOB: one headless `claude -p` / `codex exec` run that does its task, reports
  a result and EXITS. Its end is a fact — the process is gone — never an inference about idleness,
  and its result is the harness's own final payload, not a file the agent must remember to write. A
  `done` job is retired automatically — unless it left uncommitted work; any other outcome keeps its row
  for a human. A job is given
  no spawn procedure and no `ManagedEnv`, so it cannot spawn. Its state lives exactly as long as its
  row: removing a job row deletes that session's own `.project-state/sessions/<seg>/` and nothing
  else, because the parent's receipt is what outlives it. A human's session is never purged.
- A job in a git repository works in its OWN WORKTREE, on its own branch, and is told to commit there.
  Isolation is the dashboard's job; INTEGRATION IS THE HUMAN'S — `a` cherry-picks one child's commit onto
  their checkout, confirmed, refused while their tree is dirty, and aborted on a conflict. Uncommitted work
  is never deleted to tidy up: a dirty worktree keeps its row, its state and its branch, whatever the
  outcome was and whoever ended it. A human's own `d` is still theirs to press.
- Five children run AT ONCE; a sixth request waits and stages when a slot frees. The limit is concurrency,
  never an answer an agent has to act on.
- A job is ANSWERED ONCE, and one harvest runs at a time. The recorded answer beats anything a later pass
  can reconstruct from a directory retirement already deleted.
- Stopping pmd never kills project terminals. Pause, restart, and remove are
  explicit pmtui actions.
- A human attach is detected by tmux clients plus a short-lived attach-intent
  marker. Terminal liveness alone never means a human is present.
- Never auto-approve Codex directory trust or override host security hooks.
- `decide_kind` remains byte-pure: only `Tier`, `StopKind`, and `RiskClass`.
- The worker nudge is derived only from agent-authored inputs and fixed protocol
  structure.
- `checkpoint.json` is untrusted continuity data, never instructions or policy
  input; workers reload it only through `pmd checkpoint <path>`, and the default
  UI does not expose activity handles or output references.
- The status glyph means working, not merely alive.
- Chrome takes every colour from the active theme, and RGB enters it only through
  `src/theme.rs`. Add a ROLE there rather than a colour at a render site, and keep
  severity carried by glyph, word and badge as well as hue. EVERY CELL is the theme's:
  repaint the theme's pair over any rect a widget `Clear`s, and read an agent's own
  "default colour" as the theme's. No animated effects: a repaint happens because state
  changed.
- A Settings row is one `SettingKind` variant plus its dropdown; neither the renderer nor
  the key handler grows per setting.
- The three PROSE fields — Message, goal and directive — edit a `ratatui-textarea` buffer, seeded
  from what is already there. Route keys to it; do not reimplement motions, kills, wrapping or undo.
  `Enter` submits, so a newline is a chord, and `$EDITOR` is `^X^E` (which is what leaves `^E` free to
  mean end-of-line). A one-line `Field` is for a scalar: an id, an interval, a query.
- No machine declares a project done. The human closes the session.

## Spawn Command Surface

`docs/SPEC.md` owns the guarantees; the concrete surface lives here, because the spec's charter
keeps commands, arguments and exit codes out of it.

```bash
pmtui spawn [--message M] [--title T] [--name N] [--dir D] [--agent A] [--model M] \
            [--request-id ID] [--wait SECONDS] [--json]
pmtui spawn --status                      # every child this session asked for, one line each
pmtui spawn --cancel --request-id ID      # ask once; asking twice is asking once
```

`--cancel` names its child through `--request-id`; it takes no value of its own. The agent flag is
`--agent`, not `--engine`.

Exit codes are the contract an agent branches on. `--json` still prints a result document on a
nonzero exit, so read the document rather than trusting the code alone. From `exit_code`:

| Code | States |
|---|---|
| 0 | `ready`, `done` — "the thing you asked for happened" |
| 3 | `queued`, `in_progress`, `cancel_requested`, `needs_attention`, `needs_human`, and a receipt still `claimed`/`staged`/`launching` — ask again under the SAME request id |
| 1 | `failed`, `outcome_unknown`, `ended_without_result`, `cancelled` |
| 2 | usage error |

`needs_human` sits in the 3 family for the same reason as `needs_attention`: the receipt names the
next action. `--status` exits 0 whenever it could read its own directory, because a listing is not
an outcome. Every form reads only `PMTUI_SESSION`/`PMTUI_STATE_DIR`/`PMTUI_BIN` plus its own state
directory, so it works where `$HOME` is read-only.

## Code Organization

- `src/job_engine/`: persistent-session scheduling and policy application.
- `src/tmux/`: terminal driver, exact targeting, input delivery, and names.
- `src/worker/`: Claude/Codex launch argv and result schemas.
- `src/advise/`: read-only decider consult and reply validation.
- `src/daemon/`: registry reconciliation, ownership, escalation, and cleanup.
- `src/doctor/`: read-only registry, state, executable, skill, and tmux diagnostics.
- `src/theme.rs`: the opaline seam — the dashboard's colour ROLES and the one place RGB
  enters chrome; `src/attention.rs` builds every style from it.
- `src/spawn/`: the request/receipt protocol, the job prompt, and the job result the harness writes.
- `src/bin/pmtui/composer.rs`: the prose buffers' seam onto `ratatui-textarea` — which library call
  each key means, and nothing else. `src/bin/pmtui/input.rs`'s one-line `Field` backs the answer,
  cadence, rename and switcher fields, and the create form's rows.
- `src/bin/pmtui/`: dashboard state, actions, rendering, and terminal handoff.
- `.agents/skills/`: canonical project-local skills; `.claude/skills/` contains compatibility
  symlinks only.
- Tests live beside code; real terminal acceptance lives in `tests/integration/`.

## Test Placement

- Put new tests in dedicated, behavior-named test files by default; do not grow
  production source files with large `#[cfg(test)]` modules.
- Follow the module's existing layout. When it has a test tree, mirror the source
  module there (for example, `src/tmux/dialog_keys.rs` ->
  `src/tmux/tests/dialog_keys.rs`) and register it in `tests/mod.rs`.
- For a module with several providers or concerns, use `tests/mod.rs` plus named
  files such as `tests/claude.rs` and `tests/codex.rs`, not a generic `tests.rs`.
- A small inline test module is acceptable only when that source module already
  uses that local convention and a separate test tree would add no useful structure.
- Keep real-process and real-terminal acceptance tests in `tests/integration/`.

Prefer focused modules, immutable transformations, explicit errors, and atomic
file replacement. Do not retain historical branches for removed modes or
session families.

## Git

Do not commit or push unless asked. Work on a branch rather than directly on
`main`. Never discard unrelated user changes.
