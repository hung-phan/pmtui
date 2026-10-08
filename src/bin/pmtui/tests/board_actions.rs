use super::*;

/// A Standard worker wrote `needs-you.json {state: Blocked}`: the card sits in Needs You while
/// `stops` stays empty on purpose, because pmd never answers for a Standard row.
fn blocked_standard_card() -> ProjectView {
    let mut view = agent_loop_view("blocked");
    view.posture = Posture::NeedsYou;
    view
}

/// An Autopilot session past its failure threshold: Needs You, and no open stop to answer.
fn stuck_autopilot_card() -> ProjectView {
    let mut view = autopilot_loop_view("stuck");
    view.posture = Posture::Stuck;
    view
}

fn lane_app(dir: &Path, view: ProjectView) -> App {
    let (registry, _root) = reg_with_agent_loop(dir, &view.id);
    let mut app = app_with(vec![view], UiMode::Board);
    app.registry_path = registry;
    app
}

fn click(app: &mut App, area: Rect) {
    let mut handled = false;
    handle_event(
        app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(handled);
}

fn chip(app: &App, code: KeyCode) -> Option<Rect> {
    app.key_hits
        .borrow()
        .iter()
        .find(|hit| hit.code == code)
        .map(|hit| hit.area)
}

fn draw(app: &App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    terminal
}

fn assert_composer_opened_in_detail(app: &App, id: &str) {
    let UiMode::Sending { target, .. } = &app.mode else {
        panic!("s did not open the composer: {}", app.status);
    };
    assert_eq!(target.id, id);
    assert!(
        app.board_detail_open,
        "the composer opens in Task detail, where its transcript is visible"
    );
    assert!(app.return_to_board_after_send);
    assert!(!app.return_to_board_after_answer);
}

/// The lane advertised `s Send` on every Needs You card, but the Board handler only acted when a
/// stop existed, so on a Blocked Standard or Stuck Autopilot card the lane's main action was dead.
/// Lane `s` now takes the Session view's route for that row — no stop means Message — and opens it
/// in Task detail.
#[test]
fn stopless_needs_you_cards_open_message_from_the_lane_key() {
    for view in [blocked_standard_card(), stuck_autopilot_card()] {
        let dir = tempfile::tempdir().unwrap();
        let id = view.id.clone();
        let mut app = lane_app(dir.path(), view);
        assert_eq!(
            app.selected_view().map(board_column),
            Some(BoardColumn::NeedsYou)
        );
        let terminal = draw(&app);
        assert!(
            line_text(&keybar_line(&app, 100)).contains("s  Send"),
            "{id}: {}",
            screen_text(&terminal)
        );

        handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);

        assert_composer_opened_in_detail(&app, &id);
        let terminal = draw(&app);
        assert!(
            app.board_column_hits.borrow().is_empty(),
            "{id}: lanes drawn over the composer: {}",
            screen_text(&terminal)
        );
        assert!(screen_text(&terminal).contains("Message"));

        handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.mode, UiMode::Board));
        assert!(
            app.board_detail_open,
            "{id}: Esc returns to the detail shell"
        );
    }
}

#[test]
fn stopless_needs_you_cards_open_message_from_the_lane_chip() {
    for view in [blocked_standard_card(), stuck_autopilot_card()] {
        let dir = tempfile::tempdir().unwrap();
        let id = view.id.clone();
        let mut app = lane_app(dir.path(), view);
        draw(&app);
        let send = chip(&app, KeyCode::Char('s')).unwrap_or_else(|| panic!("{id}: no s chip"));

        click(&mut app, send);

        assert_composer_opened_in_detail(&app, &id);
    }
}

#[test]
fn a_lane_s_chip_is_published_exactly_where_lane_s_acts() {
    let dir = tempfile::tempdir().unwrap();

    // Pending: Message stays detail-only, so neither the chip nor the key does anything.
    let mut pending = lane_app(dir.path(), agent_loop_view("pending"));
    draw(&pending);
    assert!(chip(&pending, KeyCode::Char('s')).is_none());
    handle_key(&mut pending, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(matches!(pending.mode, UiMode::Board));
    assert!(!pending.board_detail_open);
    assert!(!pending.return_to_board_after_send);

    // An open stop routes to Answer from any lane, so its chip is shown there too.
    let mut paused = confirm_done_view();
    paused.enabled = false;
    let app = app_with(vec![paused], UiMode::Board);
    draw(&app);
    assert!(chip(&app, KeyCode::Char('s')).is_some());
    let text = line_text(&keybar_line(&app, 100));
    assert!(text.contains("s  Answer"), "{text}");
    assert!(text.contains("p  Resume"), "{text}");
}

#[test]
fn a_refused_lane_message_leaves_no_return_flag_or_open_detail() {
    // The row vanished from the registry between the frame and the key: begin_send refuses.
    let mut app = app_with(vec![blocked_standard_card()], UiMode::Board);

    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);

    assert!(matches!(app.mode, UiMode::Board));
    assert!(!app.board_detail_open);
    assert!(!app.return_to_board_after_send);
    assert!(app.status.contains("gone"), "{}", app.status);
}

/// `fork_selected` always refuses while the agent is working or a human is attached, yet the lane
/// offered `f Fork` on every card — a predictably inert chip across the whole Working lane.
#[test]
fn fork_is_offered_only_on_cards_fork_can_act_on() {
    let mut working = view("working", Posture::Running, vec![]);
    working.session_live = true;
    working.agent_working = Some(true);
    let mut attached = autopilot_loop_view("attached");
    attached.posture = Posture::Monitoring;
    attached.session_live = true;
    attached.agent_working = Some(false);
    attached.human_attached = true;
    let mut idle = attached.clone();
    idle.id = "idle".into();
    idle.human_attached = false;

    for (view, offered, refusal) in [
        (working, false, Some("still working")),
        (attached, false, Some("attached")),
        (idle, true, None),
    ] {
        let id = view.id.clone();
        let mut app = app_with(vec![view], UiMode::Board);
        draw(&app);
        assert_eq!(chip(&app, KeyCode::Char('f')).is_some(), offered, "{id}");
        assert_eq!(
            line_text(&keybar_line(&app, 100)).contains("Fork"),
            offered,
            "{id}"
        );
        if let Some(refusal) = refusal {
            // The key still reaches the same handler and says why.
            handle_key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
            assert!(matches!(app.mode, UiMode::Board));
            assert!(app.status.contains(refusal), "{id}: {}", app.status);
        }
    }
}
