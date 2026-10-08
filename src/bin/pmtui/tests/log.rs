//! The status LOG FILE and the one surface that shows a status (the keybar); the keyboard moving the
//! session selection; and the mouse wheel scrolling whichever pane the pointer is over.
//!
//! There used to be a ` STATUS ` pane under the sessions list, and most of this file was about its
//! geometry: how tall it grew, how it wrapped, how it marked entries, how far back it was scrolled.
//! The log is a file now, so those tests went with the pane; what remains is the behaviour that
//! outlived it.

use super::*;
use crate::app::scroll::{PAGE_LINES, WHEEL_LINES};

/// A status long enough to prove the point: the keybar caps a status at half the bar and truncates
/// the tail, so the whole of this one is only ever readable in the log file.
const LONG: &str = "created test (standard); running \u{2014} you drive it (Enter)";

fn logged_app() -> App {
    let mut app = app_with(vec![agent_loop_view("test")], UiMode::Normal);
    app.status = LONG.into();
    app.record_status();
    app.status = "sent \u{2192} test".into();
    app.record_status();
    app
}

/// The plain text of every screen row.
fn text_rows(t: &Terminal<TestBackend>) -> Vec<String> {
    screen_rows(t).into_iter().map(|(s, _)| s).collect()
}

/// One mouse event at a point. Built by hand rather than driven through `run`, because the routing
/// under test is `handle_event`'s and `run` needs a real terminal to read from.
fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(ratatui::crossterm::event::MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// THE LOG COSTS THE DASHBOARD NO ROWS. It used to take the bottom of the left column as a pane —
/// which is why it could not appear below `WIDE_W`, and why a short terminal had to choose between
/// history and session rows. A file has no such trade: the sessions list gets the whole column at
/// every size, and the status still reads on the keybar.
#[test]
fn the_status_log_spends_no_dashboard_rows() {
    let app = logged_app();
    for (w, h) in [(140, 36), (140, 9), (80, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rows = text_rows(&terminal);
        let joined = rows.join("\n");
        assert!(
            !joined.contains(" STATUS "),
            "{w}x{h}: the status pane is back:\n{joined}"
        );
        // The body reaches the keybar: whichever pane sits lowest in this layout tier — the list alone
        // when narrow, the list above the detail when stacked, the list beside it when wide — ends on
        // the row above the bar, with nothing in between.
        let panes = app.panes.get();
        assert_eq!(
            panes.sessions.bottom().max(panes.detail.bottom()),
            h - 1,
            "{w}x{h}: something took rows under the body"
        );
        assert!(
            rows.last().expect("keybar").contains("sent"),
            "{w}x{h}: the keybar must carry the status:\n{joined}"
        );
    }
    // And the log itself kept both lines, whatever the terminal did.
    assert_eq!(
        log_lines(&app),
        vec![LONG.to_string(), "sent \u{2192} test".to_string()]
    );
}

#[test]
fn the_keyboard_moves_the_session_selection() {
    // There is no focused pane anymore: j/k/PgUp/PgDn/Home/End always move the SESSION SELECTION,
    // and the text panes scroll only by mouse wheel (user: *"we can just use mouse to scroll"*).
    // So a keypress must NEVER touch a text-pane scroll.
    let mut app = logged_app();
    app.detail_max.set(50);
    for i in 0..30 {
        app.projects.push(agent_loop_view(&format!("s{i}")));
    }
    app.selected = 0;

    handle_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(app.selected, 1, "j moves the selection down");
    handle_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(app.selected, 0, "k moves it back up");

    // PgDn/PgUp move a screenful of ROWS.
    handle_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
    assert_eq!(app.selected, PAGE_LINES, "PgDn jumps a screenful of rows");
    handle_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(app.selected, 0, "PgUp jumps back");

    // Home/End jump to the first/last row (and clamp — no runaway).
    handle_key(&mut app, KeyCode::End, KeyModifiers::NONE);
    assert_eq!(
        app.selected,
        app.projects.len() - 1,
        "End selects the last row"
    );
    handle_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    assert_eq!(app.selected, 0, "Home selects the first row");

    // No keypress ever moved a text-pane scroll — those are mouse-only now.
    assert_eq!(
        app.detail_scroll, 0,
        "the transcript never scrolled from a key"
    );

    // The NUMBERS switch the top-level projection without moving the selected session.
    // Tab does not: it was removed for duplicating `2`, so the key is free.
    let before = app.selected;
    handle_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Normal),
        "Tab is inert on the dashboard"
    );
    handle_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Board));
    handle_key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(app.selected, before, "view switching preserves selection");
}

