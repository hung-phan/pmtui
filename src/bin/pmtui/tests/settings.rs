//! The Settings view and the preferences file behind it: a table of settings, one dropdown per row,
//! what Enter does in each state, what it says when it cannot, and what the screen shows.
//!
//! Every test that commits keeps the dropdown on the value ALREADY IN FORCE. Applying is process-wide,
//! and this binary's other tests read a role colour at assert time, so switching the palette here would
//! make a screen drawn under one theme be compared against another's hue. The commit path is the same
//! code either way — the value is data — and the real switch is exercised on a real terminal in
//! `tests/integration/`.

use super::*;
use crate::settings::{SettingKind, Settings, load, save, settings_path};

/// The cursor a test is driving: the setting's, and the open dropdown's.
fn cursors(app: &App) -> (usize, Option<usize>) {
    match app.mode {
        UiMode::Settings { cursor, open } => (cursor, open),
        ref other => panic!("not in Settings: {other:?}"),
    }
}

/// The preferences belong to the dashboard that owns the registry, so a scratch `--registry` gets
/// scratch preferences. This is what keeps a test run out of the human's `pmtui.json`.
#[test]
fn the_preferences_land_beside_the_registry_they_belong_to() {
    assert_eq!(
        settings_path(std::path::Path::new("/home/dev/.config/pmd/registry.json")),
        std::path::PathBuf::from("/home/dev/.config/pmd/pmtui.json")
    );
    assert_eq!(
        settings_path(std::path::Path::new("/tmp/scratch/registry.json")),
        std::path::PathBuf::from("/tmp/scratch/pmtui.json")
    );
    // A bare filename has no directory part: the preferences join it wherever that is.
    assert_eq!(
        settings_path(std::path::Path::new("registry.json")),
        std::path::PathBuf::from("pmtui.json")
    );
}

/// An absent file is the DEFAULT, a present one round-trips, and a present-but-broken one is an error
/// rather than a silent default — the difference between "never set" and "your setting was ignored".
#[test]
fn preferences_default_when_absent_round_trip_when_written_and_report_when_broken() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pmtui.json");
    assert_eq!(
        load(&path).expect("an absent file is not an error"),
        Settings::default()
    );
    assert_eq!(
        Settings::default().theme,
        agent_manager::theme::DEFAULT_THEME
    );

    let mine = Settings {
        theme: "tokyo-night".into(),
    };
    save(&path, &mine).expect("write");
    assert_eq!(load(&path).expect("read back"), mine);
    // Stored as the ID, which is what `theme::apply` takes — a display name would not round-trip.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"theme\""), "{text}");
    assert!(text.contains("tokyo-night"), "{text}");

    std::fs::write(&path, "{ not json").unwrap();
    assert!(load(&path).is_err(), "a broken file must be reported");
}

/// A file missing the field still parses: an older pmtui's preferences must not cost the human their
/// dashboard.
#[test]
fn a_preferences_file_from_another_pmtui_keeps_the_fields_it_has() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pmtui.json");
    std::fs::write(&path, "{}").unwrap();
    assert_eq!(
        load(&path).expect("an empty object is valid"),
        Settings::default()
    );
    std::fs::write(&path, r#"{"theme":"nord","something-newer":7}"#).unwrap();
    assert_eq!(
        load(&path).expect("an unknown field is ignored").theme,
        "nord"
    );
}

/// The row states the value in force, by NAME, and every setting gets one row — that is what makes the
/// page cost one line per setting however many there are.
#[test]
fn a_setting_row_names_its_value_and_offers_the_rest() {
    let app = app_with(Vec::new(), UiMode::Normal);
    let row = app.setting_row(SettingKind::Theme);
    let active = agent_manager::theme::active();
    let current = row.current.expect("the active theme is offered");
    assert_eq!(row.options[current].value, active);
    assert_eq!(
        row.value, row.options[current].label,
        "the value is the NAME"
    );
    assert_ne!(
        row.value, active,
        "…not the id, which is the file's business"
    );
    assert_eq!(row.options.len(), app.themes.len());
    // The note is the dark/light word, the one fact a theme's name does not always carry.
    for option in &row.options {
        assert!(
            matches!(option.note.as_str(), "dark" | "light"),
            "{option:?}"
        );
    }
    assert_eq!(SettingKind::ALL.len(), 1, "one setting today");
    assert_eq!(SettingKind::Theme.name(), "Theme");
    assert!(!SettingKind::Theme.summary().is_empty());
}

