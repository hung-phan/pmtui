//! The `n` create-a-session form: its fields, the Autonomy dial that decides which
//! of them are even shown, and the Tab/←→/typing rules that move around it. One unit
//! because every field rule reads `shows_field`, so a hidden field can never be
//! focused.

use crate::*;

/// The `n` create-a-session form. A single-screen intake interview: the human
/// tabs through the fields, sets the one **Autonomy** dial (`tier`:
/// Standard/Autopilot), and submits. Every session seeds a
/// `Mode::AgentLoop` session; goal is required, and cadence is the
/// per-session heartbeat.
#[derive(Clone, Debug)]
pub(crate) struct CreateForm {
    /// True when Task Board opened this form. Changes presentation/defaults,
    /// not the managed-session creation contract.
    pub(crate) task_mode: bool,
    // Engine, worker model, directory, name, autonomy, intent, cadence, decider,
    // and decider model. Named indices below keep navigation and rendering aligned.
    pub(crate) field: usize,
    pub(crate) engine: Engine,
    /// The WORKER model chosen at create (field 1), or `None` for the engine's own default (the
    /// `(default)` row). When set it is a catalog `value`, passed verbatim as `--model` and seeded
    /// onto the new `ProjectEntry.worker_model`. The list is per-engine, so [`CreateForm::toggle_engine`]
    /// clears this back to `None`.
    pub(crate) worker_model: Option<String>,
    /// The models the current [`CreateForm::engine`] offers — filled by the App (`models_for`) when
    /// the form opens and on every engine toggle, so the render and the Worker Model toggle read the
    /// catalog with no render-time discovery I/O. The `(default)` row is implicit (cursor 0).
    pub(crate) model_choices: Vec<ModelInfo>,
    pub(crate) dir: Field,
    /// Optional human-readable display name. The generated stable id still owns runtime state.
    pub(crate) name: Field,
    /// The one Autonomy dial: Standard (YOU drive the session — pmd does not touch it;
    /// the default) ↔ Autopilot (the harness drives it). Read by the engine's stop router
    /// (`policy::decide_kind`) AND by the daemon's drive gate
    /// (`agent_manager::daemon::pmd_drives_row`).
    pub(crate) tier: Tier,
    /// Shared intent buffer: optional launch Message on Standard, required Goal on Autopilot.
    pub(crate) goal: Field,
    /// Per-session heartbeat cadence in seconds → the ledger's `cadence_s` + the
    /// registry entry's `cadence_s`. Default is the harness
    /// [`job_engine::DEFAULT_CADENCE_S`]; edited via ←/→ on the Cadence field.
    pub(crate) cadence_s: u64,
    /// The engine the DECIDER runs on, autopilot-only (only meaningful when pmd drives) — seeds
    /// `config.decider_engine`. A toggle (Claude ↔ Codex, default Claude) edited via ←/→ on the
    /// Decider field; SHOWN only on Autopilot (see [`CreateForm::shows_decider`]).
    pub(crate) decider_engine: Engine,
    /// The DECIDER model chosen at create (field 7), or `None` for the decider engine's own default
    /// (the `(default)` row). When set it is a catalog `value`, seeded onto the new session's
    /// `config.decider_model`. Autopilot-only, exactly like [`CreateForm::decider_engine`] — a
    /// decider model on a Standard row configures a consult that never runs. The list is
    /// per-decider-engine, so [`CreateForm::toggle_decider_engine`] clears this back to `None`.
    pub(crate) decider_model: Option<String>,
    /// The models the current [`CreateForm::decider_engine`] offers — filled by the App
    /// (`models_for`) when the form opens and on every decider-engine toggle, so the render and the
    /// Decider Model toggle read the catalog with no render-time discovery I/O. The `(default)` row
    /// is implicit (cursor 0).
    pub(crate) decider_model_choices: Vec<ModelInfo>,
}

/// Step a model field's stored value over the implicit list `[(default)] ++ choices`: index 0 is
/// `(default)` (= `None`), index `i` is `choices[i-1]`. The current index is derived from the
/// stored value (a value absent from the catalog — e.g. right after an engine flip — counts as
/// `(default)`), stepped ±1 modulo `choices.len() + 1`. Returns `None` at index 0, else the
/// landed model's launch-ready `value`. Pure; the create form's Worker/Decider Model ←→ handler.
pub(crate) fn step_model(
    current: Option<&str>,
    choices: &[ModelInfo],
    forward: bool,
) -> Option<String> {
    let len = choices.len() + 1; // +1 for the (default) row
    let cur = match current {
        None => 0,
        Some(v) => choices
            .iter()
            .position(|m| m.value == v)
            .map(|i| i + 1)
            .unwrap_or(0),
    };
    let next = if forward {
        (cur + 1) % len
    } else {
        (cur + len - 1) % len
    };
    if next == 0 {
        None
    } else {
        Some(choices[next - 1].value.clone())
    }
}

