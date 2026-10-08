# Create-Form Model Combobox Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the create form's two `< value >` model *toggles* (Worker Model, Decider Model) with an inline **select-or-type combobox** in a **bigger** popup: the focused model field shows its engine's discovered models (arrow to select) plus a text input (type to filter, or enter a model discovery didn't list).

**Architecture:** A small `ModelCombo { input: Field, cursor }` field type. `input` is the source of truth; ↑↓ fill it from the filtered list; typing filters / enters custom. The resolved value flows into the existing `CreateForm.worker_model`/`.decider_model` (`Option<String>`) on every edit, so all downstream seed/launch wiring is unchanged. Only the focused model field expands its list; the popup grows to fit. No schema/discovery/launch change.

**Tech Stack:** Rust, ratatui 0.30.2 (TestBackend), crossterm, the `Field` text-buffer type (existing), `agent_manager::models::ModelInfo`, real-tmux acceptance harness.

## Global Constraints

- **No schema/launch/discovery change.** Reuse `Config.decider_model`, `ProjectEntry.worker_model`, `App::model_catalog`/`models_for`, `CreateForm.model_choices`/`decider_model_choices`, and all seed/launch threading verbatim. This is a pure input-UX change.
- **Single-writer preserved.** Create still seeds `config.json` + writes the `ProjectEntry`; no new write path; the ledger stays untouched (byte-identity still holds).
- **`None` = `(default)` = CLI default.** Empty input → `None`. Never store an empty string.
- **Full suite after every change:** `cargo test`, `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check`.
- **Real-tmux acceptance is MANDATORY before merge** (create-form change — prior create-form changes were only caught RED there). Run `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1`.
- pmtui copy: terse, imperative.

## Current state (verified anchors, post decider-model merge)

- `CreateForm` (`src/bin/pmtui/create_form.rs`): fields `field, engine, worker_model: Option<String>, model_choices: Vec<ModelInfo>, dir: Field, tier, goal: Field, cadence_s, decider_engine, decider_model: Option<String>, decider_model_choices: Vec<ModelInfo>`. Indices: engine=0, `WORKER_MODEL=1`, dir=2, tier=3, `GOAL=4`, `CADENCE=5`, `DECIDER=6`, `DECIDER_MODEL=7`; `FIELDS=8`.
- Today the model fields are toggles: `adjust_worker_model`/`adjust_decider_model` cycle `[(default)] ++ choices`; `adjust()` routes `WORKER_MODEL`/`DECIDER_MODEL` to them; `is_text_field()` returns true for field 2 (dir) or `GOAL` when shown; `text_field()` maps 2→dir, `GOAL`→goal.
- `render/create.rs`: `render_create` at width 80, `overlay_h(10)`; `field_row(idx,label,value)` for toggles, `caret_row(idx,label,&Field)` for text fields (renders the movable caret via `Field::caret_view(cols)`); `worker_model_val`/decider model row built from `model_choices`/`decider_model_choices`.
- `keys.rs handle_create_key`: `^E`→goal editor; Esc→cancel; Enter→submit; then `UiMode::Creating(form)` match — Tab/Down→`next_field`, BackTab/Up→`prev_field`, ←/→ → caret (text field) or `adjust` (toggle), Home/End/Delete/Backspace → text edits, `Char(c)` → `type_char` (text field) or space→`adjust`. After ←/→/space it captures `engine`/`decider_engine` before/after and repopulates `model_choices`/`decider_model_choices` on a flip.
- `Field` (existing text buffer) supports `insert(char)`, `insert_str(&str)`, `backspace`, `delete`, `left`/`right`/`home`/`end`, `as_str()`, `caret_view(cols)`. Construct from a `String`/`&str` via `Field::from(..)`.
- `ModelInfo { label: String, value: String }` (`agent_manager::models`), already imported in `create_form.rs`.

---

### Task 1: `ModelCombo` — the select-or-type field (behavior + keys + unit tests)

