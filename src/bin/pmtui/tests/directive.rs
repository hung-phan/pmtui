//! The standing DIRECTIVE (`i`): the PLAIN-text (keep-everything) parser it shares with the
//! brief, the atomic `directive.md` write, the DELIBERATE `^X` rescind (never an empty save), the
//! inline field's dead ends and autopilot gate, and the binding both surfaces advertise.
//!
//! The directive is the restrictive counterpart to the goal (`g`), so these mirror the goal
//! tests — with the one deliberate divergence the design locked in: an empty save KEEPS the
//! directive, and rescinding is a separate, explicit `^X`.

use super::*;

/// The contents of the inline directive BUFFER (panics if that is not the mode).
fn directive_input(app: &App) -> String {
    match &app.mode {
        UiMode::EditingDirective { input, .. } => input.text(),
        m => panic!("expected the inline directive field, got {m:?}"),
    }
}

// --- the pure parser + seed -------------------------------------------------

#[test]
fn directive_from_editor_buffer_keeps_everything_and_trims_outer() {
    // Plain text like the brief now: NOTHING is stripped (a `#` line is content), only the
    // surrounding whitespace/blank lines are trimmed — the SAME rule the brief follows.
    let raw = "\n\n  # a heading\nnever auto-approve a test edit.\n\t# a note\n\n";
    assert_eq!(
        directive_from_editor_buffer(raw),
        "# a heading\nnever auto-approve a test edit.\n\t# a note"
    );
    // A multi-line directive keeps its interior newlines/blank lines AND its `#` lines.
    let multi = "\
never auto-approve a test edit
# a heading kept on purpose

always run the full suite before merging
";
    let out = directive_from_editor_buffer(multi);
    assert_eq!(
        out,
        "never auto-approve a test edit\n# a heading kept on purpose\n\nalways run the full suite before merging"
    );
    assert!(out.contains("# a heading kept"), "a # line is kept: {out}");
    // Whitespace-only still parses to empty (⇒ keep the directive); a `#` line is content.
    assert!(directive_from_editor_buffer("   \n\n\t\n").is_empty());
    assert_eq!(directive_from_editor_buffer("# only this\n"), "# only this");
}

#[test]
fn directive_editor_seed_is_plain_text_with_no_guidance() {
    // The seed is JUST the current directive (plus a trailing newline) — no `#`-comment guidance
    // (it would round-trip back into the directive). The keep/rescind rules live on the overlay.
    let seed = directive_editor_seed("never auto-approve a test edit");
    assert_eq!(seed, "never auto-approve a test edit\n");
    assert!(!seed.contains('#'), "no guidance seeded: {seed}");
    assert_eq!(
        directive_from_editor_buffer(&seed),
        "never auto-approve a test edit",
        "seed round-trips back to just the directive"
    );
    // Empty directive → empty buffer, which parses back to empty ⇒ keep the current directive.
    assert_eq!(directive_editor_seed(""), "");
    assert!(directive_from_editor_buffer("").is_empty());
}

// --- apply + rescind (the file-only halves) ---------------------------------

#[test]
fn apply_directive_edit_writes_directive_md_atomically() {
    // A non-empty buffer replaces `directive.md` through the atomic write (temp + rename), so a
    // concurrent decider consult never reads a half-written directive — and no temp is left.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    seed_agent_loop(
        &sp,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        1000,
    )
    .unwrap();
    assert!(!sp.directive().exists(), "seed writes no directive");

    let text = "never auto-approve a test-file edit\nalways run the suite first";
    assert_eq!(
        apply_directive_edit(&sp.directive(), text).unwrap(),
        DirectiveEdit::Written {
            lines: text.lines().count()
        }
    );
    assert_eq!(
        std::fs::read_to_string(sp.directive()).unwrap(),
        text,
        "directive.md holds the whole directive the next consult will read"
    );
    let strays: Vec<String> = std::fs::read_dir(sp.state_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(strays.is_empty(), "temp files left behind: {strays:?}");
}

#[test]
fn apply_directive_edit_unchanged_writes_nothing() {
    // Saving the buffer untouched (modulo a trailing newline) is a no-op — no write at all.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    seed_agent_loop(
        &sp,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        1000,
    )
    .unwrap();
    state::write_text_atomic(&sp.directive(), "no destructive ops").unwrap();
    let mtime_before = std::fs::metadata(sp.directive())
        .unwrap()
        .modified()
        .unwrap();

    assert_eq!(
        apply_directive_edit(&sp.directive(), "no destructive ops\n").unwrap(),
        DirectiveEdit::Unchanged
    );
    assert_eq!(
        std::fs::metadata(sp.directive())
            .unwrap()
            .modified()
            .unwrap(),
        mtime_before,
        "an unchanged directive leaves directive.md untouched"
    );
}

