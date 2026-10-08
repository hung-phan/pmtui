# Create-Form Side-Panel Model Stepper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the create form's inline select-or-type combobox with a uniform interaction — **↑↓ move between fields, ←→ step the focused field's value** (model fields included) — and show the focused model field's list in a **right-hand side panel** inside a genuinely bigger two-column popup. No typing/filtering; no schema/launch/discovery change.

**Architecture:** Remove `ModelCombo`. The two model fields become ordinary ←→ steppers over `[(default)] ++ choices`, storing into the existing `worker_model`/`decider_model: Option<String>`. `render_create` splits its inner area into a left field column and a right bordered "Models" panel that lists the focused model field's choices with the current one marked `●` and bold; the panel collapses on a narrow terminal (the `< label >` summary still shows the current model). The popup width goes to the overlay max (96) and the height is floored so the popup is a consistent, roomy window that no longer grows on model-focus.

**Tech Stack:** Rust, ratatui 0.30.2 (`Layout::horizontal`, `Block::bordered`, `Paragraph`, `TestBackend`), crossterm, `agent_manager::models::ModelInfo`, the real-tmux acceptance harness + the `pmtui-ui-testing` skill.

## Global Constraints

- **No schema/launch/discovery change.** Reuse `Config.decider_model`, `ProjectEntry.worker_model`, `App::model_catalog`/`models_for`, `CreateForm.model_choices`/`decider_model_choices`, and every seed/launch threading verbatim. Pure input/render UX change. Single-writer holds (create still seeds `config.json` + writes the `ProjectEntry`; the ledger is untouched — byte-identity still holds).
- **`None` = `(default)` = the CLI's own default** (no `--model`/`-m`). A stored model value is ALWAYS `None` or a catalog `value` (no custom/typed values any more).
- **Uniform keys:** ↑↓ (and Tab/BackTab) move between fields for EVERY field; ←→ change the focused field's value for EVERY field (model fields included, exactly like Engine/Autonomy/Cadence/Decider). No field-specific ↑↓ capture.
- **Full suite after every change:** `cargo test`, `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check`.
- **Real-tmux acceptance is MANDATORY before merge** (create-form change — every prior create-form change was only caught RED there). Run `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1`.
- pmtui copy: terse, imperative. Emphasis = coloured bold text, never a fill/icon highlight.

## Current state (verified anchors)

