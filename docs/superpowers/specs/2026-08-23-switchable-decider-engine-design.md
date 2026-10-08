# Switchable decider engine (per-session `claude` ⇄ `codex`)

**Date:** 2026-08-23
**Status:** design approved; implementation not started
**Relates to:** the decider ("supervisor consult") in `src/worker/supervisor.rs` and `src/advise/`;
this design makes its engine a per-session choice. Read alongside `docs/SPEC.md` §7 (autonomy),
§10 (the decider), §16 (verification), and `2026-08-19-decider-human-directive-design.md`.

## Problem

The **decider** (the "supervisor consult") is the `claude -p` that autopilot calls to auto-answer a
low-stakes decision or refuse it (→ escalate to the human). Today it is **hardwired to `claude`**,
even when the session's *worker* runs `codex`. `build_supervisor_command` says so explicitly:

> "Claude-only, and unconditionally so even when the WORKER runs codex: only claude's headless flags
> are verified here for THIS shape (the one-JSON-envelope consult), so no codex equivalent is
> invented."

A user running codex as their worker (or in a deployment where codex is the available/preferred
engine) has no way to use codex for the decider. The request: **let the user choose the decider
engine per session, and switch to codex when they want.**

## Decisions (with rationale)

- **Per-session, independent choice.** The decider engine is its own per-session setting, not tied to
  the worker engine and not a global env. *Why:* the user wants to mix (e.g. a codex worker with a
  claude decider, or vice-versa) per project. *Rejected:* a global `PM_SUPERVISOR_ENGINE` env (would
  match how the decider's model/budget are tuned, but can't vary per project) and "follow the worker
  engine" (couples two independent choices).
- **Default is Claude, and the choice is *displayed* at create — not hidden.** The create form shows a
  **Decider** field pre-selected to Claude; the user consciously picks. *Why:* the decider
  auto-approves decisions, so the safe/validated path (claude's structured output, which the decider
  benchmark covers) is the default, but the choice is visible rather than a silent default the user
  never sees. An absent field in an existing `config.json` ⇒ Claude (backward compatible).
- **Codex is safe *by construction*, even before it is well-validated.** `advise::validate`
  re-derives every guarantee from the reply and never trusts the CLI's schema enforcement, so a codex
  reply that is not a valid verdict is **unparseable ⇒ `Capability` escalation ⇒ human**. A codex
  decider can never *mis-approve*; its worst failure mode is escalating a decision claude might have
  auto-handled. This is what makes shipping codex acceptable while its accuracy is still being
  measured.
- **The decider-benchmark re-run is the acceptance gate**, not a nice-to-have. Accuracy on
  MUST-safety and infrastructure cases is a hard gate (`docs/SPEC.md` §16). Codex is not "done" until
  it passes the benchmark.

## Config schema change

The per-session `Config` (`src/state/`) gains one field:

```rust
pub struct Config {
    pub autonomy: Tier,
    pub step_timeout_s: u64,
    // …existing fields…
    #[serde(default)]                 // absent ⇒ Engine::Claude (see the default below)
    pub decider_engine: Engine,       // NEW
}
```

- `Engine` already exists (`registry::Engine` = `Claude | Codex`); reuse it — no new enum.
- `#[serde(default)]` + a `Default`/default-fn that yields `Claude`, so every existing `config.json`
  on disk keeps working and reads as Claude.
- `pmtui` is the writer of `config.json` (never the ledger), so setting this stays within the
  single-writer boundary.

Synthetic `config.json` after the change:

```json
{ "autonomy": "autopilot", "step_timeout_s": 900, "decider_engine": "codex" }
```

## The codex consult path (the real work)

`src/worker/supervisor.rs` gains `build_supervisor_command_codex(...)` beside the claude builder. The
consult is a **different shape** for codex, because codex lacks the claude flags the current builder
leans on (`--bare`, `--json-schema`, `--output-format json`, `--system-prompt`,
`--permission-mode plan`).

**Best-known argv (every flag MUST be verified against the installed `codex` before "done"):**

```
timeout -k <grace> <secs> \
  codex exec -s read-only --skip-git-repo-check --output-last-message <tmpfile> \
  -m <model> -- <prompt>
```

- **`-s read-only`** mirrors claude's `--permission-mode plan`: the decider may read/search the tree
  (the whole point — it checks the claim it is judging) but may **not** write (the worker owns the
  tree; two writers corrupt it). Verify the exact sandbox flag/value name on the installed codex.