#[test]
fn directive_edit_never_touches_the_ledger() {
    // THE INVARIANT: pmtui must NEVER write `state.json`. A directive edit rewrites
    // `directive.md` and nothing else — a parked Blocked session stays parked.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    seed_agent_loop(
        &sp,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        1000,
    )
    .unwrap();
    job::save(&sp, &AgentLoopState::fresh(Engine::Claude, Some(300), 1000)).unwrap();
    let mut l = job::load(&sp).unwrap().unwrap();
    l.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    l.open_stops = vec![open_stop("stop-1", pmstate::StopKind::Ambiguity)];
    job::save(&sp, &l).unwrap();

    let ledger_before = std::fs::read(sp.pmstate()).unwrap();
    let mtime_before = std::fs::metadata(sp.pmstate()).unwrap().modified().unwrap();

    assert_eq!(
        apply_directive_edit(&sp.directive(), "never delete production data").unwrap(),
        DirectiveEdit::Written { lines: 1 }
    );

    assert_eq!(
        std::fs::read(sp.pmstate()).unwrap(),
        ledger_before,
        "state.json bytes must be untouched — pmtui is never a ledger writer"
    );
    assert_eq!(
        std::fs::metadata(sp.pmstate()).unwrap().modified().unwrap(),
        mtime_before,
        "state.json was not even re-written with identical bytes"
    );
    let after = job::load(&sp).unwrap().unwrap();
    assert!(
        matches!(after.run, job::JobRun::Blocked { .. }),
        "a directive edit must not un-block a parked session"
    );
}

#[test]
fn rescind_directive_leaves_directive_absent() {
    // The explicit clear: `rescind_directive` REMOVES `directive.md` so the decider's fresh read
    // yields empty and no DIRECTIVE fence downstream. It reports whether one was actually there,
    // and rescinding again is a clean no-op (idempotent).
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    seed_agent_loop(
        &sp,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        1000,
    )
    .unwrap();
    state::write_text_atomic(&sp.directive(), "never auto-approve a test edit").unwrap();

    assert!(
        rescind_directive(&sp.directive()).unwrap(),
        "a directive was present, so it is reported rescinded"
    );
    assert!(
        !sp.directive().exists(),
        "directive.md is absent after a rescind (⇒ no fence downstream)"
    );
    assert!(
        !rescind_directive(&sp.directive()).unwrap(),
        "rescinding an absent directive is a clean no-op"
    );
}

// --- selected_directive resolver -------------------------------------------

#[test]
fn selected_directive_resolves_the_path_and_refuses_cleanly() {
    // Returns `(id, directive path, current text)` for a real on-disk session, seeded from what
    // is on disk; refuses (with an explanatory status, never a bare return) on the two dead ends.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    state::write_text_atomic(&sp.directive(), "no destructive ops").unwrap();

    let mut app = loop_app(&reg_path);
    let (id, path, current) = app
        .selected_directive("i sets a session's directive")
        .expect("resolves for a real session");
    assert_eq!(id, "bot");
    assert_eq!(path, sp.directive(), "targets the per-session directive.md");
    assert_eq!(current, "no destructive ops", "seeded from what is on disk");

    // Row present but its registry entry is gone → explanatory refusal, no path.
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.registry_path = PathBuf::from("/nonexistent/registry.json");
    assert!(
        app.selected_directive("i sets a session's directive")
            .is_none()
    );
    assert!(
        app.status.contains("bot") && app.status.contains("gone from the list"),
        "gone-entry refusal, got {:?}",
        app.status
    );

    // Nothing selected at all → the other dead end, also a status.
    let mut app = app_with(vec![], UiMode::Normal);
    assert!(
        app.selected_directive("i sets a session's directive")
            .is_none()
    );
    assert!(
        app.status.contains("nothing is selected"),
        "empty-list refusal, got {:?}",
        app.status
    );
}

// --- the inline field, end to end ------------------------------------------

