# Create-Form Inline Model List + Bigger Popup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the `n` create popup genuinely bigger (~80% of the terminal) and move the model list from the right side panel to INLINE, directly below the focused model field — showing all its selections with the current one marked `●`. Keep the ←→ stepper / ↑↓ field-nav interaction. No schema/launch/discovery change.

**Architecture:** Split the shared card-drawing body out of `draw_overlay_frame` into `draw_overlay_frame_in(f, popup, …)` so `render_create` can size its own ~80% popup rect (past the shared `OVERLAY_MAX_W=96` cap) via the existing `centered_rect`, while every other overlay keeps the shared width policy. In `render_create`, drop the two-column split + the side panel and render a single column; a focused model field expands into an inline list below its summary row, and the popup height grows to fit it.

**Tech Stack:** Rust, ratatui 0.30.2 (`centered_rect`, `Block::bordered`, `Paragraph`, `TestBackend`), crossterm, `agent_manager::models::ModelInfo`, the real-tmux acceptance harness + the `pmtui-ui-testing` skill.

## Global Constraints

- **No schema/launch/discovery change.** Reuse `Config.decider_model`, `ProjectEntry.worker_model`, `App::model_catalog`/`models_for`, `CreateForm.model_choices`/`decider_model_choices`, and every seed/launch threading verbatim. Pure render change; single-writer holds; the ledger is untouched.
- **Interaction unchanged.** ↑↓ / Tab / BackTab move between fields; ←→ step the focused field's value (model fields via `CreateForm::adjust`). No typing/filter/custom. This task touches ONLY render (`render/chrome.rs`, `render/create.rs`) + tests + docs — NO edits to `create_form.rs` field/adjust logic or `keys.rs`.
- **Other overlays unchanged.** `draw_overlay_frame`'s public signature and behavior stay identical; the answer/confirm/editing/model_picker/sending overlays must render byte-identically (their render tests stay green untouched).
- **Full suite after every change:** `cargo test`, `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check`.
- **Real-tmux acceptance is MANDATORY before merge** (create-form change — every prior create-form change was only caught RED there): `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1`.
- pmtui copy: terse, imperative. Emphasis = coloured bold text, never a fill.

## Current state (verified anchors)

- `src/bin/pmtui/render/chrome.rs`: `pub(crate) fn centered_rect(width, height, area) -> Rect` (exported). `OVERLAY_MAX_W = 96`; `OVERLAY_MARGIN_X = 2`, `OVERLAY_MARGIN_Y = 1`; `overlay_h(rows) -> u16` (= rows + 2, floored at 9). `overlay_rect(area, want_w, want_h) -> Rect` calls `overlay_w` (which clamps to 96). `draw_overlay_frame(f, area, want_w, want_h, title, accent, hint) -> Rect` (lines ~143-186): `let popup = overlay_rect(area, want_w, want_h); f.render_widget(Clear, popup); let pad = Padding::horizontal(overlay_pad(popup.width)); let mut block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(accent)).style(attention::surface()).title(Line::from(Span::styled(format!(" {title} "), Style::default().fg(accent).add_modifier(Modifier::REVERSED | Modifier::BOLD)))).padding(pad); if !hint.is_empty() && usize::from(popup.width) >= text_cols(hint) + 6 { block = block.title_bottom(Line::styled(format!(" {hint} "), Style::default().add_modifier(Modifier::DIM)).centered()); } let inner = block.inner(popup); f.render_widget(block, popup); inner`. Callers of `draw_overlay_frame`: answer.rs, confirm.rs (×2), create.rs, editing.rs (×3), model_picker.rs, sending.rs — all keep the shared policy.
- `src/bin/pmtui/render/create.rs` (the CURRENT side-panel version): `PANEL_W=34`, `MIN_TWO_COL_W=78`, `CREATE_MIN_BODY_ROWS=10`. `render_create` computes `rows_shown` (one per shown field, floored), `want_h = overlay_h(rows_shown+1).min(3/4 screen)`, calls `draw_overlay_frame(f, area, 96, want_h, …)`, then splits `inner` into `left` + optional `panel` via `Layout::horizontal([Min(0), Length(PANEL_W)])` (only when `inner.width >= MIN_TWO_COL_W`). Closures: `val_style`, `field_row(idx,label,String)`, `caret_row(idx,label,&Field)`, `model_summary(choices,stored)->String`. Model rows use `field_row(idx, label, model_summary(...))`. Renders `Paragraph::new(Text::from(rows)).wrap(Wrap{trim:false})` into `left`; then `render_model_panel(f, panel, form)` for the right column. `render_model_panel` (lines ~206-283) draws a bordered "Models" block and the windowed list / dim hint.
- Field indices (in `create_form.rs`, unchanged): engine=0, `WORKER_MODEL=1`, dir=2, tier=3, `GOAL=4`, `CADENCE=5`, `DECIDER=6`, `DECIDER_MODEL=7`. `shows_goal`/`shows_cadence`/`shows_decider` are Autopilot-only.
- `ModelInfo { label: String, value: String }`.
- Tests: `src/bin/pmtui/tests/create_form.rs` has render tests asserting the side panel — `worker_model_panel_lists_choices_and_marks_current`, `non_model_field_shows_a_panel_hint`, `create_popup_width_is_focus_independent`, `the_focused_model_field_draws_its_own_catalog` (grep the file for the current names), plus the tiny-size sweep `create_form_renders_at_every_size_without_panicking` and the Standard-tier `!contains("D-Model")` / `!contains("Goal")` visibility assertions. `use crate::*` is in scope.

