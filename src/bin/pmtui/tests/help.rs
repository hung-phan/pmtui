//! The help overlay: `?` opening it, every bound normal-mode key being documented in it,
//! the scroll and the any-key close, and the tiny sizes it has to render at.

use super::*;

#[test]
fn question_mark_opens_the_help_overlay() {
    // `?` was unbound; it now opens the key reference, and the overlay lists the
    // bindings the adaptive bar may have dropped — including the newest one, `g`.
    let mut app = keybar_app();
    handle_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Help { scroll: 0 }),
        "`?` must open the help, got {:?}",
        app.mode
    );
    // Tall enough for every row: the overlay scrolls by design, and the point of this test is that each
    // of these keys is DOCUMENTED, not that the table happens to fit a particular terminal.
    let mut terminal = Terminal::new(TestBackend::new(100, 48)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render help");
    let screen = screen_text(&terminal);
    assert!(screen.contains("Keys"), "overlay title missing: {screen}");
    for row in [
        "NAVIGATION",
        "SESSION",
        "AUTONOMY",
        "OTHER",
        // `g` — the binding this overlay exists to keep discoverable.
        "goal inline",
        "with an open stop",
        // `r` — the newest binding, and the one most worth finding here: it is the
        // first chip the bar sheds after `d`. Asserted on the HEAD of the row: the
        // description column truncates at this width.
        "Restart the selected agent",
        "Quit pmtui",
        "Create form",
    ] {
        assert!(screen.contains(row), "help row missing: {row}");
    }
}

/// The overlay names the status log FILE. The status line is transient and the pane that used to hold
/// the history is gone, so "what did it say before?" is now answered by a path — and a path nothing
/// ever prints is a path nobody finds.
#[test]
fn the_help_overlay_says_where_the_status_log_is() {
    let mut app = keybar_app();
    app.status_log = std::path::PathBuf::from("/home/dev/.config/pmd/pmtui.log");
    app.mode = UiMode::Help { scroll: 0 };
    let mut terminal = Terminal::new(TestBackend::new(100, 44)).unwrap();

    terminal.draw(|f| render(f, &app)).expect("render help");

    let screen = screen_text(&terminal);
    // FIRST, not last: the body scrolls, and this overlay opens at row 1.
    assert!(
        screen.contains("STATUS LOG"),
        "the overlay does not mention the log: {screen}"
    );
    let rows: Vec<String> = screen_rows(&terminal)
        .into_iter()
        .map(|(row, _)| row)
        .collect();
    let heading = rows
        .iter()
        .position(|row| row.contains("STATUS LOG"))
        .expect("the log section");
    let first_group = rows
        .iter()
        .position(|row| row.contains("NAVIGATION"))
        .expect("the first key group");
    assert!(
        heading < first_group,
        "the log path must be visible without scrolling"
    );
    assert!(
        screen.contains("/home/dev/.config/pmd/pmtui.log"),
        "…and it must name the actual file: {screen}"
    );
}

