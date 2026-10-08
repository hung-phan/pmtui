//! The `m` dial: the per-session tier it writes, the goal it will not turn Autopilot on
//! without, the foreign config it reads as off rather than as a trap, and the promise it
//! keeps when the dial goes back to Standard.

use super::*;

#[test]
fn cycle_tier_updates_per_session_config_for_agent_loop() {
    // `m` on an agent-loop session advances the PER-SESSION config's tier (the
    // dial the JobScheduler reads) and leaves the shared root config.json
    // untouched. Because `Tier` IS the 2-value autopilot switch, landing on
    // Autopilot ALSO ensures the daemon is running (re-homed from the old
    // `toggle_autopilot_from_standard_…` coverage). Note this covers the OFF→ON
    // edge ONLY: a session already on Autopilot flips to Standard instead, so no
    // `m` press revives a dead pmd under it — that is
    // `startup_ensures_daemon_…`'s job.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // seeds Standard
    let session_paths = ProjectPaths::for_session(&root, "bot");

    // Pre-acquire the daemon singleton lock (matching `loop_app`'s "pm-test"
    // socket) and hold it across the call so the Autopilot landing takes
    // `ensure_daemon`'s "already running" no-spawn branch (never spawns pmd).
    let _held = hold_current_daemon(&reg_path, "pm-test");

    let mut app = loop_app(&reg_path);
    app.initial_message_retries
        .insert("bot".into(), "failed Standard launch".into());
    app.cycle_tier(); // Standard -> Autopilot: ASKS FOR THE GOAL FIRST
    // The flip is a two-step now: `m` opens the goal prompt and writes NOTHING, so `Esc`
    // leaves the session exactly as it was.
    assert!(
        matches!(
            app.mode,
            UiMode::EditingGoal {
                then_autopilot: true,
                ..
            }
        ),
        "`m` into Autopilot must ask for the goal: {:?}",
        app.mode
    );
    assert_eq!(
        state::read_json::<Config>(&session_paths.config())
            .unwrap()
            .autonomy,
        Tier::Standard,
        "nothing may be written before the prompt is answered"
    );
    app.submit_goal_edit(); // empty save keeps the seeded goal and completes the flip

    let cfg: Config = state::read_json(&session_paths.config()).unwrap();
    assert_eq!(
        cfg.autonomy,
        Tier::Autopilot,
        "the per-session dial advanced to Autopilot"
    );
    assert!(
        app.status.contains("Autopilot") && app.status.contains("already running"),
        "landing on Autopilot ensures the daemon (no-spawn branch): {}",
        app.status
    );
    assert!(
        !ProjectPaths::new(&root).config().exists(),
        "the shared root config.json must not be written"
    );
    assert!(!app.initial_message_retries.contains_key("bot"));
}

#[test]
fn daemon_refresh_failure_keeps_the_session_on_standard() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = loop_app(&reg_path);
    app.initial_message_retries
        .insert("bot".into(), "still retryable".into());

    let status = app.turn_autopilot_on("bot");

    assert_eq!(tier_on_disk(&root, "bot"), Tier::Standard, "{status}");
    assert!(
        !status.contains("→ Autopilot"),
        "a failed daemon ensure must not claim or publish Autopilot: {status}"
    );
    assert_eq!(
        app.initial_message_retries.get("bot").map(String::as_str),
        Some("still retryable")
    );
}

#[test]
fn cycle_tier_from_autopilot_to_standard_leaves_daemon_untouched() {
    // Cycling an agent-loop session that is already on Autopilot back to Standard
    // turns autopilot OFF: it must NOT ensure/spawn the daemon and reports
    // "autopilot off" (re-homed from the old
    // `toggle_autopilot_from_autopilot_…` coverage).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // seeds Standard
    let session_paths = ProjectPaths::for_session(&root, "bot");
    // Flip the seed to Autopilot so the cycle lands on Standard (autopilot off).
    let cfg_path = session_paths.config();
    let mut c = state::read_json::<Config>(&cfg_path).unwrap();
    c.autonomy = Tier::Autopilot;
    state::write_json_atomic(&cfg_path, &c).unwrap();

    let mut app = loop_app(&reg_path);
    app.cycle_tier(); // Autopilot -> Standard

    assert_eq!(
        state::read_json::<Config>(&cfg_path).unwrap().autonomy,
        Tier::Standard,
        "the per-session dial went back to Standard"
    );
    // A DAEMON-SIDE observable, not a claim about the status text. The off-branch
    // status is a fixed literal that never interpolates `ensure_daemon`'s result,
    // so asserting `!status.contains("daemon")` would be tautologically true and
    // would still pass with an `ensure_daemon()` call spliced into this branch.
    // `ensure_daemon` -> `lease::try_acquire` opens the lock `create(true)`, so the
    // singleton lock file existing at all proves it ran: assert it does NOT.
    assert!(
        !lease::daemon_lock_path(&reg_path, "pm-test").exists(),
        "turning autopilot off must not even probe the daemon singleton lock"
    );
    assert!(
        app.status.contains("autopilot off"),
        "and it reports the flip: {}",
        app.status
    );
}

