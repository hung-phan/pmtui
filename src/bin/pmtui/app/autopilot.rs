//! `m` and `c` — the autonomy switch and the heartbeat it runs on. One module
//! because the two CHAIN: flipping autopilot on asks for the goal, then for the
//! cadence when the session has none, and only the last submit performs the flip. The
//! single `turn_autopilot_on` is what keeps the several routes into it from disagreeing
//! about what a flip entails.

use crate::*;

impl App {
    /// `m`: flip the selected session's autonomy. `Tier` has exactly two values, so
    /// this single key IS the autopilot switch, and since m15 it is the ONLY on/off
    /// switch a human needs: Autopilot = the harness drives this session (pmd nudges it
    /// on its cadence), Standard = YOU drive it (pmd will not type into it at all — see
    /// `agent_manager::daemon::pmd_drives_row`). Landing on Autopilot also ensures a
    /// daemon exists to act on it.
    ///
    /// `p` (pause) is the other half of this pair, and it is why the Autopilot arm below also
    /// RE-ENABLES a paused row. `p` clears `registry.enabled` and turns the dial off, and the sweep
    /// returns on a disabled row before it ever reads the tier — so writing Autopilot alone would
    /// report "→ Autopilot" over a session pmd still skips entirely. That is the exact class of
    /// defect this key already shipped once (the pre-m15 chip that said "Autopilot off" and stopped
    /// nothing), and the user's spec closes it from the other side: *"only when i press m again,
    /// then it start again"* — `m` has to be sufficient by itself.
    pub(crate) fn cycle_tier(&mut self) {
        let Some(id) = self.selected_view().map(|v| v.id.clone()) else {
            self.status = "m switches a session's mode (nothing is selected)".into();
            return;
        };
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        // A row still mid-spawn refuses every mode change, so say that before reading settings
        // whose state says nothing about why `m` cannot act here.
        if entry.is_staged_spawn() && self.refuse_unstartable(entry) {
            return;
        }
        let mode = entry.mode;
        // Per-session paths for an AgentLoop session (root paths for others), so the
        // dial actually turns the tier the JobScheduler reads each wake — and so the
        // goal we judge below is the same `brief.md` the nudge injects.
        let paths = entry_state_paths(entry);
        let cfg_path = paths.config();
        match state::read_json::<Config>(&cfg_path) {
            Ok(mut c) => {
                let next = next_tier(c.autonomy);
                // A failed fork's leftover or a row still mid-spawn may not start by any route,
                // and Autopilot would start it: refuse before the goal prompt asks for a goal
                // nothing can use.
                if next == Tier::Autopilot && self.refuse_unstartable(entry) {
                    return;
                }
                // INTO AUTOPILOT on an agent-loop row: ask for the goal first and let
                // `submit_goal_edit` do BOTH writes. Nothing is written here, so `Esc` in that
                // field leaves the session exactly as it was.
                //
                // Scoped to `AgentLoop` rather than to `pmd_drives`: on a native `Mode::Auto` row
                // pmd drives at EVERY tier and the phase machine does not read `brief.md`, so a
                // goal prompt there would ask for something nothing consumes. That row keeps the
                // old empty-brief refusal below.
                if next == Tier::Autopilot && mode == Mode::AgentLoop {
                    self.begin_autopilot_goal();
                    return;
                }
                // Autopilot needs a direction (Standard does not): flipping INTO
                // Autopilot with no goal on disk would produce exactly the directionless
                // nudge this gate exists to prevent, so refuse the flip outright rather
                // than record a tier the loop cannot act on. Nothing is written, and the
                // status names the key that fixes it. The reverse flip is never gated —
                // turning autopilot OFF needs no goal.
                //
                // Scoped to rows pmd actually DRIVES, and that scope is the whole point:
                // on a `Mode::Interactive` row the tier is inert either way (the sweep
                // skips it), so a goal would not make autopilot do anything. Gating there
                // would preempt the accurate "pmd does not drive interactive sessions"
                // message below with a claim that the GOAL is what's missing — naming the
                // wrong blocker, which is worse than not gating at all.
                if pmd_drives(mode) && next == Tier::Autopilot && goal_is_empty(&paths.brief()) {
                    // Only a native `Mode::Auto` row reaches this now — an agent-loop row is
                    // asked for its goal above instead of refused.
                    // Names the ONE goal key. It used to name two (`G` inline, `g` for
                    // the editor); with the pair collapsed, naming `G` would point a
                    // human at a key that no longer exists — the exact anti-drift failure
                    // this message's own test guards against.
                    self.status = format!(
                        "{id}: autopilot needs a direction — press g to set the goal, then m; mode unchanged"
                    );
                    self.refresh();
                    return;
                }
                c.autonomy = next;
                match state::write_json_atomic(&cfg_path, &c) {
                    Ok(()) => {
                        // `Tier` IS the 2-value autopilot switch, so landing on
                        // Autopilot means "autopilot ON" and we make sure a daemon
                        // exists to drive it.
                        //
                        // This only helps from the OFF side. `next_tier` is a pure
                        // flip, so a session ALREADY on Autopilot lands on Standard
                        // here — no single `m` press can revive a dead pmd under a
                        // session that is on Autopilot right now. Recovering THAT is
                        // startup's job; see `ensure_daemon_for_enabled_autopilot`.
                        self.status = match (c.autonomy, pmd_drives(mode)) {
                            // The daemon fragment stays close to the front on purpose:
                            // `keybar_line` truncates the TAIL, and "is anything actually
                            // running?" is the half the human cannot otherwise see.
                            // "Autopilot" already means "pmd drives it" — the full
                            // sentence lives in the `m` help row and the create form's
                            // Autonomy descriptor, which have room for it.
                            // Reached only by a native `Mode::Auto` row: an agent-loop flip into
                            // Autopilot goes through the goal prompt and
                            // `App::turn_autopilot_on`, which is where the unpause and the daemon
                            // ensure live.
                            (Tier::Autopilot, true) => {
                                format!("{id} → Autopilot; {}", self.ensure_daemon())
                            }
                            // `ensure_daemon` is a GLOBAL action, and pmd's sweep skips
                            // interactive rows entirely: starting a daemon "for" this
                            // row would drive every OTHER enabled project while doing
                            // nothing here. Record the tier (it's just config) but do
                            // not start — or claim to have started — a daemon.
                            (Tier::Autopilot, false) => {
                                format!(
                                    "{id} → Autopilot (pmd does not drive interactive sessions; no daemon started)"
                                )
                            }
                            // The verdict FIRST, then the consequence that used not to
                            // be true: before m15 this said "autopilot off" while pmd
                            // carried on nudging the session. Now it means it — but only
                            // where it IS true, which is this arm alone.
                            (Tier::Standard, _) if mode == Mode::AgentLoop => {
                                format!("{id}: autopilot off — pmd won't type; you drive it")
                            }
                            // A native `Mode::Auto` row is driven at EVERY tier
                            // (`daemon::pmd_drives_row`), so promising "pmd won't type" here
                            // was simply false — the dial is recorded and the daemon carries
                            // on nudging. Say that instead of the comfortable thing.
                            (Tier::Standard, true) => format!(
                                "{id} → Standard, but a native session is driven at every mode — this does NOT stop pmd"
                            ),
                            // Interactive: the sweep skips the row entirely, so nothing types
                            // into it at either setting. The dial is inert, not protective.
                            (Tier::Standard, false) => format!(
                                "{id} → Standard (pmd does not drive interactive sessions either way)"
                            ),
                        };
                    }
                    Err(e) => self.status = format!("could not change the mode: {e}"),
                }
            }
            // Unreadable config: nothing was written, so be explicit that BOTH halves
            // of "turn autopilot on" were skipped rather than silently dropping the
            // daemon ensure the human was probably after.
            Err(e) => {
                self.status =
                    format!("{id}: settings unreadable ({e}) — mode unchanged, pmd not started");
            }
        }
        self.refresh();
    }

