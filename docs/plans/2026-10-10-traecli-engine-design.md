# TRAE CLI Engine Design

## Goal

Add TRAE CLI as a third pmtui engine for Standard interactive sessions, spawned jobs, job result parsing, and model discovery. Do not enable TRAE Autopilot in this slice.

## Current CLI Contract

The installed command is `coco version 0.121.1`, also available as `traecli`, `trae-agent`, and `ta`.

Headless execution is top-level print mode:

```bash
traecli -p --output-format stream-json --session-id <id> -- <prompt>
traecli -p --output-format stream-json --resume <id> -- <prompt>
```

The CLI supports `--output-format text|json|stream-json`, `--session-id`, `--resume[=AUTO]`, `--add-dir`, and `--permission-mode default|plan|bypass_permissions`. Model selection uses `-c model.name=<model>`. The integration must not pass `-y`, `--yolo`, or `permission_mode=bypass_permissions` by default.

TRAE hooks are configured through `.trae/traecli.yaml`. There is no `hooks` subcommand. Per-launch `-c hooks=[...]` probes failed before a turn and did not write a hook signal, so this design does not depend on per-launch hook injection.

## Approved Scope

TRAE is added to `Engine::ALL` for user selection and `pmtui spawn --agent traecli`.

Standard interactive TRAE sessions use `build_standard_command` with:

```bash
traecli [ -c model.name=<model> ] --session-id <id>
traecli [ -c model.name=<model> ] --resume <id>
```

Spawned TRAE jobs use `build_job_command` and `build_command` with:

```bash
env -u PMTUI_SESSION -u PMTUI_STATE_DIR -u PMTUI_BIN traecli -p --permission-mode default \
  --output-format stream-json --session-id <id> [ -c model.name=<model> ] -- <prompt>
```

Autopilot launches reject TRAE for now. This prevents a missing turn hook from making pmd depend on report-debt and stall backstops as its normal control flow.

## Data Flow

1. The create form and model picker cycle over `Engine::ALL`.
2. A Standard TRAE create mints a pmtui-owned conversation id, launches `traecli --session-id <id>`, then records the id in the registry.
3. A Standard TRAE resume launches `traecli --resume <id>`.
4. A spawned TRAE job runs in print mode with a pinned session id and stream JSON output.
5. `spawn::read_job_result` reads the final TRAE `{"type":"result",...}` event. A `subtype` of `success` maps to `done`; other result subtypes map to `failed`.
6. Model discovery runs `traecli models --json` and maps `name` to the launch value and `description`/`real_name` to the display label.

## Boundaries

TRAE is not a decider engine in this slice. Decider selection must remain Claude/Codex until the advise path has a tested TRAE contract.

Fork support is not part of v1 unless the CLI contract for forking is verified.

TRAE project skills are not copied in this slice. The existing `.agents/skills` path remains the compatibility path for agent-manager worker instructions.

## Tests

Add tests before implementation for:

- `Engine::Trae` label, serialization, and `Engine::ALL` membership.
- Headless command, job command, and Standard command argv.
- No unsafe TRAE permission flags.
- TRAE final result parsing.
- TRAE model discovery from stubbed `traecli models --json`.
- Create form engine cycling.
- Spawn CLI parsing and help text.
- Autopilot/loop launch rejection or gating for TRAE.

## Docs

Update `docs/SPEC.md`, `docs/GUIDE.md`, and `AGENTS.md` command surface text where engine lists or launch guarantees mention Claude/Codex only.