**Files:**
- Modify: `src/bin/pmtui/create_form.rs` (add `ModelCombo`; give the two model fields a combo; resolve into `worker_model`/`decider_model`; drop the two `adjust_*_model` toggle fns)
- Modify: `src/bin/pmtui/keys.rs` (`handle_create_key`: route combo keys) + `handle_paste` (create model-field branch)
- Test: `src/bin/pmtui/tests/create_form.rs`

**Interfaces — Produces:** `ModelCombo` (with `new`/`seed`/`filtered`/`list_len`/`cursor`/`move_cursor`/`edit`/`input_mut`/`input_str`/`resolve`); `CreateForm` `worker_combo`/`decider_combo` + `is_model_field`/`focused_combo_mut`/`focused_model_choices`/`combo_move`/`combo_edit`/`sync_model_values`.

- [ ] **Step 1: Write failing combobox unit tests.** In `tests/create_form.rs`:

```rust
fn combo_choices() -> Vec<ModelInfo> {
    vec![
        ModelInfo { label: "claude-opus-4-8[1m]".into(), value: "global.anthropic.claude-opus-4-8[1m]".into() },
        ModelInfo { label: "claude-sonnet-5".into(), value: "global.anthropic.claude-sonnet-5".into() },
        ModelInfo { label: "claude-haiku-4-5".into(), value: "global.anthropic.claude-haiku-4-5".into() },
    ]
}

#[test]
fn combo_arrow_selects_and_resolves_to_value() {
    let ch = combo_choices();
    let mut c = ModelCombo::new();               // empty input, cursor 0 = (default)
    assert_eq!(c.resolve(&ch), None);
    c.move_cursor(1, &ch);                        // -> row 1 = first choice
    assert_eq!(c.input_str(), "claude-opus-4-8[1m]");
    assert_eq!(c.resolve(&ch).as_deref(), Some("global.anthropic.claude-opus-4-8[1m]"));
    c.move_cursor(-1, &ch);                       // back to (default)
    assert_eq!(c.resolve(&ch), None);
}

#[test]
fn combo_typing_filters_and_allows_custom() {
    let ch = combo_choices();
    let mut c = ModelCombo::new();
    for ch_ in "sonnet".chars() { c.edit(|f| f.insert(ch_)); }
    assert_eq!(c.filtered(&ch).len(), 1);        // matches sonnet
    assert_eq!(c.resolve(&ch).as_deref(), Some("global.anthropic.claude-sonnet-5"));
    let mut c2 = ModelCombo::new();
    for ch_ in "claude-opus-4-9".chars() { c2.edit(|f| f.insert(ch_)); }
    assert_eq!(c2.filtered(&ch).len(), 0);       // no catalog match
    assert_eq!(c2.resolve(&ch).as_deref(), Some("claude-opus-4-9")); // custom verbatim
}

#[test]
fn combo_cursor_clamps_and_empty_catalog_is_default_only() {
    let ch: Vec<ModelInfo> = vec![];
    let mut c = ModelCombo::new();
    assert_eq!(c.list_len(&ch), 1);              // just (default)
    c.move_cursor(5, &ch);                        // clamps, stays default
    assert_eq!(c.resolve(&ch), None);
}
```

- [ ] **Step 2: Run → fail to compile** (`ModelCombo` missing).

- [ ] **Step 3: Implement `ModelCombo`** in `create_form.rs`:

```rust
/// A select-or-type model field: a caret-editable `input` (the source of truth) over a
/// filtered list of the engine's models. Row 0 of the list is the implicit `(default)`
/// (= `None`); ↑↓ fill `input` from a row, typing filters / enters a custom model.
#[derive(Clone, Debug, Default)]
pub(crate) struct ModelCombo {
    input: Field,
    cursor: usize,
}

impl ModelCombo {
    pub(crate) fn new() -> Self { Self::default() }
    /// Seed the input from a stored value's LABEL (friendly name), or empty for None. On focus-in.
    pub(crate) fn seed(&mut self, stored: Option<&str>, choices: &[ModelInfo]) {
        let text = match stored {
            None => String::new(),
            Some(v) => choices.iter().find(|m| m.value == v).map(|m| m.label.clone())
                .unwrap_or_else(|| v.to_string()),
        };
        self.input = Field::from(text);
        self.cursor = 0;
    }
    pub(crate) fn input_str(&self) -> &str { self.input.as_str() }
    pub(crate) fn cursor(&self) -> usize { self.cursor }
    /// Choices whose label contains the trimmed input (case-insensitive); all when empty.
    pub(crate) fn filtered<'a>(&self, choices: &'a [ModelInfo]) -> Vec<&'a ModelInfo> {
        let t = self.input.as_str().trim().to_ascii_lowercase();
        if t.is_empty() { choices.iter().collect() }
        else { choices.iter().filter(|m| m.label.to_ascii_lowercase().contains(&t)).collect() }
    }
    /// Displayed rows = 1 (the `(default)` row) + filtered choices.
    pub(crate) fn list_len(&self, choices: &[ModelInfo]) -> usize { 1 + self.filtered(choices).len() }
    /// Move over the displayed list and MIRROR the highlighted row into `input` (row 0 clears it).
    pub(crate) fn move_cursor(&mut self, delta: isize, choices: &[ModelInfo]) {
        let len = self.list_len(choices) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, (len - 1).max(0)) as usize;
        let text = if self.cursor == 0 { String::new() }
                   else { self.filtered(choices)[self.cursor - 1].label.clone() };
        self.input = Field::from(text);
    }
    /// Apply a text edit, then reset the cursor to the top of the refiltered list.
    pub(crate) fn edit(&mut self, f: impl FnOnce(&mut Field)) {
        f(&mut self.input);
        self.cursor = 0;
    }
    /// The launch-ready value: None if empty; a matching catalog `value`; else the typed text.
    pub(crate) fn resolve(&self, choices: &[ModelInfo]) -> Option<String> {
        let t = self.input.as_str().trim();
        if t.is_empty() { return None; }
        if let Some(m) = choices.iter().find(|m| m.label.eq_ignore_ascii_case(t) || m.value == t) {
            return Some(m.value.clone());
        }
        Some(t.to_string())
    }
}
```

- [ ] **Step 4: Give `CreateForm` a combo per model field + keep `worker_model`/`decider_model` in sync.**
  - Add `worker_combo: ModelCombo`, `decider_combo: ModelCombo` (default in `new()`).
  - Remove `adjust_worker_model`/`adjust_decider_model` and their `adjust()` arms; `adjust()` keeps engine(0)/tier(3)/cadence(`CADENCE`)/decider-engine(`DECIDER`).
  - `toggle_engine`: after `worker_model = None`, also `self.worker_combo = ModelCombo::new()`. `toggle_decider_engine`: after `decider_model = None`, also `self.decider_combo = ModelCombo::new()`.
  - Add:
    ```rust
    pub(crate) fn is_model_field(idx: usize) -> bool { idx == Self::WORKER_MODEL || idx == Self::DECIDER_MODEL }
    pub(crate) fn focused_model_choices(&self) -> &[ModelInfo] {
        if self.field == Self::DECIDER_MODEL { &self.decider_model_choices } else { &self.model_choices }
    }
    /// Recompute the stored Option<String> values from the combos (call after any combo change).
    pub(crate) fn sync_model_values(&mut self) {
        self.worker_model = self.worker_combo.resolve(&self.model_choices);
        self.decider_model = self.decider_combo.resolve(&self.decider_model_choices);
    }
    /// ↑↓ over the focused combo's list (no-op on a non-model field).
    pub(crate) fn combo_move(&mut self, delta: isize) {
        match self.field {
            Self::WORKER_MODEL => { let ch = self.model_choices.clone(); self.worker_combo.move_cursor(delta, &ch); }
            Self::DECIDER_MODEL if self.shows_decider() => { let ch = self.decider_model_choices.clone(); self.decider_combo.move_cursor(delta, &ch); }
            _ => return,
        }
        self.sync_model_values();
    }
    /// A caret/typing edit on the focused combo's input (no-op on a non-model field).
    pub(crate) fn combo_edit(&mut self, f: impl FnOnce(&mut Field)) {
        match self.field {
            Self::WORKER_MODEL => self.worker_combo.edit(f),
            Self::DECIDER_MODEL if self.shows_decider() => self.decider_combo.edit(f),
            _ => return,
        }
        self.sync_model_values();
    }
    ```
    (The `.clone()` of the choices in `combo_move` sidesteps the borrow of `self` while calling `&mut self.worker_combo`; the lists are short. If you prefer, split into a small free fn taking `(&mut ModelCombo, &[ModelInfo])`.)
  - In `next_field`/`prev_field`, when the destination `self.field` is a model field, seed its combo from the stored value so it shows the current selection: `if self.field == Self::WORKER_MODEL { let ch = self.model_choices.clone(); self.worker_combo.seed(self.worker_model.clone().as_deref(), &ch); }` and the decider equivalent. (Seeding is idempotent.)