    /// The model catalog for `engine`, discovered ONCE per engine per launch and cached. The
    /// picker's open path calls this so the render tick can read `model_catalog` without ever doing
    /// discovery I/O. Best-effort: direct Claude can supply stable aliases, while unrecoverable
    /// provider discovery failures yield an empty slice ("(default)" only).
    pub(crate) fn models_for(&mut self, engine: Engine) -> &[ModelInfo] {
        self.model_catalog
            .entry(engine)
            .or_insert_with(|| available_models(engine));
        &self.model_catalog[&engine]
    }

    /// How many models `engine` offers (the cached catalog length; fills it on first use).
    pub(crate) fn models_len(&mut self, engine: Engine) -> usize {
        self.models_for(engine).len()
    }

    /// The row a `stored` model value maps to on the `Model` stage: `0` (the `(default)`/`None` row)
    /// when unset OR when the stored value is not in the catalog; otherwise `1 + its index`.
    pub(crate) fn model_cursor_for(&mut self, stored: Option<&str>, engine: Engine) -> usize {
        match stored {
            None => 0,
            Some(s) => self
                .models_for(engine)
                .iter()
                .position(|m| m.value == s)
                .map(|i| i + 1)
                .unwrap_or(0),
        }
    }

    /// The model currently stored for `target` on session `id`, read from the pmtui-owned file
    /// (config.json for the decider, registry.json for the worker) — used to SEED the `Model`
    /// stage's cursor onto what is already set. `None` on any miss.
    fn stored_model_for(&self, target: PickTarget, id: &str) -> Option<String> {
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let entry = reg.projects.iter().find(|p| p.id == id)?;
        match target {
            PickTarget::Worker => entry.worker_model.clone(),
            PickTarget::Decider => state::read_json::<Config>(&entry_state_paths(entry).config())
                .ok()
                .and_then(|c| c.decider_model),
        }
    }

