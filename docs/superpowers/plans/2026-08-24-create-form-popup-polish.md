# Create Popup Styling Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Three visual-polish tweaks to the `n` create popup: (1) snug content-driven height (no dead space), (2) a dim group divider between Directory and Autonomy, (3) align the focused model field's inline list under the value column. Render-only; no interaction/schema/layout-model change.

**Architecture:** All changes live in `render_create` in `src/bin/pmtui/render/create.rs`. Height stops routing through `overlay_h`/`CREATE_MIN_BODY_ROWS` and is computed directly from the shown content (with 1 top pad row + 1 slack row + the 2 border rows). A dim full-width rule row is inserted between Directory and Autonomy. The `model_rows` list indent moves from 6 to 12 columns.

**Tech Stack:** Rust, ratatui 0.30.2 (`Line`/`Span`/`Style`/`Modifier`, `Paragraph`, `TestBackend`), the real-tmux acceptance harness + the `pmtui-ui-testing` skill.

## Global Constraints

- **Render-only, one function.** Only `render_create` in `src/bin/pmtui/render/create.rs` changes (plus its module doc + constants). NO edits to `create_form.rs`, `keys.rs`, `chrome.rs`, or any other overlay. No schema/launch/discovery/interaction change; single-writer untouched.
- **Keep what's on-convention:** the rounded border + raised surface, the reverse-cyan title chip, the bold-yellow emphasis for focused value + `●` current row, dim labels, the ~80% width (`want_w`), `MODEL_LIST_WINDOW = 12` (do NOT shorten the list), field order/indices, and the footer hint — all unchanged.
- **Full suite after every change:** `cargo test`, `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check`.
- **Real-tmux acceptance is MANDATORY before merge** (create-form change): `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1`.

## Current state (verified anchors)