/// A stored value this build does not offer is SAID to be unknown, rather than shown as a choice.
#[test]
fn an_unknown_stored_value_says_it_is_unknown() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.themes.clear();
    let row = app.setting_row(SettingKind::Theme);
    assert_eq!(row.current, None);
    assert!(row.value.contains("(unknown)"), "{}", row.value);
}

/// `0` is a VIEW KEY: it reaches Settings from either working view, and from Settings the other two
/// numbers go straight to their view rather than stacking up an overlay to unwind.
#[test]
fn the_numbered_views_reach_settings_and_each_other() {
    let mut app = app_with(vec![view("a", Posture::Working, vec![])], UiMode::Normal);
    handle_key(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
    assert_eq!(cursors(&app), (0, None));
    // Already here: `0` is inert rather than a reset.
    handle_key(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
    assert_eq!(cursors(&app), (0, None));

    handle_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));
    handle_key(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
    assert_eq!(cursors(&app), (0, None));
    handle_key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
}

/// ENTER OPENS, ENTER PICKS. Two presses to change a value, and the dropdown opens ON the value in
/// force so it answers "what is set?" before it offers to change it.
#[test]
fn enter_opens_the_dropdown_then_commits_the_row_under_it() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.open_settings();
    let row = app.setting_row(SettingKind::Theme);
    let current = row.current.expect("a current theme");

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        cursors(&app),
        (0, Some(current)),
        "the dropdown must open on the value in force"
    );
    // Opening changes nothing on its own.
    assert!(!app.settings_path.exists());

    let choice = row.options[current].clone();
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(cursors(&app), (0, None), "committing closes the dropdown");
    assert_eq!(agent_manager::theme::active(), choice.value);
    assert_eq!(app.settings.theme, choice.value);
    assert_eq!(
        app.status,
        format!("theme → {}", choice.label),
        "the status names the setting and the value's NAME"
    );
    assert_eq!(
        load(&app.settings_path)
            .expect("the choice was written")
            .theme,
        choice.value
    );
}

/// Esc closes the DROPDOWN, not the view: a mis-press costs a keystroke, not the place you were in. A
/// second Esc leaves, and neither applies anything.
#[test]
fn esc_closes_the_dropdown_first_and_the_view_second() {
    for key in [KeyCode::Esc, KeyCode::Char('q')] {
        let mut app = app_with(Vec::new(), UiMode::Normal);
        app.open_settings();
        let before = agent_manager::theme::active();
        handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        app.settings_move(3);
        assert!(matches!(app.mode, UiMode::Settings { open: Some(_), .. }));

        handle_key(&mut app, key, KeyModifiers::NONE);
        assert_eq!(cursors(&app), (0, None), "{key:?} must close the list only");
        handle_key(&mut app, key, KeyModifiers::NONE);
        assert!(matches!(app.mode, UiMode::Normal), "{key:?}");
        assert!(
            !app.should_quit,
            "{key:?} in Settings is the way back, not a quit"
        );
        assert_eq!(agent_manager::theme::active(), before, "{key:?}");
        assert!(
            !app.settings_path.exists(),
            "{key:?} wrote preferences it was never asked to"
        );
    }
}

/// `2` switches views from a row and is SWALLOWED while a dropdown is down — the open list owns the
/// keyboard, like every other picker in this dashboard.
#[test]
fn an_open_dropdown_keeps_the_view_keys_to_itself() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.open_settings();
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let before = cursors(&app);
    for key in ['2', '0'] {
        handle_key(&mut app, KeyCode::Char(key), KeyModifiers::NONE);
        assert_eq!(cursors(&app), before, "`{key}` escaped the open dropdown");
    }
}