    /// `e`/`w`: OPEN the reusable model picker for the SELECTED session. `e` targets the DECIDER
    /// (its engine + model, config.json); `w` the WORKER model (registry.json). Nothing is written
    /// here — [`commit_model_pick`] does the one write.
    ///
    /// The DECIDER is AUTOPILOT-ONLY, like `g`/`c`/`i`: the supervisor consult never runs on a row
    /// pmd does not drive, so choosing its engine/model on Standard would configure something inert —
    /// refuse and point at `m`. It opens on the `Engine` stage. The WORKER applies to any agent-loop
    /// session (its launch carries `--model` at every tier), so it opens straight on the `Model`
    /// stage for the entry's create-time engine.
    pub(crate) fn begin_model_pick(&mut self, target: PickTarget) {
        let Some(id) = self.selected_view().map(|v| v.id.clone()) else {
            self.status = match target {
                PickTarget::Decider => {
                    "e picks a session's decider engine + model (nothing is selected)".into()
                }
                PickTarget::Worker => {
                    "w picks a session's worker model (nothing is selected)".into()
                }
            };
            return;
        };
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        match target {
            PickTarget::Decider => {
                let paths = entry_state_paths(entry);
                match state::read_json::<Config>(&paths.config()) {
                    Ok(c) => {
                        if c.autonomy != Tier::Autopilot {
                            self.status = format!(
                                "{id}: the decider only runs on autopilot — press m to turn it on; decider unchanged"
                            );
                            self.refresh();
                            return;
                        }
                        let engine = c.decider_engine;
                        let stored_model = c.decider_model.clone();
                        let cursor = Engine::ALL.iter().position(|&e| e == engine).unwrap_or(0);
                        // Fill the catalog for the current engine now, so the first frame renders it
                        // without I/O (advancing to another engine fills that one on Enter).
                        let _ = self.models_len(engine);
                        self.mode = UiMode::ModelPicker {
                            target: PickTarget::Decider,
                            id,
                            stage: PickStage::Engine,
                            engine,
                            cursor,
                            stored_model,
                        };
                    }
                    Err(e) => {
                        self.status = format!("{id}: config unreadable ({e}) — decider unchanged");
                        self.refresh();
                    }
                }
            }
            PickTarget::Worker => {
                let engine = entry.engine.unwrap_or(Engine::Claude);
                let stored = entry.worker_model.clone();
                let cursor = self.model_cursor_for(stored.as_deref(), engine);
                self.mode = UiMode::ModelPicker {
                    target: PickTarget::Worker,
                    id,
                    stage: PickStage::Model,
                    engine,
                    cursor,
                    stored_model: stored,
                };
            }
        }
    }

