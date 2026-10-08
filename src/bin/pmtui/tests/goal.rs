//! The inline goal field: the brief it writes atomically, the empty save that keeps the
//! previous goal, the escalation to `$EDITOR` carrying what was typed, the caret tail,
//! and its dead ends.

use super::*;

#[test]
fn inline_goal_edit_writes_the_brief_atomically_and_takes_effect() {
    // The happy path: `G` seeds the field from `brief.md`, typing extends it, and
    // Enter replaces the file the next nudge reads — through `apply_goal_edit`, so
    // the write is the atomic one (no half-written brief for a concurrent nudge to
    // read as an empty mandate) and leaves no temp file behind.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot); // brief == "goal"
    let sp = ProjectPaths::for_session(&root, "bot");
    let ledger_before = std::fs::read(sp.pmstate()).unwrap();

    let mut app = loop_app(&reg_path);
    open_and_type(&mut app, " two");
    assert_eq!(goal_input(&app), "goal two", "seeded from disk, then typed");

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Normal),
        "saving closes the field, got {:?}",
        app.mode
    );
    assert_eq!(
        std::fs::read_to_string(sp.brief()).unwrap(),
        "goal two",
        "brief.md holds the whole new goal"
    );
    assert!(
        app.status.contains("goal updated") && app.status.contains("next check-in"),
        "the status says when it takes effect: {}",
        app.status
    );
    // THE INVARIANT: pmtui never writes the ledger, inline path included.
    assert_eq!(
        std::fs::read(sp.pmstate()).unwrap(),
        ledger_before,
        "state.json must be untouched"
    );
    let strays: Vec<String> = std::fs::read_dir(sp.state_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(strays.is_empty(), "temp files left behind: {strays:?}");
}

#[test]
fn inline_goal_edit_empty_save_keeps_the_previous_goal() {
    // The editor path treats an empty buffer as "keep the current goal" (and its seed
    // comment promises it). The inline field MUST agree: were it to write an empty
    // brief, the next nudge's `read_to_string(..).unwrap_or_default()` would hand the
    // agent the no-goal fallback instead of its mandate.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot); // brief == "goal"
    let sp = ProjectPaths::for_session(&root, "bot");
    let mtime_before = std::fs::metadata(sp.brief()).unwrap().modified().unwrap();

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    // Backspace the seeded goal away, then leave whitespace behind — whitespace-only
    // is the same case (the engine cannot tell it from empty).
    for _ in 0.."goal".len() {
        handle_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(goal_input(&app), " ");
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        std::fs::read_to_string(sp.brief()).unwrap(),
        "goal",
        "the previous goal survives an empty save"
    );
    assert_eq!(
        std::fs::metadata(sp.brief()).unwrap().modified().unwrap(),
        mtime_before,
        "nothing was written at all — not even identical bytes"
    );
    assert!(
        app.status.contains("goal kept"),
        "and it says so: {}",
        app.status
    );
}

#[test]
fn inline_goal_edit_escapes_to_the_editor_with_what_was_typed() {
    // Ctrl+E is the create form's escalation, reused verbatim: it hands the typed
    // text to the SAME `BriefEdit`/$EDITOR path `g` queues (so there is still one
    // editor and one write), and closes the overlay so `run()` restores into Normal.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");

    let mut app = loop_app(&reg_path);
    open_and_type(&mut app, " plus more");
    press_editor_chord(&mut app);

    assert!(
        matches!(app.mode, UiMode::Normal),
        "escalating closes the field, got {:?}",
        app.mode
    );
    let req = app
        .pending_brief_edit
        .as_ref()
        .expect("^X^E queues the editor");
    assert_eq!(
        req.goal, "goal plus more",
        "the editor opens on what I typed"
    );
    match &req.target {
        BriefEditTarget::Session {
            id,
            brief,
            then_autopilot,
        } => {
            assert_eq!(id, "bot");
            assert_eq!(brief, &sp.brief(), "the same file `g` would target");
            assert!(!then_autopilot, "`g` must not carry a tier flip");
        }
        BriefEditTarget::CreateForm => panic!("^E here must target the live session"),
    }
    // Nothing hit disk on the way out: the editor drain is what writes.
    assert_eq!(std::fs::read_to_string(sp.brief()).unwrap(), "goal");
}

