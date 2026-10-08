# Create popup: taller model area + a real ratatui Scrollbar for the inline list

**Date:** 2026-08-24
**Status:** implemented @ feat/create-form-fixed-popup-decider-label (user: make the fixed popup "a bit higher"; and — after a Context7 ratatui check — use a real `Scrollbar` gutter instead of the `… N more` text, chosen over a directional-arrow cue). **Superseded on height by the follow-up below:** the 14-row reserve was reverted to a compact **4 rows** after live use — the taller reserve pushed the popup's bottom off shorter terminals. The scrollbar decision stands; only `MODEL_LIST_VIEWPORT` changed (14 → 4).
**Builds on:** [`2026-08-24-create-form-fixed-popup-decider-label-design.md`](2026-08-24-create-form-fixed-popup-decider-label-design.md) — the fixed-height popup that reserves a `MODEL_LIST_VIEWPORT`-row model-list area and windows the inline list with a dim `… N more`. This raises that reserve and replaces `… N more` with a ratatui `Scrollbar`.

## Problem

1. *"make it a bit higher."* — the fixed reserve (`MODEL_LIST_VIEWPORT = 10`) is a touch short; the user wants the window a bit taller.
2. *"on the model selection … you display sth like 5 more. … check context7 for ratatui … better UI for that. Or just have some indication when we have more model to select."* — the one-way `… N more` line is a weak overflow cue. Context7 (ratatui 0.30 `Scrollbar`) confirms a first-class vertical scrollbar widget (`ScrollbarState::new(len).position(pos)`, `ScrollbarOrientation::VerticalRight`, `begin_symbol`/`end_symbol` `↑`/`↓`, a thumb showing position). The user chose the real scrollbar.

## Decisions (with rationale)