#[test]
fn m_into_autopilot_always_asks_and_seeds_the_old_goal() {
    // *"When they switch from standard to autopilot, we need to pop up and ask them to enter a
    // goal. With existing session, if we turn off autopilot, the brief will stay but when we
    // turn it on again, it needs to ask the new goal and put the old goal there so people can
    // update."*
    //
    // ALWAYS asks — the point that distinguishes this from the gate it replaced. The old gate
    // fired only on an EMPTY brief, which let a stale mandate through silently: a session
    // driven by hand for an hour has moved past whatever the brief said, and the brief is the
    // entire input to every nudge autopilot is about to start sending.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // seeds the goal "goal"
    let sp = ProjectPaths::for_session(&root, "bot");
    state::write_text_atomic(&sp.brief(), "ship the old thing\n").unwrap();
    let _held = hold_current_daemon(&reg_path, "pm-test");

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);

    // SEEDED with what is on disk, so this is an edit rather than a retype…
    let UiMode::EditingGoal {
        input,
        current,
        then_autopilot,
        ..
    } = &app.mode
    else {
        panic!("`m` must open the goal prompt, got {:?}", app.mode);
    };
    assert!(then_autopilot);
    assert_eq!(
        input.text(),
        "ship the old thing",
        "the old goal must be there to update"
    );
    assert_eq!(current.trim(), "ship the old thing");
    // …and the overlay SAYS what enter will do, or a human saves a goal and unknowingly hands
    // pmd the wheel.
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    t.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&t);
    assert!(
        screen.contains("Autopilot"),
        "the prompt must name the flip: {screen}"
    );
    // The keys must say what they DO, in both places a human looks: the overlay's own bottom
    // border and the keybar. (The first version of this assertion caught a hint too long for
    // the frame, which `draw_overlay_frame` drops rather than clipping — so it rendered
    // nowhere at all.)
    assert!(
        screen.contains("enter = autopilot ON"),
        "the overlay's key line must say what enter does: {screen}"
    );
    assert!(
        screen.contains("Autopilot ON") && screen.contains("Stay Standard"),
        "the keybar must say what enter and esc do: {screen}"
    );

    // Editing it lands the NEW goal and the flip together.
    for c in " now".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        std::fs::read_to_string(sp.brief()).unwrap().trim(),
        "ship the old thing now"
    );
    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Autopilot,
        "{}",
        app.status
    );
    assert!(
        app.status.contains("Autopilot") && app.status.contains("goal saved"),
        "the status reports both halves: {}",
        app.status
    );
    assert!(app.spawned_pmd.borrow().is_empty(), "no daemon spawned");
}

#[test]
fn a_goal_write_failure_does_not_turn_autopilot_on() {
    // The stale-mandate failure this prompt exists to prevent, in its worst form: the flip
    // lands, the write does not, and pmd starts nudging the OLD direction under a status that
    // said the goal was saved. The brief path is made unwritable (a directory) so
    // `apply_goal_edit` fails for a reason the UI cannot control.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    std::fs::remove_file(sp.brief()).unwrap();
    std::fs::create_dir(sp.brief()).unwrap();

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    for c in "new goal".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Standard,
        "a failed goal write must leave autopilot OFF: {}",
        app.status
    );
    assert!(
        app.status.contains("autopilot NOT turned on"),
        "and it must say so: {}",
        app.status
    );
}

#[test]
fn autopilot_is_refused_without_a_goal_and_the_field_stays_open() {
    // "Turning autopilot on later" gets the SAME direction requirement the create
    // form applies: with `brief.md` empty, the flip into Autopilot is refused
    // outright — nothing written to config, no daemon probed — and the status names
    // the keys that edit the goal so the human knows the way out. Recording
    // Autopilot here instead would hand the loop a nudge with nothing to steer by.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // seeds Standard
    let session_paths = ProjectPaths::for_session(&root, "bot");
    // `reg_with_agent_loop` seeds a goal; blank it to reach the gate. (Empty rather
    // than deleted, because that is the state `g`-then-save-nothing leaves behind.)
    state::write_text_atomic(&session_paths.brief(), "  \n").unwrap();

    let mut app = loop_app(&reg_path);
    app.cycle_tier(); // Standard -> Autopilot: opens the goal prompt
    // Enter with an empty field is REFUSED, because there is no goal on disk to fall back on.
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        state::read_json::<Config>(&session_paths.config())
            .unwrap()
            .autonomy,
        Tier::Standard,
        "the refused flip must not write the tier"
    );
    assert!(
        !lease::daemon_lock_path(&reg_path, "pm-test").exists(),
        "a refused flip must not probe the daemon singleton lock"
    );
    assert!(
        app.status.contains("autopilot needs a goal"),
        "the refusal explains itself: {}",
        app.status
    );
    // THE FIELD STAYS OPEN — the whole difference from the old refusal, which closed and left
    // the human on an unchanged dashboard having swallowed the one thing they were asked for.
    // Here the way out IS the thing in front of them.
    assert!(
        matches!(
            app.mode,
            UiMode::EditingGoal {
                then_autopilot: true,
                ..
            }
        ),
        "the refusal must keep the field open: {:?}",
        app.mode
    );
    // …and it names the OTHER way out, which must really be bound in this scope.
    assert!(app.status.contains("esc"), "{}", app.status);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal), "esc must close it");
    assert!(
        app.status.contains("autopilot") && app.status.contains("Standard"),
        "cancelling says the dial did not move: {}",
        app.status
    );
}

// --- M31: `r` restarts the agent (stop pmd -> end the pane -> start pmd) ---------