#[test]
fn the_wheel_scrolls_the_pane_under_the_pointer() {
    // The mouse routing the run loop performs, tested where the logic is. User: *"we can just use
    // mouse to scroll at that point"*. `hit` maps a point to the pane the wheel then scrolls.
    let panes = PaneRects {
        sessions: Rect::new(0, 1, 40, 10),
        detail: Rect::new(40, 1, 60, 20),
    };
    assert_eq!(panes.hit(5, 5), Some(Pane::Sessions));
    // Anywhere inside the detail pane → the transcript (the inline autopilot feed sub-rect is gone).
    assert_eq!(panes.hit(50, 2), Some(Pane::Detail));
    assert_eq!(panes.hit(50, 10), Some(Pane::Detail));
    // Below the list is nothing now: the status pane that used to sit there is gone, so a wheel
    // there scrolls nothing rather than history the human cannot see.
    assert_eq!(panes.hit(5, 12), None);
    assert_eq!(panes.hit(5, 0), None, "the top bar belongs to no pane");
    assert_eq!(panes.hit(200, 200), None, "and neither does off-screen");
    // A pane that was not drawn this frame is `Rect::ZERO`, and a zero rect must never swallow a
    // wheel — otherwise a wheel over the header would scroll a pane the human cannot see.
    assert_eq!(PaneRects::default().hit(0, 0), None);

    // A wheel notch over the DETAIL pane scrolls the transcript.
    let mut app = logged_app();
    app.detail_max.set(90);
    app.wheel_pane(Pane::Detail, false);
    // THE LITERAL 3, deliberately, not `WHEEL_LINES`. Asserting a constant against itself is a
    // tautology that holds for any value — and it did: sabotaging `WHEEL_LINES` back to 1 (the
    // laggy behaviour the user reported) left this test passing. A test that cannot fail when the
    // behaviour reverts is not covering the behaviour.
    assert_eq!(
        app.detail_scroll, 3,
        "a wheel notch must move three lines, the convention every terminal uses"
    );
    assert_eq!(
        app.detail_scroll, WHEEL_LINES,
        "…and that is what the constant says"
    );

    // In the LIST a notch is ONE ROW: the selection decides which session every key acts on, so
    // three rows per notch would fling the human past the row they were aiming at.
    app.wheel_pane(Pane::Sessions, true);
    assert_eq!(
        app.selected, 0,
        "one row per notch, and there is only one row"
    );
}

#[test]
fn wheel_scrolling_clamps_the_transcript_in_both_directions() {
    let mut app = logged_app();
    app.detail_max.set(5);

    app.wheel_pane(Pane::Detail, true);
    assert_eq!(app.detail_scroll, 0, "down at the tail saturates");
    app.wheel_pane(Pane::Detail, false);
    app.wheel_pane(Pane::Detail, false);
    assert_eq!(app.detail_scroll, 5, "up clamps at the measured max");
    app.wheel_pane(Pane::Detail, true);
    assert_eq!(app.detail_scroll, 2);
}