---

### Task 1: Bigger popup + inline model list — chrome split, render rewrite, render tests

**Files:**
- Modify: `src/bin/pmtui/render/chrome.rs` (split `draw_overlay_frame` → `draw_overlay_frame_in`)
- Modify: `src/bin/pmtui/render/create.rs` (drop side panel; ~80% popup; inline list)
- Test: `src/bin/pmtui/tests/create_form.rs` (render tests)

**Interfaces — Produces:** `pub(crate) fn draw_overlay_frame_in(f: &mut Frame, popup: Rect, title: &str, accent: Color, hint: &str) -> Rect`. **Removes:** `render_model_panel`, `PANEL_W`, `MIN_TWO_COL_W` from `create.rs`.

- [ ] **Step 1: Split the card body in `chrome.rs`.** Refactor `draw_overlay_frame` into a thin wrapper over a new `draw_overlay_frame_in` that takes a pre-computed `popup: Rect` and does the exact same `Clear` + block + hint + `inner` work:

```rust
pub(crate) fn draw_overlay_frame(
    f: &mut Frame,
    area: Rect,
    want_w: u16,
    want_h: u16,
    title: &str,
    accent: Color,
    hint: &str,
) -> Rect {
    draw_overlay_frame_in(f, overlay_rect(area, want_w, want_h), title, accent, hint)
}

/// Draw the shared modal card chrome INTO a caller-chosen `popup` rect and return the inner body
/// area. Split out of [`draw_overlay_frame`] so the create overlay can size itself LARGE (past
/// [`OVERLAY_MAX_W`]) via [`centered_rect`] while every other overlay keeps the shared width policy.
pub(crate) fn draw_overlay_frame_in(
    f: &mut Frame,
    popup: Rect,
    title: &str,
    accent: Color,
    hint: &str,
) -> Rect {
    f.render_widget(Clear, popup);
    let pad = Padding::horizontal(overlay_pad(popup.width));
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .style(attention::surface())
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
        )))
        .padding(pad);
    if !hint.is_empty() && usize::from(popup.width) >= text_cols(hint) + 6 {
        block = block.title_bottom(
            Line::styled(
                format!(" {hint} "),
                Style::default().add_modifier(Modifier::DIM),
            )
            .centered(),
        );
    }
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    inner
}
```

(This is a pure extraction — `draw_overlay_frame` behaves identically for all existing callers.)

- [ ] **Step 2: Rewrite `render_create` in `create.rs` — bigger popup, single column.** Replace the constants + the top of `render_create` + the split, and DELETE `render_model_panel`.
  - Replace the constants with:

```rust
/// Max rows of the focused model field's inline list before it windows with a dim `… N more`.
/// Generous — real claude/codex catalogs are well under this, so they show whole.
const MODEL_LIST_WINDOW: usize = 12;
/// A floor on the body rows so the create popup reads as a roomy window even on Standard (where the
/// Autonomy dial hides four fields) with no model field expanded.
const CREATE_MIN_BODY_ROWS: usize = 8;
```

  - Height accounting grows the popup when a model field is focused (each focused model field adds its inline list rows):