- [ ] **Step 5: Route keys in `keys.rs handle_create_key`.** After the `Esc`/`Enter`/`^E` handling and the `let UiMode::Creating(form) = &mut app.mode else {..}`, FIRST handle the model-combo case:

```rust
    if CreateForm::is_model_field(form.field) && form.shows_field(form.field) {
        match code {
            KeyCode::Up => { form.combo_move(-1); return; }
            KeyCode::Down => { form.combo_move(1); return; }
            KeyCode::Tab => { form.next_field(); return; }
            KeyCode::BackTab => { form.prev_field(); return; }
            KeyCode::Left => { form.combo_edit(|f| f.left()); return; }
            KeyCode::Right => { form.combo_edit(|f| f.right()); return; }
            KeyCode::Home => { form.combo_edit(|f| f.home()); return; }
            KeyCode::End => { form.combo_edit(|f| f.end()); return; }
            KeyCode::Delete => { form.combo_edit(|f| f.delete()); return; }
            KeyCode::Backspace => { form.combo_edit(|f| f.backspace()); return; }
            KeyCode::Char(c) => { form.combo_edit(|f| f.insert(c)); return; }
            _ => {}
        }
    }
```
(Place it so `Tab`/`BackTab`/`Down`/`Up` for a model field take the combo path, not the generic `next_field`/`prev_field`. `^E` is intercepted earlier and does nothing useful on a model field — leave it; it only escalates the Goal.) Keep the existing engine-flip repopulation; after repopulating `model_choices`/`decider_model_choices`, the toggle-reset already cleared the value, so also `form.worker_combo = ModelCombo::new()` / `decider_combo` as appropriate (the flip happens on the Engine/Decider field, not a model field).

- [ ] **Step 5b: Paste into a focused model combo.** In `handle_paste`, add a branch: if `UiMode::Creating(form)` and `CreateForm::is_model_field(form.field)`, `form.combo_edit(|f| f.insert_str(&text))` instead of `form.paste(&text)` (which targets dir/goal).

- [ ] **Step 6: Create-form integration tests** (`tests/create_form.rs`, pre-seed `model_choices`/`decider_model_choices`): focus WORKER_MODEL, `Down` → `form.worker_model == Some(first value)`; type a custom string → `worker_model == Some(that string)`; backspace to empty → `None`; DECIDER_MODEL equivalents on Autopilot; `Tab` off a model field lands on the next shown field; `toggle_engine` clears the worker combo + value.

- [ ] **Step 7: Green + hygiene + commit.** `cargo test`; clippy; fmt. `git commit -am "feat(pmtui): ModelCombo — create-form model fields become select-or-type (behavior + keys)"`

---

### Task 2: Render the bigger popup + expanded list; live render; acceptance; docs

**Files:**
- Modify: `src/bin/pmtui/render/create.rs`
- Modify: `README.md`, `docs/superpowers/specs/2026-08-24-create-form-model-combobox-design.md` (Status)
- Test: `tests/create_form.rs` (TestBackend render), live tmux, acceptance

