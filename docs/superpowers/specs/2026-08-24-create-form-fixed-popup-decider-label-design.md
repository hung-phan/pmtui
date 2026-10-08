# Create popup: fixed (non-resizing) height + full "Decider Model" label

**Date:** 2026-08-24
**Status:** implemented @ feat/create-form-fixed-popup-decider-label (full "Decider Model" label; fixed-size window that reserves a ~10-row model-list area — chosen — so it no longer grows when a model field is focused)
**Builds on:** [`2026-08-24-create-form-popup-polish-design.md`](2026-08-24-create-form-popup-polish-design.md) — the snug, content-driven popup with the inline model list. This CHANGES the height model (snug/growing → fixed/reserved) and the decider-model label; everything else (interaction, colors, border/chip, divider, ~80% width, inline list) stays.

## Problem

Two pieces of user feedback:
1. *"in create new panel, we use D-Model, can we have the full name decider model?"* — the decider model field is labelled `D-Model` (abbreviated to fit the 10-col label gutter). The user wants the full **Decider Model**.
2. *"the new window size expand when we choose model. Can we have fixed size window? it needs more height."* — focusing a model field expands its inline catalog, which GROWS the popup (content-driven height). The jump as you navigate is jarring. The user wants a **fixed-size** window that is **taller**, with the list scrolling inside a reserved area instead of resizing the window. (Chosen option: reserve a ~10-row model-list area.)

This reverses the earlier "snug, no dead space" height decision — the user, having used the resizing version, prefers a stable, roomier window over snugness. The reserved list area is blank when no model field is focused; that is the accepted cost of no-resize.

## Decisions (with rationale)

- **Full label "Decider Model"; widen the label column to fit.** The label gutter is a fixed left-aligned column; `Decider Model` is 13 chars, so widen the column to `LABEL_W = 14` (13 + 1 space). All labels (Engine/Model/Directory/Autonomy/Goal/Cadence/Decider/Decider Model) align their values at the new value column. *Why:* the user asked for the full name; column-aligned values read cleaner than a ragged edge, and the extra whitespace for short labels is a fair trade for clarity.
- **Fixed popup height that reserves a `MODEL_LIST_VIEWPORT = 10`-row model-list area, independent of which field is focused.** Height = top-pad + the shown fields + the group divider + the reserved 10-row viewport + a wrap-slack row + the 2 border rows — computed WITHOUT reference to `form.field`. Focusing a model field fills the reserved viewport with its list (windowed/scrolling, `● ` current kept visible, dim `… N more`); focusing a non-model field leaves the viewport blank. *Why:* directly fixes "expands when we choose model" — the window is the same size whichever field is focused, and it is taller (the reserve adds height). The list scrolls inside rather than growing the frame.
- **The inline list windows to the reserved viewport.** `win = list_len.min(MODEL_LIST_VIEWPORT)`, scrolled to keep the current (`●`) row visible, with `… N more` when the catalog overflows the 10 rows. *Why:* the reserve is fixed, so the list must fit it (claude's ~15 models show 10 + "… 5 more").
- **No interaction / schema / discovery / other-overlay change.** Only `render_create` (height, label width, the D-Model string) changes; the ←→/↑↓ interaction, field order/indices, the divider, colors, the ~80% width, and every other overlay are unchanged.

## Behavior (`render/create.rs` only)

- **Label width + value column:** add `const LABEL_W: usize = 14;`. `field_row`/`caret_row` format the label as `{label:<LABEL_W$}` (or `format!("{label:<width$}", width = LABEL_W)`). Set `VALUE_COL = 2 + LABEL_W` (= 16; the 2-col focus marker + the label column). `value_cols = usize::from(inner.width).saturating_sub(VALUE_COL + 1)`. The inline list indent (list rows AND the `… N more` note) uses `" ".repeat(VALUE_COL)`.
- **Rename:** the decider model row label `"D-Model"` → `"Decider Model"` (and update its nearby comment: the full name, not an abbreviation, distinguishes it from the worker's Model row).
- **Fixed height:** remove the `model_field_rows` closure, the `MODEL_LIST_WINDOW = 12` const, and the content-driven/`cap_h` growth. Add `const MODEL_LIST_VIEWPORT: usize = 10;`. Compute:
  ```
  let base_fields = 4                                   // Engine, Model, Directory, Autonomy
      + usize::from(form.shows_goal())
      + usize::from(form.shows_cadence())
      + if form.shows_decider() { 2 } else { 0 };       // Decider + Decider Model
  let rows_shown = base_fields + 1 /*divider*/ + MODEL_LIST_VIEWPORT;   // NO form.field term
  let body_rows = u16::try_from(rows_shown + 2 /*top pad + wrap slack*/).unwrap_or(u16::MAX);
  let want_h = body_rows
      .saturating_add(OVERLAY_CHROME_H)
      .min(area.height.saturating_sub(OVERLAY_MARGIN_Y))
      .max(1)
      .min(area.height);
  ```
  `want_w` (~80%) unchanged. Keep the prepended blank top-pad `Line::from("")` and the dim divider between Directory and Autonomy.
- **Inline list:** in `model_rows`, window to `MODEL_LIST_VIEWPORT` (was `MODEL_LIST_WINDOW`): `win = list_len.min(MODEL_LIST_VIEWPORT)`; same scroll-to-keep-`●`-visible + `… N more` logic; indent `VALUE_COL`.

## Testing

- **Render (TestBackend):**
  - The decider model row reads **"Decider Model"** on Autopilot; `"D-Model"` appears nowhere; values stay column-aligned (the worker "Model" value and the "Decider Model" value start at the same column).
  - **Fixed height (the core fix):** the popup's height is the SAME when Directory is focused as when Worker Model is focused (no growth on model-focus) — assert equal border-row counts. (This REPLACES the old "grows when a model field is focused" test, whose premise is now inverted.)
  - Remove/replace the old `create_popup_has_no_dead_space_on_standard` snugness test — the fixed design intentionally reserves the viewport; assert instead the height is stable across focus.
  - Focused Worker Model still lists `(default)` + catalog with the current `●`, windowed to 10 with `… N more` for a 15-item catalog; stepping ←→ still moves the `●`; the list indents to the new `VALUE_COL`.
  - Tiny-size sweep (1×1 … 200×50) still does not panic.
- **Live render (mandatory, pmtui-ui-testing skill):** on real tmux, confirm the popup does NOT change height when you Tab between a non-model field and a model field; the "Decider Model" label shows in full; the list scrolls inside the reserved area.
- **Real-tmux acceptance (MANDATORY — create-form change):** run `--ignored`; interaction/indices unchanged, so form-driving tests hold; fix any test asserting exact popup geometry or the `D-Model` string.
- Standing: `cargo test`, clippy `--all-targets`, `fmt --check`.

## Out of scope

- Changing the interaction, field order/indices, the ~80% width, colors, border/title-chip, or the divider; typing/filter; the live `e`/`w` ModelPicker; any schema/launch/discovery change; other overlays.