    /// The Enter action. On the `Engine` stage (decider only): adopt the cursored engine and advance
    /// to the `Model` stage, seeding its cursor onto the model already stored for this target. On the
    /// `Model` stage: commit.
    pub(crate) fn advance_model_pick(&mut self) {
        let (target, id, stage, cursor) = match &self.mode {
            UiMode::ModelPicker {
                target,
                id,
                stage,
                cursor,
                ..
            } => (*target, id.clone(), *stage, *cursor),
            _ => return,
        };
        match stage {
            PickStage::Model => self.commit_model_pick(),
            PickStage::Engine => {
                let engine = Engine::ALL[cursor.min(Engine::ALL.len() - 1)];
                // The stored model for this target does NOT change just because a different engine is
                // being eyed — so it seeds both the cursor and the `●` current marker for the new
                // engine's catalog. (An engine whose catalog lacks the stored value shows no `●`.)
                let stored = self.stored_model_for(target, &id);
                let cursor = self.model_cursor_for(stored.as_deref(), engine);
                self.mode = UiMode::ModelPicker {
                    target,
                    id,
                    stage: PickStage::Model,
                    engine,
                    cursor,
                    stored_model: stored,
                };
            }
        }
    }

    /// Commit the cursored model (and, for the decider, the chosen engine) and close to Normal.
    /// Row `0` = `(default)` = `None`; every other row stores the catalog entry's launch `value`.
    ///
    /// Writes ONLY the pmtui-owned file — config.json for the decider (read-modify-write, so no other
    /// field is clobbered), the registry.json entry for the worker (via corrupt-safe
    /// `Registry::update`). NEVER the ledger (single-writer). No autopilot re-check: the overlay owns
    /// the keyboard while it is up and both files are pmtui's alone.
    pub(crate) fn commit_model_pick(&mut self) {
        let (target, id, engine, cursor) = match &self.mode {
            UiMode::ModelPicker {
                target,
                id,
                engine,
                cursor,
                ..
            } => (*target, id.clone(), *engine, *cursor),
            _ => return,
        };
        // Resolve the chosen value and its human label from the cached catalog BEFORE leaving the
        // overlay. Row 0 is the `(default)`/`None` sentinel.
        let (chosen, model_label): (Option<String>, String) = if cursor == 0 {
            (None, "(default)".to_string())
        } else {
            match self.models_for(engine).get(cursor - 1) {
                Some(m) => (Some(m.value.clone()), m.label.clone()),
                // A stale cursor past the catalog falls back to the default rather than storing junk.
                None => (None, "(default)".to_string()),
            }
        };
        self.mode = UiMode::Normal;
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        match target {
            PickTarget::Decider => {
                let cfg_path = entry_state_paths(entry).config();
                match state::read_json::<Config>(&cfg_path) {
                    Ok(mut c) => {
                        c.decider_engine = engine;
                        c.decider_model = chosen;
                        match state::write_json_atomic(&cfg_path, &c) {
                            Ok(()) => {
                                self.status =
                                    format!("{id} → decider: {} / {model_label}", engine.label())
                            }
                            Err(e) => self.status = format!("{id}: could not set the decider: {e}"),
                        }
                    }
                    Err(e) => {
                        self.status = format!("{id}: config unreadable ({e}) — decider unchanged")
                    }
                }
            }
            PickTarget::Worker => {
                let mut found = false;
                let res = Registry::update(&self.registry_path, |reg| {
                    if let Some(e) = reg.projects.iter_mut().find(|p| p.id == id) {
                        e.worker_model = chosen;
                        found = true;
                    }
                });
                self.status = match res {
                    Err(e) => format!("{id}: could not set the worker model: {e}"),
                    Ok(()) if !found => format!("{id} is gone from the list"),
                    Ok(()) => {
                        format!("{id} → worker model: {model_label}; press r to restart to apply")
                    }
                };
            }
        }
        self.refresh();
    }

    /// The Esc action. On the decider's `Model` stage, step BACK to the `Engine` stage with the
    /// cursor on the current engine (so a mis-picked engine is one Esc from fixed). Otherwise close
    /// to Normal.
    pub(crate) fn back_or_cancel_model_pick(&mut self) {
        if let UiMode::ModelPicker {
            target: PickTarget::Decider,
            id,
            stage: PickStage::Model,
            engine,
            stored_model,
            ..
        } = &self.mode
        {
            let (id, engine, stored_model) = (id.clone(), *engine, stored_model.clone());
            let cursor = Engine::ALL.iter().position(|&e| e == engine).unwrap_or(0);
            self.mode = UiMode::ModelPicker {
                target: PickTarget::Decider,
                id,
                stage: PickStage::Engine,
                engine,
                cursor,
                stored_model,
            };
            return;
        }
        self.mode = UiMode::Normal;
        self.status = "model selection cancelled".into();
    }

