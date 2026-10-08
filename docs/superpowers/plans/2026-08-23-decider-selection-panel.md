# Decider Selection Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `e`-key decider-engine *cycle* with a single-select *panel* the user opens, navigates, and commits — structured so a decider-model section can be added later.

**Architecture:** `e` on an autopilot row opens a new modal overlay `UiMode::DeciderPicker { id, current, cursor }` (seeded from `config.json`). ↑↓/jk move the cursor over `Engine::ALL`; Enter commits the cursored engine to `config.json` (read-modify-write, config-only — never the ledger); Esc cancels. On a Standard row `e` refuses and names `m`, exactly as the cycle did. Engine now; the overlay is a labeled single-select list so a model section can be grafted on later with no new machinery.

**Tech Stack:** Rust, ratatui 0.30.2 (TestBackend for unit render), crossterm, serde/serde_json, tmux (`Driver` seam), real-tmux acceptance harness.

## Global Constraints

- **Single-writer ledger.** pmtui writes ONLY `config.json` here; `state.json` (the `AgentLoopState` ledger) is pmd's alone and MUST be byte-identical after any `e` action.
- **Autopilot-only.** The decider consult never runs on a row pmd does not drive, so `e` opens the panel ONLY on an autopilot row; on Standard it refuses with a status that names `m`. (Same rule as `g`/`c`/`i`.)
- **Default Claude, safe by construction.** `config.decider_engine` defaults to `Engine::Claude` (`#[serde(default)]`); this change does not touch that default or the always-escalate floor.
- **Full suite after every change.** `cargo test` (lib + bins + integration + doc-tests), `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check` (clean). Spawned `claude`/`codex` children need `ECC_GATEGUARD=off`.
- **Live-tmux acceptance before merge.** `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1` — a green `cargo test` is NOT proof; the panel is new geometry TestBackend cannot prove lands in-frame.
- **pmtui copy conventions.** Terse rule, imperative; no narration of harness internals.

## File Structure

- `src/registry.rs` — add `Engine::ALL: [Engine; 2]` (the one stable engine list all three surfaces index).
- `src/bin/pmtui/mode.rs` — add the `UiMode::DeciderPicker { id, current, cursor }` variant.
- `src/bin/pmtui/app/autopilot.rs` — replace `cycle_decider_engine` with `begin_decider_pick` + `commit_decider_pick`.
- `src/bin/pmtui/keys.rs` — Normal `e` → `begin_decider_pick`; new `DeciderPicker` handler block.
- `src/bin/pmtui/render/decider_picker.rs` — **new** — `render_decider_picker` (the overlay geometry).
- `src/bin/pmtui/render/mod.rs` — `mod`/`use` the new module; overlay-match arm.
- `src/bin/pmtui/render/keybar.rs` — map `UiMode::DeciderPicker` → `Scope::DeciderPicker`.
- `src/bin/pmtui/bindings.rs` — add `Scope::DeciderPicker` + its 3 rows; reword the `e` help.
- `src/bin/pmtui/tests/autopilot.rs` — rewrite the two `e_*` tests for the panel flow; add cursor/esc/commit-current tests.
- `README.md` / `docs/SPEC.md` — reword the `e` UX from "switch/toggle" to "opens a panel"; SPEC schema/safety unchanged.

**Note for the implementer — verified anchors (as of this plan):** `cycle_decider_engine` is at `src/bin/pmtui/app/autopilot.rs:158`; the Normal `e` dispatch is `keys.rs:390`; the keybar `other` match is `keybar.rs:214`; the render overlay match is `render/mod.rs:137` (its no-op arm is the explicit set `UiMode::Normal | UiMode::WakeView { .. } | UiMode::Decisions { .. } => {}`, NOT a `_` wildcard — add a real arm). `Engine` is in scope in every pmtui bin module via `use crate::*`. `Scope` is compared by `==` (never matched exhaustively), so adding a variant needs no other edits.

---

### Task 1: The decider picker (model, behavior, keys, render, bindings) + unit tests

