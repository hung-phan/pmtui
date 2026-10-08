# Decider selection panel (`e` opens a pick-list, not a cycle)

**Date:** 2026-08-23
**Status:** implemented @ feat/decider-selection-panel
**Relates to:** [`2026-08-23-switchable-decider-engine-design.md`](2026-08-23-switchable-decider-engine-design.md)
— that feature added the per-session `config.decider_engine` and the `e` key that **cycles**
claude⇄codex. This design replaces that cycle with a **selection panel**, and structures it so
selecting the decider **model** can be added later.

## Problem

`e` currently *cycles* the decider engine (`App::cycle_decider_engine`, `src/bin/pmtui/app/autopilot.rs`):
each press flips claude→codex→claude and writes `config.json`. The user's feedback: cycling is poor
UX — you can't *see* the choices, you can overshoot, and it doesn't generalize. They want `e` to open
a **panel to select** the engine, and want the same surface to eventually let them **select the
model** too.

## Decisions (with rationale)

- **Reuse `e` to OPEN a panel** (not a new key). *Why:* `e` is already the decider key with a
  "Decider" chip; the action changes from "cycle" to "open the picker," so no new binding and the
  keybar stays as-is. (`u` was considered and rejected — it adds a chip and a second thing to learn.)
- **A single-select list overlay**, cursor-navigable, current choice marked, Enter commits, Esc
  cancels. *Why:* it's the clearest "select, don't cycle" UX and mirrors the existing answer-overlay
  option cursor (`UiMode::Answering { choice }` + ↑↓), so it fits the codebase's patterns.
- **Engine now, model-ready structure** (not model now). *Why:* fixes the cycling complaint without
  over-building. Decider model ids are engine- and deployment-specific (no fixed list — see
  `SUPERVISOR_MODEL`/`PM_SUPERVISOR_CODEX_MODEL`), so model selection needs its own preset-or-type
  design; it is explicitly deferred to "the future."
- **Autopilot-only, same as the cycle it replaces.** The decider consult only runs on a row `pmd`
  drives, so on a Standard row `e` refuses and names `m` (unchanged behavior). The panel opens only
  on an autopilot row.
- **Single-writer preserved.** Committing a selection writes only `config.json` (via the same
  read-modify-write `cycle_decider_engine` already uses), never the ledger.

## The panel

**Interaction.** On an **autopilot** row, `e` opens a centered overlay:

```
        ╭ Decider · <id> ────────────────────────────╮
        │  Which engine answers low-stakes            │
        │  decisions for this session?                │
        │                                             │
        │   ▸ ● claude    (current)                   │
        │     ○ codex                                 │
        │                                             │
        ╰──────── ↑↓ move · enter select · esc ───────╯
```

- The cursor (`▸`) starts on the **current** engine. `●` marks the engine on disk, `○` the others.
- `↑`/`↓` (and `j`/`k`) move the cursor; **Enter** commits the cursored engine → writes
  `config.json` → closes → status `"<id> → decider: codex"`. **Esc** closes and writes nothing.
- Selecting the already-current engine is a no-op write-wise but still closes with a benign status.
- On a **Standard** row, `e` refuses exactly as today (`"<id>: the decider only runs on autopilot —
  press m to turn it on; engine unchanged"`) — it never opens the panel there.

**Model-ready (built now, not filled now).** The overlay is a labeled single-select list. Adding
model selection later grows it into two labeled sections in the same panel — no new machinery:

```
        │  Engine   ▸ ● claude    ○ codex             │
        │  Model      ● (default) ○ …                 │
```

That step needs a preset-list-or-type decision for model ids and is out of scope here.

## Code changes

- **`src/bin/pmtui/mode.rs`** — add `UiMode::DeciderPicker { id: String, current: Engine, cursor: usize }`
  (cursor over the engine list). `id`/`current` captured at open so a refresh under the overlay can't
  desync the choice.
- **`src/bin/pmtui/app/autopilot.rs`** — replace `cycle_decider_engine` with:
  - `begin_decider_pick(&mut self)` — the `e` handler: select row → registry lookup → read
    `config.json`; if not autopilot, set the refusal status and return (same message as today); else
    open `UiMode::DeciderPicker` seeded with the current engine and cursor on it.
  - `commit_decider_pick(&mut self)` — write the cursored engine to `config.json` (the existing
    read-modify-write, generalized from "cycle" to "set to the chosen engine"), close to
    `UiMode::Normal`, set the status, refresh.
  - The engine option list is a fixed `[Engine::Claude, Engine::Codex]` (order stable), rendered via
    `Engine::label()`.
- **`src/bin/pmtui/keys.rs`** —
  - Normal dispatch: `KeyCode::Char('e') => app.begin_decider_pick()` (was `cycle_decider_engine`).
  - Add a `UiMode::DeciderPicker { .. }` handler block (mirrors the other overlay blocks): `↑`/`k`
    and `↓`/`j` move the cursor (clamped to the option count); `Enter` → `commit_decider_pick`; `Esc`
    → `UiMode::Normal` + `"decider engine unchanged"`. No text field — it's not editable.
- **`src/bin/pmtui/render/`** — a new `render_decider_picker` module + a `UiMode::DeciderPicker` arm
  in `render/mod.rs`'s overlay `match`. Centered box, the list with `▸`/`●`/`○` markers, its own
  footer.
- **`src/bin/pmtui/bindings.rs`** — the `e` row's `help` changes from the cycle wording to
  "open the decider-engine panel (autopilot)"; add a `Scope::DeciderPicker` with its keybar rows
  (`↑↓` move · `Enter` select · `Esc` cancel), like `Scope::Cadence`/`Scope::Answer`.
- **Create form** (`create_form.rs`) — **unchanged.** The create-time Decider toggle is a different
  surface (a field in the `n` form, chosen before the session exists); it stays a toggle.

## Testing

- **pmtui (FakeDriver/TestBackend):**
  - `e` on an autopilot row opens `DeciderPicker` seeded to the current engine; on a Standard row it
    refuses with the `m` message and does NOT open.
  - Cursor `↑↓`/`jk` move and clamp to the two options.
  - Enter on `codex` writes `config.decider_engine == Codex` and closes; Enter on the current engine
    closes without changing it; **the ledger (`state.json`) is byte-unchanged** (single-writer).
  - Esc closes and writes nothing.
  - The bindings-drift test (`every_bound_normal_key_is_documented_in_the_help`) stays green with the
    updated `e` row + the new scope.
- **Live render (mandatory):** capture the panel under real tmux (per the pmtui-ui-testing skill) —
  the panel is new geometry, and TestBackend structurally can't prove it lands in-frame or that the
  cursor/markers render. Confirm the list shows, the cursor moves, and Enter commits.
- Standing basics green: `cargo test`, `cargo clippy --all-targets`, `cargo fmt --all -- --check`, and
  the `#[ignore]`d real-tmux acceptance before merge.

## Out of scope

- **Model selection** (the future second section) — needs its own preset-or-type design.
- The create-form Decider toggle (stays as a toggle).
- The send-latency feedback (separate, optional).
- Any change to the decider consult itself, the always-escalate floor, or the byte-pure policy gate.