    /// `m` on its way INTO Autopilot: ask for the goal first, seeded with the one on disk.
    ///
    /// User: *"When they switch from standard to autopilot, we need to pop up and ask them to
    /// enter a goal … when we turn it on again, it needs to ask the new goal and put the old goal
    /// there so people can update."*
    ///
    /// It ALWAYS asks — this replaced a gate that only refused when the brief was EMPTY. The
    /// reason the unconditional prompt is better is not politeness: the brief is the entire input
    /// to every nudge autopilot will send, a session that has been running by hand for an hour has
    /// almost certainly moved past whatever it said, and the moment of handing over the wheel is
    /// the one moment the human is thinking about where it should go. A gate that fires only on
    /// empty lets a STALE mandate through silently, which is the failure that actually happens.
    ///
    /// Nothing is written here — not the goal, not the tier. `submit_goal_edit` does both, so
    /// `Esc` leaves the session exactly as it was.
    /// Explain when an attached human temporarily delays pmd input.
    fn attachment_note_for_autopilot(&self, id: &str, root: &Path) -> String {
        let session = session_name(id, root);
        if self.agent_tmux.has_clients(&session).unwrap_or(true) {
            " (attached — pmd waits until you detach)".into()
        } else {
            String::new()
        }
    }

    pub(crate) fn begin_autopilot_goal(&mut self) {
        let Some((id, brief, current)) = self.selected_brief("m turns autopilot on") else {
            return;
        };
        // SEEDED with the mandate on disk, exactly as `begin_goal_edit` does: the question `m` asks
        // is "what is this session for NOW?", and the old goal is the answer worth editing rather
        // than retyping. Multi-line and all — the buffer is a `ratatui-textarea`.
        let input = Composer::seeded(current.trim().to_string(), composer::GOAL);
        self.mode = UiMode::EditingGoal {
            id,
            brief,
            current,
            input,
            then_autopilot: true,
        };
    }