**Files:**
- Modify: `src/registry.rs` (add `Engine::ALL`)
- Modify: `src/bin/pmtui/mode.rs` (add variant)
- Modify: `src/bin/pmtui/app/autopilot.rs:153-196` (replace `cycle_decider_engine`)
- Modify: `src/bin/pmtui/keys.rs:390` (dispatch) + new handler block
- Create: `src/bin/pmtui/render/decider_picker.rs`
- Modify: `src/bin/pmtui/render/mod.rs` (mod/use + overlay arm)
- Modify: `src/bin/pmtui/render/keybar.rs:214-230` (scope arm)
- Modify: `src/bin/pmtui/bindings.rs` (Scope variant, e-row help, 3 picker rows)
- Test: `src/bin/pmtui/tests/autopilot.rs:433-498` (rewrite) + new render smoke test

**Interfaces:**
- Consumes: `Registry::load`, `entry_state_paths`, `state::read_json::<Config>`, `state::write_json_atomic`, `Config { decider_engine, autonomy }`, `Tier::Autopilot`, `Engine::{Claude,Codex,label}`, render helpers `draw_overlay_frame`, `overlay_h`, `truncate`.
- Produces:
  - `Engine::ALL: [Engine; 2]` (== `[Engine::Claude, Engine::Codex]`).
  - `UiMode::DeciderPicker { id: String, current: Engine, cursor: usize }`.
  - `App::begin_decider_pick(&mut self)`, `App::commit_decider_pick(&mut self)`.
  - `render_decider_picker(f: &mut Frame, area: Rect, id: &str, current: Engine, cursor: usize)`.
  - `Scope::DeciderPicker`.

- [ ] **Step 1: Rewrite the two `e_*` tests + add the new ones (failing).**

Replace the block at `src/bin/pmtui/tests/autopilot.rs:433-498` (the `--- e: flip ...` section, both tests) with:

```rust
// --- `e`: OPEN the decider picker, navigate, commit (config-only, autopilot-gated) ---

#[test]
fn e_opens_the_decider_picker_seeded_to_current() {
    // `e` on an AUTOPILOT row OPENS the picker seeded to the engine on disk; it writes NOTHING
    // yet (the commit does). The seed's engine is Claude, so the cursor lands on index 0.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let session_paths = ProjectPaths::for_session(&root, "bot");
    let cfg_before = std::fs::read(session_paths.config()).unwrap();

    let mut app = loop_app(&reg_path);
    app.begin_decider_pick();
    match &app.mode {
        UiMode::DeciderPicker { id, current, cursor } => {
            assert_eq!(id, "bot");
            assert_eq!(*current, Engine::Claude);
            assert_eq!(*cursor, 0, "cursor seeds on the current engine");
        }
        m => panic!("e must open the picker, got {m:?}"),
    }
    assert_eq!(
        std::fs::read(session_paths.config()).unwrap(),
        cfg_before,
        "opening the picker writes nothing"
    );
}

#[test]
fn decider_picker_commit_writes_chosen_engine_config_only() {
    // Move the cursor to codex and commit: config.decider_engine flips to Codex, the mode returns
    // to Normal, the status names the decider, and the ledger (state.json) is byte-identical
    // (single-writer invariant).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let session_paths = ProjectPaths::for_session(&root, "bot");
    let ledger_before = std::fs::read(session_paths.pmstate()).unwrap();

    let mut app = loop_app(&reg_path);
    app.begin_decider_pick();
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // cursor: claude -> codex
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // commit
    assert!(matches!(app.mode, UiMode::Normal), "commit closes the picker");
    let cfg: Config = state::read_json(&session_paths.config()).unwrap();
    assert_eq!(cfg.decider_engine, Engine::Codex, "{}", app.status);
    assert!(app.status.contains("decider"), "status names the change: {}", app.status);
    assert_eq!(
        std::fs::read(session_paths.pmstate()).unwrap(),
        ledger_before,
        "commit_decider_pick must not touch the ledger"
    );
}

#[test]
fn decider_picker_commit_on_current_leaves_the_engine() {
    // Enter with the cursor still on the current engine is a harmless same-value write: engine
    // unchanged, mode Normal.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let session_paths = ProjectPaths::for_session(&root, "bot");

    let mut app = loop_app(&reg_path);
    app.begin_decider_pick();
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    let cfg: Config = state::read_json(&session_paths.config()).unwrap();
    assert_eq!(cfg.decider_engine, Engine::Claude);
}

#[test]
fn decider_picker_esc_cancels_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let session_paths = ProjectPaths::for_session(&root, "bot");
    let cfg_before = std::fs::read(session_paths.config()).unwrap();

    let mut app = loop_app(&reg_path);
    app.begin_decider_pick();
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // move, then bail
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal), "esc closes the picker");
    assert!(app.status.contains("unchanged"), "status says unchanged: {}", app.status);
    assert_eq!(
        std::fs::read(session_paths.config()).unwrap(),
        cfg_before,
        "esc writes nothing"
    );
}

#[test]
fn decider_picker_cursor_clamps_at_both_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    app.begin_decider_pick();
    // Up at the top stays 0.
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::DeciderPicker { cursor: 0, .. }));
    // Down twice cannot exceed Engine::ALL.len() - 1 (== 1).
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::DeciderPicker { cursor: 1, .. }));
}

#[test]
fn e_refuses_on_a_standard_row_and_points_at_m() {
    // AUTOPILOT-ONLY: the decider never runs on a Standard row, so `e` refuses (naming `m`),
    // stays in Normal, and writes nothing.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // seeds Standard
    let session_paths = ProjectPaths::for_session(&root, "bot");

    let mut app = loop_app(&reg_path);
    app.begin_decider_pick();
    assert!(matches!(app.mode, UiMode::Normal), "no picker on a Standard row");
    assert!(app.status.contains('m'), "status must name the m key; got {:?}", app.status);
    let cfg: Config = state::read_json(&session_paths.config()).unwrap();
    assert_eq!(cfg.decider_engine, Engine::Claude, "a refused open must not change the engine");
    assert_eq!(cfg.autonomy, Tier::Standard, "and must not write the tier");
}
```

