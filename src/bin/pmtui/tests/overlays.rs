//! Every overlay's frame: the rect that never leaves the screen however small it is, the
//! bordered card with its title chip, and the send field's keys — Ctrl-E, Esc, and a
//! paste that is stripped of control bytes.

use super::*;

#[test]
fn overlay_rect_never_leaves_the_frame_however_small_it_is() {
    // A modal keeps a margin from the terminal's edge, but never at the cost of the
    // modal itself: at every size the rect stays INSIDE the area and never collapses.
    for w in 1u16..=40 {
        for h in 1u16..=20 {
            let area = Rect::new(0, 0, w, h);
            let r = overlay_rect(area, 72, 12);
            assert!(r.width >= 1 && r.height >= 1, "{w}x{h} collapsed: {r:?}");
            assert!(
                r.x + r.width <= area.x + area.width && r.y + r.height <= area.y + area.height,
                "{w}x{h} escaped the frame: {r:?}"
            );
        }
    }
}

#[test]
fn overlay_grows_on_a_roomy_terminal_but_stops_at_the_cap() {
    // 45% of a wide terminal, so a modal is not a postage stamp on a 300-col screen —
    // and capped, because a one-line field 140 columns wide is not more usable.
    let narrow = overlay_rect(Rect::new(0, 0, 100, 40), 72, 12);
    assert_eq!(
        narrow.width, 72,
        "a modest terminal gets the asked-for width"
    );
    let wide = overlay_rect(Rect::new(0, 0, 300, 40), 72, 12);
    assert_eq!(wide.width, OVERLAY_MAX_W, "capped on a wide screen");
    // Height is content-driven and never grows.
    assert_eq!(wide.height, 12);
}

#[test]
fn overlay_h_is_content_driven_with_a_floor() {
    // The bug this closes was mine: a flat 12 rows drew a 3-line body inside 9 rows of
    // nothing, which reads as unfinished — the complaint the resize was answering.
    assert_eq!(overlay_h(10), 10 + OVERLAY_CHROME_H);
    assert_eq!(overlay_h(1), 9, "a one-line modal still reads as a window");
    assert_eq!(
        overlay_h(5),
        9,
        "…and the floor wins until the body outgrows it"
    );
    assert_eq!(overlay_h(usize::MAX), u16::MAX, "no overflow");
}

#[test]
fn an_overlay_is_a_bordered_card_with_a_title_chip() {
    // What the modal is made of, asserted rather than eyeballed: a ROUNDED border in
    // the accent colour, over a RAISED SURFACE, with the title as a filled chip and the
    // keys on the bottom border. And NO drop shadow — it was tried, and the user's
    // verdict on it was the reason the border came back.
    let app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingGoal {
            id: "bot".into(),
            brief: PathBuf::from("/nonexistent/brief.md"),
            current: "ship it".into(),
            input: goal_buf("ship it"),
            then_autopilot: false,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains('╭') && screen.contains('╯'),
        "no rounded border: {screen}"
    );
    assert!(!screen.contains('▒'), "the drop shadow is back: {screen}");
    // A raised surface UNDER the border — invisible to a text capture, so read the cell.
    let bg_of = |needle: &str| -> Color {
        let buf = terminal.backend().buffer();
        for y in buf.area.top()..buf.area.bottom() {
            let row: String = (buf.area.left()..buf.area.right())
                .filter_map(|x| buf.cell((x, y)))
                .map(|c| c.symbol())
                .collect();
            if let Some(at) = row.find(needle) {
                let x = buf.area.left() + u16::try_from(at).unwrap_or(0);
                return buf.cell((x, y)).map(|c| c.bg).unwrap_or(Color::Reset);
            }
        }
        Color::Reset
    };
    // Sampled INSIDE the buffer (its placeholder row) and on the note under it: the goal field's body
    // is a `ratatui-textarea` widget now, and a widget that does not inherit the surface it is drawn on
    // would leave a hole in the modal — which is the invariant, every cell is the theme's.
    for needle in ["ship it", "writes brief.md"] {
        assert_eq!(
            bg_of(needle),
            agent_manager::theme::surface_bg(),
            "the modal is not a raised surface under {needle:?}"
        );
    }
    let chip = styles_under(&terminal, "Goal · bot").expect("no title");
    assert!(
        chip.iter()
            .all(|(fg, m)| *fg == agent_manager::theme::accent() && m.contains(Modifier::REVERSED)),
        "the title is not a filled chip: {chip:?}"
    );
    // The keys are on the BOTTOM border, which is what buys the body its rows.
    let rows = screen_rows_styled(&terminal);
    let hint_row = rows
        .iter()
        .position(|(t, _)| t.contains("enter save"))
        .expect("no hint");
    let body_row = rows
        .iter()
        .position(|(t, _)| t.contains("ship it"))
        .expect("no body");
    assert!(
        hint_row > body_row,
        "the hint should sit on the bottom border, below the body"
    );
}