#[test]
fn inline_goal_edit_opens_a_multi_line_brief_in_the_field_and_escalates_with_all_of_it() {
    // A FIVE-LINE MANDATE OPENS IN THE FIELD, every line of it. This asserted the opposite until the
    // field became a `ratatui-textarea`: a one-line field could not edit five lines, so `g` opened
    // EMPTY and `^E` was the only way to see them. Saving straight away is therefore no longer the
    // "empty" case — it is a save of the same text, which `apply_goal_edit` reports as unchanged.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let multi = "ship vector search\n\nconstraints:\n- no new crates\n- keep tests green";
    state::write_text_atomic(&sp.brief(), multi).unwrap();

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(
        goal_input(&app),
        multi,
        "the whole brief must open IN the field"
    );
    // Saving straight away leaves the brief byte-identical. (A second App over the same files, so the
    // escalation below still starts from a freshly-opened field.)
    let mut saver = loop_app(&reg_path);
    handle_key(&mut saver, KeyCode::Char('g'), KeyModifiers::NONE);
    handle_key(&mut saver, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(std::fs::read_to_string(sp.brief()).unwrap(), multi);

    press_editor_chord(&mut app);
    assert_eq!(
        app.pending_brief_edit.as_ref().map(|r| r.goal.as_str()),
        Some(multi),
        "^E with nothing typed escalates with the brief as it stands"
    );
}

#[test]
fn inline_goal_edit_cancels_with_esc_and_types_reserved_characters() {
    // Esc backs out without writing, and while the field is up the Normal-mode keys
    // are just text: `?` must not open the help and `g`/`q` must not fire their
    // dashboard actions (the same rule the answer overlay and create form follow).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");

    let mut app = loop_app(&reg_path);
    open_and_type(&mut app, "?gq");
    assert_eq!(goal_input(&app), "goal?gq", "reserved keys type literally");
    assert!(!app.should_quit, "`q` must not quit while typing a goal");

    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal), "Esc closes the field");
    assert!(app.status.contains("cancelled"), "{}", app.status);
    assert_eq!(
        std::fs::read_to_string(sp.brief()).unwrap(),
        "goal",
        "a cancelled edit writes nothing"
    );
}

#[test]
fn inline_goal_edit_dead_ends_explain_themselves() {
    // Same two dead ends `g` has, and the same explanatory-status style: no panic,
    // no bare return, and no overlay opened over a row it could not write.
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.registry_path = PathBuf::from("/nonexistent/registry.json");
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(
        app.status.contains("bot") && app.status.contains("gone from the list"),
        "explanatory refusal, got {:?}",
        app.status
    );
    assert!(matches!(app.mode, UiMode::Normal), "no field opened");

    let mut app = app_with(vec![], UiMode::Normal);
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(
        app.status.contains("nothing is selected") && app.status.contains('g'),
        "empty-list refusal names its own key, got {:?}",
        app.status
    );
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn goal_prompt_operations_are_inert_outside_the_goal_field() {
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.status = "unchanged".into();

    app.submit_goal_edit();
    app.escalate_goal_edit();

    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(app.status, "unchanged");
    assert!(
        app.pending_brief_edit.is_none(),
        "an editor request must only come from an open goal field"
    );
}

#[test]
fn goal_update_status_keeps_an_open_stop_visible() {
    let mut v = autopilot_loop_view("bot");
    v.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    let app = app_with(vec![v], UiMode::Normal);

    let status = app.goal_edit_status("bot", Ok(GoalEdit::Written { lines: 1 }));
    assert!(
        status.contains("goal updated") && status.contains("still blocked"),
        "a goal edit must not imply that it answered the stop: {status}"
    );
    assert!(
        status.contains("press s"),
        "the status must name the action that resolves the stop: {status}"
    );

    let other = app.goal_edit_status("other", Ok(GoalEdit::Written { lines: 1 }));
    assert!(
        !other.contains("still blocked"),
        "a selected row's stop must not leak into another session's status: {other}"
    );
}

#[test]
fn the_one_goal_key_is_in_bindings_so_the_keybar_and_help_agree() {
    // The one table drives both surfaces, so being IN it is what makes the key
    // discoverable — this asserts the row exists AND that both surfaces show it.
    //
    // It also pins the SHAPE of the binding: exactly ONE Normal-mode goal key. The
    // shifted `G` twin is gone, and `^E` inside the field is the only route to
    // `$EDITOR`, so a second row here would be a second entry point to drift from.
    let goal_rows: Vec<&Binding> = BINDINGS
        .iter()
        .filter(|b| b.scope == Scope::Normal && b.help.contains("goal"))
        .collect();
    assert_eq!(
        goal_rows.len(),
        1,
        "expected exactly one Normal-mode goal key, found {:?}",
        goal_rows.iter().map(|b| b.key).collect::<Vec<_>>()
    );
    let row = goal_rows[0];
    assert_eq!(row.key, "g", "the goal key is lower-case `g`");
    assert!(!row.label.is_empty(), "no keybar chip label for `g`");
    assert!(
        row.help.contains("^E"),
        "the help must name the editor escalation: {:?}",
        row.help
    );

    let mut v = autopilot_loop_view("bot");
    v.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    let app = app_with(vec![v], UiMode::Normal);
    let bar = line_text(&keybar_line(&app, 160));
    assert!(bar.contains("g  Goal"), "no goal chip: {bar}");
    assert!(
        !bar.contains("G  Goal"),
        "the retired shifted twin is still advertised: {bar}"
    );
}

#[test]
fn inline_goal_field_renders_at_every_size_without_panicking() {
    // Fixed-size overlay clamped to the frame: a 1x1 area must degrade to the
    // too-small line, and every narrow width in between must stay inside the frame.
    let mut v = agent_loop_view("a-very-long-session-id-that-truncates");
    v.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    // The BUFFER is what varies now (it used to be the disk text behind a read-only preview): empty so
    // the placeholder draws, one line, several lines, and one line far wider than any frame.
    for buffer in [
        "",
        "one line goal",
        "line one\nline two\nline three",
        &"x".repeat(300),
    ] {
        let app = app_with(
            vec![v.clone()],
            UiMode::EditingGoal {
                id: v.id.clone(),
                brief: PathBuf::from("/nonexistent/brief.md"),
                current: "on disk".into(),
                input: goal_buf(buffer),
                then_autopilot: false,
            },
        );
        for (w, h) in [
            (1u16, 1u16),
            (20, 5),
            (24, 8),
            (26, 9),
            (30, 10),
            (50, 12),
            (80, 24),
            (200, 50),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| render(f, &app))
                .unwrap_or_else(|e| panic!("render goal field {w}x{h}: {e}"));
        }
    }
}