#[test]
fn a_left_click_on_the_sessions_pane_selects_that_row() {
    // User: *"i think the UI is not clickable on the left panel. i want click to just select it."*
    // A real render populates `row_hits` (screen row → project index); a click at a mapped row must
    // land on that project, and a click OUTSIDE the list must select nothing.
    use ratatui::crossterm::event::MouseButton;
    let mut app = app_with(
        vec![
            agent_loop_view("alpha"),
            agent_loop_view("bravo"),
            agent_loop_view("charlie"),
        ],
        UiMode::Normal,
    );
    app.selected = 0;
    let mut t = Terminal::new(TestBackend::new(140, 24)).unwrap();
    t.draw(|f| render(f, &app)).expect("render");

    // A row that is NOT the current selection, taken from the render's own click-map.
    let (row_y, target) = app
        .row_hits
        .borrow()
        .iter()
        .find(|(_, pi)| *pi != app.selected)
        .copied()
        .expect("more than one selectable row was drawn");
    let sx = app.panes.get().sessions.x + 2; // inside the pane, past the `▸ ` gutter

    let mut handled = false;
    handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), sx, row_y),
        &mut handled,
    );
    assert_eq!(
        app.selected, target,
        "a left-click selects the session row it lands on"
    );
    assert!(
        handled,
        "a click counts as input (it suppresses the refresh)"
    );

    // A click OUTSIDE the sessions pane (far right, over the preview) selects nothing — `row_hits`
    // is keyed by ROW alone, so the pane gate is what stops a same-row preview click from selecting.
    let before = app.selected;
    handle_event(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 139, row_y),
        &mut handled,
    );
    assert_eq!(
        app.selected, before,
        "a click outside the list must not move the selection"
    );
}

#[test]
fn a_wheel_burst_is_drained_into_one_frame_and_a_key_ends_the_drain() {
    // User: *"The mouse scroll on the main panel is laggy"*. A flick arrives as a BURST, and the loop
    // drew once per event — with a `tmux capture-pane` fork inside each draw. `handle_event`'s return
    // value is what lets `run` absorb the whole burst first: TRUE means "that was a scroll, keep
    // draining", and anything else stops the drain so at most one key is handled per frame.
    let mut app = logged_app();
    app.detail_max.set(90);
    app.panes.set(PaneRects {
        sessions: Rect::new(0, 1, 40, 10),
        detail: Rect::new(40, 1, 60, 20),
    });
    let mut handled = false;

    for i in 1..=4 {
        let drain = handle_event(
            &mut app,
            mouse(MouseEventKind::ScrollUp, 50, 5),
            &mut handled,
        );
        assert!(drain, "a scroll must keep the drain going");
        assert_eq!(
            app.detail_scroll,
            i * WHEEL_LINES,
            "every notch in the burst must land"
        );
    }
    assert!(
        handled,
        "a scroll counts as input (it suppresses the refresh)"
    );

    // A KEY ends the drain — that is what keeps "one key per iteration" true, so a keystroke never
    // waits behind a flick and a key that opens an overlay is drawn before the next event.
    let drain = handle_event(
        &mut app,
        Event::Key(ratatui::crossterm::event::KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
        )),
        &mut handled,
    );
    assert!(!drain, "a key must stop the drain");
}

#[test]
fn the_transcript_window_can_reach_the_very_first_line_of_a_deep_capture() {
    // The other half of *"not scroll all the ways"*: given a deep capture, the window math must reach
    // ALL the way back, not merely further. 100 lines through a 10-row region is 90 of scroll, and at
    // 90 the first line of the capture is the first line shown.
    let capture: String = (0..100).map(|i| format!("line-{i}\n")).collect();
    let (tail, max) = pane_window(&capture, 10, 0);
    assert_eq!(max, 90, "max is content minus the window");
    assert_eq!(rows(&tail).first().unwrap().trim(), "line-90");

    let (top, max_again) = pane_window(&capture, 10, max);
    assert_eq!(max_again, max, "the max must not move as we scroll");
    assert_eq!(
        rows(&top).first().unwrap().trim(),
        "line-0",
        "scrolled fully back, the FIRST captured line must be on screen"
    );
    // And one notch past the end stays at the end rather than running off.
    let (still_top, _) = pane_window(&capture, 10, max + 50);
    assert_eq!(rows(&still_top).first().unwrap().trim(), "line-0");
}