- **Raise `MODEL_LIST_VIEWPORT` 10 → 14.** *Why:* "a bit higher" — 4 more reserved rows; still fixed, still no resize on focus. The fixed-height math already flows from this const.
- **Render a real ratatui `Scrollbar` on the inline list's right edge, replacing the `… N more` line.** When the focused model field's catalog is longer than the viewport, draw a `Scrollbar(VerticalRight)` down the right edge of the list's rows — `↑`/`↓` end symbols and a thumb whose position tracks the current (`●`) selection over the whole catalog. *Why:* a proper scrollbar shows *both* that there's more and *where you are*, which `… N more` can't; it's the idiomatic ratatui control and what the user picked. It also removes the `… N more` line, which fixes a latent off-by-one (that extra line could make the list `VIEWPORT + 1` rows — one past the reserve; without it the list is exactly `≤ VIEWPORT` and fits the reserve).
- **Scrollbar only when the list overflows the viewport.** A catalog that fits (e.g. codex's short list) shows no scrollbar and no `… N more` — nothing is hidden. *Why:* a scrollbar on a non-scrolling list is noise.
- **Keep everything else.** Fixed height, the inline-below placement, ←→/↑↓ interaction, `●`/`○` markers + bold current, the divider, ~80% width, "Decider Model" label, field order/indices — all unchanged. The live `e`/`w` ModelPicker overlay is out of scope (a natural future home for the same widget).

## Follow-up: reserve 14 → 4 (compact), after live use

Live on the dashboard the 14-row reserve was **too tall**: on shorter terminals it inflated the
popup past the screen, and since the card renders its body as one top-anchored `Paragraph` (no inner
scroll), the *bottom* rows — the last fields, and the last list rows — were clipped off with no way
to reach them. Reported as *"I cannot scroll all the way to the bottom"* and *"the scroll panel is
too big — only show me 4 rows."*

- **`MODEL_LIST_VIEWPORT` 14 → 4.** *Why:* a compact list keeps the whole popup small enough to fit
  the terminal, so its bottom is never clipped. The scrollbar (unchanged) now does real work at every
  overflow — 4 rows visible, the rest reachable by ←→ stepping the value, its thumb tracking the `●`.
  The value stepper wraps both ways (`step_model` is modulo), so 4 visible rows lose no reachability.
- **Regression test:** `create_popup_fits_a_short_terminal_and_shows_its_last_field` — autopilot (the
  max-field tier) with the Worker Model list open, on a 120×26 terminal, asserts the last field
  ("Decider Model") still renders. With the 14-row reserve it clipped; with 4 it fits.
- The earlier "a bit taller" height-floor test flips to a compactness **ceiling** (the popup stays
  small); everything else about the scrollbar is retained.

## Follow-up: width like the other modals + truncated rows

*"the popup for `n` doesn't need a big popup … its size can be the same as `s` send or `e` edit."*
The create form had used a ~80% bypass of the shared `OVERLAY_MAX_W` cap. Reverted to the standard
overlay width policy.

- **Standard width via `draw_overlay_frame` (72, growing to ~45%, capped at `OVERLAY_MAX_W`).** Drop
  the `centered_rect` + `draw_overlay_frame_in` bypass — the create form now sizes exactly like `s`
  send / `g`/`i` edits.
- **Truncate every row into its width budget; remove `.wrap()`** (mirroring the `s` send overlay). At
  the narrower width, long descriptors (Autonomy, the Goal hint) wrapped and — since the card renders
  its body as one top-anchored `Paragraph` at a fixed height — pushed the bottom fields off short
  terminals. Truncating (with `…`) keeps each field to one line, so the fixed height is exact (the
  wrap-slack row is gone) and the scrollbar's row offset is exact (no wrapped rows above it).
- **Verified live** at 130×26: the full autopilot form + the 4-row model list + the scrollbar fit,
  with Decider Model (the last field) visible; the frame is the same width as the `s` send overlay.

## Behavior (`render/create.rs` only)

- **Const:** `MODEL_LIST_VIEWPORT: usize = 14;` (was 10). Update its doc comment / the module header's viewport references.
- **`model_rows`:** window the list to `win = list_len.min(MODEL_LIST_VIEWPORT)` as today, but **remove the trailing `… N more` line** — the scrollbar is the overflow cue now. The list therefore emits exactly `1` (summary) `+ win` lines.
- **Track the focused list's span for the scrollbar.** While building `rows`, when the focused field is a model field, record `list_span = Some(ListSpan { row_off, win, current, list_len })` where `row_off` = the number of `rows` (Lines) emitted BEFORE the first list row (i.e. the inner-relative Y at which the list begins), `win`/`current`/`list_len` as computed in `model_rows`. (`model_rows` can return this alongside its `Vec<Line>`, or the caller computes `row_off = rows.len() + 1` immediately before `rows.extend(model_rows(focused_field…))` and derives `win/current/list_len` from the same catalog.) Only the FOCUSED model field yields a span; a non-model focus yields `None`.
- **Render the scrollbar after the Paragraph.** After `f.render_widget(Paragraph…, inner)`, if `Some(span)` and `span.list_len > span.win`:
  ```rust
  use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
  let y = inner.y.saturating_add(u16::try_from(span.row_off).unwrap_or(u16::MAX));
  let bottom = inner.y.saturating_add(inner.height);
  if y < bottom {
      let h = u16::try_from(span.win).unwrap_or(u16::MAX).min(bottom - y);
      let bar = Rect { x: inner.x, y, width: inner.width, height: h };
      let mut state = ScrollbarState::new(span.list_len).position(span.current);
      let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
          .begin_symbol(Some("\u{2191}"))
          .end_symbol(Some("\u{2193}"))
          .thumb_symbol("\u{2588}");
      f.render_stateful_widget(sb, bar, &mut state);
  }
  ```
  The scrollbar draws only its rightmost-column track over `bar`; the list labels are indented (`VALUE_COL`) and short, so the right column under the list is blank — the bar sits there cleanly. Clamp `h` into `inner` (never past the bottom border) so it can't panic or draw outside the card.
- **`row_off` accuracy:** at the popup's ~80% width no field row above a list wraps (Directory windows via `caret_view`; the Autonomy descriptor is well under the width), so the Line index equals the rendered Y. The only wrap risk (a narrow terminal wrapping Autonomy, which sits above the Decider-Model list) could offset the bar by one row — cosmetic, and the clamp keeps it panic-free. (If the implementer prefers exactness, split `inner` into above/list/below sub-rects and render the list + scrollbar in the list rect; either is acceptable as long as it's robust and panic-free.)

## Testing

- **Render (TestBackend):**
  - Focused Worker Model with a catalog LONGER than the viewport (e.g. 20 models): a scrollbar track renders at the right edge of the list (assert the `↑`/`↓` end-symbols or the thumb glyph `█` appear in the list's right column); `… N more` no longer appears anywhere.
  - A catalog that FITS the viewport (e.g. 3 models): no scrollbar glyphs, no `… N more`.
  - The scrollbar thumb position reflects the current selection: stepping ←→ to the last model moves the thumb toward the bottom (assert the `█`/thumb row is lower than when the first model is selected) — or, more simply, assert `render_create` does not panic and the bar renders for both first- and last-selected.
  - Fixed height still holds (height with Directory focused == height with Worker Model focused) and is taller than the pre-bump value (viewport 14).
  - Tiny-size sweep (1×1 … 200×50), with a focused model field + an overflowing catalog seeded, still does not panic (exercises the clamp).
- **Live render (mandatory, pmtui-ui-testing skill):** on real tmux, focus Worker Model with the full claude catalog — confirm a scrollbar down the right of the list with `↑`/`↓` and a thumb that moves as you ←→ step; no `… N more`; the popup is a bit taller and still doesn't resize on focus.
- **Real-tmux acceptance (MANDATORY — create-form change):** run `--ignored`; interaction/indices unchanged, so form-driving tests hold; fix any test asserting the `… N more` string or exact geometry.
- Standing: `cargo test`, clippy `--all-targets`, `fmt --check`.

## Out of scope

- The live `e`/`w` ModelPicker overlay (a good future home for the same `Scrollbar`); typing/filter; any schema/launch/interaction change; other overlays; changing the inline-below placement, colors, width, or the divider.