#[test]
fn a_foreign_config_is_readable_off_and_flippable_rather_than_a_trap() {
    // THE BUG THIS CLOSES, from the dashboard's side. Reported as *"why my session with
    // autopilot off receive the prompt"*.
    //
    // Before: `Config::autonomy` had no serde default, so a `config.json` written by
    // another tool — or simply missing the field — failed to parse. That gave pmtui a tier
    // of `None` (the row read as UNKNOWN, never "off"), made `cycle_tier` refuse ("config
    // unreadable — tier unchanged"), and made `daemon::pmd_drives_row` treat it as
    // permission to DRIVE. So pmd kept typing and no key could stop it.
    //
    // After: the field defaults, so the tier reads Standard — visibly OFF, not driven, and
    // the dial turns. All three halves are asserted, because fixing any two of them still
    // leaves a trap.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let cfg = ProjectPaths::for_session(&root, "bot").config();
    // A real foreign schema: the `project-manager` skill's own config shape, which has
    // every field but this one.
    std::fs::write(
        &cfg,
        r#"{"heartbeat":{"interval_s":300},"stuck_threshold":3}"#,
    )
    .unwrap();

    let mut app = App::new(reg_path.clone(), "pm-test".into());
    let v = app
        .projects
        .iter()
        .find(|v| v.id == "bot")
        .expect("the row still renders");
    // (1) READABLE, and reads as OFF — not as the `?` an unknown tier renders.
    assert_eq!(
        v.tier,
        Some(Tier::Standard),
        "a foreign config must read as autopilot OFF, not as unknown"
    );
    // (2) NOT DRIVEN. This is the half that was putting keystrokes into the agent.
    assert!(
        !agent_manager::daemon::pmd_drives_row(v.mode, v.tier),
        "a row nobody asked to be driven must not be driven"
    );
    assert!(
        !answer_reaches_the_agent(v.mode, v.tier),
        "and `a` must not promise a delivery nothing would perform"
    );

    // (3) FLIPPABLE. `cycle_tier` used to refuse outright here, which is what made it a
    // trap rather than merely a wrong default. Hold the daemon singleton so landing on
    // Autopilot takes the no-spawn branch.
    let _held = hold_current_daemon(&reg_path, "pm-test");
    app.cycle_tier();
    // The goal prompt opens first now; the seeded brief means an empty save keeps it.
    app.submit_goal_edit();
    assert!(
        !app.status.contains("unreadable"),
        "the dial must turn on a foreign config, not refuse it: {}",
        app.status
    );
    let back = state::read_json::<Config>(&cfg).expect("still parses after the write");
    assert_eq!(
        back.autonomy,
        Tier::Autopilot,
        "one press must record the flip on disk: {}",
        app.status
    );
    // And the OTHER fields survived the round trip rather than being reset by a wholesale
    // overwrite — the flip reads, mutates one field, and writes back.
    assert_eq!(back.stuck_threshold, 3, "a foreign field was clobbered");
}

#[test]
fn turning_the_dial_to_standard_says_pmd_will_not_type() {
    // Turning autopilot off on an agent-loop row means pmd stops nudging and the human
    // drives it by hand — the status must say so plainly.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let paths = ProjectPaths::for_session(&root, "bot");
    // Seed AUTOPILOT so the single press below lands on Standard, which is the arm
    // whose wording is under test.
    let mut c = state::read_json::<Config>(&paths.config()).unwrap();
    c.autonomy = Tier::Autopilot;
    state::write_json_atomic(&paths.config(), &c).unwrap();

    let mut app = App::new(reg_path, "pm-test".into());
    app.cycle_tier();
    assert!(
        app.status.contains("pmd won't type"),
        "expected \"pmd won't type\", got: {}",
        app.status
    );
}

#[test]
fn cycle_tier_out_of_autopilot_is_never_gated_on_a_goal() {
    // The gate is one-directional: turning autopilot OFF needs no direction, so an
    // Autopilot session with an empty brief must still be able to flip back to
    // Standard. Gating both edges would strand such a session on Autopilot with no
    // key able to move it.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // seeds Standard
    let session_paths = ProjectPaths::for_session(&root, "bot");
    let cfg_path = session_paths.config();
    let mut c = state::read_json::<Config>(&cfg_path).unwrap();
    c.autonomy = Tier::Autopilot;
    state::write_json_atomic(&cfg_path, &c).unwrap();
    state::write_text_atomic(&session_paths.brief(), "").unwrap();

    let mut app = loop_app(&reg_path);
    app.cycle_tier(); // Autopilot -> Standard, allowed

    assert_eq!(
        state::read_json::<Config>(&cfg_path).unwrap().autonomy,
        Tier::Standard,
        "no goal is needed to turn autopilot off"
    );
    assert!(app.status.contains("autopilot off"), "{}", app.status);
}

#[test]
fn goal_is_empty_treats_missing_blank_and_whitespace_alike() {
    // Deliberately coarse: `JobScheduler::nudge` reads the brief with
    // `read_to_string(..).unwrap_or_default()`, so missing / empty / whitespace are
    // indistinguishable DOWNSTREAM. Anything this helper let through as "has a goal"
    // that the engine then sees as empty would be a flip into a goal-less autopilot.
    let dir = tempfile::tempdir().unwrap();
    assert!(
        goal_is_empty(&dir.path().join("absent.md")),
        "a missing brief has no goal"
    );
    let blank = dir.path().join("blank.md");
    std::fs::write(&blank, "").unwrap();
    assert!(goal_is_empty(&blank));
    let ws = dir.path().join("ws.md");
    std::fs::write(&ws, " \n\t\n").unwrap();
    assert!(goal_is_empty(&ws));
    // A directory stands in for "unreadable": the read errors, and erring on the
    // "no goal" side refuses a flip rather than arming a directionless autopilot.
    assert!(goal_is_empty(dir.path()), "an unreadable brief has no goal");
    let real = dir.path().join("real.md");
    std::fs::write(&real, "\n  ship vector search\n").unwrap();
    assert!(!goal_is_empty(&real), "a real goal is a goal");
}

// --- `e`/`w`: the reusable ModelPicker — two-stage decider (engine→model), worker model ---
//
// Every test PRE-SEEDS `app.model_catalog` so the pick list is deterministic and never shells out
// to the host's real claude/codex CLIs (`available_models` would otherwise run on open).

/// A one-entry claude catalog whose value is a launch-ready model id.
fn opus() -> ModelInfo {
    ModelInfo {
        label: "opus".into(),
        value: "global.anthropic.claude-opus-5".into(),
    }
}

