# TRAE CLI Engine Design

## Goal

Add TRAE CLI as a third pmtui engine for Standard interactive sessions, spawned jobs, job result parsing, and model discovery. Do not enable TRAE Autopilot in this slice.

## Current CLI Contract

The current installed command is `traex`. Launches in this repository use `traex` directly.

Headless execution is the `exec` subcommand:

```bash
traex exec --json --sandbox workspace-write --skip-git-repo-check --session-id <id> -- <prompt>
traex exec resume <id> --json --skip-git-repo-check -- <prompt>
```

The CLI supports `--session-id`, `--add-dir`, `--sandbox`, `--output-schema`, `-o/--output-last-message`,
`--json`, and `-m/--model` on `exec`. The old `-p` print flag is now `--profile`, so this integration
must not use it. `exec --permission-mode default` is rejected by the current CLI, so the integration
omits `--permission-mode` and relies on `--sandbox workspace-write`. It must not pass `-y`, `--yolo`,
or `bypass_permissions` by default.

TRAE hooks are configured through the CLI's project config file. There is no `hooks` subcommand.
Per-launch `-c hooks=[...]` probes failed before a turn and did not write a hook signal, so this
design does not depend on per-launch hook injection.

## Approved Scope

TRAE is added to `Engine::ALL` for user selection and `pmtui spawn --agent traex`.

Standard interactive TRAE sessions use `build_standard_command` with:

```bash
traex [ -m <model> ] --session-id <id>
traex [ -m <model> ] resume <id>
```

Spawned TRAE jobs use `build_job_command` and `build_command` with:

```bash
env -u PMTUI_SESSION -u PMTUI_STATE_DIR -u PMTUI_BIN traex exec \
  --sandbox workspace-write --skip-git-repo-check --json --session-id <id> \
  --output-schema <schema> -o <last-message> [ -m <model> ] -- <prompt>
```

Autopilot launches reject TRAE for now. This prevents a missing turn hook from making pmd depend on report-debt and stall backstops as its normal control flow.

## Data Flow

1. The create form and model picker cycle over `Engine::ALL`.
2. A Standard TRAE create mints a pmtui-owned conversation id, launches `traex --session-id <id>`, then records the id in the registry.
3. A Standard TRAE resume launches `traex resume <id>`.
4. A spawned TRAE job runs `traex exec --json` with a pinned session id, schema file, and last-message file.
5. `spawn::read_job_result` reads the `turn.completed` / `turn.failed` terminator and the last-message file. The old `type=result` event is accepted only as compatibility fallback.
6. Model discovery runs `traex models --json` and maps `name` to the launch value and `config_name` / `real_name` / `description` to the display label.

## Boundaries

TRAE is not a decider engine in this slice. Decider selection must remain Claude/Codex until the advise path has a tested TRAE contract.

Fork support is not part of v1 unless the CLI contract for forking is verified.

TRAE project skills can reuse the existing `.agents/skills` path; `traex debug prompt-input` shows TraeX discovers project skills there. That keeps Codex and TraeX on one shared copy, with `.claude/skills` as the Claude compatibility layer.

## Tests

Add tests before implementation for:

- `Engine::Trae` label, serialization, and `Engine::ALL` membership.
- Headless command, job command, and Standard command argv.
- No unsafe TRAE permission flags.
- TRAE final result parsing.
- TRAE model discovery from stubbed `traex models --json`.
- Create form engine cycling.
- Spawn CLI parsing and help text.
- Autopilot/loop launch rejection or gating for TRAE.

## Docs

Update `docs/SPEC.md`, `docs/GUIDE.md`, and `AGENTS.md` command surface text where engine lists or launch guarantees mention Claude/Codex only.