#[test]
fn inline_goal_field_shows_the_goal_it_is_editing_and_its_keys() {
    // What the human needs on screen: which session, the goal ITSELF (in the buffer, editable), and
    // the keys. This used to assert a read-only `current:` preview above a one-line caret, because a
    // one-line field could not hold a multi-line brief — the preview is the field now.
    let app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingGoal {
            id: "bot".into(),
            brief: PathBuf::from("/nonexistent/brief.md"),
            current: "ship vector search".into(),
            input: goal_buf("ship vector search fast"),
            then_autopilot: false,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("Goal \u{b7} bot"),
        "no titled field: {screen}"
    );
    assert!(
        screen.contains("ship vector search fast"),
        "the buffer must be drawn: {screen}"
    );
    for hint in ["enter save", "empty keeps it", "^X^E", "esc cancel"] {
        assert!(screen.contains(hint), "hint {hint:?} missing: {screen}");
    }

    // EVERY LINE OF A MULTI-LINE MANDATE IS ON SCREEN AND EDITABLE. The old field drew none of them:
    // it opened empty and reported "3 lines — ^E edits them all".
    let app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingGoal {
            id: "bot".into(),
            brief: PathBuf::from("/nonexistent/brief.md"),
            current: "a\nb\nc".into(),
            input: goal_buf("alpha\nbeta\ngamma"),
            then_autopilot: false,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    for line in ["alpha", "beta", "gamma"] {
        assert!(screen.contains(line), "line {line:?} missing: {screen}");
    }
    assert!(
        !screen.contains("lines \u{2014} ^E edits them all"),
        "nothing is clipped away from the human any more: {screen}"
    );

    // Past the height the overlay grows to, the LIBRARY scrolls and keeps the caret (at the end of the
    // buffer, where it opens) on screen — so the last line is visible and the first has scrolled off.
    let long: String = (1..=GOAL_EDIT_LINES + 4)
        .map(|i| format!("line {i}\n"))
        .collect();
    let app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingGoal {
            id: "bot".into(),
            brief: PathBuf::from("/nonexistent/brief.md"),
            current: long.clone(),
            input: goal_buf(long.trim()),
            then_autopilot: false,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains(&format!("line {}", GOAL_EDIT_LINES + 4)),
        "the caret's line must be on screen: {screen}"
    );
    assert!(
        !screen.contains("line 1\n") && !screen.contains("line 1 "),
        "a buffer taller than the frame scrolls rather than growing: {screen}"
    );
}

#[test]
fn inline_goal_field_keeps_the_caret_visible_on_a_short_terminal() {
    // The bug this pins: a multi-line brief on an 8-row terminal pushed the `>` caret (and
    // the write-note) off the bottom of a fixed-height body. Both are now BOTTOM-PINNED — the
    // goal PREVIEW yields rows when space is tight, never the field you type into.
    let app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingGoal {
            id: "bot".into(),
            brief: PathBuf::from("/nonexistent/brief.md"),
            current: "line one\nline two\nline three\nline four\nline five".into(),
            input: goal_buf("TYPEDINPUT"),
            then_autopilot: false,
        },
    );
    // Tall enough for the frame, short enough that the old fixed body clipped the caret.
    let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("TYPEDINPUT"),
        "the caret row must survive a short terminal: {screen}"
    );
    assert!(
        screen.contains("writes brief.md"),
        "the pinned write-note must survive too: {screen}"
    );
}