#[test]
fn i_opens_the_field_and_the_editor_chord_hands_the_directive_to_the_editor() {
    // `i` opens the inline field seeded from the file the decider reads, and `^E` from INSIDE it
    // queues the editor against that same per-session `directive.md`.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    state::write_text_atomic(&sp.directive(), "no auto-approve").unwrap();

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
    match &app.mode {
        UiMode::EditingDirective {
            id,
            directive,
            input,
        } => {
            assert_eq!(id, "bot");
            assert_eq!(directive, &sp.directive(), "targets directive.md");
            // The BUFFER is the seed — there is no second copy of the disk text beside it to drift.
            assert_eq!(input.text(), "no auto-approve", "seeded from disk");
        }
        other => panic!("`i` should open the inline directive field, got {other:?}"),
    }
    assert!(
        app.pending_directive_edit.is_none(),
        "`i` must not reach $EDITOR on its own"
    );

    for c in " test edits".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    press_editor_chord(&mut app);
    let req = app
        .pending_directive_edit
        .as_ref()
        .expect("^X^E inside the field queues the editor");
    assert_eq!(
        req.current, "no auto-approve test edits",
        "the editor opens on what I typed"
    );
    assert_eq!(req.directive, sp.directive());
    assert_eq!(req.id, "bot");
    assert!(
        matches!(app.mode, UiMode::Normal),
        "escalating closes the field"
    );
    // Nothing hit disk on the way out — the editor drain is what writes.
    assert_eq!(
        std::fs::read_to_string(sp.directive()).unwrap(),
        "no auto-approve"
    );
}

#[test]
fn directive_prompt_operations_are_inert_outside_the_directive_field() {
    let mut app = app_with(vec![], UiMode::Normal);
    app.status = "unchanged".into();

    app.submit_directive_edit();
    app.escalate_directive_edit();
    app.rescind_directive_from_field();

    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(app.status, "unchanged");
    assert!(
        app.pending_directive_edit.is_none(),
        "an editor request must only come from an open directive field"
    );

    app.begin_directive_edit();
    assert!(
        app.status.contains("nothing is selected"),
        "the public entry point must explain an empty selection: {}",
        app.status
    );
}

#[test]
fn a_multi_line_directive_opens_in_the_field_and_the_editor_gets_all_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let current = "never deploy\nwithout explicit approval";
    state::write_text_atomic(&sp.directive(), current).unwrap();

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
    // BOTH LINES OPEN IN THE FIELD. This asserted the opposite while the field was one line: it opened
    // empty and `^E` was the only way to see the directive it was about to replace.
    assert_eq!(
        directive_input(&app),
        current,
        "the whole directive must open IN the field"
    );

    press_editor_chord(&mut app);
    let req = app
        .pending_directive_edit
        .as_ref()
        .expect("^X^E queues the directive editor");
    assert_eq!(
        req.current, current,
        "an untouched field must hand the editor the complete directive"
    );
    assert_eq!(req.directive, sp.directive());
}

#[test]
fn nonempty_directive_saves_report_unchanged_and_write_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    state::write_text_atomic(&sp.directive(), "keep this rule").unwrap();
    let mtime_before = std::fs::metadata(sp.directive())
        .unwrap()
        .modified()
        .unwrap();

    let mut unchanged = loop_app(&reg_path);
    handle_key(&mut unchanged, KeyCode::Char('i'), KeyModifiers::NONE);
    handle_key(&mut unchanged, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        unchanged.status.contains("unchanged") && unchanged.status.contains("nothing written"),
        "{}",
        unchanged.status
    );
    assert_eq!(
        std::fs::metadata(sp.directive())
            .unwrap()
            .modified()
            .unwrap(),
        mtime_before,
        "an unchanged nonempty save must not rewrite the file"
    );

    let blocking_parent = dir.path().join("not-a-directory");
    std::fs::write(&blocking_parent, "blocks directory creation").unwrap();
    let invalid_path = blocking_parent.join("directive.md");
    let mut failed = loop_app(&reg_path);
    failed.mode = UiMode::EditingDirective {
        id: "bot".into(),
        directive: invalid_path.clone(),
        input: directive_buf("new rule"),
    };
    failed.submit_directive_edit();
    assert!(
        failed.status.contains("directive write failed") && failed.status.contains("directive.md"),
        "the write failure must identify the operation: {}",
        failed.status
    );
    assert!(
        !invalid_path.exists(),
        "a failed save must not leave a partial directive"
    );
}