- `src/bin/pmtui/create_form.rs`: holds `ModelCombo { input: Field, cursor: usize }` (lines 8–113) with `new`/`seed`/`input_str`/`input`/`cursor`/`filtered`/`list_len`/`move_cursor`/`edit`/`resolve`. `CreateForm` has fields `field, engine, worker_model: Option<String>, model_choices: Vec<ModelInfo>, dir: Field, tier, goal: Field, cadence_s, decider_engine, decider_model: Option<String>, decider_model_choices: Vec<ModelInfo>, worker_combo: ModelCombo, decider_combo: ModelCombo`. Indices: engine=0, `WORKER_MODEL=1`, dir=2, tier=3, `GOAL=4`, `CADENCE=5`, `DECIDER=6`, `DECIDER_MODEL=7`; `FIELDS=8`.
- Combo plumbing on `CreateForm`: `seed_focused_combo` (called from `next_field`/`prev_field`), `is_model_field`, `sync_model_values`, `combo_move`, `combo_edit`. `toggle_engine`/`toggle_decider_engine` set the model value to `None` AND reset the combo. `adjust()` routes engine(0)/tier(3)/cadence(`CADENCE`)/decider(`DECIDER`) — NOT the model fields.
- `src/bin/pmtui/keys.rs handle_create_key` (line 470): `^E`→brief editor; Esc→cancel; Enter→submit; then a `CreateForm::is_model_field(form.field) && form.shows_field(form.field)` branch (lines 500–548) routes ↑↓→`combo_move`, Tab/BackTab→field nav, typing/caret→`combo_edit`; below it the generic handling (Tab/Down→`next_field`, ←/→→caret-or-`adjust`, Char→text-or-space-adjust) + the engine-flip repopulation (lines 591–615, which also reset the combos). `handle_paste` (line 631) has a `Creating(form) if is_model_field` branch (643–645) that feeds the combo.
- `src/bin/pmtui/render/create.rs`: `render_create` at width **88**, height content-driven via a `model_field_rows` closure that GROWS the frame when a model field is focused; a `model_rows` closure renders the focused model field as a caret filter row + windowed list (`▸`/`●`/`○`, `… N more`, `MODEL_LIST_ROWS=6`) and unfocused as a `< label >` summary. `field_row`/`caret_row` closures; `value_cols = inner.width - 13`.
- `src/bin/pmtui/render/chrome.rs`: `overlay_h(rows)` = `rows + 2`, floored at 9. `draw_overlay_frame(f, area, want_w, want_h, title, accent, hint) -> Rect` centres a rounded bordered box and returns its inner Rect; `OVERLAY_MAX_W = 96` clamps width (so passing 96 yields the widest allowed popup).
- `ModelInfo { label: String, value: String }` (`src/models/mod.rs`), already imported in `create_form.rs`/`create.rs`.
- Tests: `src/bin/pmtui/tests/create_form.rs` — behaviour + `TestBackend` render tests. Combo references at lines ~15–23, ~64–71 (nav test), the `ModelCombo` unit block ~87–230 (`combo_choices`, `combo_arrow_selects_and_resolves_to_value`, `combo_arrows_walk_the_filtered_list`, `combo_seed_parks_cursor_on_the_stored_value`, `combo_typing_filters_and_allows_custom`, `combo_cursor_clamps_and_empty_catalog_is_default_only`, `combo_cursor_reports_the_highlighted_row`), and integration/render tests ~466+ (`worker_model_combo_defaults_none_walks_and_types`, `toggling_the_engine_resets_the_worker_model`, plus render tests that assert `Model`/`D-Model`/`(default)`/list markers). `use crate::*` brings in `CreateForm`, `ModelInfo`, and any `pub(crate)` free fn from `create_form.rs`.
- Design doc: `docs/superpowers/specs/2026-08-24-create-form-side-panel-design.md`.

---

### Task 1: ←→ model stepper — remove `ModelCombo`, rewire behaviour + keys, unit/integration tests

**Files:**
- Modify: `src/bin/pmtui/create_form.rs` (delete `ModelCombo`; drop the two combo fields + all combo plumbing; add `step_model`; add `WORKER_MODEL`/`DECIDER_MODEL` arms to `adjust`)
- Modify: `src/bin/pmtui/keys.rs` (`handle_create_key`: delete the model-combo branch; the engine-flip block drops its combo resets. `handle_paste`: delete the model-combo branch)
- Test: `src/bin/pmtui/tests/create_form.rs` (behaviour tests only; render tests are Task 2)

**Interfaces — Produces:** `pub(crate) fn step_model(current: Option<&str>, choices: &[ModelInfo], forward: bool) -> Option<String>`. **Removes:** `ModelCombo` and `CreateForm::{worker_combo, decider_combo, seed_focused_combo, is_model_field, sync_model_values, combo_move, combo_edit}`.

- [ ] **Step 1: Write the failing stepper unit tests.** In `tests/create_form.rs`, DELETE the entire `ModelCombo` unit block (the `combo_choices` helper and the seven `combo_*` tests, currently ~lines 87–230) and add in its place:

```rust
// --- `step_model`: the ←→ model stepper over [(default)] ++ choices --------------
fn step_choices() -> Vec<ModelInfo> {
    vec![
        ModelInfo { label: "Opus".into(), value: "v-opus".into() },
        ModelInfo { label: "Sonnet".into(), value: "v-sonnet".into() },
        ModelInfo { label: "Haiku".into(), value: "v-haiku".into() },
    ]
}

#[test]
fn step_model_walks_forward_and_wraps_through_default() {
    let ch = step_choices();
    // From (default)=None, forward walks the catalog in order…
    let a = step_model(None, &ch, true);
    assert_eq!(a.as_deref(), Some("v-opus"));
    let b = step_model(a.as_deref(), &ch, true);
    assert_eq!(b.as_deref(), Some("v-sonnet"));
    let c = step_model(b.as_deref(), &ch, true);
    assert_eq!(c.as_deref(), Some("v-haiku"));
    // …then wraps from the last model back to (default) = None.
    assert_eq!(step_model(c.as_deref(), &ch, true), None);
}

#[test]
fn step_model_walks_backward_from_default_to_last() {
    let ch = step_choices();
    // Backward from (default) lands on the LAST model, then walks up.
    let z = step_model(None, &ch, false);
    assert_eq!(z.as_deref(), Some("v-haiku"));
    let y = step_model(z.as_deref(), &ch, false);
    assert_eq!(y.as_deref(), Some("v-sonnet"));
}

#[test]
fn step_model_empty_catalog_stays_default() {
    let ch: Vec<ModelInfo> = vec![];
    assert_eq!(step_model(None, &ch, true), None);
    assert_eq!(step_model(None, &ch, false), None);
}

#[test]
fn step_model_unknown_current_is_treated_as_default() {
    // A stored value the catalog doesn't contain (e.g. an engine just flipped) steps as if from
    // (default): forward → first model.
    let ch = step_choices();
    assert_eq!(step_model(Some("not-in-catalog"), &ch, true).as_deref(), Some("v-opus"));
}
```

- [ ] **Step 2: Run → fail to compile** (`step_model` missing; the deleted combo tests no longer reference `ModelCombo`).

Run: `cargo test --bin pmtui step_model 2>&1 | tail -20`
Expected: FAIL — `cannot find function step_model`.

- [ ] **Step 3: Delete `ModelCombo`.** Remove the whole `ModelCombo` struct + `impl` (lines 8–113 of `create_form.rs`, from the `/// A select-or-type model field:` doc comment through the closing `}` of `resolve`).

- [ ] **Step 4: Drop the combo fields + plumbing from `CreateForm`.**
  - Remove the `worker_combo: ModelCombo` and `decider_combo: ModelCombo` fields (and their doc comments) from the struct.
  - Remove `worker_combo: ModelCombo::new(), decider_combo: ModelCombo::new(),` from `new()`.
  - Remove the `fn seed_focused_combo(&mut self)` method entirely; in `next_field` and `prev_field`, delete the `self.seed_focused_combo();` call so each simply `return;`s after finding a shown field.
  - In `toggle_engine`, delete `self.worker_combo = ModelCombo::new();` (keep `self.worker_model = None;`). In `toggle_decider_engine`, delete `self.decider_combo = ModelCombo::new();` (keep `self.decider_model = None;`).
  - Remove `pub(crate) fn is_model_field(idx: usize) -> bool`, `pub(crate) fn sync_model_values(&mut self)`, `pub(crate) fn combo_move(&mut self, delta: isize)`, and `pub(crate) fn combo_edit<...>(&mut self, f: ...)` entirely.

- [ ] **Step 5: Add `step_model` + wire the two model fields into `adjust`.** Add the free function at module scope (e.g. just above `impl CreateForm`):

```rust
/// Step a model field's stored value over the implicit list `[(default)] ++ choices`: index 0 is
/// `(default)` (= `None`), index `i` is `choices[i-1]`. The current index is derived from the
/// stored value (a value absent from the catalog — e.g. right after an engine flip — counts as
/// `(default)`), stepped ±1 modulo `choices.len() + 1`. Returns `None` at index 0, else the
/// landed model's launch-ready `value`. Pure; the create form's Worker/Decider Model ←→ handler.
pub(crate) fn step_model(current: Option<&str>, choices: &[ModelInfo], forward: bool) -> Option<String> {
    let len = choices.len() + 1; // +1 for the (default) row
    let cur = match current {
        None => 0,
        Some(v) => choices.iter().position(|m| m.value == v).map(|i| i + 1).unwrap_or(0),
    };
    let next = if forward { (cur + 1) % len } else { (cur + len - 1) % len };
    if next == 0 { None } else { Some(choices[next - 1].value.clone()) }
}
```

Then extend `CreateForm::adjust` with the two model arms (the RHS evaluates to an owned `Option<String>` before the assignment, so borrowing `self.worker_model`/`self.model_choices` in the call and writing `self.worker_model` in the same statement is fine):