- [ ] **Step 2: Run the tests to verify they fail to COMPILE (methods/variant not defined yet).**

Run: `cargo test --bin pmtui e_opens_the_decider_picker 2>&1 | tail -20`
Expected: compile error — `no method named begin_decider_pick`, `no variant DeciderPicker`.

- [ ] **Step 3: Add `Engine::ALL` to `src/registry.rs`.**

In `impl Engine` (just above `fn bin`), add:

```rust
    /// Every engine, in a STABLE order — the one source the create-form toggle, the decider
    /// picker overlay, and the picker cursor all index into, so they cannot disagree about
    /// which engines exist or in what order.
    pub const ALL: [Engine; 2] = [Engine::Claude, Engine::Codex];
```

- [ ] **Step 4: Add the `UiMode::DeciderPicker` variant to `src/bin/pmtui/mode.rs`.**

After the `Decisions { .. }` variant (before the closing `}` of the enum), add:

```rust
    /// SELECT the decider engine for the selected AUTOPILOT session (`e`) — a single-select
    /// list overlay, the replacement for the old blind cycle. Opened by
    /// [`App::begin_decider_pick`] only on a row pmd drives (Standard refuses, naming `m`) and
    /// committed by [`App::commit_decider_pick`], which writes ONLY `config.json`. Structured as
    /// a labeled list so a decider-MODEL section can be added later with no new machinery.
    DeciderPicker {
        /// The session PINNED at open, so a background refresh cannot swap the target under the
        /// overlay. Used for the write and the title.
        id: String,
        /// The engine on disk when the overlay opened — drawn with the `●` "current" marker.
        /// Read ONCE at open; the render path does no file I/O.
        current: Engine,
        /// Which option the cursor is on, an index into [`Engine::ALL`]. Clamped at use.
        cursor: usize,
    },
```

- [ ] **Step 5: Replace `cycle_decider_engine` in `src/bin/pmtui/app/autopilot.rs`.**

Replace the whole `cycle_decider_engine` method (its doc comment + body, `autopilot.rs:153-196`) with:

```rust
    /// `e`: OPEN the decider-engine picker for the SELECTED session — a single-select list
    /// (claude ⇄ codex), replacing the old blind cycle. AUTOPILOT-ONLY, like `g`/`c`/`i`: the
    /// decider consult never runs on a row pmd does not drive, so choosing its engine on Standard
    /// would configure something inert — refuse and point at `m`. Nothing is written here;
    /// [`commit_decider_pick`] does the one `config.json` write.
    pub(crate) fn begin_decider_pick(&mut self) {
        let Some(id) = self.selected_view().map(|v| v.id.clone()) else {
            self.status = "e switches a session's decider engine (nothing is selected)".into();
            return;
        };
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        let paths = entry_state_paths(entry);
        match state::read_json::<Config>(&paths.config()) {
            Ok(c) => {
                if c.autonomy != Tier::Autopilot {
                    self.status = format!(
                        "{id}: the decider only runs on autopilot — press m to turn it on; engine unchanged"
                    );
                    self.refresh();
                    return;
                }
                let cursor = Engine::ALL
                    .iter()
                    .position(|&e| e == c.decider_engine)
                    .unwrap_or(0);
                self.mode = UiMode::DeciderPicker {
                    id,
                    current: c.decider_engine,
                    cursor,
                };
            }
            Err(e) => {
                self.status = format!("{id}: config unreadable ({e}) — decider engine unchanged");
                self.refresh();
            }
        }
    }

    /// Commit the picker's cursored engine to `config.json` (read-modify-write, so no other config
    /// field is clobbered) and close to Normal. Writes ONLY config.json — never the ledger
    /// (single-writer). No autopilot re-check: the overlay owns the keyboard while it is up and
    /// config.json is pmtui's alone, so the tier cannot change between open and commit. Choosing the
    /// already-current engine is a harmless rewrite of the same value.
    pub(crate) fn commit_decider_pick(&mut self) {
        let UiMode::DeciderPicker { id, cursor, .. } = &self.mode else {
            return;
        };
        let (id, chosen) = (
            id.clone(),
            Engine::ALL[(*cursor).min(Engine::ALL.len() - 1)],
        );
        self.mode = UiMode::Normal;
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        let cfg_path = entry_state_paths(entry).config();
        match state::read_json::<Config>(&cfg_path) {
            Ok(mut c) => {
                c.decider_engine = chosen;
                match state::write_json_atomic(&cfg_path, &c) {
                    Ok(()) => self.status = format!("{id} → decider: {}", chosen.label()),
                    Err(e) => {
                        self.status = format!("{id}: could not set the decider engine: {e}")
                    }
                }
            }
            Err(e) => {
                self.status = format!("{id}: config unreadable ({e}) — decider engine unchanged")
            }
        }
        self.refresh();
    }
```

- [ ] **Step 6: Wire the keys in `src/bin/pmtui/keys.rs`.**

(a) Change the Normal dispatch at `keys.rs:390` from `app.cycle_decider_engine()` to open the picker, and update the comment:

```rust
        // `e` — OPEN the decider-engine picker (claude ⇄ codex) for the selected session.
        // Autopilot-only (like `g`/`c`/`i`): the handler refuses on a row pmd does not drive and
        // names `m`, since the chip is hidden there but the key still fires.
        KeyCode::Char('e') => app.begin_decider_pick(),
```

(b) Add a handler block for the picker. Place it immediately BEFORE the `if matches!(app.mode, UiMode::Help { .. })` block (~keys.rs:316):

```rust
    // The DECIDER PICKER (`e`): a single-select list, not a text field. Enter commits the cursored
    // engine, Esc cancels, ↑↓/jk move the cursor (clamped to Engine::ALL). Reached from Normal
    // only, so closing always means Normal.
    if matches!(app.mode, UiMode::DeciderPicker { .. }) {
        match code {
            KeyCode::Enter => app.commit_decider_pick(),
            KeyCode::Esc => {
                app.mode = UiMode::Normal;
                app.status = "decider engine unchanged".into();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let UiMode::DeciderPicker { cursor, .. } = &mut app.mode {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let UiMode::DeciderPicker { cursor, .. } = &mut app.mode {
                    *cursor = (*cursor + 1).min(Engine::ALL.len() - 1);
                }
            }
            _ => {}
        }
        return;
    }
```

