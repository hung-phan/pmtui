use super::*;
use std::cell::RefCell;
use std::collections::VecDeque;

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(ratatui::crossterm::event::MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn pending_card(id: &str) -> ProjectView {
    let mut view = agent_loop_view(id);
    view.posture = Posture::Fresh;
    view.last_activity = None;
    view
}

fn paused_card(id: &str) -> ProjectView {
    let mut view = agent_loop_view(id);
    view.enabled = false;
    view
}

fn autopilot_card(id: &str) -> ProjectView {
    let mut view = autopilot_loop_view(id);
    view.posture = Posture::Monitoring;
    view.agent_working = Some(false);
    view
}

fn working_card(id: &str) -> ProjectView {
    let mut view = view(id, Posture::Running, vec![]);
    view.agent_working = Some(true);
    view
}

fn select(app: &mut App, id: &str) {
    app.selected = app
        .projects
        .iter()
        .position(|view| view.id == id)
        .expect("card present");
}

fn selected_id(app: &App) -> Option<&str> {
    app.selected_view().map(|view| view.id.as_str())
}

fn drawn_columns(app: &App) -> Vec<BoardColumn> {
    app.board_column_hits
        .borrow()
        .iter()
        .map(|(_, column)| *column)
        .collect()
}

fn column_area(app: &App, column: BoardColumn) -> Rect {
    app.board_column_hits
        .borrow()
        .iter()
        .find(|(_, drawn)| *drawn == column)
        .map(|(area, _)| *area)
        .unwrap_or_else(|| panic!("{column:?} is not on screen"))
}

fn drawn_cards(app: &App) -> Vec<(Rect, String)> {
    app.board_hits.borrow().clone()
}

fn wheel_down_over(app: &mut App, area: Rect) -> bool {
    let mut handled = false;
    let coalesce = handle_event(
        app,
        mouse(MouseEventKind::ScrollDown, area.x + 1, area.y + 2),
        &mut handled,
    );
    assert!(handled);
    coalesce
}

/// The lane window was recomputed from the selected column's index every frame, and its pages
/// overlapped: selecting a card in an already-visible lane could flip the page and slide another
/// lane under the pointer, so the next wheel tick selected a card in the wrong lane.
#[test]
fn the_lane_window_holds_still_while_the_wheel_works_its_leftmost_lane() {
    let mut app = app_with(
        vec![
            paused_card("paused"),
            pending_card("pending-a"),
            pending_card("pending-b"),
            working_card("working"),
        ],
        UiMode::Board,
    );
    select(&mut app, "working");
    let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let window = vec![
        BoardColumn::Pending,
        BoardColumn::Autopilot,
        BoardColumn::Working,
    ];
    assert_eq!(drawn_columns(&app), window);
    let pending = column_area(&app, BoardColumn::Pending);

    wheel_down_over(&mut app, pending);
    assert_eq!(selected_id(&app), Some("pending-a"));
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert_eq!(drawn_columns(&app), window, "the window flipped pages");
    assert_eq!(column_area(&app, BoardColumn::Pending), pending);

    wheel_down_over(&mut app, pending);
    assert_eq!(
        selected_id(&app),
        Some("pending-b"),
        "the second tick left the lane under the pointer"
    );
}

#[test]
fn a_two_up_window_keeps_the_wheeled_lane_in_place() {
    let mut app = app_with(
        vec![
            pending_card("pending"),
            autopilot_card("autopilot"),
            working_card("working"),
        ],
        UiMode::Board,
    );
    select(&mut app, "working");
    let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let window = vec![BoardColumn::Autopilot, BoardColumn::Working];
    assert_eq!(drawn_columns(&app), window);

    let autopilot = column_area(&app, BoardColumn::Autopilot);
    wheel_down_over(&mut app, autopilot);
    assert_eq!(selected_id(&app), Some("autopilot"));
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert_eq!(drawn_columns(&app), window);
}

#[test]
fn keyboard_lane_moves_scroll_the_window_only_past_its_edge() {
    let mut app = app_with(
        vec![
            view("needs", Posture::NeedsYou, vec![]),
            pending_card("pending"),
            autopilot_card("autopilot"),
            working_card("working"),
        ],
        UiMode::Board,
    );
    select(&mut app, "working");
    let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();
    let late = vec![
        BoardColumn::Pending,
        BoardColumn::Autopilot,
        BoardColumn::Working,
    ];

    for (key, selected) in [
        (KeyCode::Char('h'), "autopilot"),
        (KeyCode::Left, "pending"),
    ] {
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert_eq!(drawn_columns(&app), late);
        handle_key(&mut app, key, KeyModifiers::NONE);
        assert_eq!(selected_id(&app), Some(selected));
    }
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert_eq!(
        drawn_columns(&app),
        late,
        "an on-screen lane flipped the page"
    );

    handle_key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(selected_id(&app), Some("needs"));
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let early = vec![
        BoardColumn::NeedsYou,
        BoardColumn::Pending,
        BoardColumn::Autopilot,
    ];
    assert_eq!(
        drawn_columns(&app),
        early,
        "leaving the window scrolls it by one lane"
    );

    handle_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
    assert_eq!(selected_id(&app), Some("pending"));
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert_eq!(drawn_columns(&app), early, "moving back inside holds still");
}

/// Within a lane the first visible card was re-anchored so the selection sat in the bottom slot,
/// so clicking an already-visible card moved it out from under the pointer.
#[test]
fn a_clicked_card_stays_under_the_pointer() {
    let mut app = app_with(
        (1..=4).map(|n| pending_card(&format!("p{n}"))).collect(),
        UiMode::Board,
    );
    select(&mut app, "p4");
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let cards = drawn_cards(&app);
    let ids: Vec<&str> = cards.iter().map(|(_, id)| id.as_str()).collect();
    assert_eq!(ids, ["p3", "p4"], "two slots, scrolled to the selection");
    let top = cards[0].0;

    for _ in 0..2 {
        let mut handled = false;
        handle_event(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                top.x + 1,
                top.y + 1,
            ),
            &mut handled,
        );
        assert_eq!(selected_id(&app), Some("p3"));
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert_eq!(drawn_cards(&app), cards, "the clicked card moved");
    }

    handle_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let ids: Vec<String> = drawn_cards(&app).into_iter().map(|(_, id)| id).collect();
    assert_eq!(ids, ["p2", "p3"], "moving past the top edge scrolls by one");
}

