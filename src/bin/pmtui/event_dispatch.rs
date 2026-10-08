//! Crossterm event routing and bounded mouse-wheel coalescing.

use crate::*;

/// Events one frame may absorb before it must draw.
pub(crate) const COALESCE_MAX: usize = 64;
pub(crate) const IDLE_POLL_MS: u64 = 500;
pub(crate) const REFRESH_MS: u128 = 500;

fn wheel_direction(kind: MouseEventKind) -> Option<bool> {
    match kind {
        MouseEventKind::ScrollDown => Some(true),
        MouseEventKind::ScrollUp => Some(false),
        _ => None,
    }
}

fn scroll_full_screen_mode(app: &mut App, down: bool) -> bool {
    let max = app.scroll_max.get();
    let answer_top = app.answer_scroll_top.get().min(max);
    match &mut app.mode {
        UiMode::Decisions { scroll, .. } => {
            *scroll = if down {
                scroll.saturating_sub(app::scroll::WHEEL_LINES)
            } else {
                scroll.saturating_add(app::scroll::WHEEL_LINES).min(max)
            };
        }
        UiMode::Answering { scroll, .. } => {
            *scroll = if down {
                answer_top.saturating_add(app::scroll::WHEEL_LINES).min(max)
            } else {
                answer_top.saturating_sub(app::scroll::WHEEL_LINES)
            };
        }
        _ => return false,
    }
    true
}