#[test]
fn every_bound_normal_key_is_documented_in_the_help() {
    // ANTI-DRIFT. Probe every printable key in Normal mode: if pressing it changes
    // anything observable it is a real binding, and it MUST have a row in the one
    // key table (which is what the help overlay renders). Adding a key to
    // `handle_key` without a `BINDINGS` row fails here.
    let documented: String = BINDINGS
        .iter()
        .filter(|b| matches!(b.scope, Scope::Normal | Scope::HelpOnly) && !b.help.is_empty())
        .map(|b| b.key)
        .collect::<Vec<_>>()
        .join(" ");

    fn fingerprint(app: &App) -> String {
        format!(
            "{:?}|{}|{}|{}|{}|{}|{}",
            app.mode,
            app.status,
            app.selected,
            app.should_quit,
            app.projects.len(),
            app.pending_brief_edit.is_some(),
            app.pending_first_chat.is_some(),
        )
    }
    // Three rows with the middle one selected, so BOTH j and k move; an open stop
    // so `a` opens; a missing registry so the write actions land on an explanatory
    // status (still an observable change).
    let fresh = || {
        let mut app = app_with(
            (0..3)
                .map(|i| {
                    let mut v = agent_loop_view(&format!("s{i}"));
                    v.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
                    v
                })
                .collect(),
            UiMode::Normal,
        );
        app.selected = 1;
        app.registry_path = PathBuf::from("/nonexistent/registry.json");
        app
    };
    let base = fingerprint(&fresh());
    let mut bound = Vec::new();
    for c in ' '..='~' {
        let mut app = fresh();
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        if fingerprint(&app) != base {
            bound.push(c);
            assert!(
                documented.contains(c),
                "`{c}` is bound in Normal mode but has no row in BINDINGS \
                 (keybar and help would disagree). Documented keys: {documented}"
            );
        }
    }
    // Sanity: the probe really did find today's set, so a broken probe can't make
    // this test pass vacuously.
    // Every absence here is a deliberate removal's guard:
    //   no 'G' — the shifted goal twin went in m29 (`g` opens the field, and `^E`
    //            inside it reaches $EDITOR);
    //   no 't' — the autonomy dial moved to `m` (Mode);
    //   no 'o' — Send moved to `s`, after the word on its own chip.
    // ('p' was in that list until m36. It went in m15 for want of an undo, and came back
    // once `Enter` on a paused row was the undo.)
    //   'i' — the standing DIRECTIVE, the restrictive counterpart to `g`'s goal (it refuses on a
    //         Standard row, an observable status change, so the probe finds it here too).
    //   'v' — the DECISION LANE: review what autopilot decided across the fleet (opens a
    //         full-screen view, an observable mode change, so the probe finds it here too).
    //   'f' — fork the selected conversation into another managed session.
    //   Tab is checked below because it is not a character binding.
    //   'e' — open the DECIDER picker (engine + model); it lands on a status (autopilot-gated, and
    //         here the missing registry makes it report), so the probe finds it.
    //   'w' — open the WORKER model picker; on the fresh() app the missing registry makes it report
    //         "gone from the list" + refresh (an observable status/len change), so the probe finds it.
    //   '2' — SELECT the Task view (the numbered view controls the status bar draws). '1' selects
    //         the Session view, which is where the probe already is, so pressing it there changes
    //         nothing by design and the probe cannot see it; both are documented by the same
    //         `1/2/0` row. Tab is NOT checked here: it is no longer a Normal-mode key, and
    //         the create form documents its own Tab in `Scope::Create`.
    let mut expected = vec![
        '/', '0', '2', '?', 'R', 'a', 'c', 'd', 'e', 'f', 'g', 'i', 'j', 'k', 'm', 'n', 'p', 'q',
        'r', 's', 'v', 'w',
    ];
    expected.sort_unstable();
    bound.sort_unstable();
    assert_eq!(bound, expected, "unexpected Normal-mode binding set");

    // The non-character bindings are documented too: Enter as a key, the arrows in
    // the `↑↓/jk` chip, and Esc in the quit row's description.
    assert!(documented.contains("Enter"));
    assert!(documented.contains('↑') && documented.contains('↓'));
    let rendered: String = help_lines(60).iter().map(line_text).collect();
    assert!(
        rendered.contains("Esc"),
        "Esc-quits is undocumented: {rendered}"
    );
    // …and every documented key actually reaches the screen.
    for b in BINDINGS.iter().filter(|b| b.scope == Scope::Normal) {
        assert!(rendered.contains(b.key), "`{}` never rendered", b.key);
    }

    // THE OTHER DIRECTION, and the half that catches this project's recurring bug:
    // no ADVERTISED key may be unbound. A `BINDINGS` row is what both the keybar and
    // the help render, so a row for a key `handle_key` does not handle is exactly the
    // lie that shipped as "A toggles autopilot" after `A` was deleted.
    //
    // Only rows whose display form IS a single character are probeable this way;
    // `Enter`/`↑↓` are asserted above, and `j`/`k` are named explicitly because they
    // ride inside the `↑↓/jk` row rather than having one of their own.
    let mut advertised: Vec<char> = BINDINGS
        .iter()
        .filter(|b| b.scope == Scope::Normal)
        .filter_map(|b| {
            let mut cs = b.key.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            }
        })
        .chain(['j', 'k'])
        .collect();
    advertised.sort_unstable();
    advertised.dedup();
    for c in advertised {
        assert!(
            bound.contains(&c),
            "`{c}` is advertised by BINDINGS (so the keybar and `?` both offer it) \
             but nothing in Normal mode handles it. Really bound: {bound:?}"
        );
    }
}

#[test]
fn every_binding_row_reaches_the_keybar_or_the_help() {
    // ANTI-DRIFT on the table itself. A row is read by exactly two surfaces: a mode's keybar
    // (`scope_chips`/`normal_chips` match on `scope` and need a label) and the `?` overlay
    // (`help_lines` skips an empty `help`). `HelpOnly` has no bar, and the full-screen wake and
    // decision views draw their own footer instead of the `Wake` bar, so a row in either scope with
    // no help text is read by nothing — a dead row that only looks like documentation.
    let dead: Vec<String> = BINDINGS
        .iter()
        .filter(|b| {
            let on_a_bar = !matches!(b.scope, Scope::HelpOnly | Scope::Wake) && !b.label.is_empty();
            !on_a_bar && b.help.is_empty()
        })
        .map(|b| format!("{} {}", b.key, b.label))
        .collect();
    assert!(
        dead.is_empty(),
        "BINDINGS rows no keybar or help overlay reads: {dead:?}"
    );
}