// The single-line input's scroll-and-caret rule now lives in `Field::caret_view`, tested
// directly in `input.rs` (`caret_view_*`); the old `caret_tail` helper it replaced is gone.

// --- M11: adaptive keybar + `?` help overlay ---------------------------------

#[test]
fn the_goal_key_is_autopilot_only() {
    // User: *"when we are not in autopilot mode, can we hide the g for goal on the session"*.
    //
    // The goal is `brief.md`, and the ONLY reader is the nudge prompt pmd injects each heartbeat —
    // which never runs on a row `pmd_drives_row` says nobody drives. On Standard the human at the
    // keyboard IS the steering, so the key would edit a file nothing consults. Same rule the
    // cadence got in m40, and the same two halves: the chip is hidden AND the key refuses, because
    // a hidden chip over a working key is the same inconsistency in reverse.
    let standard = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    let auto = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);

    // THE CHIP: offered on autopilot, gone on standard.
    assert!(
        line_text(&keybar_line(&auto, 200)).contains("Goal"),
        "an autopilot row must still advertise g"
    );
    assert!(
        !line_text(&keybar_line(&standard, 200)).contains("Goal"),
        "a standard row must not advertise a goal that steers nothing: {}",
        line_text(&keybar_line(&standard, 200))
    );

    // THE KEY: refuses on standard, and names the key that makes it matter rather than dead-ending.
    let mut app = standard;
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Normal),
        "g must not open the field on a standard row, got {:?}",
        app.mode
    );
    assert!(
        app.status.contains("press m"),
        "the refusal must point at the key that makes a goal matter: {}",
        app.status
    );

    // ...and still OPENS on autopilot, so the gate is the tier and nothing else. This half needs a
    // real on-disk session (the bare views above have no registry row or `brief.md`, which
    // `selected_brief` would refuse for an unrelated reason and hide the thing under test).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::EditingGoal { .. }),
        "g must still open on an autopilot row, got {:?} ({})",
        app.mode,
        app.status
    );
}

/// THE GOAL FIELD IS A READLINE BUFFER, and what it saves is what is in it — newlines included.
///
/// The chords themselves are `ratatui-textarea`'s and tested in `tests::composer`; what this proves is
/// that they reach it THROUGH the goal field's own handler (which used to intercept every `Char` for a
/// one-line `Field`) and that `brief.md` ends up holding the multi-line result.
#[test]
fn the_goal_field_takes_the_terminals_chords_and_saves_every_line() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    state::write_text_atomic(&sp.brief(), "ship vector search").unwrap();

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(
        goal_input(&app),
        "ship vector search",
        "opens on the mandate"
    );

    // `^W` kills the word behind the caret (which opens at the end), `^J` opens a line.
    handle_key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert_eq!(goal_input(&app), "ship vector ");
    handle_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
    for c in "and cite it".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(goal_input(&app), "ship vector \nand cite it");
    // `^A` is the start of the LINE, not of the buffer — readline's meaning, which is only available
    // because the editor moved to `^X^E`.
    handle_key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    handle_key(&mut app, KeyCode::Char('-'), KeyModifiers::NONE);
    assert_eq!(goal_input(&app), "ship vector \n-and cite it");

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal), "enter saves and closes");
    assert_eq!(
        std::fs::read_to_string(sp.brief()).unwrap().trim(),
        "ship vector \n-and cite it",
        "the file must hold BOTH lines: {}",
        app.status
    );
}
