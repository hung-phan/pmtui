# Create form: bigger popup + inline select-or-type model combobox

**Date:** 2026-08-24
**Status:** SUPERSEDED by [`2026-08-24-create-form-side-panel-design.md`](2026-08-24-create-form-side-panel-design.md) — the inline combobox interaction was replaced by a ←→ model stepper + a right-hand "Models" side panel in a bigger two-column popup (implemented @ feat/create-form-side-panel). (Historically: implemented @ feat/create-form-combobox — `ModelCombo` behavior + key routing + the bigger-popup/expanded-list render.)
**Relates to:** [`2026-08-24-model-selection-design.md`](2026-08-24-model-selection-design.md) — that added the per-session model fields (`Config.decider_model`, `ProjectEntry.worker_model`), the `src/models/` catalog, and the create-form Worker/Decider **Model** fields as `< value >` ←/→ toggles. This replaces those two toggles with an inline combobox and enlarges the create popup.

## Problem

User feedback on the `n` create-a-session popup: *"the new workspace UI can be much better, you can display a bigger popup … Model selection can be displayed so client can select or type in."* Today each model field is a `< value >` toggle cycled with ←/→ — you can't see the choices and you can't type a model that discovery didn't list. The user chose (over a picker-overlay alternative) an **inline** design: the focused Model field shows the discovered list right in a **bigger** popup, and you either arrow-select or type (filter / custom).

## Decisions (with rationale)