- [ ] **Step 1: Render the combo.** In `render_create`: width 80→88. Add a `const MODEL_LIST_ROWS: usize = 6;`. Build rows in order (engine, worker-model, dir, tier, goal?, cadence?, decider?, decider-model?), and for each model field:
  - **Focused:** a caret input row (reuse the `caret_row` caret-view logic against the combo's `input` — expose `input_mut`/an `&Field` getter, or add a `caret_row_str` variant that takes `&Field`), then the list block: iterate `[(default)] ++ combo.filtered(choices)`, windowed to `MODEL_LIST_ROWS` keeping `combo.cursor()` visible; each row: `▸ `/`  ` (cursor) + `● `(row's value == the field's stored value)/`○ `(other)/(nothing for the `(default)` row, which shows `(default)`), then the label; a trailing dim `  … {n} more` when the filtered list exceeds the window.
  - **Unfocused:** one `field_row(idx, label, summary)` where summary is `"< (default) >"` or `"< label >"` (stored value looked up in the field's choices, raw-value fallback) — today's behavior.
  - Height: `rows_shown` counts non-model shown fields (1 each) + each shown model field (1 if unfocused, `1 + min(list_len, MODEL_LIST_ROWS) + overflow_note?` if focused). `overlay_h(rows_shown + slack)`, clamped to `≤ area.height * 3/4`, keeping the existing ≥1 slack for the wrapped Autonomy row.
- [ ] **Step 2: TestBackend render tests:** focused Worker Model shows the input + `(default)` + ≥1 model label + `▸`/`●`; unfocused Decider Model shows its summary; a tiny-size sweep (1×1 … 200×50) does not panic; the popup is taller when a model field is focused than when Directory is focused.
- [ ] **Step 3: `cargo build` + live tmux** (pmtui-ui-testing skill; scratch sockets only — never `--socket pmd`, never the real registry; seed `$SCRATCH/.project-state/config.json` autopilot so the decider fields show): open `n`, Tab to Model, capture — the list shows in the bigger popup; `Down` selects (fills input, moves `▸`/`●`); typing filters and accepts a custom string; Tab to Decider Model shows its list. Report the frames; clean up both scratch servers + dir.
- [ ] **Step 4: Real-tmux acceptance (MANDATORY):** `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 2>&1 | tail -30`. Fix any form-driving test that relied on ←/→ cycling a model toggle or ↑↓ moving fields while a model field was focused — reroute to the new scheme (Tab moves fields; the model field takes ↑↓/typing), do NOT weaken assertions. Report pass/fail.
- [ ] **Step 5: Docs + Status.** README: the create form's model fields are now type-or-select (filter/custom) in a larger popup. Flip the design-doc Status to "implemented @ feat/create-form-combobox".
- [ ] **Step 6: Commit.** `git commit -am "feat(pmtui): render the create-form model combobox in a bigger popup; docs"`

---

## Self-Review

**1. Spec coverage:** inline combobox (focused expands, other summarizes) → T1 behavior + T2 render; input-is-truth / ↑↓-fills → `move_cursor`/`edit`; resolve-to-Option → `resolve` + `sync_model_values`; Tab-owns-fields / ↑↓-owns-list / ←→-caret → T1 Step 5 keys; bigger popup + grow-on-focus → T2 Step 1; no schema/launch/discovery change → reuses the existing fields untouched; mandatory live render + acceptance → T2 Steps 3-4. Out-of-scope (the `e`/`w` picker, other fields) → untouched.

**2. Placeholder scan:** no TBD; `ModelCombo` is complete code; render/keys give exact reuse points (`caret_row`, `field_row`, the engine-flip repopulation) and the load-bearing test cases as code; the one borrow subtlety (clone the short choices list in `combo_move`) is called out with an alternative.

**3. Type consistency:** `ModelCombo` (`new`/`seed`/`filtered`/`list_len`/`cursor`/`move_cursor`/`edit`/`input_str`/`resolve`), `CreateForm.worker_combo`/`decider_combo`, `is_model_field`/`focused_model_choices`/`combo_move`/`combo_edit`/`sync_model_values`, `MODEL_LIST_ROWS`, and the reused `worker_model`/`decider_model`/`model_choices`/`decider_model_choices`/`ModelInfo` are spelled identically across tasks.
