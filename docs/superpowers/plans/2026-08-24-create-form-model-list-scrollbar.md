# Create Popup: Taller Model Area + ratatui Scrollbar — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** (1) Raise the create popup's reserved model-list area from 10 to 14 rows ("make it a bit higher"). (2) Replace the inline model list's `… N more` overflow line with a real ratatui `Scrollbar` rendered down the list's right edge (thumb tracks the current `●` selection over the whole catalog). Render-only.

**Architecture:** All in `render_create` (`src/bin/pmtui/render/create.rs`). `MODEL_LIST_VIEWPORT` 10 → 14. `model_rows` drops the `… N more` line and returns, alongside its `Vec<Line>`, the focused list's `(win, current, list_len)` so the caller can record a `ListSpan { row_off, win, current, list_len }`. After the single `Paragraph` is rendered, if a model field is focused and its catalog overflows the viewport, a `Scrollbar(VerticalRight)` (↑/↓ symbols, `█` thumb) is rendered as a stateful widget over the list's row-span at `inner`'s right edge, clamped into `inner`.

**Tech Stack:** Rust, ratatui 0.30.2 (`Line`/`Span`/`Style`, `Paragraph`, `Scrollbar`/`ScrollbarOrientation`/`ScrollbarState`, `TestBackend`), real-tmux acceptance + `pmtui-ui-testing` skill.

## Global Constraints

- **Render-only, one function.** Only `render_create` in `render/create.rs` (its consts, the `model_rows` return type, the height/scrollbar body) and `tests/create_form.rs` change. NO change to `create_form.rs` logic, `keys.rs`, `chrome.rs`, other overlays, schema, launch, discovery, or the ←→/↑↓ interaction, field order/indices.
- **Keep on-convention:** rounded border + surface, reverse-cyan title chip, bold-yellow focused value + `●` current, dim labels, dim group divider between Directory and Autonomy, ~80% `want_w`, top-pad blank row, full "Decider Model" label, footer hint. Fixed (non-resizing) height must still hold.
- **Full suite after every change:** `cargo test`, `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check`.
- **Real-tmux acceptance MANDATORY before merge** (create-form change): `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1`.

## Current state (verified anchors — `render/create.rs`, post-`64e9a26`)

