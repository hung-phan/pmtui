use super::*;

fn left_click(column: u16, row: u16) -> Event {
    Event::Mouse(ratatui::crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn detail_app(status: &str) -> App {
    let mut app = app_with(
        vec![agent_loop_view("alpha"), agent_loop_view("beta")],
        UiMode::Board,
    );
    // History already on file: it used to decide whether a ` STATUS ` band owned the status
    // instead of the keybar, which is a question the file-backed log no longer raises.
    seed_log(&app, &["older action result"]);
    app.board_detail_open = true;
    app.status = status.into();
    app
}

fn bottom_row(terminal: &Terminal<TestBackend>) -> String {
    screen_rows(terminal)
        .into_iter()
        .map(|(row, _)| row)
        .next_back()
        .expect("a keybar row")
}

/// Task detail used to hide the Status band, so nothing drew `app.status` there: an Answer refusal, a
/// pmd-starting notice, or a composer refusal looked like a dead key until Esc. The keybar owns the
/// transient status on every surface now — there is no band to hand it to — so this pins the case
/// that first exposed the gap.
#[test]
fn task_detail_keeps_the_latest_status_visible_in_the_keybar() {
    let app = detail_app("alpha: autopilot off — press m, then s");
    let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let keybar = bottom_row(&terminal);
    assert!(
        keybar.contains("autopilot off — press m, then s"),
        "detail dropped the status: {keybar}"
    );
}

#[test]
fn a_detail_composer_refusal_is_visible_while_the_field_stays_open() {
    let mut app = detail_app("no prompt on screen — press Enter to look");
    app.return_to_board_after_send = true;
    app.mode = UiMode::Sending {
        target: SendTarget {
            id: "alpha".into(),
            root: PathBuf::from("/tmp/alpha"),
            session: "pm-alpha".into(),
            agent_loop: true,
            driven: false,
            in_chat: false,
        },
        input: Composer::from_text("status please".into()),
    };
    let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let screen = screen_text(&terminal);
    assert!(
        screen.contains("status please"),
        "composer closed: {screen}"
    );
    assert!(
        bottom_row(&terminal).contains("no prompt on screen"),
        "the refusal was invisible: {screen}"
    );
}

#[test]
fn an_overlay_over_task_detail_keeps_the_status_visible() {
    let mut app = app_with(
        vec![confirm_done_view()],
        UiMode::Answering {
            input: Field::new(),
            choice: 0,
            scroll: 0,
        },
    );
    seed_log(&app, &["older action result"]);
    app.board_detail_open = true;
    app.return_to_board_after_answer = true;
    app.status = "answer refused: stop changed".into();
    let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    assert!(
        bottom_row(&terminal).contains("answer refused: stop changed"),
        "{}",
        screen_text(&terminal)
    );
}

/// ONE surface draws the status, and in lane view it is the keybar — the same as everywhere else.
/// There used to be a band here that owned it instead, and the two had to agree about which of them
/// was showing it or the line was drawn twice (or lost between them). With the band gone the rule
/// has no second case to get wrong, and the log file keeps the line either way.
#[test]
fn lane_view_draws_the_status_once_on_the_keybar() {
    let mut app = detail_app("could not fork selected task: still working");
    app.board_detail_open = false;
    app.record_status();
    let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    assert!(
        bottom_row(&terminal).contains("could not fork"),
        "the keybar dropped the status: {}",
        screen_text(&terminal)
    );
    let screen = screen_text(&terminal);
    assert_eq!(
        screen.matches("could not fork").count(),
        1,
        "the status is drawn twice: {screen}"
    );
    assert_eq!(
        log_lines(&app).last().map(String::as_str),
        Some("could not fork selected task: still working")
    );
}

/// Keyboard selection (`j`/`k`/`h`/`l`) writes no status. A card click wrote
/// "selected <id> - use its card actions …" on every click, and every status lands in the Status
/// log, so clicking between cards pushed the failure the band exists to keep visible out of view.
#[test]
fn clicking_cards_selects_them_without_writing_status_history() {
    let mut app = app_with(
        vec![agent_loop_view("alpha"), autopilot_loop_view("beta")],
        UiMode::Board,
    );
    app.status = "could not fork alpha: still working".into();
    app.record_status();
    let mut terminal = Terminal::new(TestBackend::new(150, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let beta = app
        .board_hits
        .borrow()
        .iter()
        .find(|(_, id)| id == "beta")
        .map(|(area, _)| *area)
        .expect("beta card");
    let mut handled = false;

    handle_event(&mut app, left_click(beta.x + 1, beta.y + 1), &mut handled);
    app.after_input(handled);

    assert!(handled);
    assert_eq!(app.selected_view().map(|v| v.id.as_str()), Some("beta"));
    assert_eq!(app.status, "could not fork alpha: still working");
    assert_eq!(
        log_lines(&app),
        vec!["could not fork alpha: still working".to_string()]
    );
}
