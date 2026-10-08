# Model Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user pick the MODEL (not just the engine) for both the decider and the worker, with the model list derived from provider-supported sources (Claude: explicit settings allowlist or direct-account aliases; Codex: `codex debug models`), built on one small discovery module + one reusable picker so it is easy to scale.

**Architecture:** A new `src/models/` lib module discovers `[ModelInfo { label, value }]` per engine (graceful, cached in pmtui). Two serde-defaulted `Option<String>` fields store the launch-ready value: `Config.decider_model` (config.json) and `ProjectEntry.worker_model` (registry.json) — both pmtui-written, pmd-read. Launch paths pass the stored value verbatim (`--model` for claude, `-m` for codex; omitted when `None`). pmtui replaces the engine-only `DeciderPicker` with a reusable `ModelPicker` (Engine→Model stages for the decider via `e`; Model-only for the worker via `w`) and adds a create-form worker-model field.

**Tech Stack:** Rust, serde/serde_json, ratatui 0.30.2 (TestBackend), crossterm, tmux `Driver` seam, the decider benchmark + real-tmux acceptance harness.

## Global Constraints

- **Single-writer ledger.** pmtui writes ONLY `config.json` (decider) and `registry.json` (worker); `state.json` (the ledger) is pmd's alone and MUST be byte-identical after any model action.
- **`None` = the CLI's own default.** Worker: pass NO `--model`/`-m`. Decider: fall back to today's env/const behavior. Every legacy `config.json`/`registry.json` must still load (`#[serde(default)]` → `None`).
- **Stored value is launch-ready.** Claude → stable alias for direct Anthropic accounts or provider-specific id from `modelOverrides`; Codex → slug. Resolution happens ONCE at pick time in the discovery module; launch code passes the value verbatim.
- **Discovery never panics / never blocks render.** Direct Anthropic Claude can fall back to stable aliases; provider-backed Claude requires an explicit allowlist; malformed/unreadable provider data and Codex discovery failures return `[]`. pmtui caches per engine and discovers only on picker/create-form open, never per frame.
- **Autopilot-only for the decider; any agent-loop row for the worker.** `e` (decider) refuses on Standard naming `m` (unchanged); `w` (worker) applies to any agent-loop row.
- **Full suite after every change:** `cargo test`, `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check`. Spawned `claude`/`codex` children need `ECC_GATEGUARD=off`.
- **Live-tmux acceptance + decider benchmark before merge** (touching worker launch AND the decider consult).
- **pmtui copy:** terse, imperative, no harness-internals narration.

## Verified anchors (as of this plan)

- `Engine` (`src/registry.rs:43`) has `Claude`, `Codex`, `bin()`, `label()`, and `ALL: [Engine;2]`.
- `Config` (`src/state/records.rs:53`) has `decider_engine` (default Claude); add `decider_model`.
- `ProjectEntry` (`src/registry.rs:62`) fields: id, root, enabled, mode, engine, initial_prompt, conversation_id, cadence_s; add `worker_model`.
- Worker argv builder: `worker::build_loop_command(engine, resume, add_dirs, turn_signal)` at `src/worker/launch.rs:212`; call site `src/job_engine/session.rs:92` (`self.engine`, `self.work_dir` in scope).
- `JobScheduler::new(project_id, work_dir, session_id, engine)` at `src/job_engine/mod.rs:309`; field `self.engine` at `mod.rs:122`. Daemon builds it at `src/daemon/mod.rs:80-86` reading `p.engine.unwrap_or(Engine::Claude)`.
- Standard-path launch: `build_chat(engine, conversation_id)` / `build_chat_create(engine, conversation_id)` at `src/bin/pmtui/session.rs:115/147`; call sites in `start_undriven_session` at `src/bin/pmtui/app/create.rs:382-386`.
- Decider: `spawn_advice(&mut self, driver, now, next, auto, decider_engine)` at `src/job_engine/supervisor.rs:170`; the model is resolved in the `match decider_engine` at `supervisor.rs:249-293` (claude: env `PM_SUPERVISOR_MODEL` → const `SUPERVISOR_MODEL="global.anthropic.claude-sonnet-4-6"`; codex: env `PM_SUPERVISOR_CODEX_MODEL` → `None`). Call site `src/job_engine/marker.rs:519`; also the `drive_marker_only` fast-path threads `config` (`src/job_engine/drive.rs:516/586`). Builders: `build_supervisor_command(model: &str, …)` at `src/worker/supervisor.rs:127`; `build_supervisor_command_codex(model: Option<&str>, …)` at `supervisor.rs:204`.
- lib modules declared in `src/lib.rs` (add `pub mod models;`, alphabetical after `pub mod lease;`). pmtui imports lib items by path in `src/bin/pmtui/main.rs` (e.g. `use agent_manager::registry::{Engine, …}`); add `use agent_manager::models::{available_models, ModelInfo};` there.
- The picker being replaced: `UiMode::DeciderPicker`, `App::begin_decider_pick`/`commit_decider_pick` (`src/bin/pmtui/app/autopilot.rs`), `render_decider_picker` (`src/bin/pmtui/render/decider_picker.rs`), `Scope::DeciderPicker`, keys handler in `src/bin/pmtui/keys.rs`, keybar map `src/bin/pmtui/render/keybar.rs`.

