//! The create form: field navigation and editing, the autonomy dial's default, the rows
//! the dial hides on Standard, and the goal field — its one-line display, its multiline
//! render, and the Ctrl-E that hands the brief to an editor.

use super::*;

#[test]
fn create_form_navigation_and_editing() {
    let mut f = CreateForm::new();
    assert_eq!(f.field, CreateForm::GOAL);
    f.field = CreateForm::ENGINE;
    // Engine field toggles.
    assert_eq!(f.engine, Engine::Claude);
    f.adjust(true);
    assert_eq!(f.engine, Engine::Codex);
    // Worker Model field (index 1) is a ←→ stepper: pre-seed one choice so → steps the model value
    // from (default) to the first row.
    f.next_field();
    assert_eq!(f.field, CreateForm::WORKER_MODEL);
    f.model_choices = vec![ModelInfo {
        label: "Opus".into(),
        value: "opus-value".into(),
    }];
    f.adjust(true);
    assert_eq!(f.worker_model.as_deref(), Some("opus-value"));
    // Move to the directory (text) field (index 2) and type.
    f.next_field();
    assert_eq!(f.field, CreateForm::DIRECTORY);
    f.dir = Field::new();
    for c in "/tmp/proj".chars() {
        f.type_char(c);
    }
    assert_eq!(f.dir.as_str(), "/tmp/proj");
    // Optional display name is a text field between directory and autonomy.
    f.next_field();
    assert_eq!(f.field, CreateForm::NAME);
    f.type_char('N');
    assert_eq!(f.name.as_str(), "N");
    // Autonomy (tier) field cycles — the one dial.
    f.next_field();
    assert_eq!(f.field, CreateForm::AUTONOMY);
    // Set AUTOPILOT so the Goal field exists for the rest of this walk (it is
    // Autopilot-only now — see `create_form_hides_the_goal_on_standard`).
    f.tier = Tier::Standard;
    f.adjust(true);
    assert_eq!(f.tier, Tier::Autopilot);
    // Goal is a text field; toggle-field typing is ignored elsewhere.
    f.next_field();
    assert_eq!(f.field, CreateForm::GOAL);
    f.type_char('g');
    f.type_char('o');
    assert_eq!(f.goal.as_str(), "go");
    // Cadence is a toggle field: ←/→ nudges it by a minute, floored at 60s.
    f.next_field();
    assert_eq!(f.field, CreateForm::CADENCE);
    let before = f.cadence_s;
    f.adjust(true);
    assert_eq!(f.cadence_s, before + 60);
    f.adjust(false);
    assert_eq!(f.cadence_s, before);
    // Decider is a toggle field (6): ←/→ flips the decider engine (direction-agnostic).
    f.next_field();
    assert_eq!(f.field, CreateForm::DECIDER);
    assert_eq!(f.decider_engine, Engine::Claude);
    f.adjust(true);
    assert_eq!(f.decider_engine, Engine::Codex);
    f.adjust(false);
    assert_eq!(f.decider_engine, Engine::Claude);
    // Decider Model is the LAST field (index 7), APPENDED after Decider — shown here because
    // the walk switched to Autopilot above. A ←→ stepper like the worker model: → steps the value.
    f.next_field();
    assert_eq!(f.field, CreateForm::DECIDER_MODEL);
    f.decider_model_choices = vec![ModelInfo {
        label: "Sonnet".into(),
        value: "sonnet-value".into(),
    }];
    f.adjust(true);
    assert_eq!(f.decider_model.as_deref(), Some("sonnet-value"));
    // Wrap around at the last field.
    f.next_field();
    assert_eq!(f.field, CreateForm::ENGINE);
    f.prev_field();
    assert_eq!(f.field, CreateForm::DECIDER_MODEL);
}

#[test]
fn create_form_defaults_to_standard_autonomy() {
    // The single Autonomy dial lands on Standard — the collaborative default.
    let f = CreateForm::new();
    assert_eq!(f.tier, Tier::Standard);
}

#[test]
fn create_form_text_edits_follow_the_caret_and_stop_at_boundaries() {
    let mut f = CreateForm::new();
    f.field = CreateForm::DIRECTORY;
    f.dir = Field::from("ab");
    assert!(f.is_text_field());

    f.caret_home();
    f.caret_left();
    f.backspace();
    assert_eq!(f.dir.as_str(), "ab", "start-boundary edits are no-ops");
    assert_eq!(f.dir.caret(), 0);

    f.delete_forward();
    assert_eq!(
        f.dir.as_str(),
        "b",
        "Delete removes the character at the caret"
    );
    assert_eq!(f.dir.caret(), 0, "forward delete does not move the caret");

    f.paste("é🙂");
    assert_eq!(f.dir.as_str(), "é🙂b");
    assert_eq!(f.dir.caret(), 2, "paste advances by characters, not bytes");
    f.caret_right();
    f.caret_right();
    f.delete_forward();
    assert_eq!(f.dir.as_str(), "é🙂b", "end-boundary edits are no-ops");
    assert_eq!(f.dir.caret(), 3);

    f.backspace();
    f.caret_left();
    f.type_char('x');
    assert_eq!(f.dir.as_str(), "éx🙂");
    assert_eq!(f.dir.caret(), 2);
    f.caret_end();
    assert_eq!(f.dir.caret(), 3);
}

#[test]
fn standard_message_uses_the_existing_text_editor_surface() {
    let mut f = CreateForm::new();
    f.tier = Tier::Standard;
    f.field = CreateForm::GOAL;
    f.goal = Field::from("keep");
    assert!(
        f.is_text_field(),
        "Standard exposes the optional Message field"
    );

    f.type_char('!');
    f.paste(" more");
    f.backspace();
    assert_eq!(f.goal.as_str(), "keep! mor");
}