```rust
// Rows a shown field costs: 1 for a plain field; for a FOCUSED model field its summary row + the
// windowed inline list + a `… N more` note when the catalog overflows the window; 1 for an
// unfocused model field. Counted here so the frame grows to fit an open list before it is drawn.
let model_field_rows = |idx: usize, choices: &[ModelInfo]| -> usize {
    if form.field != idx {
        return 1;
    }
    let list_len = 1 + choices.len();
    let win = list_len.min(MODEL_LIST_WINDOW);
    let overflow = usize::from(list_len > MODEL_LIST_WINDOW);
    1 + win + overflow
};
let mut rows_shown = 1; // Engine
rows_shown += model_field_rows(CreateForm::WORKER_MODEL, &form.model_choices);
rows_shown += 1; // Directory
rows_shown += 1; // Autonomy
if form.shows_goal() {
    rows_shown += 1;
}
if form.shows_cadence() {
    rows_shown += 1;
}
if form.shows_decider() {
    rows_shown += 1; // Decider engine
    rows_shown += model_field_rows(CreateForm::DECIDER_MODEL, &form.decider_model_choices);
}
let rows_shown = rows_shown.max(CREATE_MIN_BODY_ROWS);
```

  - Size the popup at ~80% and draw via `draw_overlay_frame_in` + `centered_rect`:

```rust
// ~80% of the terminal, but at least the old 96 and never past the margin — the user asked twice
// for a BIGGER create popup, so it exceeds the shared OVERLAY_MAX_W (which still caps every other
// overlay). Height grows to fit the fields + an open inline list, capped at ~80% and floored by
// overlay_h so it always reads as a window.
let want_w = area
    .width
    .saturating_mul(4)
    .saturating_div(5)
    .max(96)
    .min(area.width.saturating_sub(OVERLAY_MARGIN_X))
    .max(1)
    .min(area.width);
let cap_h = area.height.saturating_mul(4).saturating_div(5).max(1);
let want_h = overlay_h(rows_shown + 1)
    .min(cap_h)
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

  - Single column: delete the `let (left, panel) = …` split and the trailing `if let Some(panel) = panel { render_model_panel(…) }`. `value_cols` reads `inner.width`: `let value_cols = usize::from(inner.width).saturating_sub(13);`. Render into `inner`.
  - Keep `val_style`, `field_row`, `caret_row` unchanged.

- [ ] **Step 3: Replace `model_summary` with an inline `model_rows` closure.** In `create.rs`, replace the `model_summary` closure with:

```rust
// A model field (Worker Model / Decider Model): a ←→ stepper row that, when FOCUSED, expands into
// an inline list of ALL its selections below the summary. The summary is `< (default) >` or
// `< label >` (stored value looked up in its catalog, raw-value fallback). Each list row is
// `● `(the stored value's row — the `(default)` row when None) / `○ ` then the label, the `●` row
// drawn bold; windowed to MODEL_LIST_WINDOW keeping the `●` visible, with a dim `… N more` when the
// catalog overflows. There is no separate cursor — ←→ move the stored value, so `●` IS the current.
let model_rows = |idx: usize, label: &str, choices: &[ModelInfo], stored: &Option<String>| -> Vec<Line> {
    let summary = match stored {
        None => "< (default) >".to_string(),
        Some(v) => {
            let lbl = choices
                .iter()
                .find(|m| &m.value == v)
                .map(|m| m.label.as_str())
                .unwrap_or(v.as_str());
            format!("< {lbl} >")
        }
    };
    let mut out = vec![field_row(idx, label, summary)];
    if form.field != idx {
        return out;
    }
    let current = match stored {
        None => 0,
        Some(v) => choices
            .iter()
            .position(|m| &m.value == v)
            .map(|i| i + 1)
            .unwrap_or(0),
    };
    let list_len = 1 + choices.len();
    let win = list_len.min(MODEL_LIST_WINDOW);
    let max_start = list_len.saturating_sub(win);
    let start = current.saturating_sub(win.saturating_sub(1)).min(max_start);
    for disp in start..start + win {
        let is_cur = disp == current;
        let sel = if is_cur { "\u{25cf} " } else { "\u{25cb} " };
        let lbl = if disp == 0 {
            "(default)".to_string()
        } else {
            choices[disp - 1].label.clone()
        };
        let style = if is_cur {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        out.push(Line::from(vec![
            Span::raw("      "),
            Span::styled(format!("{sel}{lbl}"), style),
        ]));
    }
    if list_len > win {
        out.push(Line::from(Span::styled(
            format!("      \u{2026} {} more", list_len - win),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    out
};
```

  Then build `rows` using `rows.extend(model_rows(...))` for the two model fields (Engine/Directory/Autonomy/Goal/Cadence/Decider stay as-is):

```rust
let mut rows = vec![field_row(0, "Engine", format!("< {} >", form.engine.label()))];
rows.extend(model_rows(CreateForm::WORKER_MODEL, "Model", &form.model_choices, &form.worker_model));
rows.push(caret_row(2, "Directory", &form.dir));
rows.push(field_row(3, "Autonomy", autonomy_val));
if form.shows_goal() { /* UNCHANGED goal branch: caret_row for one-line, else field_row(goal_field_display(...)) */ }
if form.shows_cadence() { rows.push(field_row(CreateForm::CADENCE, "Cadence", cadence_val)); }
if form.shows_decider() {
    rows.push(field_row(CreateForm::DECIDER, "Decider", format!("< {} >", form.decider_engine.label())));
    rows.extend(model_rows(CreateForm::DECIDER_MODEL, "D-Model", &form.decider_model_choices, &form.decider_model));
}
f.render_widget(Paragraph::new(Text::from(rows)).wrap(Wrap { trim: false }), inner);
```

  Update the `create.rs` module doc comment (lines 1-7) to describe the single-column popup with the inline-below model list and the ~80% width. Remove the now-unused `Layout`/`Constraint` imports ONLY if they become unused (grep first — `Wrap`/`Paragraph`/`Text`/`Line`/`Span`/`Style`/`Modifier`/`Color`/`Block`/`BorderType` may still be used elsewhere in the file; `Block`/`BorderType` were only used by `render_model_panel`, so they likely drop out — let clippy/compiler guide the exact import list).

- [ ] **Step 4: Rewrite the render tests** in `tests/create_form.rs`. Grep for the side-panel tests (`worker_model_panel_lists_choices_and_marks_current`, `non_model_field_shows_a_panel_hint`, `create_popup_width_is_focus_independent`, `the_focused_model_field_draws_its_own_catalog`, and anything asserting `"Models"` / `"focus Model"` / a right column) and rewrite each to the inline reality. Canonical cases (reuse the file's existing `TestBackend` buffer reader — grep for how the current render tests flatten the buffer; do NOT invent a new one):

```rust
#[test]
fn focused_worker_model_expands_inline_below_with_current_marked() {
    let mut t = Terminal::new(TestBackend::new(160, 40)).unwrap();
    let mut f = CreateForm::new();
    f.model_choices = vec![
        ModelInfo { label: "Opus".into(), value: "v-opus".into() },
        ModelInfo { label: "Sonnet".into(), value: "v-sonnet".into() },
    ];
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let screen = screen_text(&t); // reuse the file's existing buffer reader
    assert!(screen.contains("(default)") && screen.contains("Opus") && screen.contains("Sonnet"));
    assert!(screen.contains("\u{25cf} (default)"), "current = (default) marked ●: {screen}");
    assert!(!screen.contains("Models"), "no side-panel title remains: {screen}");
    // Step once → ● moves to Opus (inline).
    f.adjust(true);
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    assert!(screen_text(&t).contains("\u{25cf} Opus"), "←→ moves the inline ●");
}

#[test]
fn create_popup_is_bigger_and_grows_when_a_model_field_is_focused() {
    // Width exceeds the old 96 on a wide terminal; height is taller with a model field focused
    // (inline list) than with Directory focused (no list).
    let widest = |t: &Terminal<TestBackend>| -> usize {
        screen_text(t).lines().map(|l| l.trim_end().chars().count()).max().unwrap_or(0)
    };
    let mut t = Terminal::new(TestBackend::new(160, 40)).unwrap();
    let mut f = CreateForm::new();
    f.model_choices = vec![
        ModelInfo { label: "Opus".into(), value: "v-opus".into() },
        ModelInfo { label: "Sonnet".into(), value: "v-sonnet".into() },
    ];
    f.field = 2; // Directory
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let w_dir = widest(&t);
    let h_dir = screen_text(&t).lines().filter(|l| l.contains('\u{2502}')).count();
    assert!(w_dir > 96, "popup is wider than the old 96 on a 160-col terminal: {w_dir}");
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let h_model = screen_text(&t).lines().filter(|l| l.contains('\u{2502}')).count();
    assert!(h_model > h_dir, "focusing a model field grows the popup ({h_model} > {h_dir})");
}
```

  - REMOVE `non_model_field_shows_a_panel_hint` (there is no panel/hint now) — do not replace with a vacuous check; the two tests above cover the inline behavior.
  - KEEP the tiny-size sweep `create_form_renders_at_every_size_without_panicking` (it now exercises the single-column inline path and the width/height clamps at 1×1…200×50). Verify it still seeds a focused model field for at least one size so the inline list path is swept.
  - The Standard-tier `!contains("D-Model")` / `!contains("Goal")` visibility assertions stay valid (field visibility unchanged) — leave them. (There is no longer a hint containing "decider", so no collision.)
  - If `screen_text` doesn't exist, use the exact reader the current render tests use.

- [ ] **Step 5: Green + hygiene + commit.** `cargo test 2>&1 | tail -20` (all pass — including the untouched answer/confirm/editing/model_picker/sending render tests, which prove the `draw_overlay_frame` extraction is behavior-preserving); `cargo clippy --all-targets 2>&1 | tail -5` (clean); `cargo fmt --all -- --check`.
```bash
git commit -am "feat(pmtui): bigger create popup (~80%) with the model list inline below the field; drop the side panel"
```

---

### Task 2: Live tmux render + mandatory acceptance + docs

**Files:**
- Modify: `README.md`, `docs/superpowers/specs/2026-08-24-create-form-inline-model-list-design.md` (Status), `docs/superpowers/specs/2026-08-24-create-form-side-panel-design.md` (Status: render superseded)
- Test: live tmux, `--ignored` acceptance

- [ ] **Step 1: Build.** `cargo build` (updates the REAL `target/debug/pmtui`; `cargo test` does NOT).

- [ ] **Step 2: Live tmux render** (pmtui-ui-testing skill — scratch sockets ONLY; NEVER `--socket pmd`, NEVER the real `~/.config/pmd/registry.json`). Seed a `$SCRATCH` registry + `$SCRATCH/.project-state/config.json` autopilot config so the Decider + D-Model fields show. Launch pmtui with `ECC_GATEGUARD=off`, press `n`, capture with `capture-pane`; drive with `send-keys` (Tab / Left / Right). Pressing `n` only opens the form (no agent spawn); do NOT press Enter on a row. Verify on real cells: (a) the popup is clearly BIGGER (wider) than before; (b) ↑↓ move fields; (c) on Worker Model the list shows INLINE below the row and ←→ moves the `●`; (d) Tab to Decider Model shows its list inline; (e) NOTHING renders in a right column. Capture the frames into the report. Clean up BOTH scratch tmux servers + `$SCRATCH`.

- [ ] **Step 3: Real-tmux acceptance (MANDATORY).** `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 2>&1 | tail -40` (~8 min). Must pass. Fix — test-only, without weakening — any form-driving acceptance test that asserted the two-column/side-panel geometry. Report pass/fail counts.

- [ ] **Step 4: Docs.**
  - `README.md`: the create form's focused model field expands into an inline list below it, in a bigger (~80%) popup (update any side-panel/two-column wording).
  - Flip `docs/superpowers/specs/2026-08-24-create-form-inline-model-list-design.md` Status to `implemented @ feat/create-form-inline-model-list`.
  - Mark `docs/superpowers/specs/2026-08-24-create-form-side-panel-design.md` Status: its render (the side panel) is superseded by the inline-list design (one line; keep the body).

- [ ] **Step 5: Commit.**
```bash
git commit -am "docs(create-form): inline-model-list — README + spec statuses"
```

---

## Self-Review

**1. Spec coverage:** bigger popup (~80%) → T1 Step 2 `want_w`/`cap_h` + the `draw_overlay_frame_in` split that lets it exceed 96; model list inline below the focused field → T1 Step 3 `model_rows` (summary row + indented `●`/`○` list, grow-on-focus); show all selections → full catalog listed, windowed only past `MODEL_LIST_WINDOW=12` with `… N more`; drop the side panel → `render_model_panel`/`PANEL_W`/`MIN_TWO_COL_W`/the split removed, single column; interaction unchanged → no `create_form.rs`/`keys.rs` edits; other overlays unchanged → `draw_overlay_frame` is a pure extraction, their render tests stay green; mandatory live render + acceptance → T2 Steps 2-3. Out of scope (typing, the `e`/`w` ModelPicker, schema) → untouched.

**2. Placeholder scan:** no TBD; `draw_overlay_frame_in`, the `render_create` sizing, and `model_rows` are complete code; test cases are concrete (the one soft spot — the buffer reader name — is explicitly "reuse the file's existing reader, don't invent one"); the goal branch is called out as UNCHANGED with its exact shape.

**3. Type consistency:** `draw_overlay_frame_in(f: &mut Frame, popup: Rect, title: &str, accent: Color, hint: &str) -> Rect`, `centered_rect(width, height, area) -> Rect`, `MODEL_LIST_WINDOW`/`CREATE_MIN_BODY_ROWS: usize`, `OVERLAY_MARGIN_X`/`OVERLAY_MARGIN_Y`, `overlay_h(usize)->u16`, `model_rows(idx, label, &[ModelInfo], &Option<String>) -> Vec<Line>`, and `CreateForm::{WORKER_MODEL, DECIDER_MODEL, shows_goal, shows_cadence, shows_decider}` are spelled identically across tasks. `render_model_panel`, `PANEL_W`, `MIN_TWO_COL_W` appear ONLY in removal steps.
