# Create form: a genuinely bigger popup + the model list INLINE below the field

**Date:** 2026-08-24
**Status:** implemented @ `feat/create-form-inline-model-list` (user picked "Big — ~80% of screen"; inline-below list, no side panel)
**Supersedes the render of:** [`2026-08-24-create-form-side-panel-design.md`](2026-08-24-create-form-side-panel-design.md) — the right-hand side panel. Keeps that design's interaction (↑↓ = fields, ←→ = value stepper; no typing/combobox) unchanged.

## Problem

Two pieces of user feedback on the shipped two-column side-panel popup:
1. *"The popup window for n is really small. I want you to make it bigger."* — the popup is capped at `OVERLAY_MAX_W = 96` columns and its height is field-driven, so on a wide terminal it reads as a small card.
2. *"the model selection on the right show all the time doesn't [make] any sense. … you should move it below the model to show all the selections instead."* — the persistent right column splits attention (edited row on the left, list on the right) and wastes a column when a non-model field is focused (it shows only a hint).

## Decisions (with rationale)

- **Drop the side panel and the two-column split.** The model list moves INLINE, directly below the focused model field's row. *Why:* the options appear where the user's eyes already are, contextually, only when a model field is focused — no wasted column, no split attention. This is the user's explicit ask and the conventional "expanding select" pattern.
- **Keep the ←→ stepper interaction verbatim.** ↑↓ (Tab/BackTab) move between fields; ←→ step the focused field's value, model fields included. No typing/filter/custom. *Why:* that interaction is what fixed the original "can't get back out of the list" complaint; only the *presentation* of the list changes here. The earlier inline combobox was hard to use because ↑↓ entered a list *mode*; with the stepper there is no mode, so inline-below is now safe.
- **The focused model field shows ALL its selections inline** — `(default)` then every model in the engine's catalog — with the CURRENT value (the stored `worker_model`/`decider_model`) marked `●` and drawn bold; the rest `○`. ←→ moves the `●`. *Why:* "show all the selections." A window + dim `… N more` is a safety net only when the list would exceed the height cap (real claude/codex catalogs are small enough to show whole).
- **The popup is genuinely bigger: width ≈ 80% of the terminal, height grows to fit (capped ≈ 80%).** *Why:* directly answers "make it bigger" (twice asked). Width 80% is roomy without empty rows; height grows with the inline list (so focusing a model field visibly enlarges the popup) and is floored so it never reads as cramped. This requires letting THIS overlay exceed the shared `OVERLAY_MAX_W = 96` cap — surgically, without changing any other overlay.
- **No schema/launch/discovery change.** Reuse `Config.decider_model`, `ProjectEntry.worker_model`, `App::model_catalog`/`models_for`, `CreateForm.model_choices`/`decider_model_choices`, and all seed/launch threading. Pure render change; single-writer unchanged.

## The bigger frame (`render/chrome.rs`)

`overlay_w(area, want_w)` hard-caps every overlay at `OVERLAY_MAX_W = 96`, and `draw_overlay_frame → overlay_rect → overlay_w`. To let ONLY the create popup grow past 96 without disturbing the other overlays (answer, confirm, editing, model_picker, sending):

- **Split the shared card-drawing body out of `draw_overlay_frame`** into `draw_overlay_frame_in(f, popup: Rect, title, accent, hint) -> Rect` — it does the `Clear` + rounded-border + raised-surface + title-chip + bottom-border-hint + padding and returns the inner body area, given a pre-computed `popup` rect. `draw_overlay_frame` becomes a one-liner: `draw_overlay_frame_in(f, overlay_rect(area, want_w, want_h), title, accent, hint)`. Every existing caller keeps the shared 96-cap width policy verbatim.
- **`render_create` computes its own big popup rect** with `centered_rect(want_w, want_h, area)` (already exported) and calls `draw_overlay_frame_in`. `want_w = (area.width * 4/5).max(96)`, clamped to `area.width - OVERLAY_MARGIN_X` (so it stays 96 on narrow terminals, grows to 80% on wide ones — e.g. 112 at 140 cols). `want_h` is the content height (field rows + the inline list when a model field is focused), clamped to `(area.height * 4/5)` and `area.height - OVERLAY_MARGIN_Y`, floored by `overlay_h`'s built-in minimum.

## Render (`render/create.rs`) — one column, inline list

- **Remove** `PANEL_W`, `MIN_TWO_COL_W`, the two-column `Layout::horizontal` split, and `render_model_panel`. Single column: the field rows fill the popup width.
- **Model field, unfocused:** one summary row `< (default) >` or `< label >` (stored value looked up in its catalog, raw-value fallback) — unchanged.
- **Model field, focused:** the summary row (bold, as the focused-field marker) THEN an indented list block below it: `(default)` + every `ModelInfo.label`, each prefixed `● `(the row whose value == the field's stored value; `(default)` row when the value is `None`) or `○ `, the `●` row drawn bold. Since ←→ drive the stored value directly, the `●` row IS the current row — there is no separate cursor. Windowed to the body's available rows keeping the `●` row visible, with a dim `… N more` only when the catalog exceeds the window.
- Height accounting grows the popup when a model field is focused: a focused model field costs `1 + min(list_len, window)` (+ overflow note) rows; an unfocused one costs 1; every other shown field costs 1 — mirroring the pre-side-panel grow-on-focus, now list-only.
- Footer hint: `↑↓ field · ←→ value · ^E $EDITOR · enter create · esc`.
- Keep the `.wrap()` slack the Autonomy descriptor needs.

## Testing

- **Frame split unit/behavior:** `draw_overlay_frame` still returns the same inner area for the existing overlays (their render tests are unchanged and must stay green); `draw_overlay_frame_in` renders the card into a given rect.
- **Render (TestBackend):** focused Worker Model shows its summary row AND, below it, `(default)` + the catalog labels with the current marked `●`; stepping ←→ moves the `●`; the popup is TALLER when a model field is focused than when Directory is (grow-on-focus); the popup is WIDER than 96 on a wide terminal (e.g. 160 cols → ~128) and stays ≤ area on narrow ones; an unfocused model field is a one-line summary; Decider Model behaves the same on Autopilot; a tiny-size sweep (1×1 … 200×50) does not panic; no side panel / "Models" title / hint text remains.
- **Live render (mandatory, pmtui-ui-testing skill):** on real tmux, the `n` popup is clearly bigger; ↑↓ move fields; on Worker Model the list shows inline below the row and ←→ moves the `●`; Tab to Decider Model shows its list inline; nothing renders in a right column.
- **Real-tmux acceptance (MANDATORY — create-form change):** run `--ignored`. Field set/indices and ↑↓/←→ semantics are unchanged, so the form-driving tests (n/Tab/Space/typing/Enter) hold; fix any test that asserted the side panel / two-column geometry.
- Standing: `cargo test`, clippy `--all-targets`, `fmt --check`.

## Out of scope

- Typing / filtering / custom-model entry (still deferred).
- The live `e`/`w` `ModelPicker` overlay (unchanged).
- Any schema/launch/discovery change; reordering non-model fields; changing the ←→/↑↓ interaction.