#[test]
fn create_form_navigation_recovers_from_each_now_hidden_field() {
    for hidden in [
        CreateForm::CADENCE,
        CreateForm::DECIDER,
        CreateForm::DECIDER_MODEL,
    ] {
        let mut forward = CreateForm::new();
        forward.field = hidden;
        forward.next_field();
        assert_eq!(
            forward.field, 0,
            "forward navigation from hidden field {hidden} wraps to Engine"
        );

        let mut backward = CreateForm::new();
        backward.field = hidden;
        backward.prev_field();
        assert_eq!(
            backward.field,
            CreateForm::GOAL,
            "backward navigation from hidden field {hidden} lands on Message"
        );
    }
}

#[test]
fn create_form_adjustments_respect_numeric_and_cycle_boundaries() {
    let mut f = CreateForm::new();
    f.cadence_s = 0;
    f.adjust_cadence(false);
    assert_eq!(f.cadence_s, 60, "cadence is floored at one minute");
    f.cadence_s = u64::MAX - 30;
    f.adjust_cadence(true);
    assert_eq!(f.cadence_s, u64::MAX, "cadence increase saturates");

    f.field = CreateForm::ENGINE;
    f.engine = Engine::Codex;
    f.adjust(false);
    assert_eq!(f.engine, Engine::Claude);

    f.field = CreateForm::AUTONOMY;
    f.tier = Tier::Standard;
    f.adjust(false);
    assert_eq!(f.tier, Tier::Autopilot, "reverse adjustment wraps autonomy");
    f.adjust(false);
    assert_eq!(f.tier, Tier::Standard);
}

// --- `step_model`: the ←→ model stepper over [(default)] ++ choices --------------
fn step_choices() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            label: "Opus".into(),
            value: "v-opus".into(),
        },
        ModelInfo {
            label: "Sonnet".into(),
            value: "v-sonnet".into(),
        },
        ModelInfo {
            label: "Haiku".into(),
            value: "v-haiku".into(),
        },
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
    assert_eq!(
        step_model(Some("not-in-catalog"), &ch, true).as_deref(),
        Some("v-opus")
    );
}

#[test]
fn the_autopilot_prompts_show_their_key_lines() {
    // ANTI-DRIFT on a bug this project has now shipped TWICE: `draw_overlay_frame` drops a
    // bottom-border hint too wide for its frame rather than clipping it mid-word, so a hint one
    // column too long renders NOWHERE and no test that checks the argument would notice. Both
    // prompts in `m`'s chain are pinned here, on real cells.
    let goal = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingGoal {
            id: "bot".into(),
            brief: PathBuf::from("/nonexistent/brief.md"),
            current: "ship it".into(),
            input: goal_buf("ship it"),
            then_autopilot: true,
        },
    );
    let cadence = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingCadence {
            id: "bot".into(),
            root: PathBuf::from("/nonexistent"),
            current: 300,
            input: Field::new(),
            then_autopilot: true,
        },
    );
    for (what, app, needle) in [
        ("goal", &goal, "enter = autopilot ON"),
        ("cadence", &cadence, "enter = autopilot ON"),
    ] {
        let mut t = Terminal::new(TestBackend::new(120, 24)).unwrap();
        t.draw(|f| render(f, app)).expect("render");
        let screen = screen_text(&t);
        assert!(
            screen.contains(needle),
            "the {what} prompt's key line must FIT its frame and render: {screen}"
        );
        assert!(screen.contains("esc"), "…and name the way out: {screen}");
    }
}

#[test]
fn create_form_shows_message_but_hides_autopilot_fields_on_standard() {
    // *"when the mode is standard, we don't need to display goal. only when user selects
    // autopilot, then we need goal"* and *"if it is standard, we don't need to have cadence
    // too"*. Both describe the harness's own heartbeat inputs, so on Standard they configure
    // things that never happen — neither shown nor focusable. Both halves matter: a field that
    // renders but cannot be reached, or can be reached but does not render, is its own bug.
    let mut f = CreateForm::new();
    assert_eq!(f.tier, Tier::Standard);
    assert!(
        f.shows_field(CreateForm::GOAL),
        "Standard uses the shared field as Message"
    );
    assert!(!f.shows_cadence(), "Standard hides heartbeat fields");

    // Autonomy advances to Message, then skips the Autopilot-only fields.
    f.field = CreateForm::AUTONOMY;
    f.next_field();
    assert_eq!(f.field, CreateForm::GOAL);
    f.next_field();
    assert_eq!(
        f.field,
        CreateForm::ENGINE,
        "next_field must skip Autopilot-only fields"
    );
    // …and backwards from Engine wraps to Message, not to any hidden field.
    f.prev_field();
    assert_eq!(f.field, CreateForm::GOAL, "prev_field must skip them too");

    // Flip to Autopilot and both reappear in the walk.
    f.tier = Tier::Autopilot;
    assert!(f.shows_field(CreateForm::GOAL) && f.shows_cadence());
    f.field = CreateForm::AUTONOMY;
    f.next_field();
    assert_eq!(
        f.field,
        CreateForm::GOAL,
        "Autopilot restores the Goal field"
    );
    f.next_field();
    assert_eq!(
        f.field,
        CreateForm::CADENCE,
        "…and the Cadence field after it"
    );
}

#[test]
fn the_create_form_renders_message_on_standard_and_goal_on_autopilot() {
    // The same buffer has honest labels for its two launch semantics.
    let mut form = CreateForm::new();
    form.field = CreateForm::ENGINE;
    let app = app_with(vec![], UiMode::Creating(form.clone()));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render standard form");
    assert!(screen_text(&t).contains("Message"), "{}", screen_text(&t));
    assert!(!screen_text(&t).contains("Goal"), "{}", screen_text(&t));

    form.tier = Tier::Autopilot;
    let app = app_with(vec![], UiMode::Creating(form));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render autopilot form");
    assert!(
        screen_text(&t).contains("Goal"),
        "Autopilot must show a Goal row: {}",
        screen_text(&t)
    );
}

