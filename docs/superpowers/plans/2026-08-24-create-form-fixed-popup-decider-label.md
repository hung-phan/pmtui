# Create Popup: Fixed Height + "Decider Model" Label — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** (1) Rename the create form's `D-Model` field to the full `Decider Model` (widening the label column so values stay aligned). (2) Make the create popup a FIXED size that reserves a 10-row model-list area, so it no longer grows when a model field is focused; the inline list scrolls inside the reserve. Render-only.

**Architecture:** All in `render_create` (`src/bin/pmtui/render/create.rs`). Height is computed WITHOUT reference to `form.field` — the shown fields + divider + a fixed `MODEL_LIST_VIEWPORT` reserve — so it's stable across navigation. The label gutter widens to `LABEL_W = 14` to fit `Decider Model`; `VALUE_COL` and the inline-list indent follow.

**Tech Stack:** Rust, ratatui 0.30.2 (`Line`/`Span`/`Style`, `Paragraph`, `TestBackend`), real-tmux acceptance + `pmtui-ui-testing` skill.

## Global Constraints

- **Render-only, one function.** Only `render_create` in `render/create.rs` (its consts + body) and `tests/create_form.rs` change; also update any `"D-Model"` string/comment elsewhere (grep). NO change to `create_form.rs` logic, `keys.rs` behavior, `chrome.rs`, other overlays, schema, launch, or the ←→/↑↓ interaction.
- **Keep on-convention:** rounded border + surface, reverse-cyan title chip, bold-yellow focused value + `●` current, dim labels, dim group divider between Directory and Autonomy, ~80% `want_w`, top-pad blank row, field order/indices, footer hint.
- **Full suite after every change:** `cargo test`, `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check`.
- **Real-tmux acceptance MANDATORY before merge** (create-form change): `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1`.

## Current state (verified anchors — `render/create.rs`, post-polish)

- Consts: `const MODEL_LIST_WINDOW: usize = 12;` and `const VALUE_COL: usize = 12;`.
- Height block: a `model_field_rows(idx, choices) -> usize` closure (returns 1 for unfocused/plain, `1 + win + overflow` for a focused model field); `rows_shown` = `1`(Engine) `+ model_field_rows(WORKER_MODEL)` `+ 1`(Directory) `+ 1`(Autonomy) `+ shows_goal?1 + shows_cadence?1 + shows_decider?(1 + model_field_rows(DECIDER_MODEL))`; then `rows_shown += 1;` (divider). Then `let body_rows = u16::try_from(rows_shown + 2).unwrap_or(u16::MAX); let cap_h = area.height.saturating_mul(4).saturating_div(5).max(1); let want_h = body_rows.saturating_add(OVERLAY_CHROME_H).min(cap_h).min(area.height.saturating_sub(OVERLAY_MARGIN_Y)).max(1).min(area.height);` `want_w` = ~80% (unchanged). `let popup = centered_rect(want_w, want_h, area); let inner = draw_overlay_frame_in(f, popup, "New session", Color::Cyan, "<hint>");`
- `value_cols = usize::from(inner.width).saturating_sub(13);`. `field_row`/`caret_row` format the label `format!("{label:<10}")`.
- `model_rows(idx, label, &[ModelInfo], &Option<String>) -> Vec<Line>`: summary row via `field_row`; if focused, an inline list windowed to `MODEL_LIST_WINDOW` (`win = list_len.min(MODEL_LIST_WINDOW)`, scrolled to keep the `●` current visible), list rows + `… N more` note both indented `" ".repeat(VALUE_COL)`.
- Row build order: Engine, `model_rows(WORKER_MODEL,"Model",…)`, Directory (caret_row), divider (dim `─`), Autonomy, goal?/cadence?/decider?(Decider engine `field_row` + `model_rows(DECIDER_MODEL,"D-Model",…)`).
- `OVERLAY_CHROME_H=2`, `OVERLAY_MARGIN_X=2`, `OVERLAY_MARGIN_Y=1` (chrome.rs, in scope). `shows_goal`/`shows_cadence`/`shows_decider` autopilot-only.
- Tests (`tests/create_form.rs`, use `screen_rows` reader): `create_popup_is_bigger_and_grows_when_a_model_field_is_focused` (asserts GROWS on model-focus — premise inverts here), `create_popup_has_no_dead_space_on_standard` (snug — conflicts with the reserve), `create_popup_shows_a_group_divider`, `focused_worker_model_expands_inline_below_with_current_marked`, `focused_model_list_aligns_under_the_value` (asserts `●` byte-col ≥ 12), the tiny sweep, and Standard `!contains("D-Model")` / `!contains("Goal")`.