#[test]
fn inline_directive_empty_save_keeps_it_and_the_rescind_chord_clears_it() {
    // THE LOCKED DECISION: an empty save KEEPS the directive (no accidental wipe), and rescinding
    // is the DELIBERATE, explicit `^X` — a distinct key, never "delete the text and save".
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    state::write_text_atomic(&sp.directive(), "keep this rule").unwrap();
    let mtime_before = std::fs::metadata(sp.directive())
        .unwrap()
        .modified()
        .unwrap();

    // Empty save keeps it.
    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
    for _ in 0.."keep this rule".len() {
        handle_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(directive_input(&app), " ");
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        std::fs::read_to_string(sp.directive()).unwrap(),
        "keep this rule",
        "an empty save must NOT wipe the directive"
    );
    assert_eq!(
        std::fs::metadata(sp.directive())
            .unwrap()
            .modified()
            .unwrap(),
        mtime_before,
        "nothing was written at all"
    );
    assert!(
        app.status.contains("kept") && app.status.contains("^X"),
        "empty save says kept and points at ^X: {}",
        app.status
    );

    // `^X` in the field rescinds — deliberately, from a freshly opened field.
    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
    press_rescind_chord(&mut app);
    assert!(
        matches!(app.mode, UiMode::Normal),
        "rescind closes the field"
    );
    assert!(
        !sp.directive().exists(),
        "^X leaves directive.md absent (⇒ no fence downstream)"
    );
    assert!(
        app.status.contains("rescinded"),
        "and it says so: {}",
        app.status
    );
}

#[test]
fn directive_rescind_reports_absent_files_and_remove_failures() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);

    let mut absent = loop_app(&reg_path);
    handle_key(&mut absent, KeyCode::Char('i'), KeyModifiers::NONE);
    press_rescind_chord(&mut absent);
    assert!(
        absent.status.contains("no directive") && absent.status.contains("nothing to rescind"),
        "{}",
        absent.status
    );

    let directory = dir.path().join("directive-is-a-directory");
    std::fs::create_dir(&directory).unwrap();
    let mut failed = loop_app(&reg_path);
    failed.mode = UiMode::EditingDirective {
        id: "bot".into(),
        directive: directory.clone(),
        input: directive_buf(""),
    };
    failed.rescind_directive_from_field();
    assert!(
        failed.status.contains("directive rescind failed"),
        "{}",
        failed.status
    );
    assert!(
        directory.is_dir(),
        "a failed rescind must leave the unexpected path untouched"
    );
}

#[test]
fn inline_directive_cancels_with_esc_and_types_reserved_characters() {
    // Esc backs out without writing, and while the field is up the Normal-mode keys are just
    // text: `?`/`i`/`q` type literally and do not fire their dashboard actions.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    state::write_text_atomic(&sp.directive(), "rule").unwrap();

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
    for c in "?iq".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(
        directive_input(&app),
        "rule?iq",
        "reserved keys type literally"
    );
    assert!(
        !app.should_quit,
        "`q` must not quit while typing a directive"
    );

    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal), "Esc closes the field");
    assert!(app.status.contains("cancelled"), "{}", app.status);
    assert_eq!(
        std::fs::read_to_string(sp.directive()).unwrap(),
        "rule",
        "a cancelled edit writes nothing"
    );
}

#[test]
fn the_directive_key_is_autopilot_only() {
    // Same rule and reasoning as `g`: `directive.md` is read only by the decider consult, which
    // never runs on a row pmd does not drive. So the chip is hidden AND the key refuses on
    // Standard, naming the key (`m`) that makes it matter.
    let standard = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    let auto = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);

    assert!(
        line_text(&keybar_line(&auto, 200)).contains("Directive"),
        "an autopilot row must advertise i"
    );
    assert!(
        !line_text(&keybar_line(&standard, 200)).contains("Directive"),
        "a standard row must not advertise a directive its decider never reads: {}",
        line_text(&keybar_line(&standard, 200))
    );

    // THE KEY: refuses on standard, names `m`.
    let mut app = standard;
    handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Normal),
        "i must not open on standard"
    );
    assert!(
        app.status.contains("press m"),
        "the refusal must point at the key that makes a directive matter: {}",
        app.status
    );

    // …and OPENS on autopilot (needs a real on-disk session).
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::EditingDirective { .. }),
        "i must open on an autopilot row, got {:?} ({})",
        app.mode,
        app.status
    );
}