#[test]
fn goal_field_display_collapses_multiline_to_one_bounded_line() {
    // A multi-line goal must render to a SINGLE line (no embedded newline) with a
    // "(+N more lines)" count, so the create-form field row can't break.
    let goal = "First line of the brief.\nsecond\nthird\nfourth";
    let shown = goal_field_display(goal);
    assert!(!shown.contains('\n'), "must be a single line: {shown:?}");
    assert!(shown.contains("First line of the brief."), "{shown}");
    assert!(shown.contains("(+3 more lines)"), "{shown}");

    // A very long first line is truncated (bounded), still single-line.
    let long = format!("{}\nmore", "x".repeat(200));
    let shown = goal_field_display(&long);
    assert!(!shown.contains('\n'));
    assert!(shown.chars().count() < 80, "bounded: {}", shown.len());
    assert!(shown.contains('…'), "truncated: {shown}");
}

#[test]
fn goal_field_display_singleline_and_empty_unchanged() {
    // Empty advertises the editor + inline typing; a one-liner shows verbatim with
    // the inline text cursor (the quick one-liner path is untouched).
    let empty = goal_field_display("");
    assert!(
        empty.contains("Ctrl+E") && empty.contains("$EDITOR"),
        "{empty}"
    );
    assert_eq!(goal_field_display("just type this"), "just type this_");
}

#[test]
fn renders_create_form_with_multiline_goal_without_panicking() {
    // A multi-line goal in the form must lay out on a normal terminal without
    // panicking (the field row shows the bounded summary).
    let mut form = CreateForm::new();
    // Autopilot, where the shared intent row is labelled Goal.
    form.tier = Tier::Autopilot;
    form.goal = "Paragraph one.\n\nParagraph two with more detail.\nAnd a third line.".into();
    form.field = CreateForm::GOAL;
    let app = app_with(vec![], UiMode::Creating(form));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render multi-line goal");
    let screen = screen_text(&terminal);
    assert!(screen.contains("more lines"), "summary shown: no panic");
}

#[test]
fn ctrl_e_on_goal_field_requests_brief_edit_others_ignore() {
    // Ctrl+E while the Goal field is focused queues a brief edit seeded with the
    // current goal; on any other field it's ignored (no pending edit).
    let mut form = CreateForm::new();
    form.field = CreateForm::GOAL;
    form.goal = "partial one-liner".into();
    let mut app = app_with(vec![], UiMode::Creating(form));
    handle_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    let req = app
        .pending_brief_edit
        .as_ref()
        .expect("Ctrl+E on Goal queues an edit");
    assert_eq!(
        req.goal, "partial one-liner",
        "seeded with the current goal"
    );

    // On the Directory field (index 2), Ctrl+E is a no-op.
    let mut form = CreateForm::new();
    form.field = CreateForm::DIRECTORY;
    let mut app = app_with(vec![], UiMode::Creating(form));
    handle_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert!(
        app.pending_brief_edit.is_none(),
        "Ctrl+E off the Goal field is ignored"
    );
}

#[test]
fn plain_e_on_goal_field_still_types_inline() {
    // A plain 'e' (no CONTROL) on the Goal field must still type inline — the
    // quick one-liner path is preserved; Ctrl+E does not swallow ordinary typing.
    let mut form = CreateForm::new();
    // Autopilot, where the shared intent row is labelled Goal (Standard labels it Message).
    form.tier = Tier::Autopilot;
    form.field = CreateForm::GOAL;
    let mut app = app_with(vec![], UiMode::Creating(form));
    for c in "edit".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    match &app.mode {
        UiMode::Creating(f) => assert_eq!(f.goal.as_str(), "edit", "inline typing preserved"),
        _ => panic!("form should still be open"),
    }
    assert!(
        app.pending_brief_edit.is_none(),
        "plain typing must not queue an editor edit"
    );
}

#[test]
fn decider_field_is_autopilot_only_and_defaults_to_claude() {
    let mut form = CreateForm::new();
    assert_eq!(form.decider_engine, Engine::Claude);
    // Standard: the field is not shown / not navigable (like Goal + Cadence).
    form.tier = Tier::Standard;
    assert!(!form.shows_field(CreateForm::DECIDER));
    // Autopilot: shown, and ←/→ on it flips the engine.
    form.tier = Tier::Autopilot;
    assert!(form.shows_field(CreateForm::DECIDER));
    form.field = CreateForm::DECIDER;
    form.adjust(true);
    assert_eq!(form.decider_engine, Engine::Codex);
    form.adjust(true);
    assert_eq!(form.decider_engine, Engine::Claude);
}

#[test]
fn tab_navigation_visits_the_decider_field_only_on_autopilot() {
    let mut form = CreateForm::new();
    form.tier = Tier::Autopilot;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..CreateForm::FIELDS {
        form.next_field();
        seen.insert(form.field);
    }
    assert!(seen.contains(&CreateForm::DECIDER));
    let mut form2 = CreateForm::new(); // Standard
    let mut seen2 = std::collections::HashSet::new();
    for _ in 0..CreateForm::FIELDS {
        form2.next_field();
        seen2.insert(form2.field);
    }
    assert!(!seen2.contains(&CreateForm::DECIDER));
}