---

### Task 1: `src/models/` discovery module

**Files:**
- Create: `src/models/mod.rs`, `src/models/claude.rs`, `src/models/codex.rs`
- Modify: `src/lib.rs` (add `pub mod models;`)

**Interfaces — Produces:**
- `pub struct ModelInfo { pub label: String, pub value: String }` (derive `Debug, Clone, PartialEq, Eq`)
- `pub fn available_models(engine: crate::registry::Engine) -> Vec<ModelInfo>`

- [ ] **Step 1: Failing tests (fixtures).** Create `src/models/mod.rs` with the module wiring and a `#[cfg(test)] mod tests` that uses the per-provider parse functions on fixture strings. Write the parse logic as pure functions taking the raw input so they are testable without touching the real filesystem/CLI:

```rust
//! Discover the MODELS available to each engine's CLI, so the dashboard can offer a pick
//! list. One provider per engine (Claude combines configured allowlists with direct-account
//! aliases; Codex runs `codex debug models`); adding an engine is one match arm + one file.
//! Discovery is best-effort and never panics or blocks the render path.

use crate::registry::Engine;

mod claude;
mod codex;

/// One selectable model. `label` is shown to the human; `value` is passed to the CLI
/// verbatim (`--model <value>` for claude, `-m <value>` for codex) — already the
/// launch-ready form (Claude: stable alias or provider-specific id; Codex: slug).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub label: String,
    pub value: String,
}

/// The models available for `engine`, best-effort. Direct Claude may return stable aliases;
/// unrecoverable provider failures return empty and callers render "(default) only".
pub fn available_models(engine: Engine) -> Vec<ModelInfo> {
    match engine {
        Engine::Claude => claude::discover(),
        Engine::Codex => codex::discover(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_parses_slugs_and_resolves_overrides() {
        let settings = r#"{
            "availableModels": ["claude-opus-4-8[1m]", "claude-sonnet-5", "claude-no-override"],
            "modelOverrides": {
                "claude-opus-4-8[1m]": "global.anthropic.claude-opus-4-8[1m]",
                "claude-sonnet-5": "global.anthropic.claude-sonnet-5"
            }
        }"#;
        let got = claude::parse(settings);
        assert_eq!(got, vec![
            ModelInfo { label: "claude-opus-4-8[1m]".into(), value: "global.anthropic.claude-opus-4-8[1m]".into() },
            ModelInfo { label: "claude-sonnet-5".into(), value: "global.anthropic.claude-sonnet-5".into() },
            // no override → value falls back to the slug itself
            ModelInfo { label: "claude-no-override".into(), value: "claude-no-override".into() },
        ]);
    }

    #[test]
    fn claude_uses_aliases_without_an_allowlist_and_rejects_malformed_settings() {
        assert!(claude::parse("not json").is_empty());
        assert_eq!(
            claude::parse("{}"),
            vec![
                ModelInfo { label: "sonnet".into(), value: "sonnet".into() },
                ModelInfo { label: "opus".into(), value: "opus".into() },
                ModelInfo { label: "haiku".into(), value: "haiku".into() },
            ]
        );
    }

    #[test]
    fn codex_parses_visible_models_only() {
        let json = r#"{"models":[
            {"slug":"openai.gpt-5.6-sol","display_name":"GPT-5.6 Sol","visibility":"list","supported_in_api":true},
            {"slug":"hidden-one","display_name":"Hidden","visibility":"hidden","supported_in_api":true},
            {"slug":"no-name","display_name":"","visibility":"list","supported_in_api":true}
        ]}"#;
        let got = codex::parse(json);
        assert_eq!(got, vec![
            ModelInfo { label: "GPT-5.6 Sol".into(), value: "openai.gpt-5.6-sol".into() },
            // empty display_name falls back to the slug
            ModelInfo { label: "no-name".into(), value: "no-name".into() },
        ]);
    }

    #[test]
    fn codex_empty_on_malformed() {
        assert!(codex::parse("nonsense").is_empty());
        assert!(codex::parse(r#"{"models":[]}"#).is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test --lib models:: 2>&1 | tail -20` → fails to compile (`claude::parse`/`codex::parse`/`discover` missing).

- [ ] **Step 3: Implement `src/models/claude.rs`.**