---

### Task 1: Fixed height + "Decider Model" label + render tests

**Files:** Modify `src/bin/pmtui/render/create.rs`; Test `src/bin/pmtui/tests/create_form.rs`.

- [ ] **Step 1: Consts.** Replace `const MODEL_LIST_WINDOW: usize = 12;` with `const MODEL_LIST_VIEWPORT: usize = 10;` and `const VALUE_COL: usize = 12;` block; add `const LABEL_W: usize = 14;` and change `VALUE_COL` to `const VALUE_COL: usize = 2 + LABEL_W;` (= 16). Update the doc comments (fixed height that reserves a 10-row model-list area; label gutter `LABEL_W`).

- [ ] **Step 2: Widen the label gutter + value column.** In `field_row` and `caret_row`, change `format!("{label:<10}")` to `format!("{label:<width$}", width = LABEL_W)`. Change `value_cols` to `usize::from(inner.width).saturating_sub(VALUE_COL + 1)`.

- [ ] **Step 3: Rename the decider model label.** In the `shows_decider()` block, change `model_rows(CreateForm::DECIDER_MODEL, "D-Model", …)` to `"Decider Model"`. Update the adjacent comment (the full name, not an abbreviation, distinguishes it from the worker's Model row). Grep the whole repo for other `"D-Model"` occurrences (comments/tests) and update them to `Decider Model`.

- [ ] **Step 4: Fixed, non-resizing height.** DELETE the `model_field_rows` closure and the `rows_shown` sum + `rows_shown += 1` divider line + the `cap_h`/`body_rows`/`want_h` block, and replace with a height computed WITHOUT `form.field`:

```rust
    // FIXED height, independent of which field is focused: the shown fields + the group divider +
    // a reserved MODEL_LIST_VIEWPORT for the focused model field's inline list. Focusing a model
    // field FILLS the reserve (windowed/scrolling) instead of growing the popup, so the window
    // never resizes as you navigate. Each field counts as ONE row here (the list lives in the
    // reserve, not added on top).
    let base_fields = 4 // Engine, Model, Directory, Autonomy (always shown)
        + usize::from(form.shows_goal())
        + usize::from(form.shows_cadence())
        + if form.shows_decider() { 2 } else { 0 }; // Decider engine + Decider Model
    let rows_shown = base_fields + 1 /* group divider */ + MODEL_LIST_VIEWPORT;
    let want_w = area
        .width
        .saturating_mul(4)
        .saturating_div(5)
        .max(96)
        .min(area.width.saturating_sub(OVERLAY_MARGIN_X))
        .max(1)
        .min(area.width);
    // + 1 top-pad row + 1 wrap-slack row + the 2 border rows; clamped into the terminal.
    let body_rows = u16::try_from(rows_shown + 2).unwrap_or(u16::MAX);
    let want_h = body_rows
        .saturating_add(OVERLAY_CHROME_H)
        .min(area.height.saturating_sub(OVERLAY_MARGIN_Y))
        .max(1)
        .min(area.height);
    let popup = centered_rect(want_w, want_h, area);
    let inner = draw_overlay_frame_in(
        f,
        popup,
        "New session",
        Color::Cyan,
        "\u{2191}\u{2193} field \u{b7} \u{2190}\u{2192} value \u{b7} ^E $EDITOR \u{b7} enter create \u{b7} esc cancel",
    );
```

(`cap_h` is removed — the fixed reserve bounds the height; only the area clamp remains.)

- [ ] **Step 5: Window the inline list to the reserve.** In `model_rows`, change `MODEL_LIST_WINDOW` → `MODEL_LIST_VIEWPORT` (the `win = list_len.min(MODEL_LIST_VIEWPORT)` line and any other reference). The scroll-to-keep-`●`-visible logic, the `● `/`○ ` markers, bold-current styling, and the `" ".repeat(VALUE_COL)` indent (now 16) are otherwise unchanged.

- [ ] **Step 6: Render tests.** In `tests/create_form.rs` (reuse the `screen_rows` reader):
  - REPLACE `create_popup_is_bigger_and_grows_when_a_model_field_is_focused` with a FIXED-height test:

```rust
#[test]
fn create_popup_height_is_fixed_when_focusing_a_model_field() {
    // The window must NOT resize as you move onto a model field — the model list scrolls inside a
    // reserved area. Border-row count (rows containing '│') is the popup height proxy.
    let height = |field: usize| -> usize {
        let mut t = Terminal::new(TestBackend::new(160, 44)).unwrap();
        let mut f = CreateForm::new();
        f.model_choices = vec![
            ModelInfo { label: "Opus".into(), value: "v-opus".into() },
            ModelInfo { label: "Sonnet".into(), value: "v-sonnet".into() },
        ];
        f.field = field;
        t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
        screen_rows(&t).iter().filter(|(s, _)| s.contains('\u{2502}')).count()
    };
    assert_eq!(height(2), height(CreateForm::WORKER_MODEL), "popup height is fixed across focus");
}
```

  - REMOVE `create_popup_has_no_dead_space_on_standard` (the fixed design intentionally reserves the viewport; that snugness assertion is obsolete — do not replace with a vacuous check; the fixed-height test above is the new invariant).
  - UPDATE the Standard visibility test `!contains("D-Model")` → `!contains("Decider Model")`; keep `!contains("Goal")`. Add/keep an Autopilot assertion that the label reads `Decider Model` (and `!contains("D-Model")`).
  - `focused_model_list_aligns_under_the_value`: bump the `≥ 12` floor to `≥ VALUE_COL` (16) — the list now indents to the wider value column. Keep it a byte-offset floor.
  - Keep `focused_worker_model_expands_inline_below_with_current_marked` (still valid; the list windows to 10 now — a 15-item catalog shows `… N more`), `create_popup_shows_a_group_divider`, and the 1×1…200×50 sweep.

- [ ] **Step 7: Green + hygiene + commit.** `cargo test 2>&1 | tail -20`; `cargo clippy --all-targets 2>&1 | tail -5`; `cargo fmt --all -- --check`.
```bash
git commit -am "feat(pmtui): fixed-size create popup (reserved model-list area) + full 'Decider Model' label"
```

---

### Task 2: Live tmux render + mandatory acceptance + docs

**Files:** `README.md` (if it describes the popup look), the design-doc Status; live tmux, `--ignored` acceptance.

- [ ] **Step 1: Build.** `cargo build`.
- [ ] **Step 2: Live tmux render** (pmtui-ui-testing skill — scratch sockets ONLY; NEVER `--socket pmd`/the real registry). Seed a `$SCRATCH` registry + `$SCRATCH/.project-state/config.json` autopilot config. Launch pmtui with `ECC_GATEGUARD=off` + `--socket pmtuitest-inner` on `tmux -L pmtui-test`, press `n`, capture. Verify + paste frames: (a) the popup height does NOT change as you Tab from Directory onto Worker Model (fixed); (b) the decider model row reads **"Decider Model"** in full; (c) the model list scrolls inside the reserved area (`● ` current, `… N more` for a long catalog). Clean up BOTH scratch servers + `$SCRATCH`; confirm no stray REPLs.
- [ ] **Step 3: Real-tmux acceptance (MANDATORY).** `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 2>&1 | tail -40`. Must pass. Fix any test asserting exact popup geometry / `D-Model` — test-only, no weakening. Report counts.
- [ ] **Step 4: Docs.** README (if the create-form paragraph describes the popup): fixed-size window with a reserved scrolling model-list area; "Decider Model" label. Flip the design-doc Status to `implemented @ feat/create-form-fixed-popup-decider-label`.
- [ ] **Step 5: Commit.** `git commit -am "docs(create-form): fixed popup + Decider Model label — README + spec status"`

---

## Self-Review

**1. Spec coverage:** full "Decider Model" → T1 Steps 1-3 (`LABEL_W`, format width, rename + grep); fixed non-resizing height → T1 Step 4 (`base_fields` + divider + `MODEL_LIST_VIEWPORT`, no `form.field` term, `cap_h` dropped); list scrolls in the reserve → T1 Step 5 (`MODEL_LIST_VIEWPORT` window); tests (fixed-height, rename, alignment) → T1 Step 6; live + mandatory acceptance → T2 Steps 2-3. Interaction/width/colors/divider unchanged (Global Constraints).

**2. Placeholder scan:** no TBD; the const changes, height math, and label width are exact; the fixed-height test is concrete; the obsolete snugness test is explicitly removed (not stubbed).

**3. Type consistency:** `LABEL_W`/`VALUE_COL`/`MODEL_LIST_VIEWPORT: usize`, `VALUE_COL = 2 + LABEL_W`, `body_rows: u16`, `format!("{label:<width$}", width = LABEL_W)`, `screen_rows(&t) -> Vec<(String, Vec<Color>)>`, `centered_rect`/`draw_overlay_frame_in`, and `CreateForm::{WORKER_MODEL, DECIDER_MODEL, shows_goal, shows_cadence, shows_decider}` match the current file. `MODEL_LIST_WINDOW`, `model_field_rows`, `cap_h`, `"D-Model"` appear ONLY in removal/rename steps.