`src/bin/pmtui/render/create.rs`, `render_create`:
- Consts `MODEL_LIST_WINDOW = 12`, `CREATE_MIN_BODY_ROWS = 8`.
- `model_field_rows(idx, choices) -> usize` closure (1 for a plain/unfocused-model field; `1 + win + overflow` for a focused model field). `rows_shown` sums it: `1` (Engine) `+ model_field_rows(WORKER_MODEL)` `+ 1` (Directory) `+ 1` (Autonomy) `+ shows_goal?1 + shows_cadence?1 + shows_decider?(1 + model_field_rows(DECIDER_MODEL))`, then `let rows_shown = rows_shown.max(CREATE_MIN_BODY_ROWS);`.
- Sizing: `want_w = area.width.saturating_mul(4).saturating_div(5).max(96).min(area.width.saturating_sub(OVERLAY_MARGIN_X)).max(1).min(area.width);` `cap_h = area.height.saturating_mul(4).saturating_div(5).max(1);` `want_h = overlay_h(rows_shown + 1).min(cap_h).min(area.height.saturating_sub(OVERLAY_MARGIN_Y)).max(1).min(area.height);` `let popup = centered_rect(want_w, want_h, area); let inner = draw_overlay_frame_in(f, popup, "New session", Color::Cyan, "<hint>");`
- `value_cols = usize::from(inner.width).saturating_sub(13);` closures `val_style`, `field_row(idx,label,String)`, `caret_row(idx,label,&Field)`, `model_rows(idx,label,&[ModelInfo],&Option<String>)->Vec<Line>` (list rows + `… N more` note both indented `Span::raw("      ")` = 6 spaces).
- Build order: `rows = [field_row(0,"Engine",…)]`; `rows.extend(model_rows(WORKER_MODEL,"Model",…))`; `rows.push(caret_row(2,"Directory",&form.dir))`; `rows.push(field_row(3,"Autonomy",autonomy_val))`; then `shows_goal?`/`shows_cadence?`/`shows_decider?` (Decider engine `field_row` + `model_rows(DECIDER_MODEL,"D-Model",…)`); finally `f.render_widget(Paragraph::new(Text::from(rows)).wrap(Wrap{trim:false}), inner)`.
- `OVERLAY_CHROME_H = 2`, `OVERLAY_MARGIN_X = 2`, `OVERLAY_MARGIN_Y = 1` (chrome.rs, in scope via `use crate::*`). `centered_rect`/`draw_overlay_frame_in` unchanged.
- Tests in `src/bin/pmtui/tests/create_form.rs` (render tests use the file's `screen_rows` reader; e.g. `focused_worker_model_expands_inline_below_with_current_marked`, `create_popup_is_bigger_and_grows_when_a_model_field_is_focused`, the 1×1…200×50 sweep, Standard `!contains("D-Model")`/`!contains("Goal")`).

---

### Task 1: The three render tweaks + render tests

**Files:**
- Modify: `src/bin/pmtui/render/create.rs`
- Test: `src/bin/pmtui/tests/create_form.rs`

- [ ] **Step 1: Add a value-column constant; remove the min-body floor.** Replace `const CREATE_MIN_BODY_ROWS: usize = 8;` with:

```rust
/// Column where a field's `< value >` begins: the 2-col focus marker + the 10-col label width the
/// `field_row`/`caret_row` closures use. The focused model field's inline list indents to here so it
/// sits under the value it expands.
const VALUE_COL: usize = 12;
```

Keep `MODEL_LIST_WINDOW = 12`.

- [ ] **Step 2: Content-driven height + a divider in the row count.** Replace the `let rows_shown = rows_shown.max(CREATE_MIN_BODY_ROWS);` line and the `want_h` computation. After the existing `rows_shown` sum, add the divider to the count and size the popup directly (no `overlay_h`, no floor):

```rust
    rows_shown += 1; // the dim group divider between Directory and Autonomy (always shown)

    let want_w = area
        .width
        .saturating_mul(4)
        .saturating_div(5)
        .max(96)
        .min(area.width.saturating_sub(OVERLAY_MARGIN_X))
        .max(1)
        .min(area.width);
    let cap_h = area.height.saturating_mul(4).saturating_div(5).max(1);
    // Snug: 1 top pad row + the shown rows + 1 bottom/wrap slack row + the 2 border rows. No floor —
    // the card hugs its content (the width already delivers "bigger"), with deliberate top/bottom air
    // instead of a stack of trailing blanks. Capped at ~80% of the screen and clamped into the area.
    let body_rows = u16::try_from(rows_shown + 2).unwrap_or(u16::MAX);
    let want_h = body_rows
        .saturating_add(OVERLAY_CHROME_H)
        .min(cap_h)
        .min(area.height.saturating_sub(OVERLAY_MARGIN_Y))
        .max(1)
        .min(area.height);
```

(Leaves `want_w`/`cap_h`/`centered_rect`/`draw_overlay_frame_in` otherwise as they are.)

- [ ] **Step 3: Prepend the top pad row; insert the divider.** Change the row assembly so the first body row is a blank pad and a dim rule sits between Directory and Autonomy:
  - Change the initial `let mut rows = vec![field_row(0, "Engine", …)];` to:
    ```rust
    let mut rows = vec![
        Line::from(""), // one row of deliberate top breathing (see want_h)
        field_row(0, "Engine", format!("< {} >", form.engine.label())),
    ];
    ```
  - Keep `rows.extend(model_rows(CreateForm::WORKER_MODEL, "Model", …))` and `rows.push(caret_row(2, "Directory", &form.dir));`.
  - Immediately BEFORE `rows.push(field_row(3, "Autonomy", autonomy_val));`, insert:
    ```rust
    // A dim, unlabeled rule splits "the session" (Engine/Model/Directory) from "how it's driven"
    // (Autonomy + on autopilot Cadence/Decider/D-Model). Structure, so dim — never bold/colored.
    rows.push(Line::from(Span::styled(
        "\u{2500}".repeat(usize::from(inner.width)),
        Style::default().add_modifier(Modifier::DIM),
    )));
    rows.push(field_row(3, "Autonomy", autonomy_val));
    ```
  - The goal/cadence/decider/d-model branches and the final render are unchanged.

- [ ] **Step 4: Align the inline list under the value.** In the `model_rows` closure, change BOTH indents from 6 spaces to `VALUE_COL`:
  - List row: `Span::raw(" ".repeat(VALUE_COL))` in place of `Span::raw("      ")`.
  - `… N more` note: `format!("{}\u{2026} {} more", " ".repeat(VALUE_COL), list_len - win)` in place of the `"      \u{2026} {} more"` literal.

- [ ] **Step 5: Update the module doc comment** (lines 1-10): snug content-driven height (no floor), the group divider, the value-aligned inline list; drop the `CREATE_MIN_BODY_ROWS` mention.

- [ ] **Step 6: Render tests** in `tests/create_form.rs` (reuse the file's `screen_rows` reader — do not invent one; match its real return shape):

```rust
#[test]
fn create_popup_has_no_dead_space_on_standard() {
    // Snug height: a 4-field Standard form has no run of 3+ blank content rows inside the card.
    let mut t = Terminal::new(TestBackend::new(140, 42)).unwrap();
    let f = CreateForm::new(); // Standard, Engine focused
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let mut run = 0usize;
    let mut worst = 0usize;
    for (s, _) in screen_rows(&t) {
        let body = s.trim_start_matches('\u{2502}').trim_end_matches('\u{2502}').trim();
        if s.contains('\u{2502}') && body.is_empty() {
            run += 1;
            worst = worst.max(run);
        } else {
            run = 0;
        }
    }
    assert!(worst < 3, "no 3+ blank-row gap inside the card; worst run = {worst}");
}

#[test]
fn create_popup_shows_a_group_divider() {
    let mut t = Terminal::new(TestBackend::new(140, 42)).unwrap();
    let f = CreateForm::new();
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let has_rule = screen_rows(&t).iter().any(|(s, _)| {
        let body = s.trim_start_matches('\u{2502}').trim_end_matches('\u{2502}').trim();
        !body.is_empty() && body.chars().all(|c| c == '\u{2500}')
    });
    assert!(has_rule, "a dim horizontal rule divides the fields");
}

#[test]
fn focused_model_list_aligns_under_the_value() {
    let mut t = Terminal::new(TestBackend::new(160, 40)).unwrap();
    let mut f = CreateForm::new();
    f.model_choices = vec![
        ModelInfo { label: "Opus".into(), value: "v-opus".into() },
        ModelInfo { label: "Sonnet".into(), value: "v-sonnet".into() },
    ];
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let row = screen_rows(&t).into_iter().map(|(s, _)| s)
        .find(|s| s.contains("\u{25cf} (default)")).expect("current row present");
    let dot = row.find('\u{25cf}').unwrap();
    let bar = row.find('\u{2502}').unwrap(); // left card border
    let col_in_body = dot - bar - 1; // columns from just inside the border
    assert!(col_in_body >= 12, "list ● aligns under the value column (>=12), got {col_in_body}");
}
```

  - Adjust `create_popup_is_bigger_and_grows_when_a_model_field_is_focused` only if the top-pad/divider shifted its row-count proxy — it must still assert width > 96 and taller-when-a-model-field-is-focused; do not weaken it.
  - Keep the 1×1…200×50 sweep and the Standard `!contains("D-Model")`/`!contains("Goal")` tests.
  - If `screen_rows` returns a different shape than `(String, _)`, match the real one; the assertions (longest blank run < 3, an all-`─` row exists, `●` column ≥ 12) are the load-bearing part.

- [ ] **Step 7: Green + hygiene + commit.** `cargo test 2>&1 | tail -20`; `cargo clippy --all-targets 2>&1 | tail -5`; `cargo fmt --all -- --check`.
```bash
git commit -am "feat(pmtui): create popup — snug height, a group divider, value-aligned inline model list"
```

---

### Task 2: Live tmux render + mandatory acceptance + docs

**Files:**
- Modify: `README.md` (if its create-form paragraph describes the popup's look), `docs/superpowers/specs/2026-08-24-create-form-popup-polish-design.md` (Status)
- Test: live tmux, `--ignored` acceptance

- [ ] **Step 1: Build.** `cargo build` (updates the REAL `target/debug/pmtui`).
- [ ] **Step 2: Live tmux render** (pmtui-ui-testing skill — scratch sockets ONLY; NEVER `--socket pmd`, NEVER the real registry). Seed a `$SCRATCH` registry + `$SCRATCH/.project-state/config.json` autopilot config. Launch pmtui with `ECC_GATEGUARD=off` + `--socket pmtuitest-inner` on `tmux -L pmtui-test`, press `n`, capture. Verify + paste frames: (a) Standard has NO block of dead space between the last field and the footer; (b) a dim rule divides Directory from Autonomy; (c) on Autopilot with Worker Model focused, the inline list starts under the value column and the `●` marks the current + moves with ←→. Clean up BOTH scratch servers + `$SCRATCH`; confirm no stray REPLs.
- [ ] **Step 3: Real-tmux acceptance (MANDATORY).** `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 2>&1 | tail -40`. Must pass. Fix any test that counted exact popup rows/geometry — test-only, no weakening. Report counts.
- [ ] **Step 4: Docs.** If README's create-form paragraph describes the popup look, add the divider + snug height (one clause). Flip the design-doc Status to `implemented @ feat/create-form-popup-polish`.
- [ ] **Step 5: Commit.** `git commit -am "docs(create-form): popup polish — README + spec status"`

---

## Self-Review

**1. Spec coverage:** snug height → T1 Steps 1-2 (drop floor + `overlay_h`, direct content height with top pad + slack) + top-pad row T1 Step 3; group divider → T1 Steps 2 (+1 count) & 3 (dim `─` rule between Directory and Autonomy); aligned inline list → T1 Steps 1 (`VALUE_COL`) & 4 (both indents → 12); tests → T1 Step 6; live + mandatory acceptance → T2 Steps 2-3. Not-capping-the-list, colors, border/chip, width, interaction → untouched (Global Constraints).

**2. Placeholder scan:** no TBD; the height math, divider, and indent are exact code; tests give concrete assertions (blank-run < 3, an all-`─` row, `●` at col ≥ 12) with a fallback note to match the real `screen_rows` shape.

**3. Type consistency:** `VALUE_COL: usize = 12`, `MODEL_LIST_WINDOW: usize`, `OVERLAY_CHROME_H`/`OVERLAY_MARGIN_X`/`OVERLAY_MARGIN_Y: u16`, `body_rows: u16`, `Line::from(Span::styled("─".repeat(usize), Style))`, `model_rows(idx, label, &[ModelInfo], &Option<String>) -> Vec<Line>`, and `centered_rect`/`draw_overlay_frame_in` are spelled identically to the current file. `CREATE_MIN_BODY_ROWS` and `overlay_h` appear ONLY in removal steps.
