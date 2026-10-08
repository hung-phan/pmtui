# Model selection — decider AND worker, discovered from the installed CLIs

**Date:** 2026-08-24
**Status:** implemented @ feat/model-selection
**Relates to:** [`2026-08-23-decider-selection-panel-design.md`](2026-08-23-decider-selection-panel-design.md) — that shipped the `e` decider **engine** picker "engine-now, model-ready". This is the model-now step, extended to the **worker** as well.

## Problem & goal

The user can choose the *engine* (claude ⇄ codex) for both the decider and the worker, but not the *model*. Requested: **add model selection for the decider AND the worker**, with available models derived from provider-supported sources — Claude from an explicit `~/.claude/settings.json` allowlist or stable aliases for direct Anthropic accounts when no allowlist exists, Codex from `codex debug models` — and a solution that is **easy to scale and change**.

Two selection subjects, two engines each, two discovery sources. The design's spine is one small discovery module + one reusable picker, so adding an engine, a subject, or a new source is a localized change.

## What "model" means on each path (verified against the tools)

- **Claude** accepts stable aliases such as `sonnet`, `opus`, and `haiku` for direct subscription accounts. Provider-specific setups can expose short slugs in `~/.claude/settings.json → availableModels` and map them through `modelOverrides` to full ids (for example, a Bedrock inference profile). The launch-ready value is therefore either a stable alias or a configured full id.
- **Codex** exposes models via `codex debug models` → JSON `{"models":[{"slug":"openai.gpt-5.6-sol","display_name":"GPT-5.6 Sol","visibility":"list","supported_in_api":true, …}]}` (each entry also carries a huge `base_instructions` blob — ignore it). `codex … -m <slug>` takes the **slug**; omitting `-m` lets codex pick its own default.

**Decision — resolve once at pick time, store the launch-ready value.** For Claude, an explicit `availableModels` list is authoritative and each entry resolves through `modelOverrides`; when the field or settings file is absent on a direct Anthropic setup, the picker uses stable aliases. Provider-backed setups require an explicit list because some silently ignore bare aliases. Codex stores its discovered slug. Launch code passes the stored value verbatim — no discovery or resolution at launch. *Why:* keeps launch paths dumb, preserves provider-specific ids, and gives subscription users useful choices without version-specific hardcoding.

## Decisions (with rationale)

- **`None` means "the CLI's own default" everywhere.** A missing/unset model passes **no** `--model`/`-m` flag (worker), or falls back to today's env/const behavior (decider). *Why:* the safe, non-surprising default — silence changes nothing, and every legacy `config.json`/`registry.json` keeps working (`#[serde(default)]` → `None`).
- **Where each value lives mirrors where its engine lives (single-writer preserved).**
  - **Decider model → `config.json` `Config.decider_model: Option<String>`**, beside `decider_engine`. pmtui writes it; pmd reads it.
  - **Worker model → `registry.json` `ProjectEntry.worker_model: Option<String>`**, beside `engine` (a launch parameter). pmtui writes it; pmd reads it.
  - Both files are pmtui-owned; the ledger (`state.json`, pmd-owned) is never touched. *Why:* the worker engine is already a registry launch-param and the decider engine already a config field; models follow suit, and the single-writer invariant holds by construction.
