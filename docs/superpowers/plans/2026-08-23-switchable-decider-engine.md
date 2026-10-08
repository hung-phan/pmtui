# Switchable Decider Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let each session choose whether its decider (the "supervisor consult") runs on `claude` or `codex`, defaulting to `claude`.

**Architecture:** Add one per-session `Config.decider_engine` field (default Claude). At the decider spawn site (`JobScheduler::spawn_advice`), dispatch on that field to build either the existing `claude` argv or a new `codex exec` argv; the codex verdict is written to a `--output-last-message` file that the existing reap reads instead of the tee'd log. Everything downstream (`advise::validate`, the auto-answer/refuse routing) is engine-agnostic and unchanged, so codex is **safe by construction** — a non-verdict reply is unparseable → `Capability` escalation → human. pmtui gets a create-form toggle and an `e` one-key flip (mirroring `m`), both writing `config.json` only. The codex path's acceptance gate is a decider-benchmark re-run.

**Tech Stack:** Rust (edition 2024), serde/serde_json, tmux `Driver` seam, `claude`/`codex` headless CLIs.

## Global Constraints

- **Run the FULL suite after every change:** `cargo test` (lib + bins + integration + doc-tests), `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check` (clean). Not `--lib` alone.
- **Before any commit/merge/"done":** run the `#[ignore]`d real-tmux acceptance: `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1` (~8 min). A green `cargo test` is NOT proof — that suite is `#[ignore]`d.
- **All spawned `claude`/`codex` children need `ECC_GATEGUARD=off`** (the ECC GateGuard hook stalls a headless agent on its first tool call).
- **Single-writer ledger:** `pmd` is the ONLY writer of `state.json`. `pmtui` acts by writing `registry.json` / `config.json` / `answers.json` / `directive.md` — never the ledger. The `decider_engine` lives in `config.json`, which pmtui already owns.
- **`decide_kind` is byte-pure** and the always-escalate safety floor is untouched by this feature.
- **Safe by construction:** never make the codex path "trust" its reply. `advise::validate` re-derives every guarantee; a bad codex reply must fail-safe to escalation, never a canned approval.
- **Commit/push only when asked; if on `main`, branch first.**
- **Reference facts (verified against the installed CLIs on 2026-08-23):**
  - codex `0.146.1` is installed; `codex exec -s read-only --skip-git-repo-check --output-last-message <file> -- <prompt>` exits 0, needs no auth (Bedrock, default model `openai.gpt-5.5`), and writes exactly the final assistant message to `<file>`. It resolved a model with NO `-m` flag → the codex model pin is **opt-in**.
  - The decider's JSON contract (echo nonce, `action` ∈ {select_option, answer, refuse}, `option_index`, `text`, `reason`) is fully described in prose in `advise::SUPERVISOR_SYSTEM_PROMPT` (Rules 1–7) and restated by `advise::build_consult_prompt`. The `claude --json-schema` flag is "belt-and-braces only." So codex needs only the prompt text — no schema flag.

---

## File Structure

- `src/state/records.rs` — **modify.** Add `Config.decider_engine: Engine` + `default_decider_engine()`.
- `src/worker/supervisor.rs` — **modify.** Add `build_supervisor_command_codex(...)` + `SUPERVISOR_CODEX_MODEL_ENV`.
- `src/worker/mod.rs` — **modify.** Re-export the two new symbols.
- `src/state/paths.rs` — **modify.** Add `ProjectPaths::advice_last_message(seq)`.
- `src/job_engine/supervisor.rs` — **modify.** `AdviseInFlight.verdict_path`; engine param on `spawn_advice`; engine-aware binary probe; engine-dispatched argv; reap reads `verdict_path` when set.
- `src/job_engine/marker.rs` — **modify.** Pass `config.decider_engine` into `spawn_advice` (line ~518).
- `tests/integration/decider_bench.rs` — **modify.** `PM_DECIDER_BENCH_ENGINE` selector, `codex_on_path`, codex builder+reader; the MUST-safety gate becomes the codex acceptance gate.
- `src/bin/pmtui/create_form.rs` — **modify.** Add the autopilot-only **Decider** toggle field.
- `src/bin/pmtui/seed.rs` — **modify.** `seed_agent_loop` gains a `decider_engine` param; set it in the `Config` literal.
- `src/bin/pmtui/app/create.rs` — **modify.** Pass `form.decider_engine` into `seed_agent_loop`.
- `src/bin/pmtui/render/create.rs` — **modify.** Render the Decider row (autopilot-only).
- `src/bin/pmtui/app/autopilot.rs` — **modify.** Add `cycle_decider_engine` (`e`), mirroring `cycle_tier`.
- `src/bin/pmtui/keys.rs` — **modify.** Dispatch `KeyCode::Char('e') => app.cycle_decider_engine()`.
- `src/bin/pmtui/bindings.rs` — **modify.** Add the `e` row (`KeyGroup::Autonomy`, `Applies::Autopilot`) + mention "decider" in the create-form `←/→` toggle help.
- `docs/SPEC.md`, `README.md` — **modify.** Record the per-session engine choice + the codex path + the benchmark gate.

**Design refinement vs. the spec:** the spec proposed a "small inline overlay" for the post-create switch. `m` (tier) is a **direct one-key flip** with no overlay; for consistency and simplicity (YAGNI) the decider switch `e` is likewise a direct flip that reads-modifies-writes `config.json` and reports via the status line. No new `UiMode`/overlay/`Scope` is added.

---

### Task 1: `Config.decider_engine` field

**Files:**
- Modify: `src/state/records.rs:52-94` (the `Config` struct + its default fns)
- Modify: `src/bin/pmtui/seed.rs:81-87` (the one production `Config { .. }` literal) — and any other `Config { .. }` literal the compiler flags
- Test: `src/state/records.rs` (inline `#[cfg(test)]` — add if none exists, else append)

**Interfaces:**
- Consumes: `crate::registry::Engine` (already `pub`, `#[serde(rename_all = "snake_case")]`, values `Claude`/`Codex`, has `.bin()` → `"claude"`/`"codex"`).
- Produces: `Config.decider_engine: Engine` (field), `default_decider_engine() -> Engine` (returns `Engine::Claude`).

- [ ] **Step 1: Write the failing tests**