```rust
//! Claude uses an explicit settings allowlist when present. Direct Anthropic accounts without
//! one receive stable aliases; provider-backed setups require explicit provider-specific ids.
//! Values resolve through `modelOverrides`; malformed or unreadable settings fail closed.

use super::ModelInfo;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize, Default)]
struct Settings {
    // The implementation distinguishes an absent field from an explicit [] or invalid null.
    #[serde(default, rename = "availableModels")]
    available_models: AvailableModels,
    #[serde(default, rename = "modelOverrides")]
    model_overrides: BTreeMap<String, String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// Locate ~/.claude/settings.json — `$CLAUDE_CONFIG_DIR/settings.json` if set, else
/// `$HOME/.claude/settings.json`. Returns None if neither env is set.
fn settings_path() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Some(std::path::Path::new(&dir).join("settings.json"));
        }
    }
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty())?;
    Some(std::path::Path::new(&home).join(".claude").join("settings.json"))
}

/// Parse configured models, or stable aliases only when no allowlist/provider configuration exists.
pub(super) fn parse(body: &str) -> Vec<ModelInfo> {
    let Ok(settings) = serde_json::from_str::<Settings>(body) else {
        return Vec::new();
    };
    let slugs = match settings.available_models {
        AvailableModels::Absent if !provider_configured(&settings.env) => {
            vec!["sonnet".into(), "opus".into(), "haiku".into()]
        }
        AvailableModels::Absent => Vec::new(),
        AvailableModels::Present(slugs) => slugs,
    };
    slugs
        .into_iter()
        .map(|slug| {
            let value = settings.model_overrides.get(&slug).cloned().unwrap_or_else(|| slug.clone());
            ModelInfo { label: slug, value }
        })
        .collect()
}

pub(super) fn discover() -> Vec<ModelInfo> {
    // Process-level Bedrock/Vertex/Foundry/Mantle/Anthropic-AWS/custom URL settings also
    // disable aliases. Missing settings on a direct account uses aliases; malformed or
    // unreadable settings return [].
    /* see src/models/claude.rs for the provider-gated implementation */
}
```

- [ ] **Step 4: Implement `src/models/codex.rs`.**

```rust
//! Codex's models come from `codex debug models` → JSON with a `models` array. Each entry
//! carries a large `base_instructions` blob we ignore; we keep slug + display_name for
//! entries with `visibility == "list"`. The slug is what `codex -m` wants, so it is the
//! stored value. Best-effort: any error/non-zero exit yields an empty list.

use super::ModelInfo;
use serde::Deserialize;

#[derive(Deserialize)]
struct Models {
    #[serde(default)]
    models: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    visibility: String,
    #[serde(default = "default_true")]
    supported_in_api: bool,
}
fn default_true() -> bool {
    true
}

/// Parse `codex debug models` JSON. PURE (no process spawn) so it is unit-tested on fixtures.
/// Keeps only `visibility == "list"` && `supported_in_api`, dropping any with an empty slug;
/// label falls back to the slug when display_name is empty.
pub(super) fn parse(body: &str) -> Vec<ModelInfo> {
    let Ok(m) = serde_json::from_str::<Models>(body) else {
        return Vec::new();
    };
    m.models
        .into_iter()
        .filter(|e| e.visibility == "list" && e.supported_in_api && !e.slug.trim().is_empty())
        .map(|e| {
            let label = if e.display_name.trim().is_empty() { e.slug.clone() } else { e.display_name };
            ModelInfo { label, value: e.slug }
        })
        .collect()
}

pub(super) fn discover() -> Vec<ModelInfo> {
    match std::process::Command::new("codex").args(["debug", "models"]).output() {
        Ok(out) if out.status.success() => parse(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}
```

- [ ] **Step 5: Add `pub mod models;` to `src/lib.rs`** (alphabetical, after `pub mod lease;`).

- [ ] **Step 6: Green + hygiene.** `cargo test --lib models:: 2>&1 | tail -20` (pass); `cargo clippy --all-targets 2>&1 | tail`; `cargo fmt --all`.

- [ ] **Step 7: Commit.** `git add src/models/ src/lib.rs && git commit -m "feat(models): discover available models per engine (claude settings.json, codex debug models)"`

---

### Task 2: Schema fields — `decider_model` (config) + `worker_model` (registry)

**Files:**
- Modify: `src/state/records.rs` (add `Config.decider_model`, seed default)
- Modify: `src/registry.rs` (add `ProjectEntry.worker_model`)
- Modify: `src/bin/pmtui/seed.rs` (the `Config` literal in `seed_agent_loop` gains `decider_model: None`)
- Modify: any other `Config { … }` / `ProjectEntry { … }` struct literals that now miss a field (compile will list them — the pmtui test helpers at `src/bin/pmtui/tests/mod.rs` build both).

**Interfaces — Produces:** `Config.decider_model: Option<String>`, `ProjectEntry.worker_model: Option<String>`.

- [ ] **Step 1: Failing round-trip tests.** In `src/state/records.rs` `#[cfg(test)] mod decider_engine_tests`, add:

```rust
#[test]
fn config_without_decider_model_loads_as_none() {
    let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860,"decider_engine":"codex"}"#;
    let cfg: Config = serde_json::from_str(json).expect("legacy config must parse");
    assert_eq!(cfg.decider_model, None);
}

#[test]
fn decider_model_round_trips() {
    let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860,"decider_engine":"claude","decider_model":"global.anthropic.claude-opus-5"}"#;
    let cfg: Config = serde_json::from_str(json).expect("must parse");
    assert_eq!(cfg.decider_model.as_deref(), Some("global.anthropic.claude-opus-5"));
    assert!(serde_json::to_string(&cfg).unwrap().contains("\"decider_model\":\"global.anthropic.claude-opus-5\""));
}
```