#[test]
fn an_overlay_never_draws_outside_the_frame() {
    // A modal centred in a terminal barely bigger than itself must CLIP, not wrap onto
    // the next row and not panic. Swept from 1x1 because that is where the arithmetic
    // (45%-of-width, the margins, the content-driven height) has the least room.
    for (w, h) in [(1u16, 1u16), (2, 2), (MIN_W, MIN_H), (30, 9), (74, 13)] {
        let app = app_with(
            vec![agent_loop_view("bot")],
            UiMode::Confirming {
                id: "bot".into(),
                session: "pmchat-bot".into(),
                what: Confirmable::Remove,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, &app))
            .unwrap_or_else(|e| panic!("panicked at {w}x{h}: {e}"));
        let buf = terminal.backend().buffer();
        assert_eq!(buf.area.width, w, "the buffer grew at {w}x{h}");
        assert_eq!(buf.area.height, h, "the buffer grew at {w}x{h}");
    }
}

#[test]
fn every_overlay_is_framed_the_same_way() {
    // Modal overlays share one frame — the drift this helper exists to prevent. Each must be
    // a rounded border over a raised surface, at a size with room for both.
    let mut asking = agent_loop_view("bot");
    asking.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    for (name, mode, view) in [
        (
            "answer",
            UiMode::Answering {
                input: "yes".into(),
                choice: 0,
                scroll: 0,
            },
            asking.clone(),
        ),
        (
            "create",
            UiMode::Creating(CreateForm::new()),
            agent_loop_view("bot"),
        ),
        (
            "goal",
            UiMode::EditingGoal {
                id: "bot".into(),
                brief: PathBuf::from("/nonexistent/brief.md"),
                current: "ship it".into(),
                input: goal_buf(""),
                then_autopilot: false,
            },
            agent_loop_view("bot"),
        ),
        (
            "confirm",
            UiMode::Confirming {
                id: "bot".into(),
                session: "pmchat-bot".into(),
                what: Confirmable::Remove,
            },
            agent_loop_view("bot"),
        ),
    ] {
        let app = app_with(vec![view], mode);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).expect("render");
        let screen = screen_text(&terminal);
        // One frame for every modal overlay: a rounded border over a raised surface, and no
        // shadow anywhere.
        assert!(screen.contains('╭'), "{name}: no rounded border");
        assert!(!screen.contains('▒'), "{name}: the shadow is back");
        let buf = terminal.backend().buffer();
        let raised = (buf.area.top()..buf.area.bottom()).any(|y| {
            (buf.area.left()..buf.area.right())
                .filter_map(|x| buf.cell((x, y)))
                .any(|c| c.bg == agent_manager::theme::surface_bg())
        });
        assert!(raised, "{name}: not a raised surface");
    }
}