/// Movement is the same keys in both states, clamped to whichever list is live.
#[test]
fn the_same_keys_move_whichever_cursor_is_live() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.open_settings();
    // One setting today: the row cursor has nowhere to go, and says so by not moving.
    for key in [KeyCode::Char('j'), KeyCode::End, KeyCode::PageDown] {
        handle_key(&mut app, key, KeyModifiers::NONE);
        assert_eq!(cursors(&app), (0, None), "{key:?}");
    }

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let last = app.setting_row(SettingKind::Theme).options.len() - 1;
    app.settings_select(0);
    for (key, want) in [
        (KeyCode::Char('j'), 1),
        (KeyCode::Down, 2),
        (KeyCode::Char('k'), 1),
        (KeyCode::Up, 0),
        // Already at the top: clamped, not wrapped to the bottom.
        (KeyCode::Up, 0),
        (KeyCode::PageDown, app::scroll::PAGE_LINES),
        (KeyCode::PageUp, 0),
        (KeyCode::End, last),
        // …and at the bottom too.
        (KeyCode::Char('j'), last),
        (KeyCode::Home, 0),
    ] {
        handle_key(&mut app, key, KeyModifiers::NONE);
        assert_eq!(cursors(&app), (0, Some(want)), "{key:?}");
    }

    // Outside Settings every one of these is inert rather than a panic on the wrong mode.
    app.mode = UiMode::Normal;
    app.settings_move(1);
    app.settings_select(2);
    app.settings_open();
    app.settings_close();
    app.settings_commit();
    assert!(matches!(app.mode, UiMode::Normal));
}

/// A value that will not apply is REPORTED and not written: a preferences file naming one would fail the
/// same way at every start, with nothing on screen saying why.
#[test]
fn a_value_that_cannot_be_applied_is_reported_and_never_saved() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    let before = agent_manager::theme::active();
    // `available()` cannot produce this row — it only offers themes that load — so it is built by hand:
    // the branch exists for a picker that has somehow been handed one, and the alternative to covering
    // it is trusting that it never will be.
    app.themes = vec![agent_manager::theme::ThemeChoice {
        id: "no-such-theme".into(),
        display: "No Such Theme".into(),
        variant: "dark",
    }];
    app.mode = UiMode::Settings {
        cursor: 0,
        open: Some(0),
    };
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert!(
        app.status.contains("could not use theme no-such-theme"),
        "{}",
        app.status
    );
    assert_eq!(agent_manager::theme::active(), before);
    assert_eq!(app.settings.theme, before, "the setting was not changed");
    assert!(!app.settings_path.exists(), "a refused value was written");

    // An empty list cannot commit anything, and must not index out of bounds.
    app.themes.clear();
    app.status = "ready".into();
    app.mode = UiMode::Settings {
        cursor: 0,
        open: Some(0),
    };
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.status, "ready");
}

/// When the value applies but cannot be remembered, the status says BOTH — "it works now" and "it will
/// not survive a restart" are different facts and the human needs the second one.
#[test]
fn a_value_that_cannot_be_saved_says_it_is_on_for_now() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.settings_path = std::path::PathBuf::from("/nonexistent/pm-settings/pmtui.json");
    app.open_settings();
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let (_, open) = cursors(&app);
    let choice = app.setting_row(SettingKind::Theme).options[open.unwrap()].clone();
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(agent_manager::theme::active(), choice.value, "it did apply");
    assert!(
        app.status.starts_with(&format!(
            "theme → {} for now; could not save it",
            choice.label
        )),
        "{}",
        app.status
    );
}

