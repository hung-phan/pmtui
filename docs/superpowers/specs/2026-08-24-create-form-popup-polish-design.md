# Create popup: styling polish — snug height, a group divider, aligned inline list

**Date:** 2026-08-24
**Status:** implemented @ feat/create-form-popup-polish (user picked: snug height, group divider, aligned inline list; declined capping the list)
**Builds on:** [`2026-08-24-create-form-inline-model-list-design.md`](2026-08-24-create-form-inline-model-list-design.md) — the ~80% single-column popup with the inline model list. This is pure visual polish on that render; no interaction/schema/layout-model change.

## Problem

Rendered the shipped popup on real tmux and looked at it. Three weak spots (color/border/title-chip/emphasis are already on-convention — leave them):
1. **Dead space on Standard.** With 4 fields the popup keeps a min-body floor (`CREATE_MIN_BODY_ROWS = 8`) and `overlay_h`'s 9-row floor, so ~5 empty rows sit between *Autonomy* and the footer — reads as unfinished.
2. **Flat field stack.** On Autopilot the 8 fields are one undifferentiated column — no visual grouping of "which agent" (Engine/Model) vs "how it's driven" (Autonomy/Cadence/Decider/D-Model).
3. **Inline list indent floats.** The focused model field's list is indented 6 columns — between the label (col 2) and the value (col 12) — so it doesn't read as belonging to the `< value >` it expands.

(Explicitly NOT changing: the model list still shows the whole catalog windowed at `MODEL_LIST_WINDOW = 12` — the user declined shortening it.)

## Decisions (with rationale)

- **Snug, content-driven height with symmetric breathing, not a blank-padded floor.** Drop the `CREATE_MIN_BODY_ROWS` floor and stop routing height through `overlay_h` (whose 9-row floor is what over-pads a 4-field form). Compute the popup height directly from the actual content: one deliberate blank row at the top for breathing + the shown rows + one bottom slack row (which also absorbs the Autonomy descriptor's wrap on a narrow terminal), + the 2 border rows (`OVERLAY_CHROME_H`). The card hugs its content at any tier, with intentional 1-row top/bottom air instead of a stack of trailing blanks. *Why:* directly fixes the "unfinished" look; padding you can see the reason for reads as generous, empty rows read as broken. The width already delivers "bigger" — height should track content.
- **One dim divider between Directory and Autonomy.** A full-width dim horizontal rule (unlabeled) splits the card into "the session" (Engine, Model, Directory) above and "how it's driven" (Autonomy — plus Cadence/Decider/D-Model on Autopilot) below. *Why:* gives the eye hierarchy (the user's stated preference) with a single, tier-agnostic cue. Unlabeled because any label ("autopilot") would be wrong on Standard, where only the Autonomy dial sits below the rule. Dim, not bold/colored — it is structure, not emphasis.
- **Align the inline list under the value column.** Indent the list rows (and the `… N more` note) to column 12 — where a field's `< value >` begins (2-col marker + 10-col label) — so `●`/`○` sit under the `<` and the list's `(default)` label lands directly under the summary's `(default)`. *Why:* the expanded list visibly belongs to the value it came from, instead of floating in the label gutter.

## Behavior (`render/create.rs` only)

- **Height:** remove `const CREATE_MIN_BODY_ROWS` and the `rows_shown.max(CREATE_MIN_BODY_ROWS)` line; stop calling `overlay_h`. Keep the analytical `rows_shown` (fields + inline-list rows via `model_field_rows`) and add `+1` for the divider row. Then `body_rows = rows_shown + 2` (1 top pad line + 1 bottom/wrap slack); `want_h = (body_rows_u16 + OVERLAY_CHROME_H).min(cap_h).min(area.height - OVERLAY_MARGIN_Y).max(1).min(area.height)`, `cap_h = area.height*4/5`. `want_w` unchanged (~80%). Prepend one blank `Line::from("")` as the first body row.
- **Divider:** build a dim full-inner-width rule `Line::from(Span::styled("─".repeat(inner.width), Style::default().add_modifier(Modifier::DIM)))` and push it into `rows` between the Directory row and the Autonomy row (always — both are shown at every tier). Count it in `rows_shown` (+1) so the height fits it.
- **List indent:** replace the two `Span::raw("      ")` (6 spaces) in `model_rows` (the list-row indent and the `… N more` indent) with a 12-column indent (a `VALUE_COL`-derived constant, matching the 2-col marker + 10-col label the `field_row`/`caret_row` use), so the list aligns under the value column. Everything else in `model_rows` (the `●`/`○` markers, bold-current styling, windowing) is unchanged.
- No change to field order, indices, the ←→/↑↓ interaction, `MODEL_LIST_WINDOW`, colors, the border/title-chip/surface, or the footer hint.

## Testing

- **Render (TestBackend):** on Standard, no run of ≥3 consecutive blank rows inside the card (snug — the old floor produced them); the popup height for a 4-field Standard form is smaller than before the change. A dim rule row (`─`…) appears between Directory and Autonomy at both tiers. On Autopilot with a focused Worker Model, the inline list rows begin at column 12 (aligned under the value), the `●` still marks the current, and stepping ←→ still moves it. The whole catalog still lists (windowed at 12 with `… N more`). Tiny-size sweep (1×1 … 200×50) still does not panic. No side panel / "Models" text.
- **Live render (mandatory, pmtui-ui-testing skill):** capture Standard (no dead space) and Autopilot+focused-Model (divider present, list aligned under the value) on real tmux.
- **Real-tmux acceptance (MANDATORY — create-form change):** run `--ignored`; the field set/indices and interaction are unchanged, so form-driving tests hold; fix any test that counted exact popup rows/geometry.
- Standing: `cargo test`, clippy `--all-targets`, `fmt --check`.

## Out of scope

- Capping/shortening the model list (user declined); typing/filter; the live `e`/`w` ModelPicker; any schema/launch/interaction change; touching other overlays or `overlay_h`/`draw_overlay_frame_in` themselves.