#[test]
fn worker_model_steps_with_left_right() {
    // The Worker Model field (index 1) defaults to (default) = None. ← / → STEP the stored value
    // through the catalog and wrap back to None — the uniform toggle interaction, no list mode.
    let mut f = CreateForm::new();
    assert_eq!(f.worker_model, None, "no model chosen by default");
    assert!(f.model_choices.is_empty());
    assert!(
        f.shows_field(CreateForm::WORKER_MODEL),
        "the Model field always shows"
    );

    f.model_choices = vec![
        ModelInfo {
            label: "Opus".into(),
            value: "global.anthropic.claude-opus-5".into(),
        },
        ModelInfo {
            label: "Sonnet".into(),
            value: "global.anthropic.claude-sonnet-5".into(),
        },
    ];
    f.field = CreateForm::WORKER_MODEL;
    f.adjust(true); // (default) -> first
    assert_eq!(
        f.worker_model.as_deref(),
        Some("global.anthropic.claude-opus-5")
    );
    f.adjust(true); // -> second (a second single step walks on, never collapses)
    assert_eq!(
        f.worker_model.as_deref(),
        Some("global.anthropic.claude-sonnet-5")
    );
    f.adjust(true); // wraps back to (default) = None
    assert_eq!(f.worker_model, None);
    f.adjust(false); // backward from (default) lands on the LAST model
    assert_eq!(
        f.worker_model.as_deref(),
        Some("global.anthropic.claude-sonnet-5")
    );
    // ↑↓ move FIELDS, not the value: from WORKER_MODEL, next_field lands on Directory.
    f.next_field();
    assert_eq!(
        f.field, 2,
        "up/down move between fields; the model value is unchanged by nav"
    );
    assert_eq!(
        f.worker_model.as_deref(),
        Some("global.anthropic.claude-sonnet-5")
    );
}

#[test]
fn toggling_the_engine_resets_the_worker_model() {
    // The model list is per-engine, so a model picked for one engine may not exist for the other.
    // `toggle_engine` clears the choice back to the engine default; the App repopulates the list.
    let mut f = CreateForm::new();
    f.model_choices = vec![ModelInfo {
        label: "Opus".into(),
        value: "global.anthropic.claude-opus-5".into(),
    }];
    f.field = CreateForm::WORKER_MODEL;
    f.adjust(true);
    assert!(f.worker_model.is_some());
    f.toggle_engine();
    assert_eq!(
        f.worker_model, None,
        "an engine flip clears the previously-picked model"
    );
}

#[test]
fn the_create_form_renders_the_worker_model_row() {
    // The RENDER half, on real cells: the Model row shows `(default)` when unset and the picked
    // model's LABEL once chosen. It shows at ANY tier (a worker always launches), so Standard is
    // enough to prove it.
    let mut form = CreateForm::new();
    form.model_choices = vec![ModelInfo {
        label: "Opus 4.8".into(),
        value: "global.anthropic.claude-opus-4-8".into(),
    }];
    let app = app_with(vec![], UiMode::Creating(form.clone()));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render default model");
    let screen = screen_text(&t);
    assert!(screen.contains("Model"), "Model row label shown: {screen}");
    assert!(
        screen.contains("(default)"),
        "unset shows (default): {screen}"
    );

    form.worker_model = Some("global.anthropic.claude-opus-4-8".into());
    let app = app_with(vec![], UiMode::Creating(form));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render picked model");
    assert!(
        screen_text(&t).contains("Opus 4.8"),
        "the picked model's label renders: {}",
        screen_text(&t)
    );
}

#[test]
fn submit_create_writes_the_selected_worker_model_to_the_entry() {
    // The model chosen at create seeds the new registry entry's `worker_model`, verbatim.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let proj = std::fs::canonicalize({
        let p = dir.path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        p
    })
    .unwrap();
    let mut app = creating_agent_loop_app(&reg_path, &proj, Engine::Claude, "", 300);
    if let UiMode::Creating(f) = &mut app.mode {
        f.worker_model = Some("global.anthropic.claude-opus-5".into());
    }
    app.submit_create();
    let reg = Registry::load(&reg_path).unwrap();
    let e = reg
        .projects
        .iter()
        .find(|p| p.root == proj)
        .expect("entry created");
    assert_eq!(
        e.worker_model.as_deref(),
        Some("global.anthropic.claude-opus-5"),
        "the chosen worker model lands on the entry"
    );
}

#[test]
fn decider_model_steps_with_left_right() {
    // The Decider Model field (index 7) mirrors the Worker Model field over the DECIDER engine's
    // catalog: defaults to (default) = None; ← / → STEP the stored value through the catalog and
    // wrap back to None; a decider-engine flip resets it. Autopilot-only, so the tier is set here.
    // Choices PRE-SEEDED for determinism (no host CLI).
    let mut f = CreateForm::new();
    f.tier = Tier::Autopilot;
    assert_eq!(f.decider_model, None, "no decider model chosen by default");
    assert!(f.decider_model_choices.is_empty());

    f.decider_model_choices = vec![
        ModelInfo {
            label: "Sonnet".into(),
            value: "global.anthropic.claude-sonnet-5".into(),
        },
        ModelInfo {
            label: "Haiku".into(),
            value: "global.anthropic.claude-haiku-4-5".into(),
        },
    ];
    f.field = CreateForm::DECIDER_MODEL;
    f.adjust(true); // (default) -> first
    assert_eq!(
        f.decider_model.as_deref(),
        Some("global.anthropic.claude-sonnet-5")
    );
    f.adjust(true); // -> second (a second single step walks on, never collapses)
    assert_eq!(
        f.decider_model.as_deref(),
        Some("global.anthropic.claude-haiku-4-5")
    );
    f.adjust(true); // wraps back to (default) = None
    assert_eq!(f.decider_model, None, "the top row is (default) = None");
    f.adjust(false); // backward from (default) lands on the LAST model
    assert_eq!(
        f.decider_model.as_deref(),
        Some("global.anthropic.claude-haiku-4-5")
    );

    // A decider-engine flip clears the picked model (the list is per-decider-engine; the App
    // repopulates `decider_model_choices` after the flip).
    f.toggle_decider_engine();
    assert_eq!(
        f.decider_model, None,
        "a decider-engine flip clears the previously-picked decider model"
    );
}