/// `board_focus_column` answered false both for "already in this lane" and "this lane is empty",
/// and the wheel handler read false as the former — so a wheel over an empty lane moved the
/// selection inside a different lane.
#[test]
fn a_wheel_over_an_empty_lane_changes_nothing() {
    let mut app = app_with(
        vec![
            view("needs-1", Posture::NeedsYou, vec![]),
            view("needs-2", Posture::NeedsYou, vec![]),
        ],
        UiMode::Board,
    );
    let mut terminal = Terminal::new(TestBackend::new(150, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let paused = column_area(&app, BoardColumn::Paused);

    let coalesce = wheel_down_over(&mut app, paused);

    assert_eq!(selected_id(&app), Some("needs-1"));
    assert!(coalesce, "a wheel that changed nothing keeps coalescing");
}

/// A lane wheel that changes the selection also changes the card-specific bottom menu, so a click
/// queued behind it must wait for that menu to be redrawn — the Sessions-pane rule.
#[test]
fn a_selecting_board_wheel_forces_a_redraw_before_a_queued_click() {
    let mut app = app_with(
        vec![
            view("needs", Posture::NeedsYou, vec![]),
            autopilot_card("autopilot"),
        ],
        UiMode::Board,
    );
    select(&mut app, "autopilot");
    let mut terminal = Terminal::new(TestBackend::new(150, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let needs = column_area(&app, BoardColumn::NeedsYou);
    let pause = app
        .key_hits
        .borrow()
        .iter()
        .find(|hit| hit.code == KeyCode::Char('p'))
        .map(|hit| hit.area)
        .expect("Pause chip for the Autopilot card");
    let events = RefCell::new(VecDeque::from([
        mouse(MouseEventKind::ScrollDown, needs.x + 1, needs.y + 2),
        mouse(MouseEventKind::Down(MouseButton::Left), pause.x, pause.y),
    ]));

    assert!(
        drain_events(
            &mut app,
            |_| Ok(!events.borrow().is_empty()),
            || Ok(events.borrow_mut().pop_front().expect("event queued")),
        )
        .unwrap()
    );

    assert_eq!(selected_id(&app), Some("needs"));
    assert_eq!(
        events.borrow().len(),
        1,
        "the click waits for a fresh frame"
    );
    assert!(
        app.projects.iter().all(|view| view.enabled),
        "nothing paused"
    );
}

#[test]
fn a_board_wheel_that_keeps_the_selection_keeps_coalescing() {
    let mut app = app_with(
        vec![autopilot_card("autopilot-1"), autopilot_card("autopilot-2")],
        UiMode::Board,
    );
    select(&mut app, "autopilot-2");
    let mut terminal = Terminal::new(TestBackend::new(150, 28)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let lane = column_area(&app, BoardColumn::Autopilot);
    let events = RefCell::new(VecDeque::from([
        mouse(MouseEventKind::ScrollDown, lane.x + 1, lane.y + 2),
        mouse(MouseEventKind::ScrollDown, lane.x + 1, lane.y + 2),
    ]));

    drain_events(
        &mut app,
        |_| Ok(!events.borrow().is_empty()),
        || Ok(events.borrow_mut().pop_front().expect("event queued")),
    )
    .unwrap();

    assert_eq!(
        selected_id(&app),
        Some("autopilot-2"),
        "already the last card"
    );
    assert!(events.borrow().is_empty(), "both no-op ticks coalesced");
}