Add to `src/state/records.rs` (inside/append a `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod decider_engine_tests {
    use super::*;
    use crate::registry::Engine;

    #[test]
    fn a_config_without_decider_engine_defaults_to_claude() {
        // A config written before this field existed (or by another tool) must still load,
        // and must read as Claude — the safe/validated default. This is a safety property:
        // the decider auto-approves, so an unknown/missing engine must NOT silently become codex.
        let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860}"#;
        let cfg: Config = serde_json::from_str(json).expect("legacy config must still parse");
        assert_eq!(cfg.decider_engine, Engine::Claude);
    }

    #[test]
    fn decider_engine_round_trips_codex() {
        let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860,"decider_engine":"codex"}"#;
        let cfg: Config = serde_json::from_str(json).expect("config with decider_engine must parse");
        assert_eq!(cfg.decider_engine, Engine::Codex);
        let back = serde_json::to_string(&cfg).unwrap();
        assert!(back.contains("\"decider_engine\":\"codex\""), "got {back}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib decider_engine_tests`
Expected: FAIL to COMPILE (`no field decider_engine on Config`).

- [ ] **Step 3: Add the field + default**

In `src/state/records.rs`, add to the `Config` struct (after `coordinator_lease_s`):

```rust
    /// The engine the DECIDER (supervisor consult) runs on for THIS session — independent of
    /// the WORKER engine. DEFAULTED to Claude, and the direction of that default is a safety
    /// property, exactly like `autonomy`: the decider auto-approves low-stakes decisions, and
    /// Claude is the validated path the decider benchmark covers. A `config.json` that merely
    /// MISSED this field (written before it existed, or by another tool) must read as Claude,
    /// never silently become codex. `pmtui` is the only writer of this file.
    #[serde(default = "default_decider_engine")]
    pub decider_engine: crate::registry::Engine,
```

And add the default fn beside the others (after `default_coordinator_lease_s`):

```rust
/// `Claude` — the decider's validated default. See the field doc: a config that does not SAY
/// codex has not asked for it, and the decider auto-approves, so silence must fall to the path
/// the benchmark covers.
fn default_decider_engine() -> crate::registry::Engine {
    crate::registry::Engine::Claude
}
```

- [ ] **Step 4: Fix every `Config { .. }` literal the compiler flags**

Run: `cargo build 2>&1 | grep -A3 "missing.*decider_engine\|missing field"`
For each flagged literal, add `decider_engine: Engine::Claude,` (import `Engine` if needed). The known production site is `src/bin/pmtui/seed.rs:81` — but Task 6 rewires that to a parameter, so for THIS task just add `decider_engine: crate::registry::Engine::Claude,` to make it compile; Task 6 replaces it. Fix any test-only literals the same way.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib decider_engine_tests` → PASS. Then `cargo build` clean.

- [ ] **Step 6: Full suite + lint**

Run: `cargo test && cargo clippy --all-targets && cargo fmt --all -- --check`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add src/state/records.rs src/bin/pmtui/seed.rs
git commit -m "feat(config): add per-session decider_engine (defaults to claude)"
```

---

### Task 2: `build_supervisor_command_codex` + codex model env

**Files:**
- Modify: `src/worker/supervisor.rs` (add the builder + the env const after `SUPERVISOR_BUDGET_USD_ENV`)
- Modify: `src/worker/mod.rs:26` (re-export)
- Test: `src/worker/supervisor.rs` (append to its `#[cfg(test)]` module, or add one)

**Interfaces:**
- Consumes: nothing new (mirrors the shape of `build_supervisor_command`).
- Produces:
  - `pub const SUPERVISOR_CODEX_MODEL_ENV: &str = "PM_SUPERVISOR_CODEX_MODEL";`
  - `pub fn build_supervisor_command_codex(model: Option<&str>, system_prompt: &str, prompt: &str, last_message_path: &str, timeout_s: u64, kill_grace_s: u64) -> Vec<String>`
    - `model: None` ⇒ omit `-m` (codex uses its configured default — verified live); `Some(m)` ⇒ `-m <m>`.
    - `system_prompt` is PREPENDED into the prompt (codex has no `--system-prompt`).
    - The verdict is written to `last_message_path` via `--output-last-message` (the reap reads it).

- [ ] **Step 1: Write the failing test**

Append to the test module in `src/worker/supervisor.rs`:

```rust
#[test]
fn codex_consult_argv_is_read_only_bounded_and_prepends_the_system_prompt() {
    let argv = build_supervisor_command_codex(
        Some("openai.gpt-5.5"),
        "SYS-PROMPT-MARKER",
        "USER-PROMPT-MARKER",
        "/tmp/advice-7.last",
        180,
        5,
    );
    let joined = argv.join(" ");
    // Shell-timeout bound, same outer bound as the claude path.
    assert_eq!(&argv[0], "timeout");
    assert!(argv.contains(&"-k".to_string()));
    assert!(argv.contains(&"5".to_string()) && argv.contains(&"180".to_string()));
    // codex, non-interactive, READ-ONLY (mirrors claude's --permission-mode plan: reads yes,
    // writes never — the worker owns the tree), outside-a-git-repo tolerant.
    assert!(argv.contains(&"codex".to_string()));
    assert!(argv.contains(&"exec".to_string()));
    assert!(argv.windows(2).any(|w| w == ["-s", "read-only"]));
    assert!(argv.contains(&"--skip-git-repo-check".to_string()));
    // The verdict is isolated into the last-message file (robust vs. scraping chatter).
    assert!(argv.windows(2).any(|w| w == ["--output-last-message", "/tmp/advice-7.last"]));
    // Model pin passed when Some.
    assert!(argv.windows(2).any(|w| w == ["-m", "openai.gpt-5.5"]));
    // codex has NO --json-schema / --bare / --system-prompt: the system prompt rides IN the
    // prompt, after `--`.
    assert!(!joined.contains("--json-schema"));
    assert!(!joined.contains("--system-prompt"));
    assert!(!argv.contains(&"--bare".to_string()));
    let dashdash = argv.iter().position(|a| a == "--").expect("prompt is the trailing positional");
    let tail = argv[dashdash + 1..].join(" ");
    assert!(tail.contains("SYS-PROMPT-MARKER") && tail.contains("USER-PROMPT-MARKER"));
    // NOT the claude env prefix — that is claude-hook-specific (the codex worker arm omits it too).
    assert_ne!(&argv[0], "env");
}

#[test]
fn codex_consult_omits_model_flag_when_unset() {
    let argv = build_supervisor_command_codex(None, "s", "p", "/tmp/x.last", 180, 5);
    assert!(!argv.contains(&"-m".to_string()), "no -m when the model pin is unset (codex uses its default)");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib codex_consult_argv`
Expected: FAIL to COMPILE (`cannot find function build_supervisor_command_codex`).

- [ ] **Step 3: Implement the builder + env const**

Add near `SUPERVISOR_BUDGET_USD_ENV` in `src/worker/supervisor.rs`:

```rust
/// Env pinning the CODEX decider's model (parallel to [`SUPERVISOR_MODEL_ENV`]). UNSET BY
/// DEFAULT — unlike claude, `codex exec` resolves a model on its own (verified live: it ran on
/// `openai.gpt-5.5` with no `-m`), so we pass `-m` only when the operator names one, rather than
/// inventing a default id that is wrong on another deployment.
pub const SUPERVISOR_CODEX_MODEL_ENV: &str = "PM_SUPERVISOR_CODEX_MODEL";
```

And the builder (with a doc comment explaining the shape and why it differs from the claude one):

```rust
/// Build the argv for ONE supervisor consult run on **codex** instead of claude.
///
/// A different shape from [`build_supervisor_command`], because `codex exec` has none of the
/// claude flags that builder leans on: no `--bare`, `--json-schema`, `--output-format json`,
/// `--system-prompt`, or `--permission-mode`. So:
///
/// - `-s read-only` is codex's equivalent of claude's `--permission-mode plan`: the decider may
///   read/search the tree (the whole point — it verifies the claim it is judging) but may NOT
///   write (the worker owns this tree; two writers corrupt it). Verified against codex 0.146.1.
/// - The system instructions are PREPENDED into the prompt, since there is no `--system-prompt`.
///   `advise::validate` does not care how they were delivered, and the decider's JSON contract is
///   fully described in that prose (it is not carried by any schema flag).
/// - The verdict is captured with `--output-last-message <path>`: codex writes ONLY its final
///   assistant message there, so the reap reads a clean object rather than scraping the log.
/// - `-m` is passed ONLY when `model` is `Some` — see [`SUPERVISOR_CODEX_MODEL_ENV`].
/// - `timeout -k <grace> <secs>` is the identical outer bound as the claude path.
/// - NO `env -u CLAUDECODE ECC_GATEGUARD=off` prefix: that is claude-hook-specific (the codex
///   worker arm in [`super::build_command`]'s codex form omits it too).
///
/// Like the claude builder this is PURE over its arguments (env reads happen at the call site).
/// Safe by construction: whatever codex emits, [`crate::advise::validate`] re-derives every
/// guarantee, so a non-verdict reply escalates to a human rather than being trusted.
pub fn build_supervisor_command_codex(
    model: Option<&str>,
    system_prompt: &str,
    prompt: &str,
    last_message_path: &str,
    timeout_s: u64,
    kill_grace_s: u64,
) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    argv.push("timeout".into());
    argv.push("-k".into());
    argv.push(kill_grace_s.to_string());
    argv.push(timeout_s.to_string());
    argv.push("codex".into());
    argv.push("exec".into());
    argv.push("-s".into());
    argv.push("read-only".into());
    argv.push("--skip-git-repo-check".into());
    argv.push("--output-last-message".into());
    argv.push(last_message_path.to_string());
    if let Some(m) = model {
        argv.push("-m".into());
        argv.push(m.to_string());
    }
    // No --system-prompt on codex: fold it into the prompt head. The prompt is the trailing
    // positional after `--`, same three reasons as the claude builder (dash-leading text,
    // subcommand shadowing, variadic flags).
    argv.push("--".into());
    argv.push(format!("{system_prompt}\n\n{prompt}"));
    argv
}
```

- [ ] **Step 4: Re-export**

In `src/worker/mod.rs:26`, add the two symbols to the `pub use supervisor::{...}` list:

```rust
    SUPERVISOR_BUDGET_USD_ENV, SUPERVISOR_CODEX_MODEL_ENV, SUPERVISOR_MODEL, SUPERVISOR_MODEL_ENV,
    build_supervisor_command, build_supervisor_command_codex,
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib codex_consult` → PASS.

- [ ] **Step 6: Full suite + lint**

Run: `cargo test && cargo clippy --all-targets && cargo fmt --all -- --check` → green.

- [ ] **Step 7: Commit**

```bash
git add src/worker/supervisor.rs src/worker/mod.rs
git commit -m "feat(worker): add build_supervisor_command_codex for the codex decider path"
```

---

### Task 3: `ProjectPaths::advice_last_message(seq)`

**Files:**
- Modify: `src/state/paths.rs` (add beside `advice_log`, ~line 119)
- Test: `src/state/paths.rs` (append to its `#[cfg(test)]` module)

**Interfaces:**
- Produces: `pub fn advice_last_message(&self, seq: u64) -> PathBuf` → `{steps_dir}/advice-{seq}.last`.

- [ ] **Step 1: Write the failing test**

Append to the tests in `src/state/paths.rs`:

```rust
#[test]
fn advice_last_message_is_a_per_seq_sibling_of_the_advice_log() {
    let p = ProjectPaths::new(std::path::PathBuf::from("/tmp/proj"));
    assert_eq!(p.advice_last_message(7), p.steps_dir().join("advice-7.last"));
    // Distinct from the log and the done-signal so a codex verdict file cannot collide with them.
    assert_ne!(p.advice_last_message(7), p.advice_log(7));
    assert_ne!(p.advice_last_message(7), p.advice_done_signal(7));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib advice_last_message`
Expected: FAIL to COMPILE (no such method).

- [ ] **Step 3: Implement**

Add after `advice_log` (~line 119) in `src/state/paths.rs`:

```rust
    /// Where a **codex** decider consult writes its final message (`--output-last-message`),
    /// which the reap reads as the verdict. Its own `advice-<seq>.last` namespace, like
    /// [`ProjectPaths::advice_log`], so a consult's verdict file can never collide with its log
    /// or done-signal. (The claude path reads its verdict from the tee'd log and never touches
    /// this file.)
    pub fn advice_last_message(&self, seq: u64) -> PathBuf {
        self.steps_dir().join(format!("advice-{seq}.last"))
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib advice_last_message` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/state/paths.rs
git commit -m "feat(state): add advice_last_message path for the codex verdict file"
```

---

### Task 4: Thread the decider engine into the consult (spawn + probe + reap)

**Files:**
- Modify: `src/job_engine/supervisor.rs` (the `AdviseInFlight` struct ~78; `spawn_advice` ~172; `supervisor_binary_present` ~337; the reap read in `advise_step` ~441)
- Modify: `src/job_engine/marker.rs:518` (the one `spawn_advice` call — `config` is in scope as `dispose_report`'s param)
- Test: `src/job_engine/tests/supervisor.rs` (append — mirror the existing consult-argv test, which drives `spawn_advice` through a `FakeDriver` and inspects the argv passed to `spawn_step`)

**Interfaces:**
- Consumes: `Config.decider_engine` (Task 1); `build_supervisor_command_codex` + `SUPERVISOR_CODEX_MODEL_ENV` (Task 2); `ProjectPaths::advice_last_message` (Task 3); `crate::registry::Engine` (`.bin()`).
- Produces: `spawn_advice(&mut self, driver, now, next, auto, decider_engine: crate::registry::Engine)` (new trailing param); `AdviseInFlight.verdict_path: Option<std::path::PathBuf>` (Some for codex, None for claude).

- [ ] **Step 1: Write the failing tests**

Append to `src/job_engine/tests/supervisor.rs`. Mirror the existing test that spawns a consult through the `FakeDriver` and reads the recorded `spawn_step` argv; add a codex twin (find the existing test that asserts the claude argv — reuse its scaffold verbatim, changing only the seeded `decider_engine` and the assertions):

```rust
#[test]
fn a_codex_session_builds_the_codex_consult_argv_and_arms_the_last_message_file() {
    // <same scaffold as the existing claude consult-argv test: a JobScheduler over a FakeDriver,
    //  an auto-flow report that reaches spawn_advice> — but seed config.decider_engine = Codex.
    // Assert the argv the FakeDriver captured for the `pmsup-` spawn_step:
    let argv = /* the captured spawn_step argv, as the existing test reads it */;
    assert!(argv.contains(&"codex".to_string()) && argv.contains(&"exec".to_string()));
    assert!(argv.windows(2).any(|w| w == ["-s", "read-only"]));
    assert!(argv.iter().any(|a| a.ends_with(".last")), "codex arms --output-last-message");
    assert!(!argv.contains(&"claude".to_string()));
    // And the in-flight record points the reap at the last-message file.
    // (Expose it however the existing test reaches JobScheduler internals — e.g. a test helper
    //  returning `self.advise.as_ref().unwrap().verdict_path.clone()`.)
}

#[test]
fn a_claude_session_still_builds_the_claude_consult_argv_with_no_verdict_file() {
    // Same scaffold, decider_engine = Claude (the default). Regression guard:
    let argv = /* captured spawn_step argv */;
    assert!(argv.contains(&"claude".to_string()) && argv.contains(&"--json-schema".to_string()));
    assert!(!argv.contains(&"codex".to_string()));
    // verdict_path is None → the reap reads the tee'd log, exactly as before.
}
```

If the existing suite has no test that inspects `spawn_step` argv, add a minimal `FakeDriver` that records the argv of the `pmsup-`-prefixed `spawn_step` call and assert against it. Keep the seeded report/`auto` identical between the two tests so only `decider_engine` differs.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib consult_argv`
Expected: FAIL to COMPILE (`spawn_advice` arity; `verdict_path` field).

- [ ] **Step 3: Add `verdict_path` to `AdviseInFlight`**

In `src/job_engine/supervisor.rs`, add to the `AdviseInFlight` struct (~line 78):

```rust
    /// Where the reap reads this consult's verdict FROM. `None` ⇒ the claude path: read the
    /// tee'd `handle.log` (a `--output-format json` envelope). `Some(path)` ⇒ the codex path:
    /// read `codex exec --output-last-message <path>`, which holds ONLY the final message, so no
    /// log-scraping. Engine-agnostic at the reap: it just reads this path if set, else the log.
    pub(super) verdict_path: Option<std::path::PathBuf>,
```

- [ ] **Step 4: Make the binary probe engine-aware**

Change `supervisor_binary_present` (~337) to take the bin name (the cache is a single session-lived `Option<bool>`; the decider engine is fixed per session, so probing the selected engine's binary is correct):

```rust
    fn supervisor_binary_present(&mut self, bin: &str) -> bool {
        *self.advise_binary.get_or_insert_with(|| binary_on_path(bin))
    }
```

- [ ] **Step 5: Dispatch the argv in `spawn_advice`**

Change the `spawn_advice` signature to accept the engine, and replace the model resolution + `build_supervisor_command` call + binary probe + the pre-spawn file clears. Concretely:

Signature (~172):
```rust
    pub(super) fn spawn_advice(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        next: &AgentLoopState,
        auto: &[(String, crate::worker::StopDraft)],
        decider_engine: crate::registry::Engine,
    ) -> Result<Option<JobTick>> {
```

Binary probe (~192) — probe the SELECTED engine's binary and name it in the latch:
```rust
        let decider_bin = decider_engine.bin();
        if !self.supervisor_binary_present(decider_bin) {
            self.advise_health.latch_off(
                &self.project_id,
                &format!("`{decider_bin}` is not on PATH, so no consult can run"),
            );
            return Ok(None);
        }
```

The `SUPERVISOR_BIN` const (used only by this probe) is now the claude fallback name; keep it for the doc-comment reference but the probe uses `decider_bin`.

Argv + verdict_path (replace the `let argv = worker::build_supervisor_command(...)` block, ~250):
```rust
        let (argv, verdict_path) = match decider_engine {
            crate::registry::Engine::Claude => {
                let model = std::env::var(worker::SUPERVISOR_MODEL_ENV)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| worker::SUPERVISOR_MODEL.to_string());
                let argv = worker::build_supervisor_command(
                    &model,
                    &system_prompt,
                    advise::OUTPUT_SCHEMA,
                    &advise::build_consult_prompt(&consult),
                    SUPERVISOR_SHELL_TIMEOUT_S,
                    SUPERVISOR_KILL_GRACE_S,
                    std::env::var(worker::SUPERVISOR_BUDGET_USD_ENV)
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .as_deref(),
                );
                (argv, None)
            }
            crate::registry::Engine::Codex => {
                // Opt-in model pin (codex resolves a default itself); no --json-schema/--budget.
                let model = std::env::var(worker::SUPERVISOR_CODEX_MODEL_ENV)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let last = self.paths.advice_last_message(seq);
                let argv = worker::build_supervisor_command_codex(
                    model.as_deref(),
                    &system_prompt,
                    &advise::build_consult_prompt(&consult),
                    &last.to_string_lossy(),
                    SUPERVISOR_SHELL_TIMEOUT_S,
                    SUPERVISOR_KILL_GRACE_S,
                );
                (argv, Some(last))
            }
        };
```

Clear a stale codex verdict file too, beside the existing `remove_file(&done)`/`remove_file(&log)` (~278):
```rust
        if let Some(vp) = &verdict_path {
            let _ = std::fs::remove_file(vp);
        }
```

Set the field when constructing `AdviseInFlight` (~311): add `verdict_path,` to the struct literal.

- [ ] **Step 6: Reap reads the verdict source**

In `advise_step`, change the exit-0 read (~441) from the hardcoded log to the engine-agnostic source:

```rust
        // Exit 0. Read the verdict from wherever this engine put it: the codex path isolates it in
        // `verdict_path` (--output-last-message); the claude path leaves it in the tee'd log. An
        // unreadable/absent source is an empty string, which `validate` refuses as `NoJson` — no
        // unwrap on untrusted bytes on either path.
        let source = inflight.verdict_path.as_deref().unwrap_or(&inflight.handle.log);
        let raw = std::fs::read_to_string(source).unwrap_or_default();
```

- [ ] **Step 7: Pass the engine at the call site**

In `src/job_engine/marker.rs:518`, change:
```rust
                    if let Some(tick) = self.spawn_advice(driver, now, &next, &auto, config.decider_engine)? {
```
(`config` is `dispose_report`'s `&Config` param, already in scope.)

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --lib consult_argv` → PASS. Then `cargo build` clean.

- [ ] **Step 9: Full suite + lint**

Run: `cargo test && cargo clippy --all-targets && cargo fmt --all -- --check` → green. (The existing supervisor/marker/fallback tests exercise the claude regression path — they must stay green, proving the default is unchanged.)

- [ ] **Step 10: Commit**

```bash
git add src/job_engine/supervisor.rs src/job_engine/marker.rs src/job_engine/tests/supervisor.rs
git commit -m "feat(job-engine): dispatch the decider consult on config.decider_engine"
```

---

### Task 5: Decider benchmark — codex acceptance gate

**Files:**
- Modify: `tests/integration/decider_bench.rs` (the `model()`/`claude_on_path()`/`run_headless()` helpers ~395-574; the `#[ignore]` runner's skip gate ~1331)
- Test: the module's existing deterministic self-tests stay; add one for the engine selector

**Interfaces:**
- Consumes: `build_supervisor_command_codex`, `SUPERVISOR_CODEX_MODEL_ENV` (Task 2).
- Produces: `PM_DECIDER_BENCH_ENGINE` (`claude` default | `codex`); a codex `run_headless` path that reads a temp `--output-last-message` file. The catalog, scorer, and MUST-safety gate are UNCHANGED — running them against codex IS the acceptance gate.

- [ ] **Step 1: Write the failing test**

Add a deterministic (non-`#[ignore]`) self-test to the module:

```rust
#[test]
fn the_bench_engine_selector_defaults_to_claude() {
    // Pure: no env set ⇒ Claude. (Set/clear is process-global, so assert only the default here.)
    assert_eq!(bench_engine_from(None), Engine::Claude);
    assert_eq!(bench_engine_from(Some("codex")), Engine::Codex);
    assert_eq!(bench_engine_from(Some("CLAUDE")), Engine::Claude);
    assert_eq!(bench_engine_from(Some("nonsense")), Engine::Claude); // unknown ⇒ safe default
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test integration the_bench_engine_selector`
Expected: FAIL to COMPILE (no `bench_engine_from`).

- [ ] **Step 3: Add the selector + codex probe + codex runner**

Add to `tests/integration/decider_bench.rs`:

```rust
/// Pure parse of `PM_DECIDER_BENCH_ENGINE` — unknown/absent ⇒ Claude (the safe default the
/// benchmark has always run). Split out so the default is unit-tested without touching env.
fn bench_engine_from(v: Option<&str>) -> Engine {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("codex") => Engine::Codex,
        _ => Engine::Claude,
    }
}

fn bench_engine() -> Engine {
    bench_engine_from(std::env::var("PM_DECIDER_BENCH_ENGINE").ok().as_deref())
}

fn codex_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false; };
    std::env::split_paths(&path).any(|dir| is_executable_file(&dir.join("codex")))
}

/// The codex model pin (opt-in). None ⇒ no `-m` (codex uses its default).
fn codex_model() -> Option<String> {
    std::env::var(SUPERVISOR_CODEX_MODEL_ENV).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
```

Add `build_supervisor_command_codex` and `SUPERVISOR_CODEX_MODEL_ENV` to the `use agent_manager::worker::{...}` import (line 41). Add a codex branch to `run_headless` — it must write to a temp last-message file and return its contents as `raw` (mirroring production's read of that file). Since a codex consult writes the verdict to `--output-last-message`, read that file after the process exits:

```rust
/// Run ONE headless decider consult on the selected engine, returning (combined-or-verdict text,
/// exit code). For claude: combined stdout+stderr (unchanged). For codex: the --output-last-message
/// file's contents (the isolated verdict), matching what production's reap reads.
fn run_headless(engine: Engine, model: &str, system: &str, schema: &str, prompt: &str) -> Result<(String, Option<i32>), String> {
    match engine {
        Engine::Claude => {
            let argv = build_supervisor_command(model, system, schema, prompt, CONSULT_TIMEOUT_S, CONSULT_KILL_GRACE_S, None);
            run_argv(&argv)
        }
        Engine::Codex => {
            // A unique temp last-message path per consult (nonce keeps it collision-free).
            let last = std::env::temp_dir().join(format!("decider-bench-codex-{}.last", mint_uuid_v4()));
            let _ = std::fs::remove_file(&last);
            let argv = build_supervisor_command_codex(codex_model().as_deref(), system, prompt, &last.to_string_lossy(), CONSULT_TIMEOUT_S, CONSULT_KILL_GRACE_S);
            let (_combined, code) = run_argv(&argv)?;
            let verdict = std::fs::read_to_string(&last).unwrap_or_default();
            let _ = std::fs::remove_file(&last);
            Ok((verdict, code))
        }
    }
}

/// Spawn an argv and capture combined stdout+stderr + exit code (the old `run_headless` body).
fn run_argv(argv: &[String]) -> Result<(String, Option<i32>), String> {
    match Command::new(&argv[0]).args(&argv[1..]).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            Ok((s, o.status.code()))
        }
        Err(e) => Err(format!("spawn failed: {e}")),
    }
}
```

Thread `engine` through `run_decider` (its call to `run_headless` gains `engine`; the codex path ignores `schema`). The quality JUDGE stays on `claude` unconditionally — it is a separate grader, not the decider — so `run_quality_judge` keeps calling `run_headless(Engine::Claude, …)`.

- [ ] **Step 4: Gate the runner on the selected engine**

In `the_decider_bench_scores_the_catalog` (~1331) replace the claude-only gate:

```rust
    let engine = bench_engine();
    let present = match engine { Engine::Claude => claude_on_path(), Engine::Codex => codex_on_path() };
    if !present {
        eprintln!("skipping decider-bench: no `{}` on PATH", engine.bin());
        return;
    }
    eprintln!("decider-bench: engine={}, model={model}, ...", engine.bin());
```

Pass `engine` into every `run_decider(...)` call. The MUST-safety hard gate (gate 1) and the infra gate (gate 2) are unchanged — they now assert against whichever engine was selected.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test integration the_bench_engine_selector the_scorer_self_test the_catalog_is_inside` → PASS (deterministic self-tests, no LLM).

- [ ] **Step 6: Full suite + lint**

Run: `cargo test && cargo clippy --all-targets && cargo fmt --all -- --check` → green.

- [ ] **Step 7: THE CODEX ACCEPTANCE GATE — run the benchmark on codex**

Run:
```bash
ECC_GATEGUARD=off PM_DECIDER_BENCH=1 PM_DECIDER_BENCH_ENGINE=codex \
  cargo test --test integration decider_bench -- --ignored --nocapture
```
Expected: the run completes with **no `DECIDER MUST-CASE FAILURE`** (gate 1) and **no `HARNESS ERROR`** (gate 2). Record the scorecard (accuracy, MUST N/N) in the commit body. If a MUST case fails on codex, the codex path is NOT acceptable to ship on-by-default — STOP and report (the fail-safe design still holds — codex can only over-escalate at runtime — but a MUST miss means codex mis-approved a dangerous action in the bench, which is a hard block). Also run the claude arm once to confirm no regression: same command without `PM_DECIDER_BENCH_ENGINE`.

- [ ] **Step 8: Commit**

```bash
git add tests/integration/decider_bench.rs
git commit -m "test(decider-bench): run the catalog against a selectable engine (codex gate)

Scorecard (codex): MUST N/N, accuracy X%. <paste the run summary>"
```

---

### Task 6: Create-form Decider field

**Files:**
- Modify: `src/bin/pmtui/create_form.rs` (field indices, `shows_*`, `adjust`, `toggle_decider_engine`)
- Modify: `src/bin/pmtui/seed.rs:63-88` (`seed_agent_loop` gains `decider_engine`, set in the `Config` literal)
- Modify: `src/bin/pmtui/app/create.rs:144-151` (pass `form.decider_engine`)
- Modify: `src/bin/pmtui/render/create.rs` (push the Decider row, autopilot-only)
- Test: `src/bin/pmtui/tests/create_form.rs` (append)

**Interfaces:**
- Consumes: `Config.decider_engine` (Task 1); `CreateForm` (existing).
- Produces: `CreateForm.decider_engine: Engine`; `CreateForm::DECIDER: usize` (= 5); `CreateForm::FIELDS` becomes 6; `shows_decider()` (autopilot-only); `toggle_decider_engine()`. `seed_agent_loop(paths, tier, engine, decider_engine, brief, cadence_s, now)`.

- [ ] **Step 1: Write the failing tests**

Append to `src/bin/pmtui/tests/create_form.rs`:

```rust
#[test]
fn decider_field_is_autopilot_only_and_defaults_to_claude() {
    let mut form = CreateForm::new();
    assert_eq!(form.decider_engine, Engine::Claude);
    // Standard: the field is not shown / not navigable (like Goal + Cadence).
    form.tier = Tier::Standard;
    assert!(!form.shows_field(CreateForm::DECIDER));
    // Autopilot: shown, and ←/→ on it flips the engine.
    form.tier = Tier::Autopilot;
    assert!(form.shows_field(CreateForm::DECIDER));
    form.field = CreateForm::DECIDER;
    form.adjust(true);
    assert_eq!(form.decider_engine, Engine::Codex);
    form.adjust(true);
    assert_eq!(form.decider_engine, Engine::Claude);
}

#[test]
fn tab_navigation_visits_the_decider_field_only_on_autopilot() {
    let mut form = CreateForm::new();
    form.tier = Tier::Autopilot;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..CreateForm::FIELDS { form.next_field(); seen.insert(form.field); }
    assert!(seen.contains(&CreateForm::DECIDER));
    let mut form2 = CreateForm::new(); // Standard
    let mut seen2 = std::collections::HashSet::new();
    for _ in 0..CreateForm::FIELDS { form2.next_field(); seen2.insert(form2.field); }
    assert!(!seen2.contains(&CreateForm::DECIDER));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --bin pmtui decider_field`
Expected: FAIL to COMPILE (`DECIDER` / `decider_engine` / `toggle_decider_engine` missing).

- [ ] **Step 3: Extend `CreateForm`**

In `src/bin/pmtui/create_form.rs`:
- Add field: `pub(crate) decider_engine: Engine,` and doc it as "the engine the DECIDER runs on, autopilot-only (only meaningful when pmd drives), seeds `config.decider_engine`."
- `const FIELDS: usize = 6;` and `pub(crate) const DECIDER: usize = 5;`
- `new()`: add `decider_engine: Engine::Claude,`.
- `shows_field`: add arm `Self::DECIDER => self.shows_decider(),`.
- Add `pub(crate) fn shows_decider(&self) -> bool { self.tier == Tier::Autopilot }` (doc: autopilot-only, like goal/cadence — a decider engine on a Standard row configures a consult that never runs).
- Add `pub(crate) fn toggle_decider_engine(&mut self) { self.decider_engine = match self.decider_engine { Engine::Claude => Engine::Codex, Engine::Codex => Engine::Claude }; }`.
- `adjust`: add arm `Self::DECIDER => self.toggle_decider_engine(),`.
- `is_text_field`/`text_field` are unchanged (Decider is a toggle, not text).

- [ ] **Step 4: Thread through `seed_agent_loop`**

In `src/bin/pmtui/seed.rs`, add a `decider_engine: Engine` param to `seed_agent_loop` (after `engine`) and set it in the `Config` literal (~81):
```rust
    let cfg = Config {
        autonomy: tier,
        step_timeout_s: 1800,
        max_failures: 3,
        stuck_threshold: 3,
        coordinator_lease_s: 1860,
        decider_engine,
    };
```
Doc the param: "the engine the decider runs on for this session — seeded from the create form's Decider field (Claude on Standard, where it is inert)."

In `src/bin/pmtui/app/create.rs:144`, pass it:
```rust
        if let Err(e) = seed_agent_loop(
            &session_paths,
            tier,
            form.engine,
            form.decider_engine,
            form.goal.trim(),
            cadence,
            now,
        ) {
```
(On a Standard create `shows_decider()` is false and the field kept its `Claude` default — seeding Claude there is correct and inert.)

Fix the other `seed_agent_loop` callers the compiler flags (e.g. `src/daemon/tests/mod.rs`, `src/bin/pmtui/tests/*`) by adding `Engine::Claude,` in the new position.

- [ ] **Step 5: Render the row**

In `src/bin/pmtui/render/create.rs`, after the cadence row push, add (mirroring the cadence block):
```rust
    // AUTOPILOT-ONLY (see `CreateForm::shows_decider`): the decider only runs on a row pmd
    // drives, so on Standard this dial configures a consult that never happens.
    if form.shows_decider() {
        rows.push(field_row(
            CreateForm::DECIDER,
            "Decider",
            format!("< {} >", form.decider_engine.label()),
        ));
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --bin pmtui decider_field tab_navigation_visits` → PASS.

- [ ] **Step 7: Verify the render live (real pmtui, per the pmtui-ui-testing skill)**

Build the real binary (`cargo build`), lay a scratch registry, launch pmtui on a scratch tmux server, press `n`, toggle Autonomy to Autopilot, and `capture-pane` to confirm a `Decider   < claude >` row appears and `←/→` flips it to `< codex >`. (A `cargo test --bins` binary is STALE for this — use `cargo build`.) Clean up both scratch tmux servers.

- [ ] **Step 8: Full suite + lint**

Run: `cargo test && cargo clippy --all-targets && cargo fmt --all -- --check` → green.

- [ ] **Step 9: Commit**

```bash
git add src/bin/pmtui/create_form.rs src/bin/pmtui/seed.rs src/bin/pmtui/app/create.rs src/bin/pmtui/render/create.rs src/bin/pmtui/tests/create_form.rs src/daemon/tests/mod.rs
git commit -m "feat(pmtui): create-form Decider toggle (autopilot-only), seeds config.decider_engine"
```

---

### Task 7: `e` — flip the decider engine on an existing session

**Files:**
- Modify: `src/bin/pmtui/app/autopilot.rs` (add `cycle_decider_engine`, mirroring `cycle_tier`)
- Modify: `src/bin/pmtui/keys.rs:386` area (add `KeyCode::Char('e') => app.cycle_decider_engine()`)
- Modify: `src/bin/pmtui/bindings.rs:299` area (add the `e` row; update the create-form `←/→` toggle help)
- Test: `src/bin/pmtui/tests/autopilot.rs` (append)

**Interfaces:**
- Consumes: `Config.decider_engine` (Task 1); `state::read_json`/`write_json_atomic`; `Engine`.
- Produces: `App::cycle_decider_engine(&mut self)` — reads the selected session's `config.json`, flips `decider_engine`, writes it back atomically; refuses on a non-autopilot row with a status that names `m` (mirroring how `c`/`g` refuse off-autopilot).

- [ ] **Step 1: Write the failing tests**

Append to `src/bin/pmtui/tests/autopilot.rs` (mirror the existing `cycle_tier`/config-write test scaffold in that file):

```rust
#[test]
fn e_flips_the_decider_engine_in_config_only() {
    // <scaffold: an App over a scratch registry with one AUTOPILOT agent-loop session,
    //  config.json seeded decider_engine = claude>
    app.cycle_decider_engine();
    let cfg: Config = state::read_json(&session_paths.config()).unwrap();
    assert_eq!(cfg.decider_engine, Engine::Codex);
    app.cycle_decider_engine();
    let cfg: Config = state::read_json(&session_paths.config()).unwrap();
    assert_eq!(cfg.decider_engine, Engine::Claude);
    // The ledger (state.json) was never written by pmtui (single-writer invariant) —
    // assert it is unchanged as the existing autopilot tests do.
}

#[test]
fn e_refuses_on_a_standard_row_and_points_at_m() {
    // <scaffold: a STANDARD session selected>
    app.cycle_decider_engine();
    assert!(app.status.contains('m'), "status must name the m key; got {:?}", app.status);
    // config.json unchanged (the decider never runs on Standard).
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --bin pmtui e_flips_the_decider_engine e_refuses_on_a_standard`
Expected: FAIL to COMPILE (`cycle_decider_engine` missing).

- [ ] **Step 3: Implement `cycle_decider_engine`**

Add to `src/bin/pmtui/app/autopilot.rs` (mirror `cycle_tier`'s selection + `entry_state_paths` + config read-modify-write). Refuse when the session is not an autopilot row (the decider only runs where pmd drives):

```rust
    /// `e`: flip the SELECTED session's DECIDER engine (claude ⇄ codex) — the engine the
    /// supervisor consult runs on. A direct config flip, mirroring `m`: it reads/modifies/writes
    /// `config.json` only (never the ledger). AUTOPILOT-ONLY, for the same reason as `g`/`c`/`i`:
    /// the decider consult never runs on a row pmd does not drive, so flipping it on Standard would
    /// configure something inert — refuse and point at `m`.
    pub(crate) fn cycle_decider_engine(&mut self) {
        let Some(id) = self.selected_view().map(|v| v.id.clone()) else {
            self.status = "e switches a session's decider engine (nothing is selected)".into();
            return;
        };
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        let paths = entry_state_paths(entry);
        let cfg_path = paths.config();
        match state::read_json::<Config>(&cfg_path) {
            Ok(mut c) => {
                if c.autonomy != Tier::Autopilot {
                    self.status = format!(
                        "{id}: the decider only runs on autopilot — press m to turn it on; engine unchanged"
                    );
                    self.refresh();
                    return;
                }
                let next = match c.decider_engine {
                    Engine::Claude => Engine::Codex,
                    Engine::Codex => Engine::Claude,
                };
                c.decider_engine = next;
                match state::write_json_atomic(&cfg_path, &c) {
                    Ok(()) => self.status = format!("{id} → decider: {}", next.label()),
                    Err(e) => self.status = format!("{id}: could not set the decider engine: {e}"),
                }
                self.refresh();
            }
            Err(e) => {
                self.status = format!("{id}: config unreadable ({e}) — decider engine unchanged");
                self.refresh();
            }
        }
    }
```

- [ ] **Step 4: Dispatch `e`**

In `src/bin/pmtui/keys.rs` (Normal-scope char match, beside `Char('m')` at line 386):
```rust
        KeyCode::Char('e') => app.cycle_decider_engine(),
```

- [ ] **Step 5: Add the binding row + update the toggle help**

In `src/bin/pmtui/bindings.rs`, add after the `c` (Cadence) row (~309):
```rust
    // The decider ENGINE, autopilot-only like `c`/`g`/`i`: the supervisor consult that `e` retargets
    // never runs on a row pmd does not drive. A direct flip (claude ⇄ codex), mirroring `m`. High
    // shed rank — set-once, and the current value is not otherwise on the row.
    Binding {
        key: "e",
        label: "Decider",
        help: "Switch an autopilot session's decider engine (claude ⇄ codex)",
        group: KeyGroup::Autonomy,
        scope: Scope::Normal,
        applies: Applies::Autopilot,
        rank: 9,
    },
```
And update the create-form `←/→` toggle help (~373) to mention the new toggle:
```rust
        help: "Create form: change a toggle (engine, autonomy, cadence, decider)",
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --bin pmtui e_flips_the_decider_engine e_refuses_on_a_standard` → PASS. (The bindings-drift test `every_bound_normal_key_is_documented_in_the_help` must also pass — the new row documents `e`.)

- [ ] **Step 7: Full suite + lint**

Run: `cargo test && cargo clippy --all-targets && cargo fmt --all -- --check` → green.

- [ ] **Step 8: Commit**

```bash
git add src/bin/pmtui/app/autopilot.rs src/bin/pmtui/keys.rs src/bin/pmtui/bindings.rs src/bin/pmtui/tests/autopilot.rs
git commit -m "feat(pmtui): e flips an autopilot session's decider engine (claude <-> codex)"
```

---

### Task 8: Docs — SPEC.md + README

**Files:**
- Modify: `docs/SPEC.md` (the decider/supervisor section)
- Modify: `README.md` ("How it works" step 4; the Keys table; optionally an FAQ entry)

**Interfaces:** none (prose).

- [ ] **Step 1: Update the SPEC**

In `docs/SPEC.md`, find the decider ("supervisor consult") section (search `supervisor consult` / `decider`). Add a subsection documenting: the per-session `config.decider_engine` (default Claude, `#[serde(default)]` → Claude for legacy configs); the codex consult shape (`codex exec -s read-only --skip-git-repo-check --output-last-message`, opt-in `PM_SUPERVISOR_CODEX_MODEL`, system prompt folded into the prompt); WHY it is safe by construction (`advise::validate` re-derives every guarantee → a non-verdict codex reply escalates); and that the codex path's acceptance is the decider-benchmark MUST-safety gate (`PM_DECIDER_BENCH_ENGINE=codex`). Keep the rationale, per the SPEC's lockstep rule.

- [ ] **Step 2: Update the README**

- "How it works" step 4 (line ~135): change "auto-handled by a `claude -p` **decider** consult" to note the decider runs on the session's chosen engine (claude by default, codex optional).
- Keys table (line ~116): add a row for `e` — "switch the decider engine (autopilot)".
- Optionally add a one-line FAQ pointer: "What engine does the decider use? Claude by default; press `e` on an autopilot row to switch to codex."

- [ ] **Step 3: Verify no broken anchors**

Run: `grep -n "decider" README.md docs/SPEC.md | head` and eyeball the edited sections.

- [ ] **Step 4: Commit**

```bash
git add docs/SPEC.md README.md
git commit -m "docs: document the switchable decider engine (config, codex path, e key, gate)"
```

---

### Task 9: End-to-end verification + merge

**Files:** none (verification).

- [ ] **Step 1: Full suite + lint (final)**

Run: `cargo test && cargo clippy --all-targets && cargo fmt --all -- --check` → all green.

- [ ] **Step 2: Real-tmux acceptance (mandatory before merge)**

Run: `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1`
Expected: all `#[ignore]`d real-tmux acceptance tests pass (~8 min). Assert the OUTPUT, not just the exit code.

- [ ] **Step 3: The codex acceptance gate (re-confirm from Task 5)**

Run both arms and confirm gate 1 (MUST safety) + gate 2 (no infra) are green on each:
```bash
ECC_GATEGUARD=off PM_DECIDER_BENCH=1 PM_DECIDER_BENCH_ENGINE=codex cargo test --test integration decider_bench -- --ignored --nocapture
ECC_GATEGUARD=off PM_DECIDER_BENCH=1 cargo test --test integration decider_bench -- --ignored --nocapture
```
If the codex arm passes gate 1, the codex path is safe to ship on-by-default. If not, keep the feature but document that codex is behind a known-gap note (the runtime fail-safe still holds — codex can only over-escalate — but a MUST miss in the bench blocks shipping it as an equal default).

- [ ] **Step 4: Live smoke (optional but recommended)**

With `pmd`/`pmtui` running (`ECC_GATEGUARD=off`), create an autopilot session, press `e` to switch it to codex, and confirm `config.json` shows `"decider_engine":"codex"` and the row's status reflects the switch. This exercises the create → `e` → config-write path end to end.

- [ ] **Step 5: Merge (only when the user asks)**

Merge the feature branch to `main` once every gate above is green. Per the user's standing preference, tested/green branches merge without a permission prompt — but this feature's codex gate (Step 3) MUST be green first.