#[test]
fn help_overlay_scrolls_and_any_other_key_closes_it() {
    let mut app = app_with(vec![], UiMode::Help { scroll: 0 });
    app.scroll_max.set(7);
    for (code, want) in [
        (KeyCode::Char('j'), 1),
        (KeyCode::Down, 2),
        (KeyCode::PageDown, 7), // clamped to the last rendered max
        (KeyCode::Char('k'), 6),
        (KeyCode::Up, 5),
        (KeyCode::PageUp, 0), // saturates at the top
        (KeyCode::End, 7),
        (KeyCode::Home, 0),
    ] {
        handle_key(&mut app, code, KeyModifiers::NONE);
        match app.mode {
            UiMode::Help { scroll } => assert_eq!(scroll, want, "after {code:?}"),
            ref m => panic!("scrolling must not close the help, got {m:?}"),
        }
    }
    // Esc closes…
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Normal),
        "Esc must close the help"
    );
    // …`?` toggles it shut…
    handle_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Normal),
        "`?` must close the help"
    );
    // …and `q` CLOSES it rather than quitting pmtui (documented behaviour: one
    // reflex `q` while reading the keys must not drop you out of the TUI).
    handle_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Normal),
        "`q` must close the help"
    );
    assert!(!app.should_quit, "`q` in the help must not quit pmtui");
    // Back on the dashboard it still quits.
    handle_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(app.should_quit, "`q` still quits from Normal mode");
}

#[test]
fn help_overlay_is_not_bound_outside_normal_mode() {
    // `?` is a legitimate typed character in the answer overlay and the create
    // form, and the confirm overlay promises "any other key cancels" — so it is
    // deliberately bound in Normal mode ONLY.
    let mut app = app_with(
        vec![view(
            "x",
            Posture::NeedsYou,
            vec![stop("s1", "publish", RiskClass::Low)],
        )],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    handle_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    match &app.mode {
        UiMode::Answering { input, .. } => {
            assert_eq!(input.as_str(), "?", "`?` must type into an answer")
        }
        m => panic!("`?` hijacked the answer overlay: {m:?}"),
    }

    let mut form = CreateForm::new();
    // Autopilot, where the shared intent row is labelled Goal: a plain `?` must type into it.
    form.tier = Tier::Autopilot;
    form.field = CreateForm::GOAL;
    let mut app = app_with(vec![], UiMode::Creating(form));
    handle_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    match &app.mode {
        UiMode::Creating(f) => assert_eq!(f.goal.as_str(), "?", "`?` must type into the goal"),
        m => panic!("`?` hijacked the create form: {m:?}"),
    }
}

#[test]
fn renders_help_overlay_at_tiny_sizes_without_panicking() {
    // Same sweep as `renders_tiny_terminal_without_panicking`: the overlay is
    // sized relative to the frame and clamped, so a 1x1 area degrades (the
    // too-small path) instead of panicking.
    let cases = [
        vec![],
        vec![view(
            "x",
            Posture::NeedsYou,
            vec![stop("s1", "publish", RiskClass::Hard)],
        )],
    ];
    for projects in cases {
        let app = app_with(projects, UiMode::Help { scroll: 3 });
        for (w, h) in [
            (1u16, 1u16),
            (20, 5),
            (24, 8),
            (26, 9),
            (30, 10),
            (60, 20),
            (100, 30),
            (200, 50),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| render(f, &app))
                .unwrap_or_else(|e| panic!("render help {w}x{h}: {e}"));
        }
    }
    // An absurd scroll offset can't index out of bounds: it sticks at the LAST
    // page (whose final row is the paging keys) rather than blanking the overlay.
    let app = app_with(vec![], UiMode::Help { scroll: usize::MAX });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render over-scroll");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("PgUp/PgDn"),
        "over-scroll should stick at the last page: {screen}"
    );
}

#[test]
fn help_rows_fit_the_overlay_width() {
    // Descriptions are truncated, never wrapped: one logical row == one screen row
    // is what keeps the scroll maths exact.
    for w in [10usize, 20, 40, 62, 120] {
        for line in help_lines(w) {
            assert!(
                line.width() <= w,
                "{w}-col row is {} wide: {line:?}",
                line.width()
            );
        }
    }
}