#[test]
fn decider_model_field_is_autopilot_only() {
    // AUTOPILOT-ONLY, exactly like the Decider engine field — a decider model on a Standard row
    // configures a consult that never runs. Hidden AND unfocusable on Standard.
    let mut form = CreateForm::new();
    form.tier = Tier::Standard;
    assert!(
        !form.shows_field(CreateForm::DECIDER_MODEL),
        "Standard hides the Decider Model field"
    );
    form.tier = Tier::Autopilot;
    assert!(
        form.shows_field(CreateForm::DECIDER_MODEL),
        "Autopilot shows the Decider Model field"
    );
    // Tab reaches it on Autopilot, never on Standard.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..CreateForm::FIELDS {
        form.next_field();
        seen.insert(form.field);
    }
    assert!(seen.contains(&CreateForm::DECIDER_MODEL));
    let mut standard = CreateForm::new(); // Standard
    let mut seen2 = std::collections::HashSet::new();
    for _ in 0..CreateForm::FIELDS {
        standard.next_field();
        seen2.insert(standard.field);
    }
    assert!(!seen2.contains(&CreateForm::DECIDER_MODEL));
}

#[test]
fn the_create_form_renders_the_decider_model_row_only_on_autopilot() {
    // The RENDER half, on real cells: the "Decider Model" row appears on Autopilot (with `(default)`
    // unset, and the picked model's LABEL once chosen) and is ABSENT on Standard.
    let mut form = CreateForm::new(); // Standard
    let app = app_with(vec![], UiMode::Creating(form.clone()));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render standard form");
    assert!(
        !screen_text(&t).contains("Decider Model"),
        "Standard must not show a Decider Model row: {}",
        screen_text(&t)
    );

    form.tier = Tier::Autopilot;
    form.decider_model_choices = vec![ModelInfo {
        label: "Sonnet 4.6".into(),
        value: "global.anthropic.claude-sonnet-4-6".into(),
    }];
    let app = app_with(vec![], UiMode::Creating(form.clone()));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render autopilot form");
    let screen = screen_text(&t);
    assert!(
        screen.contains("Decider Model"),
        "Autopilot must show the Decider Model row (full label): {screen}"
    );
    assert!(
        !screen.contains("D-Model"),
        "the old abbreviation must be gone: {screen}"
    );
    assert!(
        screen.contains("(default)"),
        "unset decider model shows (default): {screen}"
    );

    form.decider_model = Some("global.anthropic.claude-sonnet-4-6".into());
    let app = app_with(vec![], UiMode::Creating(form));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render(f, &app))
        .expect("render picked decider model");
    assert!(
        screen_text(&t).contains("Sonnet 4.6"),
        "the picked decider model's label renders: {}",
        screen_text(&t)
    );
}

#[test]
fn submit_create_on_autopilot_writes_the_selected_decider_model_to_config() {
    // An AUTOPILOT create with a chosen Decider Model seeds `config.decider_model` verbatim (the
    // field is autopilot-only, so this is the only tier that carries it). Pre-seed nothing on the
    // host: the value is set on the form directly for determinism.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let proj = std::fs::canonicalize({
        let p = dir.path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        p
    })
    .unwrap();
    let mut app = creating_loop_app(
        &reg_path,
        &proj,
        Engine::Claude,
        Tier::Autopilot,
        "ship it",
        300,
    );
    if let UiMode::Creating(f) = &mut app.mode {
        f.decider_model = Some("global.anthropic.claude-sonnet-4-6".into());
    }
    app.submit_create();
    let reg = Registry::load(&reg_path).unwrap();
    let e = reg
        .projects
        .iter()
        .find(|p| p.root == proj)
        .expect("entry created");
    let cfg: Config =
        state::read_json(&ProjectPaths::for_session(&e.root, &e.id).config()).unwrap();
    assert_eq!(
        cfg.decider_model.as_deref(),
        Some("global.anthropic.claude-sonnet-4-6"),
        "the chosen decider model lands on config.json"
    );
}

// --- model-field key routing through the create form -------------------------

/// Build an App parked on the create form with the worker `model_choices` pre-seeded and the
/// Worker Model field focused — the deterministic starting point for the stepper key tests.
fn worker_model_app(choices: Vec<ModelInfo>) -> App {
    let mut form = CreateForm::new();
    form.model_choices = choices;
    form.field = CreateForm::WORKER_MODEL;
    app_with(vec![], UiMode::Creating(form))
}

fn form_of(app: &App) -> &CreateForm {
    match &app.mode {
        UiMode::Creating(f) => f,
        _ => panic!("expected the create form to be open"),
    }
}

#[test]
fn create_key_right_on_worker_model_steps_the_value() {
    // → on a focused Worker Model field STEPS its stored value and STAYS on the field (the uniform
    // toggle interaction); ↑↓ instead MOVE between fields, they no longer drive a list.
    let mut app = worker_model_app(vec![
        ModelInfo {
            label: "Opus".into(),
            value: "global.anthropic.claude-opus-5".into(),
        },
        ModelInfo {
            label: "Sonnet".into(),
            value: "global.anthropic.claude-sonnet-5".into(),
        },
    ]);
    handle_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    let f = form_of(&app);
    assert_eq!(
        f.field,
        CreateForm::WORKER_MODEL,
        "→ steps the value, it does NOT move the form field"
    );
    assert_eq!(
        f.worker_model.as_deref(),
        Some("global.anthropic.claude-opus-5"),
        "the first → step lands on the first catalog value"
    );
    // ← wraps back from the first model to (default) = None.
    handle_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(
        form_of(&app).worker_model,
        None,
        "← from the first model returns to (default)"
    );
    // Down MOVES the field now (it does not select a model).
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        form_of(&app).field,
        2,
        "↓ moves off the model field to the Directory"
    );
}