In `src/registry.rs` tests (find the existing `ProjectEntry`/`Registry` serde tests; add there):

```rust
#[test]
fn entry_without_worker_model_loads_as_none() {
    let json = r#"{"id":"x","root":"/tmp","enabled":true,"mode":"agent_loop","engine":"claude"}"#;
    let e: ProjectEntry = serde_json::from_str(json).expect("legacy entry must parse");
    assert_eq!(e.worker_model, None);
}

#[test]
fn worker_model_round_trips() {
    let json = r#"{"id":"x","root":"/tmp","enabled":true,"mode":"agent_loop","engine":"codex","worker_model":"openai.gpt-5.6-sol"}"#;
    let e: ProjectEntry = serde_json::from_str(json).expect("must parse");
    assert_eq!(e.worker_model.as_deref(), Some("openai.gpt-5.6-sol"));
}
```

- [ ] **Step 2: Run → fail to compile** (`no field decider_model`/`worker_model`).

- [ ] **Step 3: Add the fields.**

`src/state/records.rs`, in `struct Config` after `decider_engine`:
```rust
    /// The MODEL the decider runs on for THIS session, launch-ready (Claude: stable alias or
    /// provider-specific id; Codex: slug). `None` keeps the engine's own default; Claude falls back to
    /// `PM_SUPERVISOR_MODEL`/the `SUPERVISOR_MODEL` const, the codex arm omits `-m`. `pmtui` is
    /// the only writer. Defaulting to `None` keeps every legacy config loading.
    #[serde(default)]
    pub decider_model: Option<String>,
```

`src/registry.rs`, in `struct ProjectEntry` after `engine`:
```rust
    /// The MODEL the WORKER REPL launches with, launch-ready (Claude: stable alias or
    /// provider-specific id; Codex: slug).
    /// `None` (default) passes no `--model`/`-m`, so the CLI uses its own default. A launch
    /// parameter beside `engine`; `pmtui` is the only writer, applied on the next (re)launch.
    #[serde(default)]
    pub worker_model: Option<String>,
```

- [ ] **Step 4: Fix all struct literals.** `cargo build 2>&1 | grep "missing field"` — add `decider_model: None` to every `Config { … }` literal (at least `src/bin/pmtui/seed.rs:84` and `src/bin/pmtui/tests/mod.rs`) and `worker_model: None` to every `ProjectEntry { … }` literal (test helpers, and any registry-writing code). Do not change behavior — all seed/default to `None`.

- [ ] **Step 5: Green + hygiene + commit.** `cargo test 2>&1 | tail`; clippy; fmt. `git add -A && git commit -m "feat(schema): add Config.decider_model + ProjectEntry.worker_model (serde-default None)"`

---

### Task 3: Decider consult uses `decider_model`

**Files:**
- Modify: `src/job_engine/supervisor.rs` (`spawn_advice` signature + the `match decider_engine` model resolution)
- Modify: `src/job_engine/marker.rs:519` (pass `config.decider_model.as_deref()`)
- Modify: `src/job_engine/drive.rs` (the `drive_marker_only` fast-path call, ~line 516/586, if it calls spawn_advice — thread the same)
- Test: `src/job_engine/supervisor.rs` tests (or wherever spawn_advice/argv is tested)

**Interfaces — Consumes:** `Config.decider_model` (Task 2). **Produces:** `spawn_advice(&mut self, driver, now, next, auto, decider_engine, decider_model: Option<&str>)`.

- [ ] **Step 1: Failing test** — that the built claude argv contains `--model <value>` and codex `-m <value>` for a given value (the builders already support this; the real change is `spawn_advice` feeding them `decider_model` first). Adapt to the existing test harness in this file:

```rust
#[test]
fn decider_model_some_overrides_the_claude_default() {
    let argv = crate::worker::build_supervisor_command(
        "global.anthropic.claude-opus-5", "sys", "{}", "prompt", 60, 5, None,
    );
    assert!(argv.join(" ").contains("--model global.anthropic.claude-opus-5"), "{argv:?}");
}

#[test]
fn decider_model_some_sets_codex_m() {
    let argv = crate::worker::build_supervisor_command_codex(
        Some("openai.gpt-5.6-sol"), "sys", "prompt", "/tmp/last", 60, 5,
    );
    assert!(argv.windows(2).any(|w| w == ["-m", "openai.gpt-5.6-sol"]), "{argv:?}");
}
```
(If a `JobScheduler` can drive `spawn_advice` and observe the launched argv via the FakeDriver in this suite, add that as the stronger test; otherwise these builder-level tests + the resolution change suffice, and Task 7's benchmark covers integration.)

- [ ] **Step 2: Thread the parameter.** Add `decider_model: Option<&str>` to `spawn_advice` after `decider_engine`. In the `match decider_engine`:
  - **Claude arm** (`supervisor.rs:253-256`): `decider_model` when `Some`, else the existing env→const fallback:
    ```rust
    let model = decider_model.map(str::to_string).unwrap_or_else(|| {
        std::env::var(worker::SUPERVISOR_MODEL_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| worker::SUPERVISOR_MODEL.to_string())
    });
    ```
  - **Codex arm** (`supervisor.rs:278-281`): prefer `decider_model`, else the existing env→`None`:
    ```rust
    let model = decider_model.map(str::to_string).or_else(|| {
        std::env::var(worker::SUPERVISOR_CODEX_MODEL_ENV).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    });
    ```

- [ ] **Step 3: Pass it at the call sites.** `src/job_engine/marker.rs:519`: `self.spawn_advice(driver, now, &next, &auto, config.decider_engine, config.decider_model.as_deref())?`. Do the same for the `drive_marker_only` fast-path if it invokes `spawn_advice` (`src/job_engine/drive.rs` ~516/586 — `config` is in scope).

- [ ] **Step 4: Green + hygiene + commit.** `cargo test 2>&1 | tail`; clippy; fmt. `git commit -am "feat(decider): use per-session config.decider_model (falls back to env/const when unset)"`

---

### Task 4: Worker launch uses `worker_model` (pmd-driven AND Standard)

**Files:**
- Modify: `src/worker/launch.rs` (`build_loop_command` + `model` param; claude `--model`, codex `-m`)
- Modify: `src/job_engine/mod.rs` (`JobScheduler::new` + `worker_model` param/field) and `src/job_engine/session.rs:92` (pass `self.worker_model.as_deref()`)
- Modify: `src/daemon/mod.rs:80-86` (read `p.worker_model`, pass to `JobScheduler::new`)
- Modify: `src/bin/pmtui/session.rs` (`build_chat`/`build_chat_create` + `model` param) and `src/bin/pmtui/app/create.rs:382-386` (pass the entry's `worker_model`)
- Test: `src/worker/launch.rs` tests + `src/bin/pmtui/session.rs`/tests

**Interfaces — Consumes:** `ProjectEntry.worker_model` (Task 2). **Produces:** `build_loop_command(engine, resume, add_dirs, turn_signal, model: Option<&str>)`; `build_chat(engine, conversation_id, model: Option<&str>)`; `build_chat_create(engine, conversation_id, model: Option<&str>)`; `JobScheduler::new(project_id, work_dir, session_id, engine, worker_model: Option<String>)`.

- [ ] **Step 1: Failing tests** (adapt to existing `build_loop_command` tests):

```rust
#[test]
fn loop_command_claude_inserts_model_when_some() {
    let argv = build_loop_command(Engine::Claude, &Resume::Fresh { session_id: None }, &[], None, Some("global.anthropic.claude-opus-5"));
    assert!(argv.windows(2).any(|w| w == ["--model", "global.anthropic.claude-opus-5"]), "{argv:?}");
}
#[test]
fn loop_command_claude_omits_model_when_none() {
    let argv = build_loop_command(Engine::Claude, &Resume::Fresh { session_id: None }, &[], None, None);
    assert!(!argv.iter().any(|a| a == "--model"), "{argv:?}");
}
#[test]
fn loop_command_codex_inserts_m_when_some() {
    let argv = build_loop_command(Engine::Codex, &Resume::Fresh { session_id: None }, &[], None, Some("openai.gpt-5.6-sol"));
    assert!(argv.windows(2).any(|w| w == ["-m", "openai.gpt-5.6-sol"]), "{argv:?}");
}
```
Plus a `build_chat`/`build_chat_create` test: claude `--model` present iff `Some`.

- [ ] **Step 2: `build_loop_command`.** Add `model: Option<&str>` as the last param.
  - **Claude arm** (`launch.rs:223-262`): after `--permission-mode auto`, before the session-anchoring flags, `if let Some(m) = model { cmd.push("--model".into()); cmd.push(m.to_string()); }`.
  - **Codex arm** (`launch.rs:263-331`): right after pushing `codex` (before the optional `-c`/`resume`), `if let Some(m) = model { cmd.push("-m".into()); cmd.push(m.to_string()); }`.

- [ ] **Step 3: `JobScheduler`.** Add field `worker_model: Option<String>` (near `engine`, `mod.rs:122`); add the param to `new` (`mod.rs:309`), store it. At `session.rs:92`, pass `self.worker_model.as_deref()`.

- [ ] **Step 4: Daemon.** At `src/daemon/mod.rs:80-86`, read `p.worker_model.clone()` beside `p.engine` and pass to `JobScheduler::new`. Update every OTHER `JobScheduler::new` call (tests) to pass `None` (compile lists them).

- [ ] **Step 5: Standard path.** Add `model: Option<&str>` to `build_chat`/`build_chat_create` (`src/bin/pmtui/session.rs`); claude inserts `--model <m>` after `claude` (before `--resume`/`--session-id`), codex `build_chat` inserts `-m <m>` after `codex` (before `resume`). At `create.rs:382-386`, pass the entry's `worker_model.as_deref()` (read it from the same entry `engine` comes from). Update other `build_chat*` call sites to pass `None`.

- [ ] **Step 6: Green + hygiene + commit.** `cargo test 2>&1 | tail`; clippy; fmt. `git commit -am "feat(worker): launch with per-session worker_model (--model/-m) on both driven and Standard paths"`

---

### Task 5: pmtui — model catalog cache + reusable `ModelPicker` (replaces `DeciderPicker`)

**Files:**
- Modify: `src/bin/pmtui/main.rs` (`use agent_manager::models::{available_models, ModelInfo};`) + the `App` struct (add `model_catalog: HashMap<Engine, Vec<ModelInfo>>`, init empty)
- Modify: `src/bin/pmtui/mode.rs` (replace `DeciderPicker` with `ModelPicker { target, id, stage, engine, cursor }` + `PickTarget`/`PickStage`)
- Modify: `src/bin/pmtui/app/autopilot.rs` (replace `begin_decider_pick`/`commit_decider_pick` with `begin_model_pick`/`advance_model_pick`/`commit_model_pick`/`back_or_cancel_model_pick`; catalog helpers)
- Modify: `src/bin/pmtui/keys.rs` (Normal `e` → decider pick; `w` → worker pick; the picker handler block)
- Rename: `src/bin/pmtui/render/decider_picker.rs` → `model_picker.rs` (`render_model_picker`); update `render/mod.rs`
- Modify: `src/bin/pmtui/render/keybar.rs` (mode→scope), `src/bin/pmtui/bindings.rs` (`Scope::ModelPicker`, `e` reword, `w` row)
- Test: `src/bin/pmtui/tests/autopilot.rs`

**Interfaces — Consumes:** `available_models`/`ModelInfo` (T1), `Config.decider_model`/`ProjectEntry.worker_model` (T2), `Engine::ALL`. **Produces:** `UiMode::ModelPicker`, `PickTarget`, `PickStage`, `App::model_catalog` + `models_for`/`models_len`, `begin_model_pick`/`advance_model_pick`/`commit_model_pick`/`back_or_cancel_model_pick`, `render_model_picker`, `Scope::ModelPicker`.

- [ ] **Step 1: State + catalog.** In `mode.rs`:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickTarget { Decider, Worker }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickStage { Engine, Model }
```
Replace the `DeciderPicker` variant with:
```rust
    /// Pick engine and/or model for a session's DECIDER (config.json) or WORKER (registry.json).
    /// Reusable two-stage list: the decider walks Engine→Model; the worker picks Model only (its
    /// engine is create-time). Writes only the pmtui-owned file, never the ledger.
    ModelPicker { target: PickTarget, id: String, stage: PickStage, engine: Engine, cursor: usize },
```
`App`: add `pub(crate) model_catalog: std::collections::HashMap<Engine, Vec<ModelInfo>>` (init `HashMap::new()`). Helpers:
```rust
pub(crate) fn models_for(&mut self, engine: Engine) -> &[ModelInfo] {
    self.model_catalog.entry(engine).or_insert_with(|| available_models(engine));
    &self.model_catalog[&engine]
}
pub(crate) fn models_len(&mut self, engine: Engine) -> usize { self.models_for(engine).len() }
```

- [ ] **Step 2: Behavior** (in `app/autopilot.rs`, replacing the two `*_decider_pick` fns):
  - `begin_model_pick(&mut self, target: PickTarget)`: resolve selected row → id + registry entry (gone → status + return).
    - **Decider:** read `config.json`; if `autonomy != Autopilot` → the existing refusal status naming `m`, return. Else open `ModelPicker { Decider, id, stage: Engine, engine: config.decider_engine, cursor: idx of that engine in Engine::ALL }`.
    - **Worker:** open `ModelPicker { Worker, id, stage: Model, engine: entry.engine.unwrap_or(Engine::Claude), cursor: self.model_cursor_for(entry.worker_model.as_deref(), engine) }`.
  - `advance_model_pick(&mut self)` (the Enter action): if `stage == Engine` → set `engine = Engine::ALL[cursor]`, `stage = Model`, `cursor = model_cursor_for(<stored model for this target>, engine)`. If `stage == Model` → `commit_model_pick`.
  - `commit_model_pick(&mut self)`: chosen model = `if cursor == 0 { None } else { Some(self.models_for(engine)[cursor-1].value.clone()) }`.
    - **Decider:** RMW `config.json`: `decider_engine = engine`, `decider_model = chosen`. Status `"<id> → decider: <engine.label()> / <model label or default>"`.
    - **Worker:** RMW the `registry.json` entry: `worker_model = chosen`. Status `"<id> → worker model: <label or default>; press r to restart to apply"`.
    - Close to `Normal`; `refresh()`. Never touch `state.json`.
  - `back_or_cancel_model_pick(&mut self)`: if `Decider` && `stage == Model` → `stage = Engine`, cursor on current engine. Else `Normal` + `"model selection cancelled"`.
  - `model_cursor_for(&mut self, stored: Option<&str>, engine) -> usize`: `0` if None; else `1 + index in models_for(engine) whose value == stored`, else `0`.

- [ ] **Step 3: Keys** (`keys.rs`). Normal: `KeyCode::Char('e') => app.begin_model_pick(PickTarget::Decider)`; add `KeyCode::Char('w') => app.begin_model_pick(PickTarget::Worker)`. Replace the `DeciderPicker` handler with:
```rust
    if matches!(app.mode, UiMode::ModelPicker { .. }) {
        let len = match app.mode {
            UiMode::ModelPicker { stage: PickStage::Engine, .. } => Engine::ALL.len(),
            UiMode::ModelPicker { stage: PickStage::Model, engine, .. } => 1 + app.models_len(engine),
            _ => 0,
        };
        match code {
            KeyCode::Enter => app.advance_model_pick(),
            KeyCode::Esc => app.back_or_cancel_model_pick(),
            KeyCode::Up | KeyCode::Char('k') => {
                if let UiMode::ModelPicker { cursor, .. } = &mut app.mode { *cursor = cursor.saturating_sub(1); }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let UiMode::ModelPicker { cursor, .. } = &mut app.mode { *cursor = (*cursor + 1).min(len.saturating_sub(1)); }
            }
            _ => {}
        }
        return;
    }
```

- [ ] **Step 4: Render** (`render/model_picker.rs`, from `decider_picker.rs`). `render_model_picker(f, area, app, id, target, stage, engine, cursor)` — read the cached list via `app.model_catalog.get(&engine)` (do NOT discover in render; the catalog was filled on open). Title `"Decider · <id>"`/`"Worker · <id>"`; sub-header `"Engine"` or `format!("Model · {}", engine.label())`; list with `▸` cursor + `●`/`○`. Engine stage = `Engine::ALL` (● = the stored engine passed in). Model stage = `(default)` then `catalog[engine]` labels (● = the stored model, else `(default)`). Footer `"↑↓ move · enter select · esc back"`. Update `render/mod.rs` (`mod model_picker; pub(crate) use model_picker::*;`, overlay arm `UiMode::ModelPicker { target, id, stage, engine, cursor } => render_model_picker(f, area, app, id, *target, *stage, *engine, *cursor)`). keybar mode→scope: `UiMode::ModelPicker { .. } => Scope::ModelPicker`.

- [ ] **Step 5: Bindings** (`bindings.rs`). Rename `Scope::DeciderPicker` → `Scope::ModelPicker` (rows keep Enter/↑↓/Esc; Esc label "Back/Cancel"). Reword `e` help: `"Pick the decider engine + model (autopilot)"`. Add:
```rust
    Binding { key: "w", label: "Worker", help: "Set the worker model for the selected session (restart to apply)",
              group: KeyGroup::Session, scope: Scope::Normal, applies: Applies::AgentLoop, rank: 9 },
```

- [ ] **Step 6: Tests** (`tests/autopilot.rs`). Rewrite decider tests + add worker tests, pre-seeding `app.model_catalog` for determinism (e.g. `app.model_catalog.insert(Engine::Claude, vec![ModelInfo{label:"opus".into(),value:"global.anthropic.claude-opus-5".into()}])`). Cover: `e` autopilot opens Engine stage; Standard refuses naming `m`; Engine→Enter→Model with cursor on current; Model→Enter commits `decider_engine`+`decider_model` to config, **ledger byte-identical**; Esc Model→Engine, Esc Engine→cancel; `(default)` cursor 0 → `decider_model = None`; empty catalog → Model list len 1; `w` opens Worker Model stage, commit writes `worker_model` to registry only (config + ledger byte-identical), status names restart; cursor clamps; drift test green (add `'w'` to its expected Normal set); a `render_model_picker` TestBackend smoke test for a Decider Engine frame and a Worker Model frame.

- [ ] **Step 7: Green + hygiene + commit.** `cargo test 2>&1 | tail`; clippy; fmt. `git commit -am "feat(pmtui): reusable ModelPicker — e picks decider engine+model, w picks worker model"`

---

### Task 6: Create-form worker Model field

**Files:**
- Modify: `src/bin/pmtui/create_form.rs` (add `worker_model` + `model_choices`, a `WORKER_MODEL` index, renumber, `shows`/`adjust`)
- Modify: `src/bin/pmtui/render/create.rs` (render the field)
- Modify: `src/bin/pmtui/app/create.rs` (populate `model_choices` on open + engine toggle; on submit write `worker_model`)
- Test: `src/bin/pmtui/tests/create_form.rs`

**Interfaces — Consumes:** `available_models` (T1), `ProjectEntry.worker_model` (T2). **Produces:** create-time `worker_model` on the new entry.

- [ ] **Step 1: Failing test** — default `worker_model = None`; toggling the Model field (pre-seed `form.model_choices`) to the first model sets `worker_model` to its value; submit writes it to the registry entry. Mirror the existing engine/decider toggle tests in `tests/create_form.rs`.

- [ ] **Step 2: Form state.** `CreateForm`: add `pub(crate) worker_model: Option<String>` and `pub(crate) model_choices: Vec<ModelInfo>` (`new()` inits both empty/None). Add `WORKER_MODEL` index; **renumber** engine=0, worker_model=1, dir=2, tier=3, goal=4, cadence=5, decider=6; `FIELDS=7`; update `GOAL`/`CADENCE`/`DECIDER` consts and every literal index in `adjust`/`is_text_field`/`text_field`/`shows_field`. Model field `shows` always. `adjust(WORKER_MODEL)` cycles a cursor over `[(default)] ++ model_choices` setting `worker_model` (None at 0). `toggle_engine` resets `worker_model = None` (list changes with engine).

- [ ] **Step 3: App wiring.** In `begin_create` and the create-form engine-toggle path, set `form.model_choices = self.models_for(form.engine).to_vec()`. On `submit_create`, set the new `ProjectEntry.worker_model = form.worker_model.clone()`.

- [ ] **Step 4: Render.** In `render/create.rs`, add a Worker Model toggle row (`< (default) >` / `< <label> >`) styled like the other toggles; place it after Engine.

- [ ] **Step 5: Green + hygiene + commit.** `cargo test 2>&1 | tail`; clippy; fmt. `git commit -am "feat(pmtui): create-form worker Model field (per-engine list; seeds ProjectEntry.worker_model)"`

---

### Task 7: Docs + decider benchmark + real-tmux acceptance

**Files:**
- Modify: `README.md`, `docs/SPEC.md`, `docs/superpowers/specs/2026-08-24-model-selection-design.md` (Status → implemented)

- [ ] **Step 1: Build + live-tmux render** (per the `pmtui-ui-testing` skill; scratch sockets only, never `--socket pmd`, never the real registry). `cargo build`; seed a scratch autopilot agent-loop row; drive `e` (Engine → Enter → Model → Enter) and confirm `config.json` gains `decider_engine`+`decider_model`; drive `w` (Model → Enter) and confirm `registry.json` gains `worker_model` and the status names restart. claude models come from the host's real `~/.claude/settings.json`; codex from the real `codex debug models`. Report the captured frames; clean up both scratch servers + dir.
- [ ] **Step 2: Worker-launch smoke** — a scratch `registry.json` with `worker_model` set: confirm the launch argv carries `--model`/`-m` (the Task 4 argv tests already prove the builder; optionally launch on the scratch inner socket and confirm it starts, then clean up).
- [ ] **Step 3: Decider benchmark.** `ECC_GATEGUARD=off PM_DECIDER_BENCH=1 cargo test --test integration decider_bench -- --ignored --nocapture 2>&1 | tail -40` (BOTH engines). MUST-safety + infra hard gates must pass per engine — with no `decider_model` set this exercises the unchanged env/const fallback, confirming Task 3 didn't regress.
- [ ] **Step 4: Real-tmux acceptance.** `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 2>&1 | tail -30` — assert the OUTPUT (passed / 0 failed).
- [ ] **Step 5: Docs.** README: "pick the model" note under the decider (`e` = engine+model) + worker (`w` + the create-form field; restart to apply) + a `w` key-table row. SPEC: document `decider_model`/`worker_model` (discovery sources, `None`=default, launch-ready value, single-writer). Flip the design-doc Status to "implemented @ feat/model-selection".
- [ ] **Step 6: Commit.** `git commit -am "docs(model-selection): README + SPEC + design status; benchmark & acceptance green"`

---

## Self-Review

**1. Spec coverage:** discovery module → T1; schema (both fields) → T2; decider model wiring → T3; worker model wiring (driven + Standard) → T4; picker (decider engine+model via `e`, worker model via `w`) + catalog cache → T5; create-form worker model field → T6; docs + benchmark + acceptance + live render → T7. "Resolve once at pick time, store launch-ready value" → T1 (`value` = override-resolved) + T5 (commit stores `value`). "`None`=default" → T2 defaults, T3/T4 omit/fall back. Single-writer → T3 (config only), T4 (registry via pmtui at create; pmd reads), T5 (config/registry only, ledger byte-identity asserted). Out-of-scope (live worker engine, decider-model create field, reasoning effort) → in no task.

**2. Placeholder scan:** no TBD/TODO; the tricky modules (discovery, argv insertion, picker state machine) carry full code; mechanical tasks give exact signatures/fields + the load-bearing test cases as code, with "compile lists the rest" for struct-literal fixups.

**3. Type consistency:** `ModelInfo { label, value }`, `available_models(Engine)`, `Config.decider_model`, `ProjectEntry.worker_model`, `UiMode::ModelPicker { target, id, stage, engine, cursor }`, `PickTarget`/`PickStage`, `begin_model_pick`/`advance_model_pick`/`commit_model_pick`/`back_or_cancel_model_pick`, `models_for`/`models_len`, `Scope::ModelPicker`, and the launch signatures (`build_loop_command(…, model)`, `build_chat(…, model)`, `JobScheduler::new(…, worker_model)`, `spawn_advice(…, decider_model)`) are spelled identically across tasks and the Interfaces blocks.