/// A stray lifecycle key must do NOTHING here, open dropdown or not. Settings is a list of preferences,
/// and `d` on a row of it cannot be allowed to reach the session under the dashboard's selection.
#[test]
fn no_session_key_is_routed_from_settings() {
    let mut app = app_with(
        vec![view("alpha", Posture::Working, vec![])],
        UiMode::Normal,
    );
    for open in [false, true] {
        app.open_settings();
        if open {
            handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        }
        let before = format!("{:?}|{}", app.mode, app.status);
        for key in [
            'd', 'p', 'r', 'n', 's', 'g', 'i', 'm', 'c', 'v', 'f', 'e', 'w', 'R', '?',
        ] {
            handle_key(&mut app, KeyCode::Char(key), KeyModifiers::NONE);
            assert_eq!(
                format!("{:?}|{}", app.mode, app.status),
                before,
                "`{key}` did something from Settings (dropdown open: {open})"
            );
        }
    }
}

/// THE SCREEN, closed: one row per setting with its value and what it governs, and the file named where
/// a title belongs.
#[test]
fn the_settings_screen_is_a_table_of_settings() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.settings_path = std::path::PathBuf::from("/home/dev/.config/pmd/pmtui.json");
    app.open_settings();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);

    assert!(screen.contains("SETTINGS"), "{screen}");
    for want in [
        "Setting",
        "Value",
        "Theme",
        SettingKind::Theme.summary(),
        "Enter opens the list",
        // `▾` is the affordance: it says a list lives under this row.
        "▾",
        "/home/dev/.config/pmd/pmtui.json",
    ] {
        assert!(screen.contains(want), "no {want:?}: {screen}");
    }
    let row = app.setting_row(SettingKind::Theme);
    assert!(screen.contains(&row.value), "{screen}");
    // The CLOSED view is the table and nothing else: no option list is on screen until Enter.
    let other = row
        .options
        .iter()
        .find(|option| option.label != row.value)
        .expect("more than one theme");
    assert!(
        !screen.contains(&other.label),
        "an unopened dropdown is showing its options: {screen}"
    );

    // Same chrome as every other view — the tab strip (Settings active) and the keybar. The way BACK is
    // the `1 Sessions` tab, as it is on the Task view.
    let rows = screen_rows(&terminal);
    assert!(
        rows[0].0.contains("Settings"),
        "no tab strip: {}",
        rows[0].0
    );
    assert!(rows[0].0.contains("Sessions"), "no way back: {}", rows[0].0);
    let badge = styles_under_row(&terminal, "pmtui", " 0 ").expect("the Settings tab badge");
    assert!(
        badge[1].1.contains(Modifier::REVERSED),
        "the tab badge lost its shape: {badge:?}"
    );
    let keybar = &rows.last().expect("a keybar").0;
    for chip in ["Enter", "Change", "Move"] {
        assert!(
            keybar.contains(chip),
            "{chip} is not on the keybar: {keybar}"
        );
    }
}

/// THE SCREEN, open: the options under their own row, the value in force marked, and a keybar that says
/// what this state's keys do.
#[test]
fn the_open_dropdown_lists_its_values_under_the_row() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.open_settings();
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    let rows = screen_rows(&terminal);
    let row = app.setting_row(SettingKind::Theme);

    // The list is titled with the setting it belongs to and holds NAMES — no ids, no colour swatches.
    assert!(screen.contains("Theme"), "{screen}");
    for option in row.options.iter().take(3) {
        assert!(
            screen.contains(&option.label),
            "no {:?}: {screen}",
            option.label
        );
    }
    assert!(
        !screen.contains(&row.options[1].value),
        "the list is showing ids: {screen}"
    );

    // `●` marks the value IN FORCE, on exactly one option row.
    let marked: Vec<&String> = rows
        .iter()
        .map(|(text, _)| text)
        .filter(|text| text.contains('●') && row.options.iter().any(|o| text.contains(&o.label)))
        .collect();
    assert_eq!(marked.len(), 1, "{screen}");
    assert!(
        marked[0].contains(&row.value),
        "the mark is on the wrong row: {}",
        marked[0]
    );

    // It hangs UNDER its row, and never reaches the keybar.
    let table_row = rows
        .iter()
        .position(|(text, _)| text.contains("Theme") && text.contains('▾'))
        .expect("the settings row");
    let first_option = rows
        .iter()
        .position(|(text, _)| text.contains(&row.value) && text.contains('●'))
        .expect("the marked option");
    assert!(
        first_option > table_row,
        "the dropdown is not under its row: {screen}"
    );
    assert!(
        rows.last().is_some_and(|(text, _)| !text.contains('●')),
        "the dropdown reached the keybar: {screen}"
    );

    let keybar = &rows.last().expect("a keybar").0;
    for chip in ["Select", "Cancel", "Move"] {
        assert!(
            keybar.contains(chip),
            "{chip} is not on the keybar: {keybar}"
        );
    }
}