#[test]
fn e_opens_the_decider_engine_stage_seeded_to_current() {
    // `e` on an AUTOPILOT row OPENS the picker on the ENGINE stage, seeded to the engine on disk; it
    // writes NOTHING yet (the commit does). The seed engine is Claude, so the cursor lands on 0.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let cfg_before = std::fs::read(sp.config()).unwrap();

    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Decider);
    match &app.mode {
        UiMode::ModelPicker {
            target,
            id,
            stage,
            engine,
            cursor,
            stored_model,
        } => {
            assert_eq!(*target, PickTarget::Decider);
            assert_eq!(id, "bot");
            assert_eq!(*stage, PickStage::Engine);
            assert_eq!(*engine, Engine::Claude);
            assert_eq!(
                *cursor, 0,
                "cursor seeds on the current engine (claude = 0)"
            );
            assert_eq!(*stored_model, None, "the seed has no decider_model");
        }
        m => panic!("e must open the Engine stage, got {m:?}"),
    }
    assert_eq!(
        std::fs::read(sp.config()).unwrap(),
        cfg_before,
        "opening the picker writes nothing"
    );
}

#[test]
fn decider_engine_enter_advances_to_model_seeded_to_the_stored_model() {
    // Enter on the Engine stage adopts the cursored engine and advances to the Model stage, seeding
    // the Model cursor onto the model already stored (so it is an edit, not a retype).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut c = state::read_json::<Config>(&sp.config()).unwrap();
    c.decider_model = Some("global.anthropic.claude-sonnet-5".into());
    state::write_json_atomic(&sp.config(), &c).unwrap();

    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(
        Engine::Claude,
        vec![
            opus(),
            ModelInfo {
                label: "sonnet".into(),
                value: "global.anthropic.claude-sonnet-5".into(),
            },
        ],
    );
    app.begin_model_pick(PickTarget::Decider);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // Engine (claude) -> Model
    match &app.mode {
        UiMode::ModelPicker {
            stage: PickStage::Model,
            engine: Engine::Claude,
            cursor,
            ..
        } => {
            // rows = [(default), opus, sonnet]; stored = sonnet ⇒ cursor 2.
            assert_eq!(*cursor, 2, "the Model cursor seeds onto the stored model");
        }
        m => panic!("Enter must advance to the Model stage, got {m:?}"),
    }
}

#[test]
fn decider_commit_writes_engine_and_model_to_config_only_ledger_untouched() {
    // Pick codex on the Engine stage, its one model on the Model stage, and commit: config gets both
    // `decider_engine` and `decider_model`, the mode returns to Normal, the status names the change,
    // and the ledger (state.json) is byte-identical (single-writer invariant).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let ledger_before = std::fs::read(sp.pmstate()).unwrap();

    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.model_catalog.insert(
        Engine::Codex,
        vec![ModelInfo {
            label: "gpt-5.6".into(),
            value: "openai.gpt-5.6-sol".into(),
        }],
    );
    app.begin_model_pick(PickTarget::Decider);
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // Engine: claude -> codex
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // advance to Model (codex)
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // Model: (default) -> gpt-5.6
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // commit
    assert!(
        matches!(app.mode, UiMode::Normal),
        "commit closes the picker"
    );
    let cfg: Config = state::read_json(&sp.config()).unwrap();
    assert_eq!(cfg.decider_engine, Engine::Codex, "{}", app.status);
    assert_eq!(
        cfg.decider_model.as_deref(),
        Some("openai.gpt-5.6-sol"),
        "{}",
        app.status
    );
    assert!(
        app.status.contains("decider") && app.status.contains("codex"),
        "status names the change: {}",
        app.status
    );
    assert_eq!(
        std::fs::read(sp.pmstate()).unwrap(),
        ledger_before,
        "commit_model_pick must not touch the ledger"
    );
}

#[test]
fn decider_commit_on_the_default_row_stores_none() {
    // The `(default)` row (cursor 0) stores `None`, never an empty string — and it CLEARS a
    // previously-set model.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut c = state::read_json::<Config>(&sp.config()).unwrap();
    c.decider_model = Some("global.anthropic.claude-opus-5".into());
    state::write_json_atomic(&sp.config(), &c).unwrap();

    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Decider);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // Engine -> Model (cursor on opus = 1)
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE); // up to (default) = 0
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // commit
    let cfg: Config = state::read_json(&sp.config()).unwrap();
    assert_eq!(
        cfg.decider_model, None,
        "the (default) row stores None: {}",
        app.status
    );
    assert_eq!(cfg.decider_engine, Engine::Claude);
}

#[test]
fn decider_esc_steps_back_to_engine_then_cancels() {
    // Esc on the decider's Model stage steps BACK to the Engine stage (cursor on the current engine);
    // a second Esc cancels to Normal, writing nothing.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let cfg_before = std::fs::read(sp.config()).unwrap();

    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Decider);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // -> Model stage
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE); // back to Engine
    match &app.mode {
        UiMode::ModelPicker {
            stage: PickStage::Engine,
            engine: Engine::Claude,
            cursor: 0,
            ..
        } => {}
        m => panic!("Esc on Model must step back to the Engine stage, got {m:?}"),
    }
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE); // cancel
    assert!(matches!(app.mode, UiMode::Normal), "second Esc cancels");
    assert!(
        app.status.contains("cancelled"),
        "status says cancelled: {}",
        app.status
    );
    assert_eq!(
        std::fs::read(sp.config()).unwrap(),
        cfg_before,
        "esc writes nothing"
    );
}

#[test]
fn engine_stage_cursor_clamps_and_empty_catalog_leaves_only_the_default_row() {
    // Cursor clamps at both ends of the Engine stage; on the Model stage an EMPTY catalog leaves a
    // single `(default)` row, so the cursor cannot move past 0.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![]); // discovery found nothing
    app.begin_model_pick(PickTarget::Decider);
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE); // clamp at top
    assert!(matches!(
        app.mode,
        UiMode::ModelPicker {
            stage: PickStage::Engine,
            cursor: 0,
            ..
        }
    ));
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE); // clamp at Engine::ALL.len()-1
    assert!(matches!(
        app.mode,
        UiMode::ModelPicker {
            stage: PickStage::Engine,
            cursor: 1,
            ..
        }
    ));
    // Back to claude, advance to the (empty) Model stage.
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // only the (default) row exists
    assert!(matches!(
        app.mode,
        UiMode::ModelPicker {
            stage: PickStage::Model,
            cursor: 0,
            ..
        }
    ));
    assert_eq!(
        app.models_len(Engine::Claude),
        0,
        "the cached catalog stays empty"
    );
}