/// THE APPLY CONFIRM SAYS WHICH TREE MOVES AND WHAT A CONFLICT COSTS. It is the only confirm here that
/// writes to the human's own files rather than ending an agent, so it must not read like the others: a
/// person deciding whether to press `y` needs to know the pick lands on their current branch and that a
/// conflict leaves them exactly where they were.
#[test]
fn the_apply_confirmation_names_the_checkout_and_the_conflict_cost() {
    let app = app_with(
        vec![],
        UiMode::Confirming {
            id: "proj-2".into(),
            session: "pm-proj-2-abc".into(),
            what: Confirmable::Apply,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);

    for expected in [
        "Confirm apply",
        "Apply proj-2's commit to this checkout?",
        "Cherry-picks its commit onto the project's current branch.",
        "your checkout is left as it is",
        "y apply",
    ] {
        assert!(screen.contains(expected), "missing {expected:?}: {screen}");
    }
    assert!(
        !screen.contains("kills its tmux session"),
        "it ends no agent, so it must not borrow the remove copy: {screen}"
    );
}

#[test]
fn create_directory_confirmation_shows_the_path_and_safe_action() {
    let app = app_with(
        vec![],
        UiMode::ConfirmCreateDir {
            form: CreateForm::new(),
            dir: "/workplace/new-project".into(),
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);

    for expected in [
        "Create directory?",
        "This folder does not exist yet:",
        "/workplace/new-project",
        "Create it and set up the session here?",
        "y / enter create",
    ] {
        assert!(screen.contains(expected), "missing {expected:?}: {screen}");
    }
    assert!(
        !screen.contains("Confirm remove"),
        "a safe create must not use destructive confirmation copy"
    );
}

#[test]
fn create_directory_confirmation_keeps_the_tail_of_a_long_path() {
    let leaf = "the-project-name-that-must-stay-visible";
    let dir = format!("/very/long/prefix/{}/{}", "nested/".repeat(20), leaf);
    let app = app_with(
        vec![],
        UiMode::ConfirmCreateDir {
            form: CreateForm::new(),
            dir,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);

    assert!(
        screen.contains('…'),
        "a clipped path must signal truncation"
    );
    assert!(
        screen.contains(leaf),
        "the folder name at the path tail must remain visible: {screen}"
    );
    assert!(
        !screen.contains("/very/long/prefix"),
        "the path head should be the part clipped: {screen}"
    );
}

#[test]
fn the_send_overlay_survives_a_tiny_terminal() {
    for (w, h) in [(1, 1), (MIN_W, MIN_H), (24, 8), (80, 24)] {
        let app = app_with(
            vec![view("bot", Posture::Working, vec![])],
            UiMode::Sending {
                target: send_target(true),
                input: Composer::from_text("x".repeat(400)),
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, &app))
            .unwrap_or_else(|e| panic!("panicked at {w}x{h}: {e}"));
    }
}

#[test]
fn inline_composer_names_its_target() {
    let target = SendTarget {
        id: "bot".into(),
        root: PathBuf::from("/nonexistent"),
        session: "pm-bot".into(),
        agent_loop: false,
        driven: false,
        in_chat: false,
    };
    let app = app_with(
        vec![view("bot", Posture::Working, vec![])],
        UiMode::Sending {
            target,
            input: msg_buf(""),
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");

    let screen = screen_text(&terminal);
    assert!(screen.contains("Message"));
    assert!(screen.contains("bot"));
}

#[test]
fn message_composer_wraps_and_scrolls_to_keep_the_caret_visible() {
    let target = SendTarget {
        id: "bot".into(),
        root: PathBuf::from("/nonexistent"),
        session: "pm-bot".into(),
        agent_loop: true,
        driven: false,
        in_chat: false,
    };
    let input = Composer::from_text("0123456789".repeat(20));
    let app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::Sending { target, input },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
    terminal.draw(|frame| render(frame, &app)).expect("render");
    let rows = screen_rows_styled(&terminal);
    let composer_rows: Vec<_> = rows
        .iter()
        .filter(|(text, _)| text.contains("Message") || text.contains("012345"))
        .collect();
    assert!(
        composer_rows.len() >= 2,
        "message text did not wrap: {}",
        screen_text(&terminal)
    );
}

/// The composer draws at every size, with its caret on screen.
///
/// This used to fuzz a hand-rolled wrapper (`wrapped_input_lines`) directly. That arithmetic is gone —
/// `ratatui-textarea` owns wrapping, the caret across a wrapped row, and the scroll that keeps it
/// visible — so the sweep moved to the REAL render path, which is the part that can still be wired up
/// wrong. It walks the caret through a multi-byte, multi-LINE buffer at every size the composer gets.
#[test]
fn the_composer_draws_at_every_size_with_its_caret_on_screen() {
    let target = SendTarget {
        id: "bot".into(),
        root: PathBuf::from("/nonexistent"),
        session: "pm-bot".into(),
        agent_loop: true,
        driven: false,
        in_chat: false,
    };
    let value = "abé🎉0123456789\nsecond line\nthird";
    let steps = value.chars().count();
    for width in [MIN_W, 40, 80] {
        for height in [10u16, 16, 24] {
            for step in [0, steps / 3, steps] {
                let mut input = Composer::from_text(value.to_string());
                input.set_cursor(0, 0);
                for _ in 0..step {
                    input.key(KeyCode::Right, KeyModifiers::NONE);
                }
                let app = app_with(
                    vec![agent_loop_view("bot")],
                    UiMode::Sending {
                        target: target.clone(),
                        input,
                    },
                );
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| render(frame, &app)).expect("render");
                let carets = screen_rows_styled(&terminal)
                    .iter()
                    .flat_map(|(_, cells)| cells.iter())
                    .filter(|(_, m)| m.contains(Modifier::REVERSED))
                    .count();
                assert!(
                    carets >= 1,
                    "{width}x{height} at step {step}: no caret on screen:\n{}",
                    screen_text(&terminal)
                );
            }
        }
    }
}

#[test]
fn ctrl_e_in_the_send_field_queues_an_editor_compose_seeded_with_what_was_typed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _pane, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    for c in "half a thought".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    // `^X^E` — bash's `edit-and-execute-command`. Bare `^E` is end-of-line in the composer now.
    handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    handle_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    let req = app.pending_send.as_ref().expect("no editor request queued");
    assert_eq!(req.seed, "half a thought");
    assert_eq!(req.target.id, "bot");
    // The composer closes first — `run()` restores the terminal into whatever mode is
    // current when the editor returns.
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn esc_parks_the_send_draft() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, pane, _, _) = send_app(dir.path(), IDLE_CLAUDE_PANE);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    assert!(pane.sends().is_empty());
    assert_eq!(app.status, "draft saved for bot");
    assert_eq!(
        app.message_drafts.get("bot").map(Composer::text),
        Some("h".to_string())
    );
}

// --- M28: bracketed paste ----------------------------------------------------

#[test]
fn a_paste_lands_in_whichever_field_is_open() {
    let mut app = app_with(vec![view("bot", Posture::NeedsYou, vec![])], UiMode::Normal);
    app.mode = UiMode::Answering {
        input: "a".into(),
        choice: 0,
        scroll: 0,
    };
    handle_paste(&mut app, "b\nc");
    match &app.mode {
        // Flattened, because every field pmtui has is a single row.
        UiMode::Answering { input, .. } => assert_eq!(input.as_str(), "ab c"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_paste_in_normal_mode_presses_nothing() {
    // Without bracketed paste the first newline SUBMITS and the rest is read as
    // Normal-mode bindings: `d` opens the remove confirm, `q` quits, `m` flips
    // autopilot. This is that regression, pinned.
    let mut app = app_with(vec![view("bot", Posture::Working, vec![])], UiMode::Normal);
    handle_paste(&mut app, "delete\nquit\ntoggle");
    assert!(matches!(app.mode, UiMode::Normal), "a paste changed mode");
    assert!(!app.should_quit, "a pasted `q` quit pmtui");
    assert!(
        app.status.contains("open a field first"),
        "{:?}",
        app.status
    );
}

#[test]
fn the_paste_refusal_names_only_keys_the_dashboard_binds() {
    // The hint is the way out of the refusal, so every key it names must be live. `a` was
    // removed when `s` took over Answer, and pressing it now does nothing at all.
    let mut app = app_with(vec![view("bot", Posture::Working, vec![])], UiMode::Normal);
    handle_paste(&mut app, "some text");
    let keys = app
        .status
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(')'))
        .map(|(keys, _)| keys)
        .unwrap_or_else(|| panic!("no key list in {:?}", app.status));
    let keys: Vec<&str> = keys
        .split([',', ' '])
        .filter(|key| !key.is_empty() && *key != "or")
        .collect();
    assert_eq!(keys, ["/", "R", "g", "i", "s"], "{:?}", app.status);
    for key in keys {
        assert!(
            BINDINGS
                .iter()
                .any(|b| b.key == key && matches!(b.scope, Scope::Normal | Scope::HelpOnly)),
            "the paste hint names unbound key {key:?}"
        );
    }
}

#[test]
fn a_paste_strips_control_bytes_before_it_reaches_a_field() {
    // A pasted ANSI log or a CRLF file must not carry ESC (which arrives as
    // shift+tab and cycles claude's permission mode) or CR (a second submit).
    let mut app = app_with(vec![view("bot", Posture::Working, vec![])], UiMode::Normal);
    app.mode = UiMode::Answering {
        input: Field::new(),
        choice: 0,
        scroll: 0,
    };
    handle_paste(&mut app, "red\u{1b}[31m\ttext\r\n");
    match &app.mode {
        UiMode::Answering { input, .. } => {
            assert!(!input.contains('\u{1b}') && !input.contains('\r'));
            // The ESC becomes a SPACE, not nothing, so words separated by a control
            // byte cannot silently run together.
            assert_eq!(input.as_str(), "red [31m text");
        }
        other => panic!("{other:?}"),
    }
}

// --- M19: edit a LIVE session's goal INLINE (`G`) ----------------------------