- [ ] **Step 7: Create the renderer `src/bin/pmtui/render/decider_picker.rs`.**

```rust
//! The `e` decider picker: a single-select list of decider engines over the dashboard. Its own
//! file, one-per-overlay like `sending`/`confirm`, because its geometry (a fixed header line plus
//! one row per engine) is its own. Engine now; a decider-MODEL section can be added here later
//! without new machinery.

use crate::*;

/// Decider-engine picker (`e`) — a fixed-height single-select list. `current` draws the `●` marker
/// (the engine on disk); `cursor` draws the `▸` focus, indexing [`Engine::ALL`].
pub(crate) fn render_decider_picker(
    f: &mut Frame,
    area: Rect,
    id: &str,
    current: Engine,
    cursor: usize,
) {
    // prompt line + blank + one row per engine.
    let inner = draw_overlay_frame(
        f,
        area,
        60,
        overlay_h(2 + Engine::ALL.len()),
        &format!("Decider \u{b7} {}", truncate(id, 24)),
        Color::Cyan,
        "\u{2191}\u{2193} move \u{b7} enter select \u{b7} esc cancel",
    );
    let text_w = usize::from(inner.width);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut body = vec![
        Line::styled(
            truncate("Which engine answers low-stakes decisions?", text_w),
            dim,
        ),
        Line::raw(""),
    ];
    let picked = cursor.min(Engine::ALL.len().saturating_sub(1));
    for (i, eng) in Engine::ALL.iter().enumerate() {
        let on = i == picked;
        let is_current = *eng == current;
        // ▸ = the movable cursor (teaches the interaction); ● / ○ = the engine on disk vs not.
        let focus = if on { "\u{25b8} " } else { "  " };
        let mark = if is_current { "\u{25cf}" } else { "\u{25cb}" };
        let suffix = if is_current { "  (current)" } else { "" };
        let label = format!("{focus}{mark} {}{suffix}", eng.label());
        // Bold cyan for the cursored row — the same "this is focused" language the answer overlay's
        // option cursor uses, not a reversed fill (this overlay is already a raised surface).
        body.push(Line::styled(
            truncate(&label, text_w),
            if on {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ));
    }
    f.render_widget(Paragraph::new(Text::from(body)), inner);
}
```

- [ ] **Step 8: Register the module + overlay arm in `src/bin/pmtui/render/mod.rs`.**

(a) Add `mod decider_picker;` in the `mod` list (after `mod decisions;`) and `pub(crate) use decider_picker::*;` in the re-export list (after `pub(crate) use decisions::*;`).

(b) In the overlay `match &app.mode` at `render/mod.rs:137`, add an arm (before the `UiMode::Normal | UiMode::WakeView { .. } | UiMode::Decisions { .. } => {}` arm):

```rust
        UiMode::DeciderPicker { id, current, cursor } => {
            render_decider_picker(f, area, id, *current, *cursor)
        }
```

- [ ] **Step 9: Map the mode → scope in `src/bin/pmtui/render/keybar.rs`.**

In the `other` match (`keybar.rs:214`), add after the `UiMode::Sending { .. } => Scope::Send,` arm:

```rust
                UiMode::DeciderPicker { .. } => Scope::DeciderPicker,
```

- [ ] **Step 10: Add the bindings in `src/bin/pmtui/bindings.rs`.**

(a) Add a `DeciderPicker` variant to the `Scope` enum (after `Send`):

```rust
    /// The decider-engine picker (`e`).
    DeciderPicker,
```

(b) Reword the `e` row's `help` (currently "Switch an autopilot session's decider engine (claude ⇄ codex)"):

```rust
        help: "Choose an autopilot session's decider engine — opens a panel (claude ⇄ codex)",
```

(c) Add the picker's own bar rows. Insert after the `Scope::Send` block (after its `Esc`/`Cancel` row, ~bindings.rs:539):