/// THE CURSOR IS DRAWN, not merely highlighted — and it moves.
///
/// The regression this pins: the cursor used to be a selection BACKGROUND only, and `bg.selection` is
/// the same colour as `bg.elevated` in Catppuccin Mocha, so inside the dropdown the cursor row looked
/// exactly like every other row. `j` worked and the screen did not say so — user: *"the high light on
/// the row item makes it looks like it doesn't work"*. So the assertion is about FOREGROUND and a drawn
/// caret, the two things a background cannot collide with.
#[test]
fn the_dropdown_cursor_is_visible_and_follows_the_keys() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.open_settings();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();

    // Closed: the caret is on the settings row, because that is what the keys move.
    let rows = screen_rows(&terminal);
    let carets: Vec<&String> = rows
        .iter()
        .map(|(text, _)| text)
        .filter(|text| text.contains('▸'))
        .collect();
    assert_eq!(carets.len(), 1, "{}", screen_text(&terminal));
    assert!(carets[0].contains("Theme"), "{}", carets[0]);

    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let row = app.setting_row(SettingKind::Theme);
    let caret_row = |terminal: &Terminal<TestBackend>| {
        let rows = screen_rows(terminal);
        let found: Vec<String> = rows
            .iter()
            .map(|(text, _)| text.clone())
            .filter(|text| text.contains('▸'))
            .collect();
        assert_eq!(found.len(), 1, "one caret, not {}", found.len());
        found[0].clone()
    };

    // Open: the caret is in the LIST, on the value in force, and nowhere else — the settings row must
    // not keep a cursor while the list owns the keys.
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let on_open = caret_row(&terminal);
    assert!(on_open.contains(&row.value), "{on_open}");
    assert!(
        !on_open.contains('▾'),
        "the caret stayed on the settings row: {on_open}"
    );

    // …and it FOLLOWS the keys, one row per press.
    for step in 1..=3usize {
        handle_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let wanted = &row.options[row.current.unwrap() + step].label;
        let line = caret_row(&terminal);
        assert!(
            line.contains(wanted),
            "after {step} presses the caret is not on {wanted:?}: {line}"
        );
    }

    // THE PROPERTY A BACKGROUND COULD NOT KEEP: the cursor row differs from its neighbours in
    // FOREGROUND, so it reads on a theme whose selection colour is its surface colour.
    let painted = screen_rows_styled(&terminal);
    let cursor_fg: Vec<Color> = painted
        .iter()
        .find(|(text, _)| text.contains('▸'))
        .map(|(_, cells)| cells.iter().map(|(fg, _)| *fg).collect())
        .expect("the cursor row");
    let plain = &row.options[0].label;
    let plain_fg: Vec<Color> = painted
        .iter()
        .find(|(text, _)| text.contains(plain) && !text.contains('▸'))
        .map(|(_, cells)| cells.iter().map(|(fg, _)| *fg).collect())
        .expect("a row that is not the cursor");
    assert_ne!(
        cursor_fg, plain_fg,
        "the cursor row is only distinguishable by background, which some themes do not show"
    );
}

/// A short terminal puts the list where the room is rather than off the bottom edge.
#[test]
fn the_dropdown_stays_on_screen_when_the_terminal_is_short() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.open_settings();
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let mut terminal = Terminal::new(TestBackend::new(80, MIN_H + 2)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    let row = app.setting_row(SettingKind::Theme);
    assert!(
        screen.contains(&row.value),
        "the list vanished instead of fitting: {screen}"
    );
    assert!(
        !app.settings_hits.borrow().is_empty(),
        "no clickable options: {screen}"
    );
}