#[test]
fn e_refuses_the_decider_on_a_standard_row_and_points_at_m() {
    // AUTOPILOT-ONLY: the decider never runs on a Standard row, so `e` refuses (naming `m`), stays in
    // Normal, and writes nothing.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // seeds Standard
    let sp = ProjectPaths::for_session(&root, "bot");

    let mut app = loop_app(&reg_path);
    app.begin_model_pick(PickTarget::Decider);
    assert!(
        matches!(app.mode, UiMode::Normal),
        "no decider picker on a Standard row"
    );
    assert!(
        app.status.contains("press m"),
        "status must name the m key; got {:?}",
        app.status
    );
    let cfg: Config = state::read_json(&sp.config()).unwrap();
    assert_eq!(
        cfg.decider_engine,
        Engine::Claude,
        "a refused open must not change the engine"
    );
    assert_eq!(cfg.autonomy, Tier::Standard, "and must not write the tier");
}

#[test]
fn w_opens_the_worker_model_stage_and_commits_to_the_registry_only() {
    // `w` opens the WORKER picker straight on the Model stage (the worker's engine is fixed at create
    // time), at ANY tier. Committing writes `worker_model` to the registry ONLY — config.json and the
    // ledger are byte-identical — and the status names the model + the restart needed to apply it.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // Standard, engine Claude, worker_model None
    let sp = ProjectPaths::for_session(&root, "bot");
    let cfg_before = std::fs::read(sp.config()).unwrap();
    let ledger_before = std::fs::read(sp.pmstate()).unwrap();

    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Worker);
    match &app.mode {
        UiMode::ModelPicker {
            target: PickTarget::Worker,
            id,
            stage: PickStage::Model,
            engine: Engine::Claude,
            cursor: 0,          // worker_model unset ⇒ (default)
            stored_model: None, // …and nothing is marked current
        } => assert_eq!(id, "bot"),
        m => panic!("w must open the Worker Model stage, got {m:?}"),
    }
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // (default) -> opus
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // commit
    assert!(matches!(app.mode, UiMode::Normal));
    let stored = Registry::load(&reg_path)
        .unwrap()
        .projects
        .iter()
        .find(|p| p.id == "bot")
        .unwrap()
        .worker_model
        .clone();
    assert_eq!(
        stored.as_deref(),
        Some("global.anthropic.claude-opus-5"),
        "{}",
        app.status
    );
    assert!(
        app.status.contains("worker model") && app.status.contains("restart"),
        "status names the model + restart: {}",
        app.status
    );
    assert_eq!(
        std::fs::read(sp.config()).unwrap(),
        cfg_before,
        "the worker commit must not touch config.json"
    );
    assert_eq!(
        std::fs::read(sp.pmstate()).unwrap(),
        ledger_before,
        "the worker commit must not touch the ledger"
    );
}

#[test]
fn renders_the_decider_engine_frame() {
    // The Engine stage renders the title, the sub-header, both engines, and the `(current)` marker.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Decider);

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render engine frame");
    let screen = screen_text(&terminal);
    for s in ["Decider", "bot", "Engine", "claude", "codex", "(current)"] {
        assert!(screen.contains(s), "engine frame missing {s:?}: {screen}");
    }
}

#[test]
fn decider_engine_frame_signals_the_model_step_and_enter_reaches_it() {
    // Report: pressing `e` showed only claude/codex and "didn't let me choose a model". The model
    // step IS there — Enter on the Engine stage advances to the model list — but the engine screen
    // must SAY so, and Enter must actually reach a model list carrying the catalog.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(
        Engine::Claude,
        vec![ModelInfo {
            label: "opus-label".into(),
            value: "global.anthropic.claude-opus-5".into(),
        }],
    );
    app.begin_model_pick(PickTarget::Decider);

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("engine frame");
    let engine_screen = screen_text(&terminal);
    // The Engine stage must advertise that Enter leads to the model choice (discoverability): both
    // the sub-header and the footer name it.
    assert!(
        engine_screen.contains("Enter") && engine_screen.contains("model"),
        "the engine stage must signal Enter \u{2192} model: {engine_screen}"
    );

    // Enter advances to the Model stage, which lists this engine's catalog — the model IS choosable.
    app.advance_model_pick();
    terminal.draw(|f| render(f, &app)).expect("model frame");
    let model_screen = screen_text(&terminal);
    assert!(
        model_screen.contains("opus-label") && model_screen.contains("(default)"),
        "Enter must reach a model list carrying the catalog: {model_screen}"
    );
}

#[test]
fn renders_the_worker_model_frame() {
    // The worker Model stage renders the title, the "Model · <engine>" sub-header, the `(default)`
    // row, and the cached catalog labels.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(
        Engine::Claude,
        vec![ModelInfo {
            label: "opus-label".into(),
            value: "global.anthropic.claude-opus-5".into(),
        }],
    );
    app.begin_model_pick(PickTarget::Worker);

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render worker frame");
    let screen = screen_text(&terminal);
    for s in [
        "Worker",
        "bot",
        "Model",
        "claude",
        "(default)",
        "opus-label",
    ] {
        assert!(screen.contains(s), "worker frame missing {s:?}: {screen}");
    }
}

#[test]
fn renders_model_picker_at_tiny_sizes_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Decider);
    // Exercise BOTH stages: the first pass renders the Engine stage, then Enter advances to Model.
    for advance in [false, true] {
        if advance {
            handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        }
        for (w, h) in [(1u16, 1u16), (20, 5), (40, 10), (100, 30), (200, 50)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| render(f, &app))
                .unwrap_or_else(|e| panic!("render picker {w}x{h}: {e}"));
        }
    }
}