#[test]
fn create_key_typing_on_worker_model_is_inert() {
    // A model field is a pure stepper now: an ordinary letter is inert (there is no free-text
    // "custom model" entry), while space STEPS it like → does.
    let mut app = worker_model_app(vec![ModelInfo {
        label: "Opus".into(),
        value: "global.anthropic.claude-opus-5".into(),
    }]);
    for c in "xyz".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(
        form_of(&app).worker_model,
        None,
        "typing letters is inert on a stepper field"
    );
    handle_key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(
        form_of(&app).worker_model.as_deref(),
        Some("global.anthropic.claude-opus-5"),
        "space steps the value like →"
    );
}

#[test]
fn create_key_backspace_on_worker_model_is_inert() {
    // Backspace is a text-field edit; on a model stepper it is a no-op (never touches the value).
    let mut app = worker_model_app(vec![ModelInfo {
        label: "Opus".into(),
        value: "global.anthropic.claude-opus-5".into(),
    }]);
    handle_key(&mut app, KeyCode::Right, KeyModifiers::NONE); // step to a real value first
    assert!(form_of(&app).worker_model.is_some());
    handle_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(
        form_of(&app).worker_model.as_deref(),
        Some("global.anthropic.claude-opus-5"),
        "backspace does not edit a stepper value"
    );
}

#[test]
fn create_key_tab_off_a_model_field_lands_on_the_next_shown_field() {
    // Tab on a focused model field MOVES fields (it does not edit the value). On Standard the field
    // after Worker Model (1) is the Directory (2).
    let mut app = worker_model_app(vec![]);
    handle_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        form_of(&app).field,
        2,
        "Tab off the model field advances to the next shown field (Directory)"
    );
}

#[test]
fn create_key_paste_into_a_worker_model_is_inert() {
    // A bracketed paste while a model (stepper) field is focused is a no-op — a stepper takes no
    // text, so the create-form paste path (`form.paste`) leaves the value alone.
    let mut app = worker_model_app(vec![]);
    handle_paste(&mut app, "pasted-model");
    assert_eq!(
        form_of(&app).worker_model,
        None,
        "a paste onto a model stepper field is inert"
    );
}

#[test]
fn create_key_right_on_decider_model_steps_on_autopilot() {
    // The Decider Model field is autopilot-only; on Autopilot, → steps its value just like the
    // worker one.
    let mut form = CreateForm::new();
    form.tier = Tier::Autopilot;
    form.decider_model_choices = vec![ModelInfo {
        label: "Sonnet".into(),
        value: "global.anthropic.claude-sonnet-5".into(),
    }];
    form.field = CreateForm::DECIDER_MODEL;
    let mut app = app_with(vec![], UiMode::Creating(form));
    handle_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    let f = form_of(&app);
    assert_eq!(f.field, CreateForm::DECIDER_MODEL, "→ stayed on the field");
    assert_eq!(
        f.decider_model.as_deref(),
        Some("global.anthropic.claude-sonnet-5")
    );
}

#[test]
fn the_focused_model_field_draws_its_own_catalog() {
    // The FOCUSED model field expands its OWN catalog inline: focusing Worker Model shows the
    // WORKER catalog, focusing Decider Model shows the DECIDER catalog, and the OTHER (unfocused)
    // model row collapses to a `< (default) >` summary. Distinct labels ("Wonly"/"Donly") prove the
    // inline list tracks the focused field and never leaks the other engine's list.
    let mut form = CreateForm::new();
    form.tier = Tier::Autopilot; // so the Decider Model field is shown
    form.model_choices = vec![ModelInfo {
        label: "Wonly".into(),
        value: "w".into(),
    }];
    form.decider_model_choices = vec![ModelInfo {
        label: "Donly".into(),
        value: "d".into(),
    }];

    form.field = CreateForm::WORKER_MODEL;
    let app = app_with(vec![], UiMode::Creating(form.clone()));
    let mut t = Terminal::new(TestBackend::new(100, 40)).unwrap();
    t.draw(|f| render(f, &app)).expect("render worker focused");
    let worker = screen_text(&t);
    assert!(
        worker.contains("Wonly"),
        "the focused Worker Model draws the worker catalog: {worker}"
    );
    assert!(
        !worker.contains("Donly"),
        "…and not the decider catalog (its row is an unfocused summary): {worker}"
    );

    form.field = CreateForm::DECIDER_MODEL;
    let app = app_with(vec![], UiMode::Creating(form));
    let mut t = Terminal::new(TestBackend::new(100, 40)).unwrap();
    t.draw(|f| render(f, &app)).expect("render decider focused");
    let decider = screen_text(&t);
    assert!(
        decider.contains("Donly"),
        "the focused Decider Model draws the decider catalog: {decider}"
    );
    assert!(
        !decider.contains("Wonly"),
        "…and the unfocused Worker Model row is a summary, not an expanded list: {decider}"
    );
}

// --- The single-column create popup + the INLINE (below-the-field) model list RENDER --------

#[test]
fn focused_worker_model_expands_inline_below_with_current_marked() {
    // Focus the Worker Model field → its catalog expands INLINE below the summary (no side panel):
    // (default) + the catalog labels, with the current value marked ● (bold). Stepping ←→ moves ●.
    let mut t = Terminal::new(TestBackend::new(160, 40)).unwrap();
    let mut f = CreateForm::new();
    f.model_choices = vec![
        ModelInfo {
            label: "Opus".into(),
            value: "v-opus".into(),
        },
        ModelInfo {
            label: "Sonnet".into(),
            value: "v-sonnet".into(),
        },
    ];
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let screen = screen_text(&t);
    assert!(
        screen.contains("(default)") && screen.contains("Opus") && screen.contains("Sonnet"),
        "the inline list shows (default) + the catalog labels: {screen}"
    );
    assert!(
        screen.contains("\u{25cf} (default)"),
        "current = (default) marked ●: {screen}"
    );
    assert!(
        !screen.contains("Models"),
        "no side-panel title remains: {screen}"
    );
    // Step once → ● moves to Opus (inline).
    f.adjust(true);
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    assert!(
        screen_text(&t).contains("\u{25cf} Opus"),
        "←→ moves the inline ●"
    );
}

