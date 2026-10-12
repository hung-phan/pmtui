# TRAE CLI Engine Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

> **2026-10-12 update:** TRAE CLI 2.0 supersedes the v1 command snippets below. Runtime launches now
> call `traex`; headless jobs use `traex exec --json` with `--output-schema` and `-o`; model selection
> uses `-m`; `--agent traex` is the supported command surface.

**Goal:** Add TRAE CLI as a selectable pmtui engine for Standard sessions, spawned jobs, result parsing, and model discovery, while keeping TRAE Autopilot disabled.

**Architecture:** Extend the existing engine enum and per-engine adapters instead of adding a parallel path. TRAE uses caller-chosen session ids like Claude for Standard and job launches, but it does not get loop/autopilot command support until its hook contract is proven.

**Tech Stack:** Rust 2024, serde JSON, ratatui UI state, tmux launch argv builders, local CLI model discovery.

---

### Task 1: Add TRAE to the engine model

**Files:**
- Modify: `src/registry.rs`
- Modify: `src/bin/pmtui/spawn_cli.rs`
- Test: `src/registry.rs`
- Test: `src/bin/pmtui/tests/spawn_cli.rs`

**Step 1: Write failing tests**

Add assertions that `Engine::ALL` is `[Claude, Codex, Trae]`, `Engine::Trae.label()` is `traex`, and serde round-trips `"trae"`.

Add spawn CLI tests for:

```rust
assert_eq!(parse(&["--message", "x", "--agent", "traex"]), Some(Engine::Trae));
assert!(usage_for_bad_agent.contains("claude, codex or traex"));
```

**Step 2: Run focused tests**

Run:

```bash
cargo test registry::tests::engine_labels
cargo test --bin pmtui spawn_cli
```

Expected: fail because `Engine::Trae` does not exist.

**Step 3: Implement minimal enum support**

Add `Trae` to `Engine`, update `Engine::ALL`, `Engine::bin`, and spawn CLI error text. Keep the serde value as `"trae"`, while the launch label is `traex`.

**Step 4: Verify**

Run the same focused tests. Expected: pass.

### Task 2: Add TRAE launch argv support

**Files:**
- Modify: `src/worker/launch.rs`
- Modify: `src/bin/pmtui/session.rs`
- Modify: `src/bin/pmtui/app/create.rs`
- Test: `src/worker/tests.rs`
- Test: `src/worker/tests/job.rs`
- Test: `src/bin/pmtui/tests/session.rs`
- Test: `src/bin/pmtui/tests/create.rs`

**Step 1: Write failing tests**

Cover:

```rust
build_command(Engine::Trae, "work", PermissionMode::Auto, &[dir], Resume::Fresh { session_id: Some("sid".into()) })
```

Expected shape:

```text
traex exec --json --sandbox workspace-write --skip-git-repo-check --session-id sid --add-dir <dir> -- work
```

Cover `Resume::Continue("sid")` using `--resume sid`, model override using `-c model.name=<model>`, and the absence of `-y`, `--yolo`, or `bypass_permissions`.

Cover Standard create/resume:

```text
traex --session-id sid
traex resume sid
```

Cover `build_loop_command(Engine::Trae, ...)` returning an explicit unsupported result or a panic that callers never reach. Prefer a typed unsupported return only if the local function signatures already make that easy; otherwise gate TRAE before loop launch.

**Step 2: Run focused tests**

Run:

```bash
cargo test worker::
cargo test --bin pmtui session
cargo test --bin pmtui create
```

Expected: fail on missing TRAE match arms.

**Step 3: Implement launch support**

In `build_command`, add TRAE print-mode argv with `--permission-mode default`, `--session-id` or `--resume`, `--output-format stream-json`, and `--add-dir`.

In `build_job_command`, treat TRAE like Claude for caller-chosen job session ids. Add `-c model.name=<model>` before `--`.

In `build_standard_command`, add TRAE interactive create/resume with optional `-c model.name=<model>`. Ignore turn hook for TRAE in v1.

In `build_chat_create`, allow `Engine::Trae` with `Resume::Fresh { session_id: Some(..) }`.

In `fresh_standard_argv` and `start_undriven_session`, treat fresh TRAE like Claude, not Codex.

**Step 4: Verify**

Run the focused tests again. Expected: pass.

### Task 3: Gate TRAE Autopilot and decider paths