#[test]
fn the_directive_key_is_in_bindings_so_the_keybar_and_help_agree() {
    // Exactly ONE Normal-mode directive key, in the one table both surfaces read, and its help
    // names the two chords the field adds.
    let rows: Vec<&Binding> = BINDINGS
        .iter()
        .filter(|b| b.scope == Scope::Normal && b.help.contains("directive"))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one Normal-mode directive key, found {:?}",
        rows.iter().map(|b| b.key).collect::<Vec<_>>()
    );
    let row = rows[0];
    assert_eq!(row.key, "i", "the directive key is lower-case `i`");
    assert!(!row.label.is_empty(), "no keybar chip label for `i`");
    assert!(
        row.help.contains("^E") && row.help.contains("^X"),
        "the help must name the editor + rescind chords: {:?}",
        row.help
    );

    let mut v = autopilot_loop_view("bot");
    v.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    let app = app_with(vec![v], UiMode::Normal);
    let bar = line_text(&keybar_line(&app, 260));
    assert!(bar.contains("i  Directive"), "no directive chip: {bar}");
}

// --- the inline field's rendering ------------------------------------------

#[test]
fn inline_directive_field_renders_at_every_size_without_panicking() {
    // Fixed-size overlay clamped to the frame: a 1x1 area degrades to the too-small line, and
    // every narrow width in between stays inside the frame (bottom-pinned caret, like the goal).
    let v = agent_loop_view("a-very-long-session-id-that-truncates");
    // The BUFFER is what varies now (it used to be the disk text behind a read-only preview): empty so
    // the placeholder draws, one line, several lines, and one line far wider than any frame.
    for buffer in [
        "",
        "one line directive",
        "line one\nline two\nline three",
        &"x".repeat(300),
    ] {
        let app = app_with(
            vec![v.clone()],
            UiMode::EditingDirective {
                id: v.id.clone(),
                directive: PathBuf::from("/nonexistent/directive.md"),
                input: directive_buf(buffer),
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
                .unwrap_or_else(|e| panic!("render directive field {w}x{h}: {e}"));
        }
    }
}

#[test]
fn inline_directive_field_shows_the_directive_it_is_editing_and_its_keys() {
    // What the human needs on screen: which session, the directive ITSELF (in the buffer, editable),
    // and the keys — including the distinct `^X^R rescind`.
    let app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingDirective {
            id: "bot".into(),
            directive: PathBuf::from("/nonexistent/directive.md"),
            input: directive_buf("no destructive ops without asking"),
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("Directive \u{b7} bot"),
        "no titled field: {screen}"
    );
    assert!(
        screen.contains("no destructive ops without asking"),
        "the buffer must be drawn: {screen}"
    );
    for hint in ["enter save", "^X^E $EDITOR", "^X^R rescind", "esc cancel"] {
        assert!(screen.contains(hint), "key hint {hint:?} missing: {screen}");
    }
    for note in ["writes directive.md", "empty keeps it", "^X^R rescinds"] {
        assert!(screen.contains(note), "note {note:?} missing: {screen}");
    }
}

/// RESCIND TAKES BOTH KEYS. `^X` alone is the prefix `^X^E` starts with, so it must not clear a
/// standing directive on its own — and the key after a stray `^X` has to behave normally.
#[test]
fn a_bare_ctrl_x_neither_rescinds_nor_eats_the_next_key() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    state::write_text_atomic(&sp.directive(), "no force pushes").unwrap();

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert!(
        matches!(app.mode, UiMode::EditingDirective { .. }),
        "the prefix alone must not close the field: {:?}",
        app.mode
    );
    assert!(sp.directive().exists(), "the prefix alone must not rescind");

    // The key after the spent prefix types, rather than being swallowed or read as a chord.
    handle_key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE);
    assert_eq!(directive_input(&app), "no force pushes!");

    // …and `^R` on its own is the library's redo, not a rescind.
    handle_key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(sp.directive().exists(), "bare ^R must not rescind either");

    press_rescind_chord(&mut app);
    assert!(!sp.directive().exists(), "^X^R rescinds: {}", app.status);
    assert!(app.status.contains("rescinded"), "{}", app.status);
}

/// The directive field saves EVERY line, and bare `^E` is end-of-line inside it.
#[test]
fn the_directive_field_takes_the_terminals_chords_and_saves_every_line() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
    for c in "never deploy".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
    for c in "without approval".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    // `^A` then `^E`: to the start of the second line and back to its end, so the insert lands there
    // rather than at the buffer's end. A field where `^E` opened an editor could not do this.
    handle_key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    handle_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    for c in " first".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(
        directive_input(&app),
        "never deploy\nwithout approval first"
    );
    assert!(
        matches!(app.mode, UiMode::EditingDirective { .. }),
        "^E must be end-of-line, not the editor: {:?}",
        app.mode
    );

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        std::fs::read_to_string(sp.directive()).unwrap().trim(),
        "never deploy\nwithout approval first",
        "{}",
        app.status
    );
}