#[test]
fn create_popup_height_is_fixed_when_focusing_a_model_field() {
    // The window must NOT resize as you move onto a model field — the model list scrolls inside a
    // reserved area. Border-row count (rows containing '│') is the popup height proxy.
    let height = |field: usize| -> usize {
        let mut t = Terminal::new(TestBackend::new(160, 44)).unwrap();
        let mut f = CreateForm::new();
        f.model_choices = vec![
            ModelInfo {
                label: "Opus".into(),
                value: "v-opus".into(),
            },
            ModelInfo {
                label: "Sonnet".into(),
                value: "v-sonnet".into(),
            },
        ];
        f.field = field;
        t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
        screen_rows(&t)
            .iter()
            .filter(|(s, _)| s.contains('\u{2502}'))
            .count()
    };
    assert_eq!(
        height(2),
        height(CreateForm::WORKER_MODEL),
        "popup height is fixed across focus"
    );
    // …and the reserve is COMPACT (4 rows): the popup stays small so it fits within the terminal
    // rather than pushing its bottom off shorter screens. The border-row count stays well under what
    // a large reserve would give (a 14-row reserve produced ~21 at 160×44), so a regression back to a
    // tall reserve fails this ceiling. (Standard tier here: Engine/Model/Directory/Autonomy + divider
    // + the 4-row reserve + chrome.)
    assert!(
        height(CreateForm::WORKER_MODEL) <= 16,
        "the compact 4-row model reserve keeps the popup small (got {})",
        height(CreateForm::WORKER_MODEL)
    );
}

#[test]
fn create_popup_fits_a_short_terminal_and_shows_its_last_field() {
    // Regression guard for the bug the compact reserve fixes: a TALL model-list reserve inflated the
    // autopilot popup past shorter terminals, and the top-anchored Paragraph clipped its own bottom
    // rows with no way to scroll — so the last field (Decider Model) fell off the screen. With the
    // 4-row reserve the popup fits a short terminal and every field renders. Autopilot (the max-field
    // tier) with the Worker Model list expanded (the tallest content) is the worst case.
    let mut t = Terminal::new(TestBackend::new(120, 28)).unwrap();
    let mut f = CreateForm::new();
    f.tier = Tier::Autopilot;
    f.model_choices = (0..20)
        .map(|i| ModelInfo {
            label: format!("model-{i}"),
            value: format!("v-{i}"),
        })
        .collect();
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let text = screen_text(&t);
    assert!(
        text.contains("Decider Model"),
        "the popup's last field must stay visible on a short terminal (no bottom clip): {text}"
    );
}

#[test]
fn create_popup_shows_a_group_divider() {
    let mut t = Terminal::new(TestBackend::new(140, 42)).unwrap();
    let f = CreateForm::new();
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let has_rule = screen_rows(&t).iter().any(|(s, _)| {
        // Trim the centering margin first, THEN strip the card's border bars (see the sibling test).
        let body = s
            .trim()
            .trim_start_matches('\u{2502}')
            .trim_end_matches('\u{2502}')
            .trim();
        !body.is_empty() && body.chars().all(|c| c == '\u{2500}')
    });
    assert!(has_rule, "a dim horizontal rule divides the fields");
}

#[test]
fn focused_model_list_aligns_under_the_value() {
    let mut t = Terminal::new(TestBackend::new(160, 40)).unwrap();
    let mut f = CreateForm::new();
    f.model_choices = vec![
        ModelInfo {
            label: "Opus".into(),
            value: "v-opus".into(),
        },
        ModelInfo {
            label: "Sonnet".into(),
            value: "v-sonnet".into(),
        },
    ];
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let row = screen_rows(&t)
        .into_iter()
        .map(|(s, _)| s)
        .find(|s| s.contains("\u{25cf} (default)"))
        .expect("current row present");
    let dot = row.find('\u{25cf}').unwrap();
    let bar = row.find('\u{2502}').unwrap(); // left card border
    let col_in_body = dot - bar - 1; // columns from just inside the border
    assert!(
        col_in_body >= VALUE_COL,
        "list ● aligns under the value column (>={VALUE_COL}), got {col_in_body}"
    );
}

#[test]
fn create_form_unfocused_decider_model_shows_only_its_summary() {
    // With focus elsewhere (the Directory field), the Decider Model row collapses to one summary
    // line showing the picked model's LABEL — no list, no `▸`/`○` bullets from an open list.
    let mut form = CreateForm::new();
    form.tier = Tier::Autopilot; // the Decider Model row is autopilot-only
    form.decider_model_choices = vec![ModelInfo {
        label: "Sonnet 4.6".into(),
        value: "global.anthropic.claude-sonnet-4-6".into(),
    }];
    form.decider_model = Some("global.anthropic.claude-sonnet-4-6".into());
    form.field = CreateForm::DIRECTORY; // neither model field is focused
    let app = app_with(vec![], UiMode::Creating(form));
    let mut t = Terminal::new(TestBackend::new(100, 40)).unwrap();
    t.draw(|f| render(f, &app))
        .expect("render unfocused decider");
    let screen = screen_text(&t);
    assert!(
        screen.contains("Decider Model"),
        "the Decider Model row is labelled: {screen}"
    );
    assert!(
        screen.contains("< Sonnet 4.6 >"),
        "the unfocused Decider Model shows its picked label as a summary: {screen}"
    );
    assert!(
        !screen.contains('\u{25b8}'),
        "no field is a focused combo, so no ▸ list cursor is drawn: {screen}"
    );
}