```rust
pub(crate) fn adjust(&mut self, forward: bool) {
    match self.field {
        0 => self.toggle_engine(),
        Self::WORKER_MODEL => {
            self.worker_model = step_model(self.worker_model.as_deref(), &self.model_choices, forward);
        }
        3 => {
            self.tier = if forward { next_tier(self.tier) } else { prev_tier(self.tier) }
        }
        Self::CADENCE => self.adjust_cadence(forward),
        Self::DECIDER => self.toggle_decider_engine(),
        Self::DECIDER_MODEL => {
            self.decider_model =
                step_model(self.decider_model.as_deref(), &self.decider_model_choices, forward);
        }
        _ => {}
    }
}
```

Update the `adjust` doc comment: model fields are now ←→ steppers like the other toggles (drop the "no longer routes them" wording).

- [ ] **Step 6: Rewire `keys.rs`.**
  - In `handle_create_key`, DELETE the whole model-combo branch (`if CreateForm::is_model_field(form.field) && form.shows_field(form.field) { match code { … } }`, lines ~500–548). Model fields now fall through to the generic block: `is_text_field()` is false for them, so ←/→ call `form.adjust(false/true)`, ↑↓/Tab/BackTab call `next_field`/`prev_field`, space calls `adjust(true)`, and Home/End/Delete/Backspace are the existing text no-ops — exactly the toggle behaviour.
  - In the engine-flip repopulation block below it, delete the two `form.worker_combo = ModelCombo::new();` / `form.decider_combo = ModelCombo::new();` resets (and their comments). Keep the `form.model_choices = choices;` / `form.decider_model_choices = choices;` refreshes verbatim (`toggle_engine`/`toggle_decider_engine` already reset the stored model to `None`).
  - Update the block's lead comment (lines ~496–499) to describe the model fields as ordinary ←→ toggles.
  - In `handle_paste`, DELETE the `UiMode::Creating(form) if CreateForm::is_model_field(form.field) => { form.combo_edit(|f| f.insert_str(&text)); }` arm. The remaining `UiMode::Creating(form) => form.paste(&text)` arm already handles a create-form paste as a no-op on non-text (model/toggle) fields (`text_field()` returns `None`).

- [ ] **Step 7: Fix the two nav tests + rewrite the integration tests.** In `tests/create_form.rs`:
  - `create_form_navigation_and_editing` (~line 15–23): the field is `WORKER_MODEL` with one pre-seeded choice — replace `f.combo_move(1);` with `f.adjust(true);` and keep `assert_eq!(f.worker_model.as_deref(), Some("opus-value"));`. Update the inline comment to "←→ steps the model value" (drop "combo"/"↓ fills the input").
  - The decider-model spot (~line 64–71): replace `f.combo_move(1);` with `f.adjust(true);`; keep the resolve assertion; update the comment.
  - Replace `worker_model_combo_defaults_none_walks_and_types` (~line 466) with:

```rust
#[test]
fn worker_model_steps_with_left_right() {
    // The Worker Model field (index 1) defaults to (default) = None. ← / → STEP the stored value
    // through the catalog and wrap back to None — the uniform toggle interaction, no list mode.
    let mut f = CreateForm::new();
    assert_eq!(f.worker_model, None, "no model chosen by default");
    assert!(f.model_choices.is_empty());
    assert!(f.shows_field(CreateForm::WORKER_MODEL), "the Model field always shows");

    f.model_choices = vec![
        ModelInfo { label: "Opus".into(), value: "global.anthropic.claude-opus-5".into() },
        ModelInfo { label: "Sonnet".into(), value: "global.anthropic.claude-sonnet-5".into() },
    ];
    f.field = CreateForm::WORKER_MODEL;
    f.adjust(true); // (default) -> first
    assert_eq!(f.worker_model.as_deref(), Some("global.anthropic.claude-opus-5"));
    f.adjust(true); // -> second (a second single step walks on, never collapses)
    assert_eq!(f.worker_model.as_deref(), Some("global.anthropic.claude-sonnet-5"));
    f.adjust(true); // wraps back to (default) = None
    assert_eq!(f.worker_model, None);
    f.adjust(false); // backward from (default) lands on the LAST model
    assert_eq!(f.worker_model.as_deref(), Some("global.anthropic.claude-sonnet-5"));
    // ↑↓ move FIELDS, not the value: from WORKER_MODEL, next_field lands on Directory.
    f.next_field();
    assert_eq!(f.field, 2, "up/down move between fields; the model value is unchanged by nav");
    assert_eq!(f.worker_model.as_deref(), Some("global.anthropic.claude-sonnet-5"));
}
```

  - `toggling_the_engine_resets_the_worker_model` (~line 525): replace the `f.combo_move(1);` that sets a model with `f.adjust(true);`; keep the assertion that `toggle_engine` clears `worker_model` to `None`.
  - grep the file once more for `combo`, `ModelCombo`, `combo_move`, `combo_edit`, `.seed(`, `move_cursor`, `filtered(`, `list_len(`, `resolve(` and reroute any remaining BEHAVIOUR test to `adjust`/`next_field`/`step_model`. Leave RENDER tests (TestBackend) that assert list markers for Task 2 — if any fails to COMPILE because it calls a removed combo method, minimally convert that call to `adjust`/`next_field` now (Task 2 revisits its assertions). Do NOT weaken assertions.