/// Per-row text of the rendered buffer — the model picker draws one choice per row, so this lets a
/// test assert WHICH row carries the `●` current glyph vs the `▸` cursor glyph.
fn picker_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    screen_rows_styled(terminal)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

const GLYPH_CURRENT: &str = "\u{25cf}"; // ●
const GLYPH_CURSOR: &str = "\u{25b8}"; // ▸

#[test]
fn model_stage_current_marker_stays_on_the_stored_model_when_the_cursor_moves() {
    // F1: on the Model stage the `●` marks the STORED model INDEPENDENT of the cursor. Move `▸` off
    // the stored model and the `●` must NOT follow — it stays on the stored row (and the cursor row
    // is a different, un-marked row).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut c = state::read_json::<Config>(&sp.config()).unwrap();
    c.decider_model = Some("global.anthropic.claude-sonnet-5".into()); // stored = sonnet
    state::write_json_atomic(&sp.config(), &c).unwrap();

    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(
        Engine::Claude,
        vec![
            ModelInfo {
                label: "opus-label".into(),
                value: "global.anthropic.claude-opus-5".into(),
            },
            ModelInfo {
                label: "sonnet-label".into(),
                value: "global.anthropic.claude-sonnet-5".into(),
            },
        ],
    );
    app.begin_model_pick(PickTarget::Decider);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // Engine -> Model (cursor seeds on sonnet = row 2)
    // Move the cursor OFF the stored model, up to the (default) row (0).
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE); // 2 -> 1 (opus)
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE); // 1 -> 0 (default)
    assert!(
        matches!(
            app.mode,
            UiMode::ModelPicker {
                stage: PickStage::Model,
                cursor: 0,
                ..
            }
        ),
        "cursor moved to (default): {:?}",
        app.mode
    );

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render model frame");
    let rows = picker_rows(&terminal);

    let stored_row = rows
        .iter()
        .find(|r| r.contains("sonnet-label"))
        .expect("the stored model row is drawn");
    assert!(
        stored_row.contains(GLYPH_CURRENT),
        "the STORED model row must carry ● even though the cursor left it: {stored_row:?}"
    );
    assert!(
        !stored_row.contains(GLYPH_CURSOR),
        "the cursor is elsewhere, so the stored row has no ▸: {stored_row:?}"
    );
    // Find the overlay's (default) row by its unique label (NOT by glyph — the dashboard behind the
    // overlay draws its own `▸` selection arrow). The cursor is there, and it is NOT current.
    let default_row = rows
        .iter()
        .find(|r| r.contains("(default)"))
        .expect("the (default) row is drawn");
    assert!(
        default_row.contains(GLYPH_CURSOR),
        "the cursor is on the (default) row: {default_row:?}"
    );
    assert!(
        !default_row.contains(GLYPH_CURRENT),
        "with a model stored, the (default) row is NOT current: {default_row:?}"
    );
}

#[test]
fn model_stage_marks_default_current_when_no_model_is_stored() {
    // The mirror of the above: with no model stored, the `(default)` row is the current one — and it
    // STAYS marked when the cursor moves onto a real model.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot); // decider_model None
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(
        Engine::Claude,
        vec![ModelInfo {
            label: "opus-label".into(),
            value: "global.anthropic.claude-opus-5".into(),
        }],
    );
    app.begin_model_pick(PickTarget::Decider);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // Engine -> Model (cursor on default = 0)
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // cursor -> opus (row 1)

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render model frame");
    let rows = picker_rows(&terminal);
    let default_row = rows
        .iter()
        .find(|r| r.contains("(default)"))
        .expect("the (default) row is drawn");
    assert!(
        default_row.contains(GLYPH_CURRENT),
        "with no model stored, (default) is current: {default_row:?}"
    );
    assert!(
        !default_row.contains(GLYPH_CURSOR),
        "the cursor moved to the model row, so (default) has no ▸: {default_row:?}"
    );
    let opus_row = rows
        .iter()
        .find(|r| r.contains("opus-label"))
        .expect("the model row is drawn");
    assert!(
        opus_row.contains(GLYPH_CURSOR) && !opus_row.contains(GLYPH_CURRENT),
        "the cursored model is not current (nothing is stored): {opus_row:?}"
    );
}

#[test]
fn decider_commit_preserves_unrelated_config_fields() {
    // MINOR: the decider commit is a read-modify-write, so it must touch ONLY decider_engine +
    // decider_model and leave every other Config field intact. Pins the RMW.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let mut c = state::read_json::<Config>(&sp.config()).unwrap();
    c.step_timeout_s = 4242; // non-default sentinels
    c.max_failures = 9;
    state::write_json_atomic(&sp.config(), &c).unwrap();

    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Decider);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // Engine -> Model
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE); // (default) -> opus
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // commit

    let back = state::read_json::<Config>(&sp.config()).unwrap();
    assert_eq!(
        back.decider_model.as_deref(),
        Some("global.anthropic.claude-opus-5"),
        "{}",
        app.status
    );
    assert_eq!(back.decider_engine, Engine::Claude);
    assert_eq!(back.autonomy, Tier::Autopilot, "autonomy preserved");
    assert_eq!(
        back.step_timeout_s, 4242,
        "step_timeout_s preserved by the RMW"
    );
    assert_eq!(back.max_failures, 9, "max_failures preserved by the RMW");
}

#[test]
fn cycle_tier_refuses_an_unreadable_config_without_replacing_it() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let cfg_path = ProjectPaths::for_session(&root, "bot").config();
    let mut app = loop_app(&reg_path);
    std::fs::write(&cfg_path, "{not json").unwrap();

    app.cycle_tier();

    assert!(
        app.status.contains("settings unreadable")
            && app.status.contains("mode unchanged")
            && app.status.contains("pmd not started"),
        "{}",
        app.status
    );
    assert_eq!(
        std::fs::read_to_string(cfg_path).unwrap(),
        "{not json",
        "a refused mode change must preserve the unreadable file for recovery"
    );
}

