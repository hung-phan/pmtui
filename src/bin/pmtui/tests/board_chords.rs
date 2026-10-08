use super::*;

/// In raw mode crossterm delivers Ctrl+P as `Char('p')+CONTROL`. The Task Board's own key match
/// keyed only on the code, so every Ctrl/Alt chord fired the bare-letter card action: a reflex
/// Ctrl+P paused (killed) the selected agent with no confirm, and Ctrl+C did nothing. The Board
/// now applies the same chord gate as the Session view.
#[test]
fn a_ctrl_or_alt_chord_on_the_board_does_not_fire_a_card_action() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let mut card = autopilot_loop_view("bot");
    card.posture = Posture::Monitoring;
    card.agent_working = Some(false);
    let mut app = app_with(vec![card], UiMode::Board);
    app.registry_path = reg_path.clone();
    app.status = "untouched".into();

    for (code, mods) in [
        (KeyCode::Char('p'), KeyModifiers::CONTROL),
        (KeyCode::Char('f'), KeyModifiers::CONTROL),
        (KeyCode::Char('f'), KeyModifiers::ALT),
        (KeyCode::Char('n'), KeyModifiers::CONTROL),
        (KeyCode::Char('r'), KeyModifiers::CONTROL),
        (KeyCode::Char('d'), KeyModifiers::CONTROL),
        (KeyCode::Char('q'), KeyModifiers::CONTROL),
        (KeyCode::Enter, KeyModifiers::ALT),
    ] {
        handle_key(&mut app, code, mods);
        assert!(
            matches!(app.mode, UiMode::Board),
            "{mods:?}+{code:?} left the Board"
        );
        assert!(!app.board_detail_open, "{mods:?}+{code:?} opened detail");
        assert_eq!(app.status, "untouched", "{mods:?}+{code:?} ran an action");
        assert!(!app.should_quit, "{mods:?}+{code:?} quit");
        assert!(
            enabled_on_disk(&reg_path, "bot"),
            "{mods:?}+{code:?} paused the session"
        );
        assert_eq!(app.projects.len(), 1, "{mods:?}+{code:?} forked");
    }

    // The bare letter still works: plain `p` pauses.
    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(!enabled_on_disk(&reg_path, "bot"), "plain p still pauses");

    // On the now-paused card Ctrl+P must not resume it either.
    app.mode = UiMode::Board;
    app.status = "untouched".into();
    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert!(
        !enabled_on_disk(&reg_path, "bot"),
        "Ctrl+P resumed the card"
    );
    assert_eq!(app.status, "untouched");

    // Ctrl+C is the reflex quit, on the Board as everywhere else.
    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(app.should_quit, "Ctrl+C quits from the Board");
}

#[test]
fn a_chord_inside_task_detail_is_swallowed_too() {
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Board);
    app.board_detail_open = true;
    app.status = "untouched".into();

    for (code, mods) in [
        (KeyCode::Char('s'), KeyModifiers::CONTROL),
        (KeyCode::Char('m'), KeyModifiers::ALT),
        (KeyCode::Enter, KeyModifiers::CONTROL),
        (KeyCode::Esc, KeyModifiers::ALT),
    ] {
        handle_key(&mut app, code, mods);
        assert!(matches!(app.mode, UiMode::Board), "{mods:?}+{code:?}");
        assert!(app.board_detail_open, "{mods:?}+{code:?} closed detail");
        assert_eq!(app.status, "untouched", "{mods:?}+{code:?} ran an action");
    }
}