impl CreateForm {
    pub(crate) const FIELDS: usize = 9;
    pub(crate) const ENGINE: usize = 0;
    /// Field index of the Worker Model toggle (right after Engine — a worker always launches).
    pub(crate) const WORKER_MODEL: usize = 1;
    pub(crate) const DIRECTORY: usize = 2;
    pub(crate) const NAME: usize = 3;
    pub(crate) const AUTONOMY: usize = 4;
    /// Field index of the Goal/brief text field — where inline typing lands and
    /// where Ctrl+E opens the external editor.
    pub(crate) const GOAL: usize = 5;
    /// Field index of the Cadence stepper.
    pub(crate) const CADENCE: usize = 6;
    /// Field index of the Decider engine toggle.
    pub(crate) const DECIDER: usize = 7;
    /// Field index of the Decider Model toggle — APPENDED after Decider, so no existing index moved.
    /// Autopilot-only, exactly like [`CreateForm::DECIDER`].
    pub(crate) const DECIDER_MODEL: usize = 8;
    pub(crate) fn new() -> Self {
        CreateForm {
            task_mode: false,
            field: Self::GOAL,
            engine: Engine::Claude,
            worker_model: None,
            model_choices: Vec::new(),
            dir: Field::from(
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            name: Field::new(),
            // Standard is the sensible landing for the Autonomy dial: a brand-new
            // session is yours to steer until you hand it to the harness with `m`.
            tier: Tier::Standard,
            goal: Field::new(),
            cadence_s: job_engine::DEFAULT_CADENCE_S,
            decider_engine: Engine::Claude,
            decider_model: None,
            decider_model_choices: Vec::new(),
        }
    }
    /// Is the Cadence field part of this form right now?
    ///
    /// AUTOPILOT ONLY — user: *"if it is standard, we don't need to have cadence too. only when we
    /// have autopilot then we have cadence"*. The cadence IS the
    /// heartbeat on which pmd nudges, so on a Standard row it configures a thing that never
    /// happens. Asking for it there is asking the human to decide something inert.
    pub(crate) fn shows_cadence(&self) -> bool {
        self.tier == Tier::Autopilot
    }
    /// Is the Decider field part of this form right now?
    ///
    /// AUTOPILOT ONLY, exactly like cadence — a decider engine on a Standard row configures a
    /// consult that never runs (the decider only runs on a row pmd drives). Consulted by BOTH the
    /// render and the Tab navigation, so a hidden field can never be focused.
    pub(crate) fn shows_decider(&self) -> bool {
        self.tier == Tier::Autopilot
    }
    /// Does field `idx` render (and take focus) in this form's current state? The shared
    /// Message/Goal row is always shown; its label and launch semantics follow the tier.
    pub(crate) fn shows_field(&self, idx: usize) -> bool {
        match idx {
            Self::CADENCE => self.shows_cadence(),
            // The Decider Model rides with the Decider engine: autopilot-only, since a decider model
            // on a Standard row configures a consult that never runs.
            Self::DECIDER | Self::DECIDER_MODEL => self.shows_decider(),
            _ => true,
        }
    }
    /// Tab / ↓. Skips every field this form is not showing — a LOOP rather than a single step,
    /// because on Standard the three hidden fields (Cadence, Decider, Decider Model) are
    /// adjacent at the end, and stepping once would land on the first of them.
    ///
    /// Bounded by `FIELDS`, so even a hypothetical form with nothing to show terminates instead of
    /// spinning on a dashboard render.
    pub(crate) fn next_field(&mut self) {
        for _ in 0..Self::FIELDS {
            self.field = (self.field + 1) % Self::FIELDS;
            if self.shows_field(self.field) {
                return;
            }
        }
    }
    /// Shift+Tab / ↑ — the mirror of [`CreateForm::next_field`], same skipping.
    pub(crate) fn prev_field(&mut self) {
        for _ in 0..Self::FIELDS {
            self.field = (self.field + Self::FIELDS - 1) % Self::FIELDS;
            if self.shows_field(self.field) {
                return;
            }
        }
    }
    pub(crate) fn toggle_engine(&mut self) {
        self.engine = match self.engine {
            Engine::Claude => Engine::Codex,
            Engine::Codex => Engine::Claude,
        };
        // The model list is PER-ENGINE, so a model picked for the old engine may not exist for the
        // new one. Reset to the engine default; the App repopulates `model_choices` after the flip.
        self.worker_model = None;
    }
    /// Flip the DECIDER engine (Claude ↔ Codex). A no-op off the Decider field since only it
    /// routes here, and inert on Standard where the field is not even shown.
    pub(crate) fn toggle_decider_engine(&mut self) {
        self.decider_engine = match self.decider_engine {
            Engine::Claude => Engine::Codex,
            Engine::Codex => Engine::Claude,
        };
        // The decider model list is PER-DECIDER-ENGINE, so a model picked for the old engine may not
        // exist for the new one. Reset to the engine default; the App repopulates
        // `decider_model_choices` after the flip (mirrors `toggle_engine` for the worker model).
        self.decider_model = None;
    }
    /// Nudge the heartbeat cadence by one minute, floored at 60s (a zero/near-zero
    /// cadence would hammer the heartbeat). A no-op off the Cadence field since only
    /// it routes here.
    pub(crate) fn adjust_cadence(&mut self, forward: bool) {
        const STEP_S: u64 = 60;
        self.cadence_s = if forward {
            self.cadence_s.saturating_add(STEP_S)
        } else {
            self.cadence_s.saturating_sub(STEP_S).max(STEP_S)
        };
    }
    /// Is the FOCUSED field one of the text fields (Directory/Name/Message-or-Goal)? Consulted by
    /// the key handler so `←/→` move the caret here but still cycle the value on a toggle field.
    pub(crate) fn is_text_field(&self) -> bool {
        matches!(self.field, Self::DIRECTORY | Self::NAME | Self::GOAL)
    }

    /// The focused text field, or `None` when a toggle field (Engine/Model/Autonomy/Cadence) is
    /// focused — the ONE place the field index maps to a buffer, so typing and every caret
    /// motion route the same way.
    fn text_field(&mut self) -> Option<&mut Field> {
        match self.field {
            Self::DIRECTORY => Some(&mut self.dir),
            Self::NAME => Some(&mut self.name),
            Self::GOAL => Some(&mut self.goal),
            _ => None,
        }
    }

    /// Type into whichever text field is focused (dir/goal), at the caret; other fields ignore.
    pub(crate) fn type_char(&mut self, c: char) {
        if let Some(f) = self.text_field() {
            f.insert(c);
        }
    }
    /// Insert a whole pasted block into the focused text field in ONE `Field::insert_str` — the
    /// same O(n) path the inline overlays use. Typing it char-by-char through `type_char` would be
    /// O(n²) (each char re-walks `char_indices().nth(caret)`), which a large paste feels as lag.
    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(f) = self.text_field() {
            f.insert_str(text);
        }
    }
    pub(crate) fn backspace(&mut self) {
        if let Some(f) = self.text_field() {
            f.backspace();
        }
    }
    /// Delete the character AT the caret (the Delete key) on the focused text field.
    pub(crate) fn delete_forward(&mut self) {
        if let Some(f) = self.text_field() {
            f.delete();
        }
    }
    pub(crate) fn caret_left(&mut self) {
        if let Some(f) = self.text_field() {
            f.left();
        }
    }
    pub(crate) fn caret_right(&mut self) {
        if let Some(f) = self.text_field() {
            f.right();
        }
    }
    pub(crate) fn caret_home(&mut self) {
        if let Some(f) = self.text_field() {
            f.home();
        }
    }
    pub(crate) fn caret_end(&mut self) {
        if let Some(f) = self.text_field() {
            f.end();
        }
    }
    /// Left/right on a toggle field cycles it. All non-text fields are now uniform ←→ steppers:
    /// engine/Autonomy/cadence/decider toggle, and the two MODEL fields step their stored value
    /// over `[(default)] ++ choices` via [`step_model`] (space and ←/→ share this one handler).
    pub(crate) fn adjust(&mut self, forward: bool) {
        match self.field {
            Self::ENGINE => self.toggle_engine(),
            Self::WORKER_MODEL => {
                self.worker_model =
                    step_model(self.worker_model.as_deref(), &self.model_choices, forward);
            }
            Self::AUTONOMY => {
                self.tier = if forward {
                    next_tier(self.tier)
                } else {
                    prev_tier(self.tier)
                }
            }
            Self::CADENCE => self.adjust_cadence(forward),
            Self::DECIDER => self.toggle_decider_engine(),
            Self::DECIDER_MODEL => {
                self.decider_model = step_model(
                    self.decider_model.as_deref(),
                    &self.decider_model_choices,
                    forward,
                );
            }
            _ => {}
        }
    }
}