- **Discovery is graceful and cached.** Direct Anthropic Claude uses stable aliases when settings or `availableModels` is absent. Provider switches in the process or `settings.json.env`, plus a custom `ANTHROPIC_BASE_URL`, disable inference and require an explicit list. Claude honors an explicit list including `[]` and fails closed on malformed/unreadable settings. Codex returns `[]` when discovery fails. pmtui caches per engine for its process lifetime (restart to refresh). *Why:* subscription installs commonly have no allowlist, while provider restrictions and model identifiers must remain authoritative.
- **The worker model is a launch argument, so a live change needs a relaunch.** Changing an existing session's worker model writes the registry and reports *"press r to restart the agent to apply"*; it does not hot-swap the running REPL. *Why:* honest — the model is baked into the REPL's argv at spawn.
- **Worker engine stays create-time; the live worker picker changes model only.** Switching a live worker's engine claude⇄codex would strand its pinned `conversation_id`; that footgun is out of scope. The picker's stage machine still *supports* an engine stage (used by the decider), so adding live worker-engine change later is a one-line target change. *Why:* scalable without shipping the footgun.
- **Decider model is settable at create (autopilot-only) AND live (via `e`).** The create form gains a **Decider Model** field beside the Decider engine — shown only on Autopilot, exactly like the Decider engine field, since a decider model on a Standard row configures a consult that never runs. It seeds `config.decider_model` (`None` = the decider engine's own default). *Why:* the user asked for decider model selection at create too; it mirrors the Worker Model field (per-engine catalog, `(default)` row = `None`, reset on the engine flip) and stays live-editable via `e` afterward. (Originally deferred as live-only; now added.)
- **The Claude decider keeps a launch-ready value defensively.** When `decider_model` is set it is already a direct-account alias or provider-specific id resolved at pick time. When unset, the Claude arm keeps its current env(`PM_SUPERVISOR_MODEL`)→const(`SUPERVISOR_MODEL`) fallback; the Codex arm keeps env(`PM_SUPERVISOR_CODEX_MODEL`)→omit. *Why:* backward compatible — the decider benchmark (which sets no `decider_model`) is unaffected.

## Architecture

### 1. Discovery module — `src/models/` (new, in the core lib)

```
src/models/mod.rs     ModelInfo { label, value }; available_models(Engine) -> Vec<ModelInfo>; dispatch by engine
src/models/claude.rs  read availableModels + modelOverrides, else stable aliases
src/models/codex.rs   run `codex debug models` → parse slugs, filter visibility=="list"
```

- `pub struct ModelInfo { pub label: String, pub value: String }` — `label` shown in the UI, `value` passed to the CLI.
- `pub fn available_models(engine: Engine) -> Vec<ModelInfo>` — `match engine { Claude => claude::discover(), Codex => codex::discover() }`. **Adding an engine = one arm + one file.**
- `claude::discover()`: locate settings (`$CLAUDE_CONFIG_DIR/settings.json` if set, else `$HOME/.claude/settings.json`). An explicit `availableModels` list, including `[]`, is authoritative. For direct Anthropic accounts, if the field or file is absent, use `sonnet`, `opus`, and `haiku`. Provider switches and custom endpoints disable this fallback. Resolve every explicit entry through `modelOverrides` when present. Malformed, invalid, or unreadable settings return `[]`.
- `codex::discover()`: run `codex debug models`, parse `{"models":[{slug, display_name, visibility, supported_in_api}]}`; keep entries with `visibility == "list"` (and `supported_in_api != false`); emit `ModelInfo { label: display_name (or slug if empty), value: slug }`. Any error/non-zero exit → `[]`. (Deserialize only the fields needed; ignore `base_instructions` et al.)
- Unit-tested with fixtures (a sample settings.json and a trimmed codex-models JSON), including the empty/malformed cases.

### 2. Schema (serde-defaulted, backward compatible)

- `src/state/records.rs`: `Config.decider_model: Option<String>` with `#[serde(default)]` (→ `None`). Round-trips; a legacy config without it loads as `None`. `seed_agent_loop` seeds it `None` (decider model is live-only) — its `Config` literal gains `decider_model: None`.
- `src/registry.rs`: `ProjectEntry.worker_model: Option<String>` with `#[serde(default)]` (→ `None`). Round-trips; legacy entries load as `None`.

### 3. Launch wiring

- **Worker (pmd-driven):** `worker::build_loop_command(engine, resume, add_dirs, turn_signal, model: Option<&str>)` — claude arm inserts `--model <value>` (before the session-anchoring flags); codex arm inserts `-m <value>` right after `codex` (before the `resume` subcommand), **only when `Some`**. Threaded: `daemon/mod.rs` reads `p.worker_model` beside `p.engine` → `JobScheduler::new(…, worker_model)` → `self.worker_model` → the call site at `session.rs:92`.
- **Worker (human-driven / Standard):** `build_chat`/`build_chat_create` (`src/bin/pmtui/session.rs`) gain the same `model: Option<&str>` and emit `--model`/`-m`; the call sites in `start_undriven_session` (`src/bin/pmtui/app/create.rs`) pass the entry's `worker_model`. *Why include this:* the worker is the worker regardless of who drives it; skipping Standard would make the model silently not apply to hand-driven sessions.
- **Decider:** `spawn_advice(…, decider_engine, decider_model: Option<&str>)` — sourced from `config.decider_model` at `marker.rs:519` (and the `drive_marker_only` fast-path). Claude arm: `decider_model` if `Some` else env→const (today's logic). Codex arm: `decider_model` if `Some` else env→omit. The builders (`build_supervisor_command`, `build_supervisor_command_codex`) are unchanged in shape (claude takes `&str`, codex `Option<&str>`); only the resolved value changes.

### 4. pmtui — one reusable picker + a create-form field

**Model catalog cache.** `App` gains `model_catalog: HashMap<Engine, Vec<ModelInfo>>`, filled lazily via `crate::models::available_models(engine)` when a picker or the create form first needs an engine's list. Render never does I/O — it reads the cache.

**The picker overlay** replaces `UiMode::DeciderPicker` with a reusable:

```rust
UiMode::ModelPicker { target: PickTarget, id: String, stage: PickStage, engine: Engine, cursor: usize }
enum PickTarget { Decider, Worker }
enum PickStage  { Engine, Model }
```

- **`e` (decider):** open `{ Decider, id, stage: Engine, engine: config.decider_engine, cursor: idx(engine) }` (autopilot-only; Standard refuses and names `m`, unchanged).
  - **Engine stage:** list `Engine::ALL`, current `●`, cursor `▸`; ↑↓/jk move; **Enter** → set `engine = ALL[cursor]`, go `stage: Model`, cursor on the current model; **Esc** → cancel.
  - **Model stage:** list `[(default)] ++ catalog[engine]`, current model `●`, cursor `▸`; **Enter** → commit `(engine, model)` to `config.json` (`decider_engine` + `decider_model`); **Esc** → back to Engine stage.
- **`w` (worker):** open `{ Worker, id, stage: Model, engine: <entry.engine>, cursor: idx(worker_model) }` for the selected agent-loop row. Model stage only; **Enter** → commit model to `registry.json` (`worker_model`), status *"worker model → <label>; press r to restart the agent to apply"*; **Esc** → cancel. (No Engine stage — worker engine is create-time.)
- `[(default)]` is the first model item; selecting it stores `None` (clear the field → CLI default). The `●` current marker compares the stored value against catalog `value`s; a stored value absent from the catalog renders as its raw string.
- One renderer `render_model_picker` (generalized from `render_decider_picker`): title `Decider · <id>` / `Worker · <id>`, a sub-header (`Engine` / `Model · <engine>`), the marked list. One keys handler block (stage-aware clamp/commit/back). `Scope::ModelPicker` in bindings + the keybar mode→scope map.
- **`begin_decider_pick`/`commit_decider_pick` become `begin_model_pick(target)` / advance / `commit_model_pick`.**

**Create form:** add a **Worker Model** field (after Engine). `CreateForm` gains `worker_model: Option<String>` and `model_choices: Vec<String>` (the current engine's labels, `[(default)] ++ catalog[engine]`), repopulated when the form opens and on engine toggle; ←/→ cycles it. Field indices/`FIELDS`/`shows_field` renumber accordingly; the model field shows always (a worker always launches). On submit, the chosen value is written to the new entry's `worker_model`. A **Decider Model** field was later appended after the Decider engine field (autopilot-only, index 7) and seeds `config.decider_model` — see Decisions.

## Keys

- **`e`** — reworded: open the decider **engine + model** picker (autopilot). Existing binding, `Applies::Autopilot`.
- **`w`** — new: open the **worker** model picker for the selected session (any agent-loop row), `Applies::AgentLoop`. Chip label "Worker", high shed rank. Help: "Set the worker model for the selected session (restart to apply)".
- Picker keybar (`Scope::ModelPicker`): `Enter` select · `↑↓/jk` move · `Esc` back/cancel. The Normal-key help-drift test stays green (`e` and `w` are Normal-bound with help).

## Testing

- **models module:** fixture-based unit tests — Claude explicit allowlists, subscription aliases, override resolution, explicit-empty preservation, and malformed/null fail-closed behavior; Codex visibility filtering, display-name fallback, and failure handling.
- **schema:** round-trip + legacy-load (missing field → `None`) for both `decider_model` and `worker_model`.
- **launch argv:** `build_loop_command` claude `--model`/codex `-m` present iff `Some`, correct position; `build_chat`/`build_chat_create` same; `spawn_advice`/builders use `decider_model` when set and fall back when not.
- **pmtui (FakeDriver/TestBackend):** `e` opens Engine→Model, commit writes `decider_engine`+`decider_model` to config only (ledger byte-identical); `w` opens Model, commit writes `worker_model` to registry only; `(default)` clears to `None`; Esc back/cancel; cursor clamps; catalog empty → only `(default)`; create-form model field cycles and seeds `worker_model`; bindings-drift test green.
- **Live render (mandatory, per pmtui-ui-testing skill):** capture both pickers on real tmux (engine→model for `e`, model for `w`) — new geometry TestBackend can't prove lands in-frame.
- **Decider benchmark** (touching the decider consult): run it (`PM_DECIDER_BENCH=1`, both engines) — MUST-safety + infra hard gates must stay green; with no `decider_model` set it exercises the unchanged env/const fallback.
- Standing basics: `cargo test`, `cargo clippy --all-targets`, `cargo fmt --all -- --check`, and the `#[ignore]`d real-tmux acceptance before merge.

## Out of scope (deferred, not precluded)

- Live worker **engine** switch (footgun; the picker's stage machine already supports adding it).
- Per-engine **reasoning effort** selection (codex exposes `supported_reasoning_levels`; a natural follow-on, same picker).
- Any change to the decider's always-escalate floor, the byte-pure policy gate, or `advise::validate` (model-agnostic).