**Files:**
- Modify: `src/bin/pmtui/create_form.rs`
- Modify: `src/bin/pmtui/app/create.rs`
- Modify: `src/bin/pmtui/app/autopilot.rs`
- Modify: `src/job_engine/session.rs`
- Modify: `src/worker/supervisor.rs`
- Modify: `src/job_engine/supervisor.rs`
- Test: `src/bin/pmtui/tests/create_form.rs`
- Test: `src/bin/pmtui/tests/autopilot.rs`
- Test: `src/job_engine/tests/session.rs`

**Step 1: Write failing tests**

Cover worker engine cycling through Claude -> Codex -> Trae -> Claude.

Cover decider engine cycling remains Claude -> Codex -> Claude.

Cover submitting an Autopilot create with `Engine::Trae` keeps the form open and reports that TRAE Autopilot is not supported yet.

Cover daemon/session launch refusing a TRAE-driven Autopilot row without building a loop argv.

**Step 2: Run focused tests**

Run:

```bash
cargo test --bin pmtui create_form
cargo test --bin pmtui autopilot
cargo test job_engine::tests::session
```

Expected: fail because TRAE is not modeled yet and create accepts all engines.

**Step 3: Implement gates**

Make `CreateForm::toggle_engine` cycle over `Engine::ALL`.

Keep `toggle_decider_engine` on a new `Engine::DECIDERS` constant or local `[Claude, Codex]`.

Reject TRAE when the form tier is Autopilot. Also reject `m`/autopilot enable for an existing TRAE row until hooks are implemented.

Keep loop/session builders from silently constructing a TRAE Autopilot command.

**Step 4: Verify**

Run the focused tests again. Expected: pass.

### Task 4: Add TRAE job result parsing

**Files:**
- Modify: `src/spawn/result.rs`
- Test: `src/spawn/tests/result.rs`

**Step 1: Write failing tests**

Add TRAE stream JSON fixtures:

```json
{"type":"result","subtype":"success","session_id":"s","result":"done","is_error":false}
{"type":"result","subtype":"error_during_execution","result":"bad","is_error":true}
```

Assert success maps to `JobOutcome::Done`, non-success maps to `JobOutcome::Failed`, and the last result event wins.

**Step 2: Run focused tests**

Run:

```bash
cargo test spawn::tests::result
```

Expected: fail.

**Step 3: Implement parser**

Route `Engine::Trae` in `read_job_result`. Reuse the Claude result-event fallback logic, but do not require Claude-only `structured_output`.

**Step 4: Verify**

Run the focused test again. Expected: pass.

### Task 5: Add TRAE model discovery

**Files:**
- Create: `src/models/trae.rs`
- Modify: `src/models/mod.rs`
- Modify: `src/models/tests/mod.rs`
- Create or modify: `src/models/tests/trae.rs`

**Step 1: Write failing tests**

Stub `PATH` with a `traex` script that prints:

```json
[
  {"name":"seed-1","real_name":"Seed 1","description":"fast","context_window":200000},
  {"name":"seed-2","description":"large","context_window":1000000}
]
```

Assert `available_models(Engine::Trae)` returns launch values `seed-1`, `seed-2` and useful labels.

**Step 2: Run focused tests**

Run:

```bash
cargo test models::tests::trae
```

Expected: fail because no adapter exists.

**Step 3: Implement adapter**

Run `traex models --json`, parse a list of objects with `name`, optional `config_name`, optional `real_name`, optional `description`, and optional `context_window`. Return empty on command failure or malformed output.

**Step 4: Verify**

Run the focused test again. Expected: pass.

### Task 6: Update docs and command surface text

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/SPEC.md`
- Modify: `docs/GUIDE.md`
- Modify: `src/bin/pmtui/spawn_cli.rs`

**Step 1: Update docs**

Document that `--agent` accepts `claude|codex|traex`, TRAE supports Standard and job launches, and TRAE Autopilot is intentionally disabled until hook delivery is verified.

**Step 2: Run docs-adjacent tests**

Run:

```bash
bash tests/repo_contract_test.sh
cargo test --bin pmtui spawn_cli
```

Expected: pass.

### Task 7: Final verification

**Files:**
- No new files.

**Step 1: Run focused suite**

Run:

```bash
cargo test registry
cargo test worker
cargo test spawn
cargo test models
cargo test --bin pmtui
```

Expected: pass.

**Step 2: Run required repo suite as time permits**

Run:

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

Expected: pass, or report exact remaining failures.