#[test]
fn model_picker_dead_ends_are_no_ops_with_specific_statuses() {
    let mut app = app_with(vec![], UiMode::Normal);

    app.begin_model_pick(PickTarget::Decider);
    assert!(
        app.status.contains("decider") && app.status.contains("nothing is selected"),
        "{}",
        app.status
    );
    app.begin_model_pick(PickTarget::Worker);
    assert!(
        app.status.contains("worker") && app.status.contains("nothing is selected"),
        "{}",
        app.status
    );

    let status = app.status.clone();
    app.advance_model_pick();
    app.commit_model_pick();
    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(
        app.status, status,
        "picker actions outside the picker must be inert"
    );
}

#[test]
fn decider_picker_refuses_an_unreadable_config_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let cfg_path = ProjectPaths::for_session(&root, "bot").config();
    let mut app = loop_app(&reg_path);
    std::fs::write(&cfg_path, "{not json").unwrap();

    app.begin_model_pick(PickTarget::Decider);

    assert!(matches!(app.mode, UiMode::Normal));
    assert!(
        app.status.contains("config unreadable") && app.status.contains("decider unchanged"),
        "{}",
        app.status
    );
}

#[test]
fn worker_engine_stage_recovers_the_stored_model_from_the_registry() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].worker_model = Some(opus().value);
    reg.save(&reg_path).unwrap();
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    // Worker pickers normally open directly on Model. This pins the recovery behavior if a
    // stale overlay snapshot presents the shared picker on its Engine stage.
    app.mode = UiMode::ModelPicker {
        target: PickTarget::Worker,
        id: "bot".into(),
        stage: PickStage::Engine,
        engine: Engine::Claude,
        cursor: 0,
        stored_model: None,
    };

    app.advance_model_pick();

    match &app.mode {
        UiMode::ModelPicker {
            target: PickTarget::Worker,
            stage: PickStage::Model,
            cursor: 1,
            stored_model: Some(model),
            ..
        } => assert_eq!(model, "global.anthropic.claude-opus-5"),
        mode => panic!("worker recovery must advance to its stored model, got {mode:?}"),
    }
}

#[test]
fn stale_model_cursor_commits_the_default_instead_of_junk() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut reg = Registry::load(&reg_path).unwrap();
    reg.projects[0].worker_model = Some(opus().value);
    reg.save(&reg_path).unwrap();
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.mode = UiMode::ModelPicker {
        target: PickTarget::Worker,
        id: "bot".into(),
        stage: PickStage::Model,
        engine: Engine::Claude,
        cursor: usize::MAX,
        stored_model: Some("global.anthropic.claude-opus-5".into()),
    };

    app.commit_model_pick();

    let reg = Registry::load(&reg_path).unwrap();
    assert_eq!(
        reg.projects[0].worker_model, None,
        "an out-of-range cursor must resolve to the default"
    );
    assert!(app.status.contains("(default)"), "{}", app.status);
}

#[test]
fn model_commit_reports_a_row_removed_while_the_picker_was_open() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Decider);
    app.advance_model_pick();
    Registry::default().save(&reg_path).unwrap();

    app.commit_model_pick();

    assert!(matches!(app.mode, UiMode::Normal));
    assert!(
        app.status.contains("bot is gone from the list"),
        "{}",
        app.status
    );
    assert!(
        app.projects.is_empty(),
        "the stale row must be refreshed away"
    );
}

#[test]
fn decider_commit_preserves_a_config_that_became_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let cfg_path = ProjectPaths::for_session(&root, "bot").config();
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Decider);
    app.advance_model_pick();
    std::fs::write(&cfg_path, "{broken while picker open").unwrap();

    app.commit_model_pick();

    assert!(
        app.status.contains("config unreadable") && app.status.contains("decider unchanged"),
        "{}",
        app.status
    );
    assert_eq!(
        std::fs::read_to_string(cfg_path).unwrap(),
        "{broken while picker open"
    );
}

#[test]
fn worker_commit_preserves_a_registry_that_became_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = loop_app(&reg_path);
    app.model_catalog.insert(Engine::Claude, vec![opus()]);
    app.begin_model_pick(PickTarget::Worker);
    std::fs::write(&reg_path, "{broken while picker open").unwrap();

    app.commit_model_pick();

    assert!(
        app.status.contains("bot is gone from the list"),
        "{}",
        app.status
    );
    assert_eq!(
        std::fs::read_to_string(reg_path).unwrap(),
        "{broken while picker open",
        "a corrupt registry must never be replaced with a default"
    );
}

#[test]
fn autopilot_goal_handles_no_selection_and_multiline_briefs() {
    let mut app = app_with(vec![], UiMode::Normal);
    app.begin_autopilot_goal();
    assert!(app.status.contains("nothing is selected"), "{}", app.status);

    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let brief = ProjectPaths::for_session(&root, "bot").brief();
    std::fs::write(&brief, "first line\nsecond line\n").unwrap();
    let mut app = loop_app(&reg_path);
    app.begin_autopilot_goal();
    match &app.mode {
        UiMode::EditingGoal { current, input, .. } => {
            assert_eq!(current, "first line\nsecond line\n");
            // THE BUFFER HOLDS THE WHOLE MANDATE. This asserted the opposite until the field became a
            // `ratatui-textarea`: a one-line field could not show two lines, so it opened EMPTY and
            // pointed at `$EDITOR`. Opening on the goal is the reason to have a real buffer at all.
            assert_eq!(
                input.text(),
                "first line\nsecond line",
                "the multi-line brief must open IN the field, trailing newline trimmed"
            );
        }
        mode => panic!("expected the autopilot goal editor, got {mode:?}"),
    }
}