- Consts: `const MODEL_LIST_VIEWPORT: usize = 10;`, `const LABEL_W: usize = 14;`, `pub(crate) const VALUE_COL: usize = 2 + LABEL_W;` (= 16).
- Fixed height (no `form.field` term): `let base_fields = 4 + shows_goal + shows_cadence + if shows_decider {2} else {0}; let rows_shown = base_fields + 1 /*divider*/ + MODEL_LIST_VIEWPORT;` then `body_rows = u16::try_from(rows_shown + 2)…; want_h = body_rows.saturating_add(OVERLAY_CHROME_H).min(area.height.saturating_sub(OVERLAY_MARGIN_Y)).max(1).min(area.height);` `want_w` = ~80% (`.max(96)`). `popup = centered_rect(want_w, want_h, area); inner = draw_overlay_frame_in(f, popup, "New session", Color::Cyan, "<hint>")`.
- `field_row`/`caret_row` format the label `format!("{label:<width$}", width = LABEL_W)`; `value_cols = usize::from(inner.width).saturating_sub(VALUE_COL + 1)`.
- `model_rows(idx, label, &[ModelInfo], &Option<String>) -> Vec<Line>`: builds `summary` via `field_row`; if `form.field != idx` returns just `[summary]`; else computes `current` (0 = `(default)`, else stored value's index + 1), `list_len = 1 + choices.len()`, `win = list_len.min(MODEL_LIST_VIEWPORT)`, `max_start = list_len.saturating_sub(win)`, `start = current.saturating_sub(win.saturating_sub(1)).min(max_start)`; pushes each `disp in start..start+win` as `Span::raw(" ".repeat(VALUE_COL))` + `● `/`○ ` + label (bold-yellow when `disp == current`); then **if `list_len > win`** pushes a dim `format!("{}… {} more", " ".repeat(VALUE_COL), list_len - win)` line.
- Row build order: `Line::from("")` (top-pad), `field_row(0,"Engine",…)`, `rows.extend(model_rows(WORKER_MODEL,"Model",&form.model_choices,&form.worker_model))`, `caret_row(2,"Directory",&form.dir)`, dim `─` divider (`"─".repeat(inner.width)`), `field_row(3,"Autonomy",…)`, goal? / cadence? / decider?(`field_row(DECIDER,"Decider",…)` + `rows.extend(model_rows(DECIDER_MODEL,"Decider Model",&form.decider_model_choices,&form.decider_model))`).
- Final render: `f.render_widget(Paragraph::new(Text::from(rows)).wrap(Wrap { trim: false }), inner);`.
- `use crate::*;` at top. `Rect`, `Color`, `Style`, `Modifier`, `Span`, `Line`, `Paragraph`, `Text`, `Wrap`, `Frame`, `centered_rect`, `draw_overlay_frame_in`, `OVERLAY_CHROME_H`, `OVERLAY_MARGIN_X`, `OVERLAY_MARGIN_Y`, `ModelInfo`, `CreateForm`, `Field` are in scope; `Scrollbar`/`ScrollbarOrientation`/`ScrollbarState` are NOT yet imported.
- Tests (`tests/create_form.rs`, `screen_rows(&t) -> Vec<(String, Vec<Color>)>` reader): `create_popup_height_is_fixed_when_focusing_a_model_field`, `focused_worker_model_expands_inline_below_with_current_marked`, `focused_model_list_aligns_under_the_value` (asserts `●` byte-col ≥ `VALUE_COL`), `create_popup_shows_a_group_divider`, the 1×1…200×50 tiny sweep, Autopilot `contains("Decider Model")`, Standard `!contains("Decider Model")`/`!contains("Goal")`.

---

### Task 1: Taller viewport + Scrollbar gutter + render tests

**Files:** Modify `src/bin/pmtui/render/create.rs`; Test `src/bin/pmtui/tests/create_form.rs`.

- [ ] **Step 1: Bump the viewport.** Change `const MODEL_LIST_VIEWPORT: usize = 10;` → `const MODEL_LIST_VIEWPORT: usize = 14;`. Update its doc comment (and the module-header sentence referencing the reserve) to say the reserve is 14 rows.

- [ ] **Step 2: `model_rows` — drop `… N more`, return list metadata.** Change the closure's return type to `-> (Vec<Line>, Option<(usize, usize, usize)>)` — the lines plus, when this field is FOCUSED, `Some((win, current, list_len))`.
  - When `form.field != idx`: `return (vec![summary_line], None);`.
  - When focused: keep computing `current`, `list_len`, `win`, `max_start`, `start` exactly as today, and the `for disp in start..start+win { … }` list rows exactly as today (indent `VALUE_COL`, `● `/`○ `, bold-yellow current).
  - **DELETE the `if list_len > win { out.push(… "… N more" …) }` block entirely** — the scrollbar is the overflow cue now. The list now emits exactly `1 + win` lines.
  - Return `(out, Some((win, current, list_len)))`.

- [ ] **Step 3: Record the focused list's span at build time.** Add a small local struct above the row build (or a `(usize,usize,usize,usize)` tuple — struct is clearer):

```rust
    struct ListSpan {
        row_off: usize, // inner-relative Y of the first list row (rows before it)
        win: usize,     // visible list rows
        current: usize, // 0-based index of the ● selection in the full catalog
        list_len: usize,
    }
    let mut list_span: Option<ListSpan> = None;
```

  Replace each `rows.extend(model_rows(…))` with a capture that records the span from the returned metadata. For the Worker Model row:

```rust
    let (lines, meta) = model_rows(
        CreateForm::WORKER_MODEL,
        "Model",
        &form.model_choices,
        &form.worker_model,
    );
    if let Some((win, current, list_len)) = meta {
        // summary sits at rows.len(); the list's first row is the next line.
        list_span = Some(ListSpan { row_off: rows.len() + 1, win, current, list_len });
    }
    rows.extend(lines);
```

  And identically for the Decider Model row inside the `shows_decider()` block (`CreateForm::DECIDER_MODEL`, `"Decider Model"`, `&form.decider_model_choices`, `&form.decider_model`). Only ONE model field is ever focused, so at most one assignment fires.

- [ ] **Step 4: Import the scrollbar widgets.** At the top of the file, below `use crate::*;`, add:

```rust
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
```

(If `crate::*` already re-exports any of these, drop the duplicate to keep clippy clean — verify by building.)

- [ ] **Step 5: Render the scrollbar after the Paragraph.** Immediately AFTER the existing `f.render_widget(Paragraph…, inner);`, add:

```rust
    // Overflow cue for the focused model field's inline list: a real ratatui scrollbar down the
    // list's right edge, its thumb tracking the current ● over the whole catalog. Only when the
    // catalog is longer than the reserved viewport. Clamped into `inner` so it can never draw past
    // the card border or panic on a tiny terminal.
    if let Some(span) = list_span {
        if span.list_len > span.win {
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
        }
    }
```

  The scrollbar draws only its rightmost-column track over `bar`; the list labels are indented and short, so `inner`'s right column under the list is blank. `Frame::render_stateful_widget` is the ratatui 0.30 method.

- [ ] **Step 6: Render tests** (`tests/create_form.rs`, reuse the `screen_rows` reader; seed catalogs on `CreateForm` as the existing tests do).
  - **`… N more` is gone; a scrollbar shows on overflow.** New test: build a `CreateForm` with `field = CreateForm::WORKER_MODEL` and a `model_choices` catalog LONGER than the viewport (e.g. 20 `ModelInfo`s), render at 160×44, and assert the joined screen text `!contains("more")` (the old cue is gone) AND contains at least one of the scrollbar glyphs (`\u{2191}` `↑`, `\u{2193}` `↓`, or `\u{2588}` `█`).

```rust
#[test]
fn focused_model_list_over_viewport_shows_a_scrollbar_not_more_text() {
    let mut t = Terminal::new(TestBackend::new(160, 44)).unwrap();
    let mut f = CreateForm::new();
    f.model_choices = (0..20)
        .map(|i| ModelInfo { label: format!("model-{i}"), value: format!("v-{i}") })
        .collect();
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let text: String = screen_rows(&t).iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join("\n");
    assert!(!text.contains("more"), "the '… N more' cue must be replaced by the scrollbar");
    assert!(
        text.contains('\u{2191}') || text.contains('\u{2193}') || text.contains('\u{2588}'),
        "an overflowing model list must render a scrollbar (↑/↓/█)"
    );
}
```

  - **A catalog that FITS shows no scrollbar and no `more`.** New test: `field = WORKER_MODEL`, `model_choices` of 2 items (list_len 3 ≤ 14), render, assert the text contains neither `more` nor any of `↑`/`↓`/`█`.
  - **Thumb tracks the selection.** New test (or extend the overflow test): render the 20-item catalog once with `worker_model = None` (current = 0, `●` on `(default)`) and once with `worker_model = Some("v-19")` (current = 20, last). Assert both render without panic AND the row index of the `█` thumb glyph is strictly greater in the last-selected frame than in the first-selected frame (find the first `screen_rows` index whose string contains `\u{2588}`). This proves `position(current)` drives the thumb.
  - **Fixed height still holds AND is taller than before.** Keep/adapt `create_popup_height_is_fixed_when_focusing_a_model_field` (height with Directory focused == height with Worker Model focused). Add an assertion that the height now exceeds what a 10-row reserve gave — assert the border-row count `>= 18` at 160×44 (base fields + divider + 14-row reserve + chrome comfortably exceeds this; pick the floor from the actual render, do not overfit).
  - Keep `focused_worker_model_expands_inline_below_with_current_marked` (still valid — the list windows to 14 now; use a catalog ≤ 14 so every row shows, or assert the `●` row is present), `focused_model_list_aligns_under_the_value` (`●` byte-col ≥ `VALUE_COL`), `create_popup_shows_a_group_divider`, the 1×1…200×50 sweep. **Extend the tiny sweep** to also render once with `field = WORKER_MODEL` and a 20-item catalog seeded, so the scrollbar's clamp path is exercised across all sizes (no panic).

- [ ] **Step 7: Green + hygiene + commit.**
```bash
cargo test 2>&1 | tail -20
cargo clippy --all-targets 2>&1 | tail -5
cargo fmt --all -- --check
git commit -am "feat(pmtui): taller create-popup model area + ratatui scrollbar for the inline list"
```

---

### Task 2: Live tmux render + mandatory acceptance + docs

**Files:** `README.md` (the create-form FAQ paragraph), the design-doc Status; live tmux, `--ignored` acceptance.

- [ ] **Step 1: Build the real binary.** `cargo build` (updates `target/debug/pmtui` — `cargo test` does NOT).
- [ ] **Step 2: Live tmux render** (pmtui-ui-testing skill — scratch sockets ONLY; NEVER `--socket pmd`, NEVER the real `~/.config/pmd/registry.json`). Seed a `$SCRATCH` registry + `$SCRATCH/.project-state/config.json` autopilot config. Launch pmtui with `ECC_GATEGUARD=off --socket pmtuitest-inner` on `tmux -L pmtui-test`, press `n`, Tab to Worker Model, capture (`-e -p | cat -v` to see the bar glyphs). Verify + paste frames: (a) a scrollbar with `↑`/`↓` and a `█` thumb down the right of the list when the claude catalog overflows 14 rows; (b) `… N more` no longer appears; (c) stepping ←→ moves the thumb; (d) the popup is a bit taller and does NOT resize when Tabbing between a non-model field and the model field. Clean up BOTH scratch servers + `$SCRATCH`; confirm no stray `claude`/`codex` REPLs.
- [ ] **Step 3: Real-tmux acceptance (MANDATORY).** `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 2>&1 | tail -40`. Must pass. Fix any test asserting the `… N more` string or exact popup geometry — test-only, no weakening. Report pass/fail counts.
- [ ] **Step 4: Docs.** In `README.md`, update the `w` / create-form FAQ paragraph: the reserved model-list area is taller and a scrollbar (not `… N more`) marks a catalog longer than the viewport. Flip the new design-doc Status to `implemented @ feat/create-form-fixed-popup-decider-label`.
- [ ] **Step 5: Commit.** `git commit -am "docs(create-form): taller model area + scrollbar — README + spec status"`

---

## Self-Review

**1. Spec coverage:** viewport 10→14 → T1 Step 1 (+ fixed-height/taller test T1 Step 6); scrollbar replacing `… N more` → T1 Steps 2 (drop line + return meta), 3 (record span), 4 (import), 5 (render); only-on-overflow → T1 Step 5 guard (`list_len > win`); thumb tracks current → `position(span.current)` (T1 Step 5) + the thumb-tracks test (T1 Step 6); no-panic clamp → T1 Step 5 clamp + extended tiny sweep (T1 Step 6); live + mandatory acceptance → T2 Steps 2–3; docs → T2 Step 4. Interaction/width/colors/divider/label unchanged (Global Constraints).

**2. Placeholder scan:** no TBD; the const, the `model_rows` return-type change, the `ListSpan` capture, the import, and the scrollbar block are all exact code; tests are concrete; the `… N more` line is explicitly deleted (not stubbed).

**3. Type consistency:** `MODEL_LIST_VIEWPORT`/`LABEL_W`/`VALUE_COL: usize`; `model_rows` now `-> (Vec<Line>, Option<(usize, usize, usize)>)` and BOTH call sites (Worker + Decider) destructure `(lines, meta)`; `ListSpan { row_off, win, current, list_len }: usize`; `row_off = rows.len() + 1`; `Rect { x, y, width, height }: u16`; `ScrollbarState::new(list_len: usize).position(current: usize)`; `Scrollbar::new(ScrollbarOrientation::VerticalRight)`; `f.render_stateful_widget(sb, bar, &mut state)`; `screen_rows(&t) -> Vec<(String, Vec<Color>)>`; `CreateForm::{WORKER_MODEL, DECIDER_MODEL, new, shows_decider}`, `ModelInfo { label, value }` match the current file. `"… N more"` appears ONLY in the removal step.