- **Inline combobox, only the FOCUSED model field expands.** The focused Worker/Decider Model field shows a text input plus a short scrollable list of its engine's models; the *other* model field stays a one-line summary. *Why:* the user asked for the list "displayed" in a bigger popup (not behind a second keypress), but two full lists at once would dominate the form — expand-on-focus keeps it readable and bounded.
- **The typed FILTER and the cursor SELECTION are decoupled.** Typing edits the filter and re-filters the list (the cursor resets to the top of the new list); ↑↓ move the cursor over the *filtered* list WITHOUT rewriting the filter, so you can walk the whole list. *Why:* an initial "the input is the single source of truth, ↑↓ fills the input from the row" design was corrected (the round-1 arrow-walk fix) because rewriting the filter on every ↑↓ re-filtered the list to the highlighted label and collapsed arrow navigation onto the first match — you could never step to the second entry.
- **Resolve at edit-time into the existing `Option<String>`.** On every keystroke/cursor-move the field recomputes its stored value: empty input → `None` (the `(default)` row / CLI default); input equal (case-insensitive) to a catalog `label` or `value` → that catalog **`value`** (launch-ready); otherwise → the typed text verbatim, trimmed (empty → `None`). *Why:* keeps `worker_model`/`decider_model` and all downstream seed/launch wiring exactly as shipped — the combobox only changes how the value is entered. "Type in" therefore accepts a model discovery didn't list (custom slug/id passed verbatim).
- **Tab owns field navigation; ↑↓ owns the list.** When a model field is focused, ↑↓ move the combo cursor (not the form's field), and Tab/Shift+Tab leave to the next/prev field. ←/→ move the input caret. On every non-combo field, ↑↓/←/→ behave as today (field nav / toggle cycle). *Why:* ↑↓ can't both move fields and drive the list; Tab-for-fields + arrows-for-widget is the conventional split, and it's the only reroute needed.
- **Bigger popup, height grows only when a combo is open.** Width bumps (80→~88) and the height is computed = the visible field rows + (the expanded list's rows when a model field is focused). Clamped to a fraction of the terminal. *Why:* honors "bigger popup" without a fixed oversized box when no list is showing.
- **No schema/launch/discovery change.** Reuses `Config.decider_model`, `ProjectEntry.worker_model`, `App::model_catalog`/`models_for`, and the seed/launch threading verbatim. *Why:* this is a pure input-UX change; the plumbing already works and is tested.

## The combobox

A small reusable field type (in `create_form.rs`, or a `create_form/combo.rs` submodule):

```rust
pub(crate) struct ModelCombo {
    input: Field,   // caret-editable FILTER text (decoupled from the cursor)
    cursor: usize,  // SELECTION index into the *displayed* list (row 0 = (default))
}
```

- **Displayed list** for a combo over `choices: &[ModelInfo]` and the current `input`:
  row 0 is always `(default)`; rows 1.. are the `choices` whose `label` contains `input.trim()` case-insensitively (all of them when the input is empty or exactly equals a choice — see below). The cursor is clamped to `0..list_len`.
- **Keys (when a model field is focused):**
  - printable / Backspace / Delete / ←/→ / Home / End → edit `input` (via the shared `Field`); after any edit, re-filter and clamp the cursor to `0` (top of the new list).
  - ↑/↓ → move `cursor` over the displayed (filtered) list; **↑↓ move the cursor only; the filter text is unchanged**, so consecutive steps walk the list instead of collapsing onto the first match.
  - Tab / Shift+Tab (↓/↑ are taken) → leave to next/prev field.
  - `^E` is NOT offered on a model field (it belongs to Goal).
- **Resolved value** (`fn resolve(&self, choices) -> Option<String>`): an explicit arrow SELECTION wins; otherwise the typed FILTER decides — `if cursor > 0 { filtered.get(cursor - 1).map(|m| m.value) } else { let t = input.trim(); if t.is_empty() { None } else if let Some(m) = choices.find(|m| m.label.eq_ignore_ascii_case(t) || m.value == t) { Some(m.value) } else if filtered is exactly one { that one's value } else { Some(t.to_string()) } }`. (`filtered` = the choices matching the current filter; so typing `sonnet` down to a single match resolves to it.) Kept in sync into `CreateForm.worker_model` / `.decider_model` on every edit/move.

The two `CreateForm` fields `worker_model`/`decider_model` (`Option<String>`) stay; add a `ModelCombo` per model field (or one `ModelCombo` reused for whichever is focused, re-seeded on focus-in from the stored value). `model_choices`/`decider_model_choices` (already present) feed the filter. `toggle_engine`/`toggle_decider_engine` still reset the stored value to `None` and the App still repopulates the choices on an engine flip; additionally re-seed the combo's `input` to empty.

## Render (`render/create.rs`)

- Width ~88; body height = field rows + the focused combo's list block.
- A focused model field renders: the label + an input line with the create form's movable caret (reuse `caret_row`'s caret view), then up to `MODEL_LIST_ROWS` (≈6) list rows: `▸`/`  ` cursor + `●`(stored value)/`○`(other)/(default) marker + label, windowed to keep the cursor visible, with a dim `… N more` when the filtered list overflows the window.
- An unfocused model field renders one summary line: `< (default) >` or `< label >` (label looked up by stored value in its choices, raw value fallback) — same as today's toggle line.
- Keep the `.wrap()` slack the Autonomy descriptor needs.

## Keys (`keys.rs handle_create_key`)

Route to the focused combo when `form.field` is a model field: printable/Backspace/Delete/←/→/Home/End → combo input edit; ↑/↓ → combo cursor move; Tab/BackTab → field nav; Enter → submit (unchanged); Esc → cancel (unchanged). For every other field, keep today's behavior (←/→ toggle, ↑↓ field nav, typing into dir/goal). `is_text_field`/`text_field` gain the model fields as "accepts caret keys" without becoming the goal/dir text buffer — cleanest is a `focused_model_combo(&mut self) -> Option<&mut ModelCombo>` accessor the handler checks first.

## Testing

- **Combobox unit tests** (pure, deterministic — pre-seed choices): filter by substring (case-insensitive); ↑↓ walk the *filtered* list without rewriting the filter and resolve to the highlighted catalog **value**; the `(default)` row resolves to `None`; typing a custom string not in the catalog resolves to that string verbatim; typing then clearing → `None`; cursor clamps on filter change; empty catalog → only `(default)`.
- **Create-form integration:** focusing Worker Model expands the list and Decider Model collapses (and vice versa); Tab still walks all shown fields; submitting with a selected model writes the catalog `value`, with a typed custom string writes that string, with empty writes `None` — into `worker_model` (registry entry) and `decider_model` (config, via `seed_agent_loop`). Single-writer unchanged (ledger byte-identity still holds — no new write path).
- **Render (TestBackend):** the focused combo shows the input + list rows + markers; the unfocused one shows a summary; the popup fits (no panic) across widths incl. narrow, and the height grows when a combo is open.
- **Live render (mandatory, pmtui-ui-testing skill):** capture the bigger popup with a model field focused on real tmux — new geometry.
- **Real-tmux acceptance (mandatory — create-form change):** the field *set* is unchanged (still engine, worker-model, dir, tier, goal, cadence, decider, decider-model) so Tab counts should hold, but the model fields now consume ↑↓/typing differently — run `--ignored` and fix any form-driving test that relied on ←/→ cycling a model toggle. (Prior create-form changes were only caught RED here.)
- Standing: `cargo test`, clippy `--all-targets`, `fmt --check`.

## Out of scope

- The live `e`/`w` `ModelPicker` overlay stays as-is (list only; no typing). Adding "type a custom model" there is a natural follow-on but not part of this change. (If desired later, the same `ModelCombo` could back the picker's Model stage.)
- Reordering/removing any non-model field; changing discovery, schema, or launch wiring.