```rust
    // The decider picker's own bar. `Esc` carries no `help` (documented once, by the answer
    // overlay's Esc row); Enter and the move keys are unique to this list.
    Binding {
        key: "Enter",
        label: "Select",
        help: "Decider picker: select the highlighted engine",
        group: KeyGroup::Overlays,
        scope: Scope::DeciderPicker,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "↑↓/jk",
        label: "Move",
        help: "Decider picker: move between engines",
        group: KeyGroup::Overlays,
        scope: Scope::DeciderPicker,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "Esc",
        label: "Cancel",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::DeciderPicker,
        applies: Applies::Always,
        rank: 3,
    },
```

- [ ] **Step 11: Add a TestBackend render smoke test in `src/bin/pmtui/tests/autopilot.rs`.**

Append after the `e_*` tests from Step 1:

```rust
#[test]
fn renders_the_decider_picker_with_marks_and_cursor() {
    // The panel renders the title, both engines, and the current marker. (Landing in-frame is
    // proven by the live-tmux acceptance in Task 2; this is the cheap structural check.)
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    app.begin_decider_pick();
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // cursor -> codex

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render picker");
    let screen = screen_text(&terminal);
    for s in ["Decider", "bot", "claude", "codex", "(current)"] {
        assert!(screen.contains(s), "picker missing {s:?}: {screen}");
    }
}

#[test]
fn renders_decider_picker_at_tiny_sizes_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    app.begin_decider_pick();
    for (w, h) in [(1u16, 1u16), (20, 5), (40, 10), (100, 30), (200, 50)] {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, &app))
            .unwrap_or_else(|e| panic!("render picker {w}x{h}: {e}"));
    }
}
```

(If `Terminal`/`TestBackend`/`screen_text`/`render` are not already imported in this test file, copy the `use` lines the neighboring render tests use — e.g. from `tests/help.rs`, which uses all four.)

- [ ] **Step 12: Run the full suite and fix to green.**

Run:
```bash
cargo test 2>&1 | tail -30
cargo clippy --all-targets 2>&1 | tail -20
cargo fmt --all -- --check
```
Expected: all pass; `every_bound_normal_key_is_documented_in_the_help` still green (`e` stays Normal-bound with help); the new `e_*`/render tests pass.

- [ ] **Step 13: Commit.**

```bash
git add src/registry.rs src/bin/pmtui/mode.rs src/bin/pmtui/app/autopilot.rs \
        src/bin/pmtui/keys.rs src/bin/pmtui/render/decider_picker.rs \
        src/bin/pmtui/render/mod.rs src/bin/pmtui/render/keybar.rs \
        src/bin/pmtui/bindings.rs src/bin/pmtui/tests/autopilot.rs
git commit -m "feat(pmtui): decider engine picker — e opens a select-list, not a cycle"
```

---

### Task 2: Live-tmux render acceptance + docs + spec status

**Files:**
- Modify: `README.md:341` (the `<details>` decider block)
- Modify: `docs/superpowers/specs/2026-08-23-decider-selection-panel-design.md` (flip Status)
- (No `docs/SPEC.md` change — see Step 3.)

**Interfaces:**
- Consumes: the binary from Task 1; the `pmtui-ui-testing` skill's scratch-tmux procedure.
- Produces: an empirical confirmation the panel renders, the cursor moves, and Enter commits; corrected user docs.

- [ ] **Step 1: Build the real binary and drive the panel on scratch tmux (per the `pmtui-ui-testing` skill).**

