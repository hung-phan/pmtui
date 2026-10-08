# Create form: ←→ model stepper + a side list panel, in a genuinely bigger popup

**Date:** 2026-08-24
**Status:** implemented @ feat/create-form-side-panel — but its RENDER (the right-hand side panel / two-column split) is SUPERSEDED by [`2026-08-24-create-form-inline-model-list-design.md`](2026-08-24-create-form-inline-model-list-design.md), which moves the model list inline below the focused field in a bigger ~80% popup. The ←→ stepper interaction it introduced is kept; only the two-column presentation is replaced. Body below is retained for history.
**Supersedes the interaction of:** [`2026-08-24-create-form-model-combobox-design.md`](2026-08-24-create-form-model-combobox-design.md) — the inline combobox (↑↓ entered a filtered list, typing filtered/entered custom) was hard to use: "when we tap down to select model, it is hard to go back", and the popup didn't read as bigger (it only grew when a model field was focused). This replaces that interaction.

## Problem

Two pieces of user feedback after using the shipped combobox:
1. *"I don't see the creation window panel with n is bigger."* — the popup only grew when a Model field was focused (the list appeared then); on open (Engine focused) it was the same size, and the width bump (80→88) is imperceptible.
2. *"the button to select model is hard to use, since when we tap down to select model, it is hard to go back."* — ↑↓ switched into a list-navigation mode, so leaving/returning was unclear.

User's design (adopted): **"up and down is used to select field, and left and right is to select value. We can keep the selection box as before. but put it on the side and show for user to know where they are in model selection."**

## Decisions (with rationale)

- **↑↓ always navigate fields; ←→ change the value — for EVERY field, model fields included.** No mode switch, so there is no "getting stuck in the list." *Why:* the confusion was ↑↓ meaning two different things; making it uniformly field-nav is what the user asked for and removes the trap.
- **Model fields are a ←→ STEPPER over `[(default)] ++ choices`** (exactly like the Engine/Autonomy/Cadence toggles), storing `None` at `(default)` else the catalog `value`. *Why:* consistent with the other fields; this is the pre-combobox behavior restored. The value is derived-from / written-to `worker_model`/`decider_model` — no separate cursor state, no `ModelCombo`.
- **A SIDE PANEL (right column) lists the focused model field's models with the current one marked**, so ←→ stepping is visible, not blind ("show where they are in model selection"). *Why:* the user wants to see the list + position while stepping; a blind `< value >` cycle hides it.
- **No typing / no filter / no custom value** (the user chose pure ←→ select over select-or-type). *Why:* simplest, matches the described interaction; a custom/undiscovered model is out of scope now (deferred). So a stored value is always `None` or a catalog `value` — it always maps to a panel row.
- **The popup is genuinely, consistently bigger** — a two-column layout (fields | side panel) at a wider width, present whenever the form is open (not only when a model field is focused). *Why:* directly fixes "I don't see it's bigger."
- **No schema/launch/discovery change.** Reuse `Config.decider_model`, `ProjectEntry.worker_model`, `App::model_catalog`/`models_for`, `CreateForm.model_choices`/`decider_model_choices`, and all seed/launch threading. Pure input-UX change; single-writer unchanged.

## Behavior

- Remove the combobox: drop `ModelCombo` and its typing/filter/↑↓-list handling. Model fields hold only `worker_model`/`decider_model: Option<String>` (+ the existing `model_choices`/`decider_model_choices` catalogs).
- Restore ←→ stepping for the two model fields (in `CreateForm::adjust`): a `step_model(current: Option<String>, choices, forward) -> Option<String>` that derives the current index in `[(default)] ++ choices` (0 = `(default)` = None; else the index of the stored `value` + 1, defaulting to 0 if not found), steps ±1 mod `len+1`, and returns `None` at 0 else `choices[i-1].value`. Wire `WORKER_MODEL`/`DECIDER_MODEL` arms to it. `toggle_engine`/`toggle_decider_engine` still reset the matching model value to `None` (list changes with engine), and the App still repopulates the choices on an engine flip.
- Keys: model fields are once again ordinary ←→ toggles — REMOVE the special `is_model_field` combo branch in `keys.rs handle_create_key` (↑↓ → `next_field`/`prev_field`, ←→ → `adjust`, as for every other toggle). `is_text_field`/`text_field` unchanged (model fields are not text). No paste-into-combo branch.

## Render (`render/create.rs`) — two columns

- Split the overlay `inner` horizontally: a **left fields column** and a **right side panel** (e.g. Layout `[Min(fields_w), Length(panel_w)]`, `panel_w ≈ 34`). Overall width grows to ~120 (or clamp to the area) so both columns fit — the popup is now clearly bigger on open.
- **Left column:** the field rows as today, but model rows show `‹ label ›` (or `< label >`), with the focused-field marker. `(default)` shows `‹ (default) ›`.
- **Right side panel** (a bordered/titled sub-block): the list for the **focused model field's** engine — `(default)` then each `ModelInfo.label` — with the CURRENT value marked `●` (derived from the stored value; `(default)` row `●` when None) and, since ← → drive it, the current row is the highlight (bold). Windowed to the panel height with a dim `… N more` when the catalog overflows; keep the current row visible. When a NON-model field is focused, the panel shows a dim hint (e.g. "Models — focus Model / D-Model and use ←→") and/or the Worker model list as context — pick one; the column stays allocated so the popup size is stable.
- Footer hint updates to `↑↓ field · ←→ value · enter create · esc`.
- Keep the `.wrap()` slack the Autonomy descriptor needs; the side panel must not clip it. Height is the field-rows height (no longer grows on model-focus, since the list is a side column of fixed height).

## Testing

- **Stepping unit tests** (`step_model`, pure): from None, forward → first choice value; wraps at the end back to None; backward from None → last choice; empty catalog → stays None. Toggling the engine resets the value to None.
- **Create-form integration:** ←→ on Worker Model steps `worker_model` through the catalog values and back to None; ↑↓ still moves between fields (model fields no longer capture ↑↓); the same for Decider Model on Autopilot; submit writes the stepped value into the entry / config.
- **Render (TestBackend):** with Worker Model focused, the side panel lists `(default)` + the catalog labels and marks the current with `●`; stepping ←→ moves the `●`; with a non-model field focused the panel shows its hint/context; a tiny-size sweep (1×1 … 200×50) does not panic; the popup is wider than the pre-change single column.
- **Live render (mandatory, pmtui-ui-testing skill):** capture the two-column popup on real tmux; confirm it is visibly bigger on open, ↑↓ move fields, ←→ step the model with the side-panel `●` tracking, and both model fields drive their own panel.
- **Real-tmux acceptance (MANDATORY — create-form change):** run `--ignored`. The field set/indices are unchanged and ↑↓/←→ semantics for non-model fields are unchanged, so the existing form-driving tests (n/Tab/Space/typing/Enter) should hold; fix any test that assumed the combobox's ↑↓-list/typing behavior. (Every prior create-form change was only caught RED here.)
- Standing: `cargo test`, clippy `--all-targets`, `fmt --check`.

## Out of scope

- Typing / filtering / custom-model entry (the user chose pure select; deferred — could return as a keypress that opens a text entry).
- The live `e`/`w` `ModelPicker` overlay (unchanged).
- Any schema/launch/discovery change; reordering non-model fields.