- **`--output-last-message <tmpfile>`** writes codex's final assistant message to a file we read and
  parse — cleaner and more robust than scraping the `--json` event stream. (Fallback: `--json` + take
  the last agent message, if `--output-last-message` isn't available.)
- **System prompt is prepended into the prompt** (codex has no `--system-prompt`). The decider's
  instructions (fence rules, refuse-on-doubt, directive semantics) become the head of the prompt text;
  `advise::validate` doesn't care how they were delivered.
- **Model pin:** new env `PM_SUPERVISOR_CODEX_MODEL` (parallel to `PM_SUPERVISOR_MODEL`), because the
  resolving model id is deployment-specific and must be changeable without a rebuild.
- **No `ECC_GATEGUARD=off env -u CLAUDECODE` prefix** — that is claude-hook-specific (the existing
  codex worker arm in `launch.rs` omits it too).
- **Timeout wrapper** (`timeout -k`) is identical to the claude path; the caller's own Rust reap
  deadline is the inner bound, unchanged.

**Output parsing.** `src/advise/` already parses a claude `--json-schema` envelope, a
`--output-format json` `.result` string, AND a bare JSON object. For codex we read the last-message
file (or last stream message) and hand its text to the existing bare-object parse. No new trust: an
unparseable/invalid verdict is treated exactly as it is today — a missing reply ⇒ escalation.

## Threading the choice

The decider spawn site (the caller that builds the supervisor argv and runs it) reads the selected
session's `config.decider_engine` and dispatches:

```
match config.decider_engine {
    Engine::Claude => build_supervisor_command(model, sys, schema, prompt, …),
    Engine::Codex  => build_supervisor_command_codex(codex_model, sys, prompt, …),
}
```

Everything downstream (the reply parse, `validate`, the auto-answer / refuse routing) is
engine-agnostic and unchanged.

## UI (`pmtui`)

- **Create form (`n`):** a new **Decider** toggle field (`Claude ⇄ Codex`), **shown autopilot-only**
  (like Goal/Cadence — it only matters when `pmd` drives the row), **pre-selected to Claude**. It
  seeds `config.decider_engine` at `submit_create`/`finish_create`.
- **Post-create switch:** a small inline toggle overlay to flip the decider engine on an existing
  autopilot row, bound to **`e`** (currently unbound; "decider **e**ngine"). Autopilot-only, refusing
  on a standard row the way `g`/`i`/`c` do (nothing consults it there — "press `m`"). Writes only
  `config.json`.
- Bindings table (`bindings.rs`) + keybar scope get one row; the `?` help overlay documents it.

## Safety model

1. **Fail-safe:** a codex verdict that doesn't validate ⇒ `Capability` escalation ⇒ human. Codex can
   never auto-approve something it shouldn't; it can only be *more* likely to escalate.
2. **The always-escalate floor is unchanged:** `publish`/`deploy`/`merge`/credentials/destructive
   actions still escalate regardless of engine (that gate is byte-pure and never consults the LLM).
3. **Benchmark gate:** the decider is only "done" on codex once `PM_DECIDER_BENCH` passes with a codex
   decider (MUST-safety + infra hard gates). The benchmark harness is extended to run the codex path.

## Testing plan

- **Unit:** both argv builders (`build_supervisor_command` unchanged + `build_supervisor_command_codex`
  shape), the `Config` serde default (absent field ⇒ Claude), and the reply parser on a codex
  last-message payload.
- **pmtui (FakeDriver/TestBackend):** the create form shows/omits the Decider field by tier and seeds
  the config; the `e` overlay flips `config.json` and refuses on a standard row; keybar/help render it.
- **Real-substrate:** a real `codex exec` decider consult against the installed CLI (verifies the
  flags this design assumes) — the load-bearing check no unit test can make.
- **Decider benchmark:** `PM_DECIDER_BENCH` with the codex engine; hard gate on MUST-safety + infra.
- Plus the standing basics green (build, clippy, fmt) and the `#[ignore]`d real-tmux acceptance.

## Risks & unknowns

- **Codex headless flags are unverified from here.** The exact read-only sandbox flag, model flag,
  and whether `--output-last-message` exists on the installed codex all need live confirmation. The
  plan front-loads a "drive `codex exec` headlessly, get a parseable verdict" spike.
- **Fallback:** if codex cannot be driven headlessly to emit a parseable verdict, we keep the decider
  claude-only, leave the `Config` field + UI in place but disabled (or drop them), and document why.
  The feature degrades to "claude decider" rather than shipping something unsafe.
- **Codex decision accuracy is unknown** until the benchmark runs; the fail-safe design bounds the
  downside to "escalates more than claude would," never "approves what it shouldn't."

## Out of scope

- Changing the *worker* engine post-create (a separate, larger change).
- A global decider-engine setting (rejected above).
- Per-decision engine selection.
- Any change to the always-escalate safety floor or the byte-pure policy gate.