/// Apply one input event. Returns whether it was a mouse-wheel scroll, the only
/// event class the run loop coalesces before redrawing.
pub(crate) fn handle_event(app: &mut App, ev: Event, handled_key: &mut bool) -> bool {
    match ev {
        Event::Key(k) if k.kind == KeyEventKind::Press => {
            handle_key(app, k.code, k.modifiers);
            *handled_key = true;
        }
        Event::Paste(text) => {
            handle_paste(app, &text);
            *handled_key = true;
        }
        Event::Mouse(m) => {
            if let Some(down) = wheel_direction(m.kind)
                && scroll_full_screen_mode(app, down)
            {
                *handled_key = true;
                return true;
            }
            if let Some(down) = wheel_direction(m.kind)
                && matches!(app.mode, UiMode::Board)
            {
                if app.board_detail_open
                    && app.panes.get().hit(m.column, m.row) == Some(Pane::Detail)
                {
                    app.wheel_pane(Pane::Detail, down);
                    *handled_key = true;
                    return true;
                }
                // A lane wheel MOVES the selection, so it is as inert as j/k while a detail is
                // open (`handle_key`'s Board arm): the lanes are not drawn there, and a queued
                // wheel over the frame before Enter must not swap the card being inspected.
                let column = (!app.board_detail_open)
                    .then(|| {
                        app.board_column_hits
                            .borrow()
                            .iter()
                            .find(|(area, _)| rect_contains(*area, m.column, m.row))
                            .map(|(_, column)| *column)
                    })
                    .flatten();
                if let Some(column) = column {
                    let selected = app.board_wheel_column(column, down);
                    *handled_key = true;
                    // A new selection changes the card-specific bottom menu. Stop coalescing so a
                    // queued click sees freshly rendered chips (the Sessions-pane rule).
                    return !selected;
                }
            }
            // A wheel over the Settings rows — or over an open dropdown's options — moves that cursor,
            // the same thing it does over the session list. Only over the ROWS: elsewhere in the view
            // there is nothing to scroll, and a wheel that moved a cursor the pointer was not on would
            // change what Enter does.
            if let Some(down) = wheel_direction(m.kind)
                && matches!(app.mode, UiMode::Settings { .. })
                && app.settings_hits.borrow().iter().any(|(y, _)| *y == m.row)
            {
                app.settings_move(if down { 1 } else { -1 });
                *handled_key = true;
                return false;
            }
            if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                let action = app
                    .key_hits
                    .borrow()
                    .iter()
                    .find(|hit| hit.contains(m.column, m.row))
                    .map(|hit| (hit.code, hit.modifiers));
                if let Some((code, modifiers)) = action {
                    handle_key(app, code, modifiers);
                    *handled_key = true;
                    return false;
                }
                let top_action = app
                    .top_hits
                    .borrow()
                    .iter()
                    .find(|hit| hit.contains(m.column, m.row))
                    .map(|hit| (hit.code, hit.modifiers));
                if let Some((code, modifiers)) = top_action {
                    handle_key(app, code, modifiers);
                    *handled_key = true;
                    return false;
                }
                let create_field = app
                    .create_hits
                    .borrow()
                    .iter()
                    .find(|(area, _)| rect_contains(*area, m.column, m.row))
                    .map(|(_, field)| *field);
                if let Some(field) = create_field
                    && let UiMode::Creating(form) = &mut app.mode
                    && form.shows_field(field)
                {
                    form.field = field;
                    *handled_key = true;
                    return false;
                }
                let switch_result = app
                    .switch_hits
                    .borrow()
                    .iter()
                    .find(|(area, _)| rect_contains(*area, m.column, m.row))
                    .map(|(_, result)| *result);
                if let Some(result) = switch_result {
                    app.choose_switch_result(result);
                    *handled_key = true;
                    return false;
                }
                if matches!(app.mode, UiMode::Board) && app.board_detail_open {
                    if rect_contains(app.preview_attach_hit.get(), m.column, m.row) {
                        handle_key(app, KeyCode::Enter, KeyModifiers::NONE);
                        *handled_key = true;
                        return false;
                    }
                    if rect_contains(app.composer_hit.get(), m.column, m.row) {
                        handle_key(app, KeyCode::Char('s'), KeyModifiers::NONE);
                        *handled_key = true;
                        return false;
                    }
                }
                // A Settings row, or an option of the open dropdown — the hit list carries whichever is
                // on screen. A click does what the pointer means on a dropdown: on a ROW it opens the
                // list, on an OPTION it picks that value, through the same `settings_open` /
                // `settings_commit` the keyboard uses rather than a shortcut past them.
                let settings_row = app
                    .settings_hits
                    .borrow()
                    .iter()
                    .find(|(y, _)| *y == m.row)
                    .map(|(_, index)| *index);
                if let Some(index) = settings_row
                    && matches!(app.mode, UiMode::Settings { .. })
                {
                    let picking = matches!(app.mode, UiMode::Settings { open: Some(_), .. });
                    app.settings_select(index);
                    if picking {
                        app.settings_commit();
                    } else {
                        app.settings_open();
                    }
                    *handled_key = true;
                    return false;
                }
                let board_session = app
                    .board_hits
                    .borrow()
                    .iter()
                    .find(|(area, _)| rect_contains(*area, m.column, m.row))
                    .map(|(_, id)| id.clone());
                if let Some(id) = board_session
                    && matches!(app.mode, UiMode::Board)
                {
                    app.open_board_session(&id);
                    *handled_key = true;
                    return false;
                }
            }
            if matches!(app.mode, UiMode::Normal | UiMode::Sending { .. }) {
                match m.kind {
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                        // The open composer is bound to the session it opened on, and no key can
                        // move the selection while it is open. A sessions-pane wheel would, so the
                        // preview above the field would stop showing where Enter delivers.
                        if let Some(pane) = app.panes.get().hit(m.column, m.row).filter(|pane| {
                            *pane != Pane::Sessions || matches!(app.mode, UiMode::Normal)
                        }) {
                            app.wheel_pane(pane, m.kind == MouseEventKind::ScrollDown);
                            *handled_key = true;
                            // A sessions-pane wheel changes selection. Stop coalescing so the
                            // next queued click sees freshly rendered row/preview geometry.
                            return pane != Pane::Sessions;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if matches!(app.mode, UiMode::Normal)
                            && app.panes.get().hit(m.column, m.row) == Some(Pane::Sessions) =>
                    {
                        let pick = app
                            .row_hits
                            .borrow()
                            .iter()
                            .find(|(y, _)| *y == m.row)
                            .map(|(_, index)| *index);
                        if let Some(index) = pick {
                            app.move_sel(index as isize - app.selected as isize);
                            *handled_key = true;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if matches!(app.mode, UiMode::Normal)
                            && rect_contains(app.preview_attach_hit.get(), m.column, m.row) =>
                    {
                        app.request_attach();
                        *handled_key = true;
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if matches!(app.mode, UiMode::Normal)
                            && rect_contains(app.composer_hit.get(), m.column, m.row) =>
                    {
                        handle_key(app, KeyCode::Char('s'), KeyModifiers::NONE);
                        *handled_key = true;
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    false
}

pub(crate) fn drain_events(
    app: &mut App,
    mut poll: impl FnMut(Duration) -> std::io::Result<bool>,
    mut read: impl FnMut() -> std::io::Result<Event>,
) -> Result<bool> {
    // A fork `f` queued last frame runs now that its "forking" status is on screen. Input that
    // arrived while it blocked was aimed at a frame that no longer exists: the fork may have
    // selected its child, where a stray Enter would attach and `d`, `y` would delete it.
    if app.run_pending_fork() {
        while poll(Duration::ZERO)? {
            read()?;
        }
        return Ok(true);
    }
    let mut handled_key = false;
    // ONE cadence. There used to be a second, eight times faster, whenever any row was on
    // Autopilot — it existed to animate a TachyonFX sweep over the row's id, and that effect is gone,
    // so the fast poll was spending seven extra wakeups a second to redraw an unchanged frame.
    // Keystroke latency is unaffected: `poll` returns the moment an event arrives, not at the timeout.
    if poll(Duration::from_millis(IDLE_POLL_MS))? {
        for _ in 0..COALESCE_MAX {
            let scrolled = handle_event(app, read()?, &mut handled_key);
            if !scrolled || !poll(Duration::ZERO)? {
                break;
            }
        }
    }
    Ok(handled_key)
}