/// The log does not repeat itself, and a session's worth of lines all survive. The in-memory log kept
/// only the newest twenty because it was drawn in a five-row pane and held in RAM; a file has neither
/// limit, so the twenty-entry bound is gone. (It is not infinite either — see the roll-over at
/// `MAX_LOG_BYTES` in `tests::status_log` — but that is a megabyte away, not twenty lines.)
#[test]
fn the_status_log_keeps_every_line_without_repeating_itself() {
    let mut app = app_with(vec![], UiMode::Normal);
    // A refused key pressed twice must not write the same line twice.
    app.status = "no question to answer".into();
    app.record_status();
    app.record_status();
    assert_eq!(log_lines(&app).len(), 1, "consecutive duplicates collapse");
    // An empty status is not an entry — plenty of paths clear it.
    app.status = String::new();
    app.record_status();
    assert_eq!(log_lines(&app).len(), 1);

    for i in 0..45 {
        app.status = format!("entry {i}");
        app.record_status();
    }
    let lines = log_lines(&app);
    assert_eq!(lines.len(), 46, "every line is kept: {lines:?}");
    assert_eq!(
        lines.first().map(String::as_str),
        Some("no question to answer")
    );
    assert_eq!(lines.last().map(String::as_str), Some("entry 44"));

    // A line that recurs LATER is its own entry, with its own timestamp — the dedupe is only against
    // the line just written.
    app.status = "no question to answer".into();
    app.record_status();
    assert_eq!(log_lines(&app).len(), 47);
}

#[test]
fn the_frame_bookkeeping_feeds_the_log_so_the_real_app_does_too() {
    // THE WIRING, not the mechanism. Every other test here calls `record_status` itself, so all of
    // them passed while the run loop never called it and the pane stayed empty in the real dashboard.
    // This drives the one step the loop actually performs after input — `after_input` — and asserts
    // the status a KEY produced ended up in the log.
    let mut app = app_with(vec![], UiMode::Normal);
    handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    let from_key = app.status.clone();
    assert!(!from_key.is_empty(), "the key under test must set a status");
    assert!(log_lines(&app).is_empty(), "nothing has recorded it yet");

    // `true` = this frame consumed input, which suppresses the tmux-shelling refresh. The log must
    // be fed on that path too — most statuses a human sees are the answer to a keystroke.
    app.after_input(true);
    assert_eq!(
        log_lines(&app).last().map(String::as_str),
        Some(from_key.as_str()),
        "the frame bookkeeping must record the status a key produced: {:?}",
        log_lines(&app)
    );

    // And an idle frame that changed nothing must not pile up copies of the same line.
    app.after_input(true);
    app.after_input(true);
    assert_eq!(
        log_lines(&app).len(),
        1,
        "idle frames must not duplicate: {:?}",
        log_lines(&app)
    );
}

#[test]
fn status_is_failure_catches_both_phrasings() {
    // The keybar used to test only "failed", so a "could not …" status — the wording most
    // `self.status = …` error arms use — rendered GREEN, the colour of success, whenever it
    // was the newest line on a narrow terminal. ONE predicate now colours both the keybar and
    // this pane, and it matches both ways a failure is phrased.
    assert!(status_is_failure("send failed: no such session"));
    assert!(status_is_failure("bot: could not change the mode"));
    // …and does not cry wolf over the ordinary success lines.
    assert!(!status_is_failure("created test (standard); running"));
    assert!(!status_is_failure(
        "bot resumed — you drive it; m starts autopilot"
    ));
}