```bash
cargo build
```
Then follow the skill: seed a scratch registry with an **autopilot** agent-loop row (seed `.project-state/config.json` with `autonomy: autopilot` so `e` is live), launch pmtui on `tmux -L pmtui-test` with `--socket pmtuitest-inner` and `ECC_GATEGUARD=off`, then:
```bash
tmux -L pmtui-test send-keys -t ui e ; sleep 0.5
tmux -L pmtui-test capture-pane -p -t ui            # panel: "Decider · <id>", ● claude (current), ○ codex
tmux -L pmtui-test send-keys -t ui Down ; sleep 0.3
tmux -L pmtui-test capture-pane -e -p -t ui | cat -v # cursor ▸ + bold on codex
tmux -L pmtui-test send-keys -t ui Enter ; sleep 0.5
tmux -L pmtui-test capture-pane -p -t ui            # back on dashboard, status "→ decider: codex"
```
Expected: the panel lands in-frame with both engines and the `(current)` marker; Down moves the `▸` cursor and bolds codex; Enter closes and the status names the change. Clean up BOTH scratch servers (`tmux -L pmtuitest-inner kill-server`, `tmux -L pmtui-test kill-server`) and the scratch dir; do NOT touch the `pmd` socket or the user's real registry.

- [ ] **Step 2: Run the `#[ignore]`d real-tmux acceptance suite.**

Run: `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 2>&1 | tail -30`
Expected: PASS (~8 min). Assert the OUTPUT, not just the exit code.

- [ ] **Step 3: Update the user docs.**

(a) `README.md` — replace the `<details>` body at line ~341 (currently: "press **`e`** to switch that session's decider to `codex` (or back)…") with:

```markdown
**Claude** by default. On an **autopilot** row, press **`e`** to open a small **panel** and pick that
session's decider engine (`claude` or `codex`); ↑↓ move, Enter selects, Esc cancels. It writes only
`config.json`, so it takes effect on the next decision, no restart. Whatever the engine, a reply that
isn't a clean verdict escalates to you, so the always-escalate floor holds either way. (`e` on a
`standard` row refuses and points you at `m`.)
```

The key-table row at `README.md:125` ("switch the decider engine (autopilot)") is still accurate — leave it.

(b) `docs/SPEC.md` — confirm no change is needed:
```bash
grep -in "cycle\|toggle" docs/SPEC.md | grep -i decider
```
Expected: no line describes the `e` key as a cycle/toggle (SPEC §10 documents `config.decider_engine` + safety, not the key's UX). If the grep is empty, make NO SPEC edit. If it finds cycle/toggle wording, reword it to "opens a panel to select" to match README.

- [ ] **Step 4: Flip the spec Status.**

In `docs/superpowers/specs/2026-08-23-decider-selection-panel-design.md`, change the header line `**Status:** design approved; implementation not started` to `**Status:** implemented @ feat/decider-selection-panel`.

- [ ] **Step 5: Commit.**

```bash
git add README.md docs/superpowers/specs/2026-08-23-decider-selection-panel-design.md
# add docs/SPEC.md ONLY if Step 3(b) required an edit
git commit -m "docs(decider-panel): README + spec status — e opens a panel to pick the decider engine"
```

---

## Self-Review

**1. Spec coverage** — every spec section maps to a task:
- "reuse `e` to OPEN a panel" → Task 1 Step 6a. "single-select list overlay" → variant + renderer (Steps 4, 7). "Engine now, model-ready" → `Engine::ALL` + labeled-list renderer (Steps 3, 7). "Autopilot-only, refuse & name m" → `begin_decider_pick` gate + test (Steps 5, 1). "Single-writer preserved" → config-only write + ledger-byte test (Steps 5, 1). "Code changes" list (mode/autopilot/keys/render/bindings) → Steps 4–10. "Create form unchanged" → not touched (confirmed by omission). "Testing" (FakeDriver + live render + basics) → Task 1 Steps 1/11/12, Task 2 Steps 1–2. "Out of scope" (model, create-form toggle, send-latency) → not in any task.

**2. Placeholder scan** — no TBD/TODO; every code step carries the actual code; the one conditional (SPEC edit) is gated on a concrete grep with a defined outcome either way.

**3. Type consistency** — `begin_decider_pick`/`commit_decider_pick`/`render_decider_picker`/`Engine::ALL`/`Scope::DeciderPicker`/`UiMode::DeciderPicker { id, current, cursor }` are spelled identically across Steps 3–11 and the Interfaces blocks; `Engine::ALL.len()` used consistently for cursor clamping in keys.rs, the picker, and the commit.