/// THE POINTER DOES WHAT THE KEYS DO: a click on a row opens its list, a click on an option picks it,
/// and a wheel moves whichever cursor is live.
#[test]
fn clicks_open_a_row_and_pick_an_option() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.open_settings();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();

    let click = |app: &mut App, row: u16| {
        let mut handled = false;
        handle_event(
            app,
            Event::Mouse(ratatui::crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 20,
                row,
                modifiers: KeyModifiers::NONE,
            }),
            &mut handled,
        );
        handled
    };
    let wheel = |app: &mut App, kind, row| {
        let mut handled = false;
        handle_event(
            app,
            Event::Mouse(ratatui::crossterm::event::MouseEvent {
                kind,
                column: 20,
                row,
                modifiers: KeyModifiers::NONE,
            }),
            &mut handled,
        );
        handled
    };

    // A click on the Theme row opens its dropdown, on the value in force.
    let (row_y, _) = app.settings_hits.borrow()[0];
    assert!(click(&mut app, row_y));
    let current = app.setting_row(SettingKind::Theme).current.unwrap();
    assert_eq!(cursors(&app), (0, Some(current)));
    assert!(!app.settings_path.exists(), "opening wrote preferences");

    // Now the OPTIONS own the hit list. A wheel over them moves the dropdown's cursor…
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let (option_y, _) = app.settings_hits.borrow()[1];
    assert!(wheel(&mut app, MouseEventKind::ScrollDown, option_y));
    assert_eq!(cursors(&app), (0, Some(current + 1)));
    assert!(wheel(&mut app, MouseEventKind::ScrollUp, option_y));
    assert_eq!(cursors(&app), (0, Some(current)));
    // …and a wheel over a row nothing was drawn on does nothing at all.
    let empty = app
        .settings_hits
        .borrow()
        .iter()
        .map(|(y, _)| *y)
        .max()
        .expect("drawn rows")
        + 1;
    assert!(!wheel(&mut app, MouseEventKind::ScrollDown, empty));
    assert_eq!(cursors(&app), (0, Some(current)));

    // A click on the option in force commits it — the same path Enter takes — and closes the list.
    let in_force = app
        .settings_hits
        .borrow()
        .iter()
        .find(|(_, index)| *index == current)
        .map(|(y, _)| *y)
        .expect("the current option is on screen");
    assert!(click(&mut app, in_force));
    assert_eq!(cursors(&app), (0, None), "a pick closes the list");
    assert!(app.status.starts_with("theme → "), "{}", app.status);
    assert!(app.settings_path.exists(), "a pick did not save");

    // A stale hit region is a lie: leaving the view clears them, so a click on the next frame's session
    // list cannot land on a setting.
    handle_key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert!(app.settings_hits.borrow().is_empty());
}

/// Nothing here scrolls under the pointer, so no pane rect is published: a wheel over Settings must not
/// scroll a session list that is not on screen.
#[test]
fn settings_publishes_no_pane_rects() {
    let mut app = app_with(vec![view("a", Posture::Working, vec![])], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert_ne!(
        app.panes.get(),
        PaneRects::default(),
        "the list view publishes rects"
    );
    app.open_settings();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert_eq!(app.panes.get(), PaneRects::default());
}

/// A terminal below the floor says so, in Settings as anywhere else, rather than drawing a frame with
/// nothing in it.
#[test]
fn a_tiny_terminal_says_so_instead_of_drawing_settings() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.open_settings();
    let mut terminal = Terminal::new(TestBackend::new(MIN_W - 1, MIN_H - 1)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("too small"), "{screen}");
    assert!(!screen.contains("SETTINGS"), "{screen}");

    // …and the smallest terminal it DOES draw in still draws the table rather than a bare frame.
    let mut terminal = Terminal::new(TestBackend::new(MIN_W, MIN_H)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert!(screen_text(&terminal).contains("SETTINGS"));
}