    /// Turn autopilot ON for `id` and report what happened — the ONE path that does it, shared by
    /// `m`'s goal prompt and by the `^E` editor drain, so the two cannot disagree about what a
    /// flip entails.
    ///
    pub(crate) fn turn_autopilot_on(&mut self, id: &str) -> String {
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            return format!("{id} is gone from the list — autopilot not turned on");
        };
        // Rechecked here because the goal and cadence prompts and the `^E` drain all end in this
        // flip, and the row may have changed while a prompt was open.
        if let Some(refusal) = super::forking::start_refusal(entry) {
            return refusal;
        }
        let paused = !entry.enabled;
        let paths = entry_state_paths(entry);
        let cfg_path = paths.config();
        let mut cfg = match state::read_json::<Config>(&cfg_path) {
            Ok(c) => c,
            Err(e) => {
                return format!("{id}: config unreadable ({e}) — autopilot not turned on");
            }
        };
        let daemon = self.ensure_daemon();
        if !daemon.is_up() {
            return format!("{id}: {daemon}");
        }
        cfg.autonomy = Tier::Autopilot;
        if let Err(e) = state::write_json_atomic(&cfg_path, &cfg) {
            return format!("{id}: could not change the mode: {e}");
        }
        // The required Goal has now replaced any failed Standard first-launch Message as
        // the session's work instruction. A later return to Standard must not replay it.
        self.initial_message_retries.remove(id);
        // START DRIVING NOW — both brakes, not just the park. The session may be parked on a long
        // `Monitoring` nap chosen while nobody was driving it, AND it may still carry a
        // report-generation debt from an earlier autopilot stint that the agent never answered,
        // which the
        // engine reads as "still working" for a further 1800s. Releasing only the park was m41's
        // gap, and it left *"when i switch to autopilot mode, pmd doesn't drive it immediately"*.
        // Best-effort: a ledger that cannot be written still gets the tier, and the park expires.
        let root = entry.root.clone();
        let _ = start_driving_now(&paths);
        // The existing terminal stays alive with its launch-time posture. If a human
        // is attached, pmd simply waits for detach before sending its first nudge.
        let handover = self.attachment_note_for_autopilot(id, &root);
        let lifted = paused.then(|| self.set_row_enabled(id, true));
        match lifted {
            None => format!("{id} → Autopilot{handover}; {daemon}"),
            Some(Ok(())) => format!("{id} unpaused → Autopilot{handover}; {daemon}"),
            Some(Err(e)) => format!(
                "{id} → Autopilot, but it is still PAUSED ({e}) — pmd skips it; Enter resumes"
            ),
        }
    }

    /// `c` on a Normal-mode row: open the inline CADENCE field.
    ///
    /// Agent-loop only. On an interactive row pmd never nudges at all, and on a native
    /// `Mode::Auto` row the rhythm belongs to the phase machine rather than to
    /// `ledger.cadence_s` — offering the dial there would move a number nothing reads.
    ///
    /// The current value is read from the LEDGER, not the registry: the ledger is what
    /// `job_engine` actually consults, so a session whose cadence was already re-timed by its
    /// own agent must show the rhythm it is really on.
    pub(crate) fn begin_cadence_edit(&mut self) {
        let Some(v) = self.selected_view() else {
            self.status = "c sets how often pmd nudges a session (nothing is selected)".into();
            return;
        };
        let (id, mode, tier) = (v.id.clone(), v.mode, v.tier);
        // The tier, matching the chip (`Applies::Autopilot`). A cadence on a row pmd does not
        // drive is a number nothing reads, so this refuses rather than half-works — and it names
        // the key that makes it matter, the way `a`'s refusal names `Enter`.
        if !agent_manager::daemon::pmd_drives_row(mode, tier) {
            self.status =
                format!("{id}: the check-in interval only applies on autopilot — press m first");
            return;
        }
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(e) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        let paths = entry_state_paths(e);
        // Falls back through the registry seed to the harness default, so the field always
        // opens on a real number rather than on a blank the human has to guess at.
        let current = state::read_control(&paths)
            .ok()
            .and_then(|c| c.human_cadence_s)
            .or_else(|| {
                state::read_json::<AgentLoopState>(&paths.pmstate())
                    .ok()
                    .and_then(|l| l.cadence_s)
            })
            .or(e.cadence_s)
            .unwrap_or(job_engine::DEFAULT_CADENCE_S);
        self.mode = UiMode::EditingCadence {
            root: e.root.clone(),
            id,
            current,
            input: Field::new(),
            then_autopilot: false,
        };
    }

    /// This row's project root, from the registry — the same lookup every other write path uses,
    /// so nothing here re-derives a path from a state directory.
    pub(crate) fn session_root(&self, id: &str) -> Option<PathBuf> {
        Registry::load(&self.registry_path)
            .unwrap_or_default()
            .projects
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.root.clone())
    }

    /// The cadence recorded for `id`, or `None` when the session has never been given one.
    ///
    /// The LEDGER first (what `job_engine` actually consults, and where the agent's own adaptive
    /// proposals land), then the registry seed. `None` is a real state since m40: a Standard
    /// session is created without a cadence, because the heartbeat it describes does not run.
    pub(crate) fn recorded_cadence(&self, id: &str) -> Option<u64> {
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let e = reg.projects.iter().find(|p| p.id == id)?;
        let paths = entry_state_paths(e);
        state::read_control(&paths)
            .ok()
            .and_then(|c| c.human_cadence_s)
            .or_else(|| {
                state::read_json::<AgentLoopState>(&paths.pmstate())
                    .ok()
                    .and_then(|l| l.cadence_s)
            })
            .or(e.cadence_s)
    }

    /// Ask for the cadence on the way into Autopilot — the SECOND half of `m`'s prompt chain,
    /// opened by [`App::submit_goal_edit`] only when the session has no cadence recorded.
    ///
    /// Seeded EMPTY with the harness default shown as the current value, so pressing Enter adopts
    /// that default explicitly (see [`App::submit_cadence_edit`]) rather than leaving the session
    /// with an unset number that would prompt again on the next flip.
    pub(crate) fn begin_autopilot_cadence(&mut self, id: &str, root: &Path) {
        self.mode = UiMode::EditingCadence {
            root: root.to_path_buf(),
            id: id.to_string(),
            current: job_engine::DEFAULT_CADENCE_S,
            input: Field::new(),
            then_autopilot: true,
        };
    }

    /// Enter in the cadence field: parse, clamp, write, and say exactly what landed.
    ///
    /// An empty save KEEPS the current cadence, matching the goal field — the two inline
    /// fields look identical, so one of them silently zeroing a setting would be a trap.
    pub(crate) fn submit_cadence_edit(&mut self) {
        let UiMode::EditingCadence {
            id,
            root,
            input,
            then_autopilot,
            ..
        } = &self.mode
        else {
            return;
        };
        let (id, root, typed, then_autopilot) = (
            id.clone(),
            root.clone(),
            input.trim().to_string(),
            *then_autopilot,
        );
        if typed.is_empty() {
            self.mode = UiMode::Normal;
            // ON THE AUTOPILOT PATH an empty save ADOPTS THE DEFAULT EXPLICITLY rather than
            // "keeping" an unset value. Leaving it unset would park the session on `job_engine`'s
            // fallback anyway but prompt again on the next flip — asking twice for something the
            // human has already waved through. So the default is written, named, and done with.
            if then_autopilot {
                let want = job_engine::DEFAULT_CADENCE_S;
                let paths = ProjectPaths::for_session(&root, &id);
                let wrote = apply_cadence_edit(&paths, &self.registry_path, &id, want);
                self.status = match wrote {
                    Err(e) => format!("{id}: cadence unset ({e}) — autopilot NOT turned on"),
                    Ok(_) => format!(
                        "{}; every {} (default)",
                        self.turn_autopilot_on(&id),
                        job_engine::human_cadence(want)
                    ),
                };
                self.refresh();
                self.finish_board_action();
                return;
            }
            self.status = format!("{id} cadence kept (empty save changes nothing)");
            self.finish_board_action();
            return;
        }
        // A PARSE failure keeps the overlay OPEN with the text intact: the human mistyped a
        // unit, and closing the field would make them retype the whole thing to fix one
        // character. Only a decided outcome closes it.
        let want = match parse_cadence(&typed) {
            Ok(n) => n,
            Err(e) => {
                self.status = format!("{id}: {e}");
                return;
            }
        };
        self.mode = UiMode::Normal;
        let paths = ProjectPaths::for_session(&root, &id);
        let outcome = apply_cadence_edit(&paths, &self.registry_path, &id, want);
        // THE PENDING FLIP, if this field was opened by `m`. Same rule as the goal prompt: a failed
        // write must NOT flip, because autopilot would then start a heartbeat on a rhythm the human
        // never got to choose.
        if then_autopilot {
            self.status = match &outcome {
                Err(e) => format!("{id}: cadence unchanged ({e}) — autopilot NOT turned on"),
                Ok(_) => format!(
                    "{}; every {}",
                    self.turn_autopilot_on(&id),
                    job_engine::human_cadence(match &outcome {
                        Ok(CadenceEdit::Written { secs, .. }) => *secs,
                        _ => want,
                    })
                ),
            };
            self.refresh();
            self.finish_board_action();
            return;
        }
        self.status = match outcome {
            Ok(CadenceEdit::Unchanged(secs)) => {
                format!(
                    "{id} already checks in every {}",
                    job_engine::human_cadence(secs)
                )
            }
            // Names the ADOPTED value, and says so when it came from a clamp — a field that
            // silently substituted a different number would be the same lie as a dial that
            // reads one thing and does another.
            Ok(CadenceEdit::Written { secs, clamped }) => {
                let adopted = job_engine::human_cadence(secs);
                if clamped {
                    format!(
                        "{id} checks in every {adopted} ({} is outside {}..{})",
                        job_engine::human_cadence(want),
                        job_engine::human_cadence(job_engine::CADENCE_MIN_S),
                        job_engine::human_cadence(job_engine::CADENCE_MAX_S)
                    )
                } else {
                    format!("{id} checks in every {adopted} — the timer restarted")
                }
            }
            Err(e) => format!("{id}: cadence unchanged ({e})"),
        };
        self.refresh();
        self.finish_board_action();
    }
}