#[test]
fn create_form_renders_at_every_size_without_panicking() {
    // The single-column popup must lay out at ANY terminal size — a 1×1 clamp up through a roomy
    // 200×50 — with a model field focused, so the inline-list path (and its window/overflow +
    // width/height clamps) is swept at every size. No panic is the assertion.
    let mut form = CreateForm::new();
    form.tier = Tier::Autopilot;
    form.model_choices = vec![
        ModelInfo {
            label: "Opus 4.8".into(),
            value: "global.anthropic.claude-opus-4-8".into(),
        },
        ModelInfo {
            label: "Sonnet 5".into(),
            value: "global.anthropic.claude-sonnet-5".into(),
        },
    ];
    form.decider_model_choices = form.model_choices.clone();
    for &(w, h) in &[
        (1u16, 1u16),
        (2, 2),
        (3, 3),
        (5, 4),
        (10, 6),
        (20, 8),
        (40, 12),
        (80, 24),
        (120, 30),
        (200, 50),
    ] {
        for field in [CreateForm::WORKER_MODEL, CreateForm::DECIDER_MODEL, 2] {
            form.field = field;
            let app = app_with(vec![], UiMode::Creating(form.clone()));
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| render(f, &app))
                .unwrap_or_else(|_| panic!("render must not panic at {w}x{h} field {field}"));
        }
        // …and once more with a catalog LONGER than the 14-row viewport and a model field focused,
        // so the SCROLLBAR clamp path (its Rect clamped into `inner`) is exercised at every size —
        // including the tiny clamps where the list rows don't fit. No panic is the assertion.
        let mut overflow = form.clone();
        overflow.model_choices = (0..20)
            .map(|i| ModelInfo {
                label: format!("model-{i}"),
                value: format!("v-{i}"),
            })
            .collect();
        overflow.field = CreateForm::WORKER_MODEL;
        let app = app_with(vec![], UiMode::Creating(overflow));
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| render(f, &app))
            .unwrap_or_else(|_| panic!("overflow scrollbar must not panic at {w}x{h}"));
    }
}

#[test]
fn focused_model_list_over_viewport_shows_a_scrollbar_not_more_text() {
    // A catalog LONGER than the 4-row viewport must replace the old dim `… N more` line with a real
    // scrollbar down the list's right edge. The thumb glyph `█` (\u{2588}) is UNIQUE to the scrollbar
    // (the popup keybar carries `↑↓`/`←→`, so those two glyphs are NOT proof of a scrollbar — only
    // the thumb is), so we key the assertion on it: it fails cleanly if the scrollbar regresses.
    let mut t = Terminal::new(TestBackend::new(160, 44)).unwrap();
    let mut f = CreateForm::new();
    f.model_choices = (0..20)
        .map(|i| ModelInfo {
            label: format!("model-{i}"),
            value: format!("v-{i}"),
        })
        .collect();
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let text: String = screen_rows(&t)
        .iter()
        .map(|(s, _)| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !text.contains("more"),
        "the '… N more' cue must be replaced by the scrollbar: {text}"
    );
    assert!(
        text.contains('\u{2588}'),
        "an overflowing model list must render a scrollbar thumb (█): {text}"
    );
}

#[test]
fn fits_model_list_shows_no_scrollbar_and_no_more() {
    // A catalog that FITS the viewport (list_len 3 ≤ 4) draws neither the old `… N more` line nor a
    // scrollbar. We assert on the unique thumb glyph `█`: the popup keybar always carries `↑↓`, so
    // only the thumb's ABSENCE proves no scrollbar was drawn.
    let mut t = Terminal::new(TestBackend::new(160, 44)).unwrap();
    let mut f = CreateForm::new();
    f.model_choices = (0..2)
        .map(|i| ModelInfo {
            label: format!("model-{i}"),
            value: format!("v-{i}"),
        })
        .collect();
    f.field = CreateForm::WORKER_MODEL;
    t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
    let text: String = screen_rows(&t)
        .iter()
        .map(|(s, _)| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !text.contains("more"),
        "a fitting list shows no '… N more': {text}"
    );
    assert!(
        !text.contains('\u{2588}'),
        "a fitting list must NOT draw a scrollbar thumb (█): {text}"
    );
}

#[test]
fn scrollbar_thumb_tracks_the_selected_model() {
    // `ScrollbarState::position(current)` must drive the thumb: with the selection at the TOP of a
    // 20-item catalog (worker_model = None → current 0) the `█` thumb sits higher than with the
    // selection at the BOTTOM (worker_model = Some(last) → current 20). Proves the thumb follows ●.
    let thumb_row = |wm: Option<&str>| -> usize {
        let mut t = Terminal::new(TestBackend::new(160, 44)).unwrap();
        let mut f = CreateForm::new();
        f.model_choices = (0..20)
            .map(|i| ModelInfo {
                label: format!("model-{i}"),
                value: format!("v-{i}"),
            })
            .collect();
        f.field = CreateForm::WORKER_MODEL;
        f.worker_model = wm.map(str::to_string);
        t.draw(|fr| render_create(fr, fr.area(), &f)).unwrap();
        screen_rows(&t)
            .iter()
            .position(|(s, _)| s.contains('\u{2588}'))
            .expect("an overflowing list must draw a thumb")
    };
    let top = thumb_row(None); // current = 0, ● on (default)
    let bottom = thumb_row(Some("v-19")); // current = 20, ● on the last model
    assert!(
        bottom > top,
        "the thumb must sit lower when the last model is selected (top row {top}, bottom row {bottom})"
    );
}

// --- M9: edit a LIVE session's goal (`g`) ------------------------------------