#[test]
fn turn_autopilot_on_reports_missing_rows_and_unreadable_configs() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let cfg_path = ProjectPaths::for_session(&root, "bot").config();
    let mut app = loop_app(&reg_path);

    let missing = app.turn_autopilot_on("missing");
    assert!(
        missing.contains("gone from the list") && missing.contains("not turned on"),
        "{missing}"
    );

    std::fs::write(&cfg_path, "{not json").unwrap();
    let unreadable = app.turn_autopilot_on("bot");
    assert!(
        unreadable.contains("config unreadable") && unreadable.contains("not turned on"),
        "{unreadable}"
    );
    assert_eq!(std::fs::read_to_string(cfg_path).unwrap(), "{not json");
}

#[test]
fn cadence_open_reports_a_stale_row_and_reads_the_ledger_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let stale_registry = dir.path().join("stale-registry.json");
    Registry::default().save(&stale_registry).unwrap();
    let mut stale = app_with(vec![autopilot_loop_view("ghost")], UiMode::Normal);
    stale.registry_path = stale_registry;

    stale.begin_cadence_edit();

    assert!(
        stale.status.contains("ghost is gone from the list"),
        "{}",
        stale.status
    );
    assert!(stale.projects.is_empty(), "refresh removes the stale row");

    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let paths = ProjectPaths::for_session(&root, "bot");
    state::write_control(&paths, &state::Control::default()).unwrap();
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.cadence_s = Some(777);
    job::save(&paths, &ledger).unwrap();
    let mut app = loop_app(&reg_path);

    app.begin_cadence_edit();

    assert!(
        matches!(
            app.mode,
            UiMode::EditingCadence {
                current: 777,
                then_autopilot: false,
                ..
            }
        ),
        "the ledger cadence must win when control has no human override: {:?}",
        app.mode
    );
}

#[test]
fn cadence_submit_outside_its_editor_is_inert() {
    let mut app = app_with(vec![], UiMode::Normal);
    let status = app.status.clone();

    app.submit_cadence_edit();

    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(app.status, status);
}

#[test]
fn cadence_write_failures_never_complete_a_pending_autopilot_flip() {
    let dir = tempfile::tempdir().unwrap();
    for (suffix, typed, then_autopilot, expected) in [
        ("empty-auto", "", true, "cadence unset"),
        ("typed-auto", "5m", true, "cadence unchanged"),
        ("typed-manual", "5m", false, "cadence unchanged"),
    ] {
        let id = format!("bot-{suffix}");
        let root = dir.path().join(suffix);
        let paths = ProjectPaths::for_session(&root, &id);
        std::fs::create_dir_all(paths.control()).unwrap();
        let mode = UiMode::EditingCadence {
            root,
            id: id.clone(),
            current: 300,
            input: Field::from(typed),
            then_autopilot,
        };
        let mut app = app_with(vec![], mode);
        app.registry_path = dir.path().join(format!("{suffix}-registry.json"));

        app.submit_cadence_edit();

        assert!(
            matches!(app.mode, UiMode::Normal),
            "{suffix}: {:?}",
            app.mode
        );
        assert!(
            app.status.contains(expected),
            "{suffix}: expected {expected:?}, got {}",
            app.status
        );
        if then_autopilot {
            assert!(
                app.status.contains("autopilot NOT turned on"),
                "{suffix}: {}",
                app.status
            );
        }
    }
}

#[test]
fn unchanged_cadence_reports_the_existing_value_without_rewriting_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let paths = ProjectPaths::for_session(&root, "bot");
    state::write_control(
        &paths,
        &state::Control {
            human_cadence_s: Some(300),
            wake_generation: 7,
        },
    )
    .unwrap();
    let before = std::fs::read(paths.control()).unwrap();
    let mut app = app_with(
        vec![],
        UiMode::EditingCadence {
            root,
            id: "bot".into(),
            current: 300,
            input: Field::from("5m"),
            then_autopilot: false,
        },
    );

    app.submit_cadence_edit();

    assert!(
        app.status.contains("already checks in every 5m"),
        "{}",
        app.status
    );
    assert_eq!(
        std::fs::read(paths.control()).unwrap(),
        before,
        "an unchanged cadence must not rewrite control.json"
    );
}

#[test]
fn unchanged_cadence_still_completes_a_pending_autopilot_flip() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let paths = ProjectPaths::for_session(&root, "bot");
    state::write_control(
        &paths,
        &state::Control {
            human_cadence_s: Some(300),
            wake_generation: 0,
        },
    )
    .unwrap();
    let _held = hold_current_daemon(&reg_path, "pm-test");
    let mut app = loop_app(&reg_path);
    app.mode = UiMode::EditingCadence {
        root,
        id: "bot".into(),
        current: 300,
        input: Field::from("5m"),
        then_autopilot: true,
    };

    app.submit_cadence_edit();

    assert_eq!(
        state::read_json::<Config>(&paths.config())
            .unwrap()
            .autonomy,
        Tier::Autopilot,
        "{}",
        app.status
    );
    assert!(
        app.status.contains("Autopilot") && app.status.contains("every 5m"),
        "{}",
        app.status
    );
}

#[test]
fn typed_cadence_from_tasks_returns_to_the_task_view() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let paths = ProjectPaths::for_session(&root, "bot");
    let _held = hold_current_daemon(&reg_path, "pm-test");
    let mut app = loop_app(&reg_path);
    app.mode = UiMode::EditingCadence {
        root,
        id: "bot".into(),
        current: 300,
        input: Field::from("10m"),
        then_autopilot: true,
    };
    app.return_to_board_after_action = true;

    app.submit_cadence_edit();

    assert!(matches!(app.mode, UiMode::Board), "{:?}", app.mode);
    assert!(!app.return_to_board_after_action);
    assert_eq!(
        state::read_control(&paths).unwrap().human_cadence_s,
        Some(600)
    );
    assert_eq!(
        state::read_json::<Config>(&paths.config())
            .unwrap()
            .autonomy,
        Tier::Autopilot
    );
}