- [ ] **Step 8: Green + hygiene + commit.**

Run: `cargo test 2>&1 | tail -20` (all pass), then `cargo clippy --all-targets 2>&1 | tail -5` (clean), `cargo fmt --all -- --check`.
```bash
git commit -am "feat(pmtui): create-form model fields become ←→ steppers; remove ModelCombo (behavior + keys)"
```

---

### Task 2: Two-column popup + right-hand model side panel — render, render tests, live tmux, acceptance, docs

**Files:**
- Modify: `src/bin/pmtui/render/create.rs`
- Modify: `README.md`, `docs/superpowers/specs/2026-08-24-create-form-side-panel-design.md` (Status), `docs/superpowers/specs/2026-08-24-create-form-model-combobox-design.md` (Status: superseded)
- Test: `src/bin/pmtui/tests/create_form.rs` (TestBackend render tests), live tmux, `--ignored` acceptance

- [ ] **Step 1: Rewrite `render_create` as a two-column layout.** Delete `const MODEL_LIST_ROWS` and the `model_field_rows` closure. Add near the top of the file:

```rust
/// Width of the right-hand model side panel (a bordered list). The left field column takes the
/// rest. Below `MIN_TWO_COL_W` inner columns the panel is dropped and the fields use the full
/// width (the ←→ stepper's `< label >` summary still shows the current model).
const PANEL_W: u16 = 34;
const MIN_TWO_COL_W: u16 = 78;
/// A floor on the body rows so the create popup is a consistent, roomy window (with room for the
/// side panel's list) regardless of how many fields the Autonomy dial is showing.
const CREATE_MIN_BODY_ROWS: usize = 10;
```

  Compute the height from a per-field-shown count (each model field is ONE row now — the list is in the panel), floored:

```rust
let mut rows_shown = 4; // Engine, Model, Directory, Autonomy (always shown)
if form.shows_goal() { rows_shown += 1; }
if form.shows_cadence() { rows_shown += 1; }
if form.shows_decider() { rows_shown += 2; } // Decider engine + D-Model
let rows_shown = rows_shown.max(CREATE_MIN_BODY_ROWS);
// ONE spare row for the Autonomy descriptor's wrap (pinned by
// renders_loop_create_form_with_cadence_without_panicking); cap at 3/4 of the screen so an open
// popup never swallows the dashboard behind it.
let height_cap = (area.height / 4).saturating_mul(3).max(1);
let want_h = overlay_h(rows_shown + 1).min(height_cap);
```

  Draw the frame at width **96** (the overlay max — the biggest allowed, so the popup reads clearly bigger on open), with the updated hint:

```rust
let inner = draw_overlay_frame(
    f, area, 96, want_h, "New session", Color::Cyan,
    "\u{2191}\u{2193} field \u{b7} \u{2190}\u{2192} value \u{b7} ^E $EDITOR \u{b7} enter create \u{b7} esc cancel",
);
```

  Split `inner` into the field column + the panel (panel only when there's room):

```rust
let (left, panel) = if inner.width >= MIN_TWO_COL_W {
    let [l, r] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(PANEL_W)]).areas(inner);
    (l, Some(r))
} else {
    (inner, None)
};
```

  Compute `value_cols` from `left.width` (not `inner.width`): `let value_cols = usize::from(left.width).saturating_sub(13);`. Keep the `field_row`/`caret_row` closures as-is (they close over `form`/`value_cols`/`val_style`).

  Replace the `model_rows` closure with a plain summary via `field_row`. Add a helper:

```rust
// A model field is now an ordinary ←→ toggle row: `< (default) >` or `< label >` (the stored
// value looked up in its catalog, raw-value fallback). The engine's list lives in the side panel.
let model_summary = |choices: &[ModelInfo], stored: &Option<String>| -> String {
    match stored {
        None => "< (default) >".to_string(),
        Some(v) => {
            let lbl = choices.iter().find(|m| &m.value == v).map(|m| m.label.as_str()).unwrap_or(v.as_str());
            format!("< {lbl} >")
        }
    }
};
```

  Build `rows` in field order (Worker Model + D-Model use `field_row(idx, label, model_summary(...))`), then render into `left` and, if present, the panel:

```rust
let mut rows = vec![field_row(0, "Engine", format!("< {} >", form.engine.label()))];
rows.push(field_row(CreateForm::WORKER_MODEL, "Model", model_summary(&form.model_choices, &form.worker_model)));
rows.push(caret_row(2, "Directory", &form.dir));
rows.push(field_row(3, "Autonomy", autonomy_val));
if form.shows_goal() { /* UNCHANGED: caret_row for a one-line goal, else field_row(goal_field_display(...)) */ }
if form.shows_cadence() { rows.push(field_row(CreateForm::CADENCE, "Cadence", cadence_val)); }
if form.shows_decider() {
    rows.push(field_row(CreateForm::DECIDER, "Decider", format!("< {} >", form.decider_engine.label())));
    rows.push(field_row(CreateForm::DECIDER_MODEL, "D-Model", model_summary(&form.decider_model_choices, &form.decider_model)));
}
f.render_widget(Paragraph::new(Text::from(rows)).wrap(Wrap { trim: false }), left);
if let Some(panel) = panel {
    render_model_panel(f, panel, form);
}
```

  (Keep the existing `autonomy_val`/`cadence_val` construction and the exact Goal branch — a single-line goal uses `caret_row`, empty/multiline uses `field_row(goal_field_display(...))`.)

- [ ] **Step 2: Add `render_model_panel`.** New free fn in `create.rs`:

```rust
/// The right-hand side panel: the FOCUSED model field's list — `(default)` then the engine's
/// models — with the CURRENT value (the one ←→ steps) marked `\u{25cf}` and drawn in the focused
/// accent (bold), every other row `\u{25cb}`. Windowed to the panel height keeping the current row
/// visible, with a dim `… N more` when the list overflows. When a NON-model field is focused it
/// shows a dim hint so the column reads as inactive. Emphasis is coloured bold text, not a fill.
fn render_model_panel(f: &mut Frame, area: Rect, form: &CreateForm) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(dim)
        .title(Line::from(Span::styled(" Models ", dim)));
    let list_area = block.inner(area);
    f.render_widget(block, area);
    if list_area.width == 0 || list_area.height == 0 {
        return;
    }

    let (choices, stored): (&[ModelInfo], &Option<String>) = match form.field {
        CreateForm::WORKER_MODEL => (&form.model_choices, &form.worker_model),
        CreateForm::DECIDER_MODEL if form.shows_decider() => {
            (&form.decider_model_choices, &form.decider_model)
        }
        _ => {
            let hint = Paragraph::new(Text::from(vec![
                Line::from(Span::styled("Model list", dim)),
                Line::from(Span::styled("focus Model / D-Model,", dim)),
                Line::from(Span::styled("step with \u{2190}\u{2192}", dim)),
            ]))
            .wrap(Wrap { trim: false });
            f.render_widget(hint, list_area);
            return;
        }
    };

    // The current row = the stored value's position; 0 = (default) = None.
    let current = match stored {
        None => 0,
        Some(v) => choices.iter().position(|m| &m.value == v).map(|i| i + 1).unwrap_or(0),
    };
    let list_len = 1 + choices.len();
    let rows = usize::from(list_area.height).max(1);
    let overflow = list_len > rows;
    // When the list overflows, spend the LAST panel row on the overflow note and window the rest.
    let list_rows = if overflow { rows.saturating_sub(1).max(1) } else { list_len.min(rows) };
    let max_start = list_len.saturating_sub(list_rows);
    let start = current.saturating_sub(list_rows.saturating_sub(1)).min(max_start);
    let mut lines: Vec<Line> = Vec::with_capacity(list_rows + 1);
    for disp in start..start + list_rows {
        let is_cur = disp == current;
        let sel = if is_cur { "\u{25cf} " } else { "\u{25cb} " };
        let label = if disp == 0 { "(default)".to_string() } else { choices[disp - 1].label.clone() };
        let style = if is_cur {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(format!("{sel}{label}"), style)));
    }
    if overflow {
        let hidden = list_len - list_rows;
        lines.push(Line::from(Span::styled(format!("\u{2026} {hidden} more"), dim)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), list_area);
}
```

  Update the `render/create.rs` module doc comment (lines 1–7) to describe the two-column layout: the model fields are ←→ stepper rows in the left column, and the focused one's list shows in the right "Models" side panel; the frame height is field-driven (no longer grows on model-focus).

- [ ] **Step 3: Rewrite the render tests** in `tests/create_form.rs` to the panel model. For each TestBackend render test that focused a model field and asserted the OLD inline list (caret filter row / `▸` cursor / expand-on-focus / `… N more` height growth), reassert against the side panel instead. Canonical cases (add/adapt; keep existing test names where they still describe the behaviour, else rename):

```rust
#[test]
fn worker_model_panel_lists_choices_and_marks_current() {
    // Focus the Worker Model field on a wide terminal → the right "Models" panel lists (default)
    // + the catalog labels, with the current value marked ● (bold). Stepping ←→ moves the ●.
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut f = CreateForm::new();
    f.model_choices = vec![
        ModelInfo { label: "Opus".into(), value: "v-opus".into() },
        ModelInfo { label: "Sonnet".into(), value: "v-sonnet".into() },
    ];
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let screen = screen_text(&t); // reuse the surrounding render tests' TestBackend buffer reader
    assert!(screen.contains("Models"), "side panel titled: {screen}");
    assert!(screen.contains("(default)") && screen.contains("Opus") && screen.contains("Sonnet"));
    assert!(screen.contains("\u{25cf} (default)"), "current = (default) marked ●: {screen}");
    // Step once → ● moves to Opus.
    f.adjust(true);
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let screen = screen_text(&t);
    assert!(screen.contains("\u{25cf} Opus"), "stepping ←→ moves the ●: {screen}");
}

#[test]
fn non_model_field_shows_a_panel_hint() {
    // With a non-model field focused, the panel shows a dim hint, not a live list.
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut f = CreateForm::new();
    f.field = 0; // Engine
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let screen = screen_text(&t);
    assert!(screen.contains("focus Model"), "panel hint when no model field is focused: {screen}");
}

#[test]
fn create_popup_width_is_focus_independent() {
    // The popup width does NOT depend on which field is focused (the list moved to the fixed side
    // panel), and it uses the wide overlay. Measure via the same buffer reader the file uses.
    let width_when = |field: usize| -> usize {
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut f = CreateForm::new();
        f.model_choices = vec![ModelInfo { label: "Opus".into(), value: "v-opus".into() }];
        f.field = field;
        t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
        // Widest non-blank rendered row = the popup's drawn width. If the file has a popup-width
        // helper, use it; otherwise compute from the flattened buffer rows.
        screen_text(&t).lines().map(|l| l.trim_end().chars().count()).max().unwrap_or(0)
    };
    assert!(width_when(0) >= 90, "popup uses the wide overlay");
    assert_eq!(width_when(0), width_when(CreateForm::WORKER_MODEL), "width is focus-independent");
}
```

  - If there is no `screen_text` helper, use the exact buffer-flattening the surrounding render tests already use (grep the file for how existing `render_create` tests read the `TestBackend` buffer — reuse it verbatim; do NOT invent a new reader).
  - KEEP the tiny-size sweep test (the `for (w, h)` … `render_create` … no-panic loop, ~lines 972/1136) — it now also covers the narrow single-column fallback and the panel's zero-size guard. Verify it still passes; do not weaken it.
  - The `Standard hides the Decider Model field` / `Autopilot shows the Decider Model field` render assertions (~lines 681–686) stay valid (field visibility is unchanged) — leave them.

- [ ] **Step 4: Green + hygiene.** `cargo test 2>&1 | tail -20`; `cargo clippy --all-targets 2>&1 | tail -5`; `cargo fmt --all -- --check`.

- [ ] **Step 5: Build + live tmux render** (pmtui-ui-testing skill — scratch sockets only; NEVER `--socket pmd`, NEVER the real registry; seed `$SCRATCH/.project-state/config.json` autopilot so the Decider + D-Model fields show):

```bash
cargo build
```
Then, per the skill: launch pmtui on `tmux -L pmtui-test` with `--socket pmtuitest-inner`, press `n`, and capture. Verify on real cells: (a) the popup is a two-column card, visibly wider than before, present on open (Engine focused → panel hint); (b) ↑↓ move between fields; (c) on Worker Model, ←→ step the value and the side-panel `●` tracks it; (d) Tab to Decider Model shows the decider engine's list in the same panel. Report the frames. Clean up BOTH scratch servers + `$SCRATCH`.

- [ ] **Step 6: Real-tmux acceptance (MANDATORY).**

Run: `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 2>&1 | tail -30`
The field set/indices and the ↑↓/←→ semantics for non-model fields are unchanged, so the form-driving acceptance tests (n / Tab / Space / typing / Enter) should hold. Fix — test-only, without weakening assertions — any test that drove a model field via the old combo (↑↓-into-list or typing a filter); reroute it to ←→ stepping. Report pass/fail counts.

- [ ] **Step 7: Docs + Status.**
  - `README.md`: the create form's model fields are ←→ steppers and the focused one's list shows in a side panel in a bigger two-column popup (update any combobox wording added by the prior change).
  - Flip `docs/superpowers/specs/2026-08-24-create-form-side-panel-design.md` **Status** to `implemented @ feat/create-form-side-panel`.
  - Mark `docs/superpowers/specs/2026-08-24-create-form-model-combobox-design.md` **Status** superseded by the side-panel design (one line; do not rewrite its body).

- [ ] **Step 8: Commit.**
```bash
git commit -am "feat(pmtui): two-column create popup with a model side panel; render tests; docs"
```

---

## Self-Review

**1. Spec coverage:** ↑↓=fields / ←→=value for all fields → T1 keys (delete the combo branch, model fields fall through to `adjust`) + `step_model` + the `adjust` arms; side panel showing the current position → T2 `render_model_panel` (`●`/bold on the stored value's row, windowed, `… N more`); bigger, consistent two-column popup → T2 width 96 + `CREATE_MIN_BODY_ROWS` floor + `Layout::horizontal` split, focus-independent width test; no typing/filter/custom → `ModelCombo` and the paste/typing branches removed, stored value is `None`|catalog-`value` only; no schema/launch/discovery change → reuses `worker_model`/`decider_model`/`model_choices`/`decider_model_choices`/`models_for` untouched; narrow-terminal fallback → `MIN_TWO_COL_W` single-column path (summary still shows current); mandatory live render + acceptance → T2 Steps 5–6. Out of scope (the live `e`/`w` `ModelPicker`; other fields; discovery) → untouched.

**2. Placeholder scan:** no TBD; `step_model` and `render_model_panel` are complete code; the removals name exact items/line ranges; the one soft spot — the buffer-reader helper name — is explicitly "reuse the existing render tests' reader, don't invent one".

**3. Type consistency:** `step_model(current: Option<&str>, choices: &[ModelInfo], forward: bool) -> Option<String>`, `worker_model`/`decider_model: Option<String>`, `model_choices`/`decider_model_choices: Vec<ModelInfo>`, `PANEL_W`/`MIN_TWO_COL_W: u16`, `CREATE_MIN_BODY_ROWS: usize`, `render_model_panel(f, area, form)`, and the reused `CreateForm::{WORKER_MODEL, DECIDER_MODEL, adjust, next_field, prev_field, shows_decider, is_text_field}` are spelled identically across both tasks. `ModelCombo`, `worker_combo`/`decider_combo`, `combo_move`/`combo_edit`/`sync_model_values`/`is_model_field`/`seed_focused_combo` appear ONLY in removal steps.
