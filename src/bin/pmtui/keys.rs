//! Every keypress and every paste lands here. One dispatcher per input surface —
//! the dashboard and its overlays, the create form, and a bracketed paste — each of
//! which only decides WHICH `App` method or mode change a key means; the work itself
//! belongs to `App`.

use crate::*;

/// The text buffer of whichever ONE-LINE overlay is open, or `None` in every other mode.
///
/// The answer, cadence, rename and switcher overlays edit a [`Field`] — a caret and a `String`, which
/// is all a decision reply, an interval, an id or a query is — so the shared editing keys reach it
/// through this one place. The three PROSE surfaces (Message, goal, directive) are absent on purpose:
/// each edits a [`Composer`] and routes its own keys to the library. The create form is excluded too
/// — it has two text fields and its own `←/→` (which also cycle the toggle fields), so it routes
/// through [`CreateForm`] instead.
fn active_field(mode: &mut UiMode) -> Option<&mut Field> {
    match mode {
        UiMode::Answering { input, .. }
        | UiMode::EditingCadence { input, .. }
        | UiMode::Renaming { input, .. }
        | UiMode::Switching { query: input, .. } => Some(input),
        _ => None,
    }
}

/// Apply a shared single-line EDITING key to the open text field, returning whether it consumed
/// the key. Typing, Backspace, and the caret motions `←/→/Home/End/Delete` edit a [`Field`] the
/// same way in every overlay, so this is defined ONCE and each overlay's own handler keeps only
/// its mode-specific keys (Enter/Esc, and the answer's option cursor or the `^E`/`^X` chords).
///
/// Returns `false` — leaving the key for the caller — on Enter/Esc/Up/Down/PgUp/PgDn, which carry
/// per-overlay meaning (submit, cancel, the option cursor, question scroll). Every Ctrl chord is
/// intercepted by the caller BEFORE this runs, so a `Char` here is always plain typing.
fn edit_active_field(app: &mut App, code: KeyCode) -> bool {
    let Some(field) = active_field(&mut app.mode) else {
        return false;
    };
    match code {
        KeyCode::Char(c) => field.insert(c),
        KeyCode::Backspace => field.backspace(),
        KeyCode::Delete => field.delete(),
        KeyCode::Left => field.left(),
        KeyCode::Right => field.right(),
        KeyCode::Home => field.home(),
        KeyCode::End => field.end(),
        _ => return false,
    }
    true
}

fn message_routes_to_answer(app: &App) -> bool {
    app.selected_view()
        .is_some_and(|view| matches!(message_route(view), MessageRoute::Answer))
}

/// A Ctrl/Alt CHORD must not fire a bare-letter binding. In raw mode crossterm delivers e.g.
/// Ctrl+P as Char('p')+CONTROL, and the dashboard and Board matches key only on `code` — so every
/// Ctrl+letter used to fire the plain-letter action: Ctrl+P ran `pause_session` (killed the agent,
/// no confirm) and Ctrl+C — the reflex abort — opened the cadence editor. The five editing overlays
/// already gate their `^E`/`^X` chords on CONTROL; both projections do the same here. Ctrl+C maps
/// to the quit it should be; any other chord is swallowed. Returns whether the key was a chord.
/// (SHIFT is not gated: a shifted letter is a different `Char` that simply matches no arm.)
fn swallow_chord(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return false;
    }
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.should_quit = true;
    }
    true
}

/// `s` — the ONE Message/Answer route both projections share: an open stop opens Answer, anything
/// else opens the composer. From the Board the surface keeps Task view as its shell, and the
/// composer opens in Task detail because only detail shows the transcript its delivery lands in.
/// A refused open leaves no stale return flag and no detail the key did not open.
fn begin_message_or_answer(app: &mut App, from_board: bool) {
    let answer = message_routes_to_answer(app);
    app.return_to_board_after_answer = from_board && answer;
    app.return_to_board_after_send = from_board && !answer;
    if answer {
        app.begin_answer();
        if !matches!(app.mode, UiMode::Answering { .. }) {
            app.return_to_board_after_answer = false;
        }
        return;
    }
    app.begin_send();
    if !matches!(app.mode, UiMode::Sending { .. }) {
        app.return_to_board_after_send = false;
    } else if from_board {
        app.board_detail_open = true;
    }
}

pub(crate) fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    if matches!(app.mode, UiMode::Answering { .. }) {
        // How many options this stop offers, so ↑↓ can clamp. Read from the SELECTION rather
        // than remembered at open: a refresh can replace the stop under the overlay, and a
        // cursor that outlives a shorter list would send an option that no longer exists.
        let opts = app
            .selected_view()
            .and_then(|view| {
                let expected = app.answering_stop_id.as_deref();
                view.stops.iter().find(|stop| match expected {
                    Some(id) => stop.id == id,
                    None => true,
                })
            })
            .map(|s| s.options.len())
            .unwrap_or(0);
        let ceiling = app.scroll_max.get();
        let rendered_top = app.answer_scroll_top.get().min(ceiling);
        // Typing and caret motion (←/→/Home/End/Delete/Backspace) are the SHARED field edits;
        // ↑↓ (option cursor) and PgUp/PgDn (question scroll) below are the answer overlay's own.
        if edit_active_field(app, code) {
            if let UiMode::Answering { scroll, .. } = &mut app.mode {
                *scroll = rendered_top;
            }
            return;
        }
        match code {
            // ↑↓ move the option cursor — the "interactable" half of the report. They do NOT
            // move the dashboard selection: the overlay owns the keyboard while it is up, and
            // `begin_answer` already resolved which stop this is.
            KeyCode::Up | KeyCode::Down => {
                if let UiMode::Answering { choice, scroll, .. } = &mut app.mode
                    && opts > 0
                {
                    *choice = if code == KeyCode::Down {
                        (*choice + 1).min(opts - 1)
                    } else {
                        choice.saturating_sub(1)
                    };
                    *scroll = usize::MAX;
                }
            }
            // PgUp/PgDn read a long question. Separate keys from ↑↓ on purpose: conflating
            // "scroll the text" with "choose the answer" on one pair is how a reader ends up
            // having silently changed their decision.
            KeyCode::PageUp | KeyCode::PageDown => {
                if let UiMode::Answering { scroll, .. } = &mut app.mode {
                    *scroll = if code == KeyCode::PageDown {
                        rendered_top.saturating_add(ANSWER_PAGE_ROWS).min(ceiling)
                    } else {
                        rendered_top.saturating_sub(ANSWER_PAGE_ROWS)
                    };
                }
            }
            KeyCode::Enter => app.submit_answer(),
            KeyCode::Esc => {
                app.answering_stop_id = None;
                app.mode = if app.return_to_board_after_answer {
                    UiMode::Board
                } else {
                    UiMode::Normal
                };
                app.return_to_board_after_answer = false;
            }
            _ => {}
        }
        return;
    }
    if matches!(app.mode, UiMode::Creating(_)) {
        handle_create_key(app, code, mods);
        return;
    }
    if matches!(app.mode, UiMode::Renaming { .. }) {
        if edit_active_field(app, code) {
            return;
        }
        match code {
            KeyCode::Enter => app.submit_rename(),
            KeyCode::Esc => app.cancel_rename(),
            _ => {}
        }
        return;
    }
    // "Create this directory?" — y/Y/Enter makes it (Enter CONFIRMS here, unlike the destructive
    // `Confirming` overlay, because creating a folder is safe and reversible); any other key
    // returns to the create form with its fields intact.
    if matches!(app.mode, UiMode::ConfirmCreateDir { .. }) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => app.confirm_create_dir(),
            _ => app.cancel_create_dir(),
        }
        return;
    }
    // Inline goal field. Ctrl+E is intercepted BEFORE the Char arm for the same reason
    // `handle_create_key` does it — otherwise it types a literal 'e' into the goal —
    // and everything below is the create form's single-line mechanism: Char pushes,
    // Backspace pops.
    if matches!(app.mode, UiMode::EditingCadence { .. }) {
        // No `^E`: there is no multi-line version of a duration, so an editor here would be
        // ceremony around one number. Typing + caret motion are the shared field edits.
        if edit_active_field(app, code) {
            return;
        }
        match code {
            KeyCode::Enter => app.submit_cadence_edit(),
            KeyCode::Esc => {
                // Same honesty as the goal prompt's cancel: when this field is `m`'s second step,
                // the human pressed `m` to turn autopilot ON, so "cadence unchanged" would leave
                // them believing the flip happened and only a number was dropped.
                let then_autopilot = matches!(
                    app.mode,
                    UiMode::EditingCadence {
                        then_autopilot: true,
                        ..
                    }
                );
                app.mode = if app.return_to_board_after_action {
                    UiMode::Board
                } else {
                    UiMode::Normal
                };
                app.return_to_board_after_action = false;
                app.status = if then_autopilot {
                    "cancelled — autopilot stays off, still on Standard".into()
                } else {
                    "cadence unchanged".into()
                };
            }
            _ => {}
        }
        return;
    }
    // The inline goal field. A [`Composer`] like the Message field, so this ROUTES rather than
    // edits: the buffer answers which of the dashboard's actions a key meant and keeps every
    // readline chord for the library.
    if let UiMode::EditingGoal { input, .. } = &mut app.mode {
        let edit = input.key(code, mods);
        match edit {
            composer::Edit::Consumed | composer::Edit::Chord(_) => return,
            composer::Edit::Send => {
                app.submit_goal_edit();
                return;
            }
            // `^X^E`. Bare `^E` is readline's end-of-line in this buffer, as in every other one.
            composer::Edit::Editor => {
                app.escalate_goal_edit();
                return;
            }
            composer::Edit::Cancel => {}
        }
        {
            // A cancel from `m`'s prompt has to say that the DIAL did not move, not just that
            // an edit was dropped: the human pressed `m` to turn autopilot on, and "goal edit
            // cancelled" would leave them believing the flip happened and only the text was
            // discarded.
            let then_autopilot = matches!(
                app.mode,
                UiMode::EditingGoal {
                    then_autopilot: true,
                    ..
                }
            );
            app.mode = if app.return_to_board_after_action {
                UiMode::Board
            } else {
                UiMode::Normal
            };
            app.return_to_board_after_action = false;
            app.status = if then_autopilot {
                "cancelled — autopilot stays off, still on Standard".into()
            } else {
                "goal edit cancelled".into()
            };
        }
        return;
    }
    // The inline directive field (`i`). The goal field's mechanism plus ONE extra chord: `^X^R` =
    // RESCIND, the deliberate, explicit clear. A distinct chord ON PURPOSE — an empty save keeps the
    // directive (anti-wipe), so rescinding can never be "delete the text and save".
    if let UiMode::EditingDirective { input, .. } = &mut app.mode {
        match input.key(code, mods) {
            composer::Edit::Consumed => {}
            composer::Edit::Send => app.submit_directive_edit(),
            composer::Edit::Editor => app.escalate_directive_edit(),
            composer::Edit::Chord('r') => app.rescind_directive_from_field(),
            // Any other `^X` chord is a typo, not an action: the prefix is already spent.
            composer::Edit::Chord(_) => {}
            composer::Edit::Cancel => {
                app.mode = UiMode::Normal;
                app.status = "directive edit cancelled".into();
            }
        }
        return;
    }
    // The send field (`s`). Identical mechanism to the inline goal field above —
    // including `Ctrl+E` intercepted BEFORE the `Char` arm, or it types a literal 'e'.
    // THE MESSAGE COMPOSER is the one surface with a real editor behind it: a [`Composer`] wrapping
    // `ratatui-textarea`, whose own map is readline's. So this routes rather than edits — the composer
    // answers which of the dashboard's actions a key meant, and keeps everything else for the library.
    if let UiMode::Sending { input, .. } = &mut app.mode {
        match input.key(code, mods) {
            // A Message has no `^X` chord of its own beyond the editor, so one is a typo to swallow,
            // not an action. The directive field is the surface that uses `Chord`.
            composer::Edit::Consumed | composer::Edit::Chord(_) => {}
            composer::Edit::Send => app.submit_send(),
            composer::Edit::Cancel => app.park_send_draft(),
            // `^X^E`, bash's own `edit-and-execute-command`. Bare `^E` is readline's end-of-line here,
            // which is what a terminal-grade editor has to mean by it.
            composer::Edit::Editor => app.escalate_send(),
        }
        return;
    }
    if matches!(app.mode, UiMode::Switching { .. }) {
        if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return;
        }
        let before = match &app.mode {
            UiMode::Switching { query, .. } => query.as_str().to_string(),
            _ => String::new(),
        };
        if edit_active_field(app, code) {
            if let UiMode::Switching { query, cursor, .. } = &mut app.mode
                && query.as_str() != before
            {
                *cursor = 0;
            }
            return;
        }
        match code {
            KeyCode::Enter => app.choose_switch_cursor(),
            KeyCode::Esc => {
                app.mode = if app.return_to_board_after_switch {
                    UiMode::Board
                } else {
                    UiMode::Normal
                };
                app.return_to_board_after_switch = false;
            }
            KeyCode::Up => app.move_switch_cursor(-1),
            KeyCode::Down => app.move_switch_cursor(1),
            KeyCode::PageUp => app.move_switch_cursor(-(app::scroll::PAGE_LINES as isize)),
            KeyCode::PageDown => app.move_switch_cursor(app::scroll::PAGE_LINES as isize),
            _ => {}
        }
        return;
    }
    // SETTINGS is a view, so its keys read like the other views': the numbers select a view, the
    // movement keys move a cursor, Enter commits the row under it. Nothing here acts on a session, so
    // none of the lifecycle keys are routed — a stray `d` in Settings must not delete anything.
    if let UiMode::Settings { open, .. } = &app.mode {
        let picking = open.is_some();
        match code {
            // MOVEMENT IS THE SAME KEYS IN BOTH STATES; which cursor they move is `open`'s business
            // (`App::settings_move`), so a human never has to know which one they are driving.
            KeyCode::Char('j') | KeyCode::Down => app.settings_move(1),
            KeyCode::Char('k') | KeyCode::Up => app.settings_move(-1),
            KeyCode::PageDown => app.settings_move(app::scroll::PAGE_LINES as isize),
            KeyCode::PageUp => app.settings_move(-(app::scroll::PAGE_LINES as isize)),
            KeyCode::Home => app.settings_move(isize::MIN / 2),
            KeyCode::End => app.settings_move(isize::MAX / 2),
            // Enter DROPS the dropdown, then picks from it. Two presses to change a value, and the
            // list is on screen for the second one.
            KeyCode::Enter if picking => app.settings_commit(),
            KeyCode::Enter => app.settings_open(),
            // An open dropdown swallows the ways out: Esc closes IT, not the view, so a mis-press
            // costs a keystroke rather than the place you were standing in.
            KeyCode::Esc | KeyCode::Char('q') if picking => app.settings_close(),
            KeyCode::Char('1') | KeyCode::Esc | KeyCode::Char('q') => app.close_settings(),
            KeyCode::Char('2') if !picking => app.open_board(),
            _ => {}
        }
        return;
    }
    if matches!(app.mode, UiMode::Board) {
        if swallow_chord(app, code, mods) {
            return;
        }
        match code {
            // The numbered views SELECT, they do not toggle: `2` here is already-there, and `1` is
            // the way back — including out of an open card detail, where the board's own movement
            // keys are inert. `Tab` is not bound: `1`, `q` and `Esc` all already leave.
            KeyCode::Char('1') => app.close_board(),
            KeyCode::Char('2') => {}
            KeyCode::Char('0') => app.open_settings(),
            KeyCode::Char('q') => app.close_board(),
            KeyCode::Esc if app.board_detail_open => app.close_board_detail(),
            KeyCode::Esc => app.close_board(),
            KeyCode::Enter if app.board_detail_open => app.request_attach(),
            KeyCode::Enter => app.board_detail_open = true,
            KeyCode::Char(' ') => app.board_detail_open = !app.board_detail_open,
            // An OPEN DETAIL owns the whole work area: the lanes these keys navigate are not on
            // screen, so moving the selection only swapped the card being inspected out from under
            // the human (user: *"i don't want that to happen when we are in the big screen"*). They
            // are inert here rather than routed elsewhere — Esc closes the detail and hands them
            // back. The pointer's equivalent, a wheel over a lane, is gated the same way in
            // `handle_event`.
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Char('h' | 'j' | 'k' | 'l')
                if app.board_detail_open => {}
            KeyCode::Left | KeyCode::Char('h') => app.board_move_horizontal(-1),
            KeyCode::Right | KeyCode::Char('l') => app.board_move_horizontal(1),
            KeyCode::Up | KeyCode::Char('k') => app.board_move_vertical(-1),
            KeyCode::Down | KeyCode::Char('j') => app.board_move_vertical(1),
            KeyCode::Char('n') => app.begin_task_create(),
            KeyCode::Char('f') => {
                app.request_fork(true);
                app.board_detail_open = false;
            }
            KeyCode::Char('R') => {
                app.return_to_board_after_action = true;
                app.begin_rename();
                if !matches!(app.mode, UiMode::Renaming { .. }) {
                    app.return_to_board_after_action = false;
                }
            }
            // Lane `s` acts exactly where its chip is published (`board_lane_message_applies`).
            KeyCode::Char('s')
                if app.board_detail_open
                    || app.selected_view().is_some_and(board_lane_message_applies) =>
            {
                begin_message_or_answer(app, true)
            }
            KeyCode::Char('m') if app.board_detail_open => {
                app.return_to_board_after_action = true;
                app.cycle_tier();
                if matches!(app.mode, UiMode::Board) {
                    app.return_to_board_after_action = false;
                }
            }
            KeyCode::Char('p') if app.selected_view().is_some_and(|view| !view.enabled) => {
                app.request_attach()
            }
            KeyCode::Char('p') => app.pause_session(),
            KeyCode::Char('r') => {
                app.return_to_board_after_action = true;
                app.begin_restart();
                if !matches!(app.mode, UiMode::Confirming { .. }) {
                    app.return_to_board_after_action = false;
                }
            }
            KeyCode::Char('d') => {
                app.return_to_board_after_action = true;
                app.begin_delete();
                if !matches!(app.mode, UiMode::Confirming { .. }) {
                    app.return_to_board_after_action = false;
                }
            }
            KeyCode::Char('v') => {
                app.return_to_board_after_action = true;
                app.open_decisions();
                if !matches!(app.mode, UiMode::Decisions { .. }) {
                    app.return_to_board_after_action = false;
                }
            }
            KeyCode::Char('/') => {
                app.return_to_board_after_switch = true;
                app.begin_switcher();
                if !matches!(app.mode, UiMode::Switching { .. }) {
                    app.return_to_board_after_switch = false;
                }
            }
            _ => {}
        }
        return;
    }
    if let UiMode::Confirming { id, session, what } = &app.mode {
        let (id, session, what) = (id.clone(), session.clone(), *what);
        match code {
            // Only an explicit y/Y confirms. Enter is deliberately NOT a confirm
            // key — the overlay promises "any other key cancels", and Enter=attach
            // in Normal mode, so treating it as confirm would be a footgun that
            // kills a live agent on a reflex keystroke.
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let return_to_board = app.return_to_board_after_action;
                match what {
                    Confirmable::Remove => app.remove_project(&id, &session),
                    Confirmable::Restart => app.restart_agent(&id),
                    Confirmable::Apply => app.apply_job_commit(&id),
                }
                if return_to_board {
                    app.mode = UiMode::Board;
                    app.return_to_board_after_action = false;
                }
            }
            _ => {
                // Any other key (Enter/n/Esc/…) cancels — the safe default.
                app.mode = if app.return_to_board_after_action {
                    UiMode::Board
                } else {
                    UiMode::Normal
                };
                app.return_to_board_after_action = false;
                app.status = match what {
                    Confirmable::Remove => "removal cancelled".into(),
                    Confirmable::Restart => "restart cancelled".into(),
                    Confirmable::Apply => "nothing was applied".into(),
                };
            }
        }
        return;
    }
    // Full-screen wake-follow view: Esc/q exits (needs &mut app.mode to reassign),
    // so handle exit first, THEN borrow the scroll for the navigation keys.
    if matches!(app.mode, UiMode::WakeView { .. }) {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
            app.mode = UiMode::Normal;
            app.status = "left wake view".into();
            return;
        }
        // Clamp UP-scrolls against the last-rendered max so over-scroll can't run
        // `scroll` unbounded (which would freeze Down/PageDown). Read it BEFORE the
        // `&mut app.mode` borrow to avoid a double-borrow. Down/PageDown/End only
        // reduce scroll, so they need no clamp.
        let max = app.scroll_max.get();
        if let UiMode::WakeView { scroll, .. } = &mut app.mode {
            match code {
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_add(1).min(max),
                KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_sub(1),
                KeyCode::PageUp => *scroll = scroll.saturating_add(10).min(max),
                KeyCode::PageDown => *scroll = scroll.saturating_sub(10),
                KeyCode::End => *scroll = 0, // snap back to following the tail
                _ => {}
            }
        }
        return;
    }
    // The DECISION LANE view: Esc/q returns; the rest TAIL-FOLLOW like the wake view, because it is a
    // LOG — oldest at the top, newest at the bottom, `scroll` = lines UP from the tail. So Down moves
    // toward the newest (reduces scroll), Up scrolls into history (raises it, clamped against the
    // last-rendered max), End snaps to the newest and Home jumps to the oldest. Exit needs
    // `&mut app.mode`, so handle it before borrowing `scroll`.
    if matches!(app.mode, UiMode::Decisions { .. }) {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
            app.mode = if app.return_to_board_after_action {
                UiMode::Board
            } else {
                UiMode::Normal
            };
            app.return_to_board_after_action = false;
            app.status = "left the decision lane".into();
            return;
        }
        if matches!(code, KeyCode::Tab | KeyCode::BackTab) {
            if let UiMode::Decisions {
                tab,
                scroll,
                other_scroll,
                ..
            } = &mut app.mode
            {
                std::mem::swap(scroll, other_scroll);
                *tab = tab.other();
                app.scroll_max.set(0);
                app.status = format!(
                    "audit tab → {}",
                    match tab {
                        AuditTab::Turns => "turns",
                        AuditTab::Decisions => "decisions",
                    }
                );
            }
            return;
        }
        let max = app.scroll_max.get();
        if let UiMode::Decisions { scroll, .. } = &mut app.mode {
            match code {
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_add(1).min(max),
                KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_sub(1),
                KeyCode::PageUp => *scroll = scroll.saturating_add(10).min(max),
                KeyCode::PageDown => *scroll = scroll.saturating_sub(10),
                KeyCode::Home => *scroll = max, // top = oldest
                KeyCode::End => *scroll = 0,    // bottom = newest (follow the tail)
                _ => {}
            }
        }
        return;
    }
    // The MODEL PICKER (`e` decider / `w` worker): a single-select list, not a text field. Enter
    // ADVANCES (Engine→Model) or commits (on Model); Esc steps back (decider Model→Engine) or
    // cancels; ↑↓/jk move the cursor, clamped to the stage's row count. Reached from Normal only, so
    // closing always means Normal.
    if matches!(app.mode, UiMode::ModelPicker { .. }) {
        let len = match app.mode {
            UiMode::ModelPicker {
                stage: PickStage::Engine,
                ..
            } => Engine::ALL.len(),
            UiMode::ModelPicker {
                stage: PickStage::Model,
                engine,
                ..
            } => 1 + app.models_len(engine),
            _ => 0,
        };
        match code {
            KeyCode::Enter => app.advance_model_pick(),
            KeyCode::Esc => app.back_or_cancel_model_pick(),
            KeyCode::Up | KeyCode::Char('k') => {
                if let UiMode::ModelPicker { cursor, .. } = &mut app.mode {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let UiMode::ModelPicker { cursor, .. } = &mut app.mode {
                    *cursor = (*cursor + 1).min(len.saturating_sub(1));
                }
            }
            _ => {}
        }
        return;
    }
    // `?` key reference. Bound in NORMAL MODE ONLY, deliberately: `?` is a legitimate
    // typed character in the answer overlay and the create form's text fields, and the
    // confirm overlay promises "any other key cancels" — binding it there would either
    // steal a keystroke or break a promise. Closing therefore always means Normal, with
    // no previous-mode to remember.
    if matches!(app.mode, UiMode::Help { .. }) {
        // Scroll keys first; the max comes from the last render (the run loop always
        // draws before reading a key, so it is real by the time this matters).
        let max = app.scroll_max.get();
        if let UiMode::Help { scroll } = &mut app.mode {
            match code {
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll = scroll.saturating_add(1).min(max);
                    return;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll = scroll.saturating_sub(1);
                    return;
                }
                KeyCode::PageDown => {
                    *scroll = scroll.saturating_add(10).min(max);
                    return;
                }
                KeyCode::PageUp => {
                    *scroll = scroll.saturating_sub(10);
                    return;
                }
                KeyCode::Home => {
                    *scroll = 0;
                    return;
                }
                KeyCode::End => {
                    *scroll = max;
                    return;
                }
                _ => {}
            }
        }
        // Anything else closes — including `q`, which here CLOSES THE HELP rather than
        // quitting pmtui. One reflex `q` while reading the key list must not drop you
        // out of the TUI; a second `q`, back on the dashboard, still quits.
        app.mode = UiMode::Normal;
        return;
    }
    // The match below keys only on `code`, so a Ctrl/Alt chord is gated first (`swallow_chord`).
    if swallow_chord(app, code, mods) {
        return;
    }
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        // The movement keys move the SESSION SELECTION — there is no focused pane. The panes
        // (detail transcript, status log, autopilot feed) scroll by MOUSE WHEEL over whichever one
        // the pointer is on (user: *"we can just use mouse to scroll at that point"*).
        // `1`/`2` SELECT one of the two top-level projections — the numbers the status bar's view
        // controls carry, so what the header advertises is what the keyboard does. Pressing the
        // number of the view you are already on is deliberately a silent no-op, not a toggle:
        // "show me the Session view" is already satisfied, so there is no dead end to report.
        // Tab is deliberately NOT bound here. It duplicated `2`, and a key that every other
        // terminal program spends on focus or completion is worth more held in reserve than
        // spent on a second way to do what a labelled number key already does.
        KeyCode::Char('1') => {}
        KeyCode::Char('2') => app.open_board(),
        KeyCode::Char('0') => app.open_settings(),
        KeyCode::Char('j') | KeyCode::Down => app.move_sel(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_sel(-1),
        KeyCode::PageDown => app.move_sel(crate::app::scroll::PAGE_LINES as isize),
        KeyCode::PageUp => app.move_sel(-(crate::app::scroll::PAGE_LINES as isize)),
        // A page past either end; `move_sel` clamps, so `projects.len()` always reaches the edge.
        KeyCode::Home => app.move_sel(-(app.projects.len() as isize)),
        KeyCode::End => app.move_sel(app.projects.len() as isize),
        KeyCode::Enter => app.request_attach(),
        KeyCode::Char('/') => {
            app.return_to_board_after_switch = false;
            app.begin_switcher();
        }
        KeyCode::Char('n') => app.begin_create(),
        KeyCode::Char('f') => app.request_fork(false),
        KeyCode::Char('R') => {
            app.return_to_board_after_action = false;
            app.begin_rename();
        }
        // `m` for MODE — see its `BINDINGS` row for why the autonomy dial could not keep
        // the letter its old name ("tier") pointed at.
        KeyCode::Char('m') => {
            app.return_to_board_after_action = false;
            app.cycle_tier();
        }
        // `e` — OPEN the DECIDER picker (engine → model) for the selected session. Autopilot-only
        // (like `g`/`c`/`i`): the handler refuses on a row pmd does not drive and names `m`, since
        // the chip is hidden there but the key still fires.
        KeyCode::Char('e') => app.begin_model_pick(PickTarget::Decider),
        // `a` — APPLY the selected job's commit to this project's checkout. A job works in its own
        // worktree, so this is the one step that brings that work into the files a human is using, and
        // it is a human's step: the dashboard isolates, a person integrates.
        KeyCode::Char('a') => app.begin_apply(),
        // `w` — OPEN the WORKER model picker for the selected session. Any agent-loop row, at any
        // tier (the worker launch carries `--model` on both driven and human-driven paths), so this
        // opens straight on the Model stage for the entry's create-time engine.
        KeyCode::Char('w') => app.begin_model_pick(PickTarget::Worker),
        // `?` opens the key reference — the safety net for the adaptive keybar, which
        // drops chips on a narrow pane. Previously unbound.
        KeyCode::Char('?') => app.mode = UiMode::Help { scroll: 0 },
        // `g` edits the SELECTED session's live goal in $EDITOR. Previously unbound;
        // deliberately NOT folded into any existing key. Shifted `G` is the INLINE
        // field for the same goal — a shifted pair, so the two intensities of one
        // action stay on one letter instead of stealing an unrelated key.
        // ONE goal key. `g` opens the inline field and `^E` inside it hands off to
        // `$EDITOR` — the user's own words: *"Press g will allow to edit the goal inline,
        // when we in the view, press Ctrl+E will allow to edit it in editor."* The old
        // split (`G` inline, `g` straight to the editor) made a shifted pair carry a
        // distinction nobody can make before seeing the goal, and spent two keybar chips
        // saying it. `G` is now unbound.
        KeyCode::Char('g') => app.begin_goal_edit(),
        // `i` — the session's standing DIRECTIVE, the RESTRICTIVE counterpart to `g`'s goal.
        // `g` says what to do; `i` sets the one rule the decider may only ever forbid by.
        // Previously unbound.
        KeyCode::Char('i') => app.begin_directive_edit(),
        // `c` — how often pmd nudges this session. The other half of `g`: what to do, and
        // how often to be asked about it.
        KeyCode::Char('c') => app.begin_cadence_edit(),
        KeyCode::Char('d') => {
            app.return_to_board_after_action = false;
            app.begin_delete();
        }
        // `s` — Say one thing to the running agent, without taking over the terminal.
        // agent-deck spells this `s` (`hotkeyPromptSession = "o"`); we spell it after the
        // word on the chip.
        KeyCode::Char('s') => begin_message_or_answer(app, false),
        // `r` — restart the agent in place. Always confirms: it throws away the turn in
        // flight, and it sits one key from `d`.
        KeyCode::Char('r') => {
            app.return_to_board_after_action = false;
            app.begin_restart();
        }
        // `p` — pause: stop the driving AND kill the agent. No confirm, because it is
        // reversible with one keystroke (Enter) and loses nothing but the turn in flight —
        // the same thing `r` costs, which is why `r` confirms and this does not.
        KeyCode::Char('p') => app.pause_session(),
        // `v` — the DECISION LANE: review what autopilot decided across the fleet. Opens a
        // full-screen read-only view (like the wake follow); Esc/q returns. Fleet-wide, so it
        // does not gate on the selected row.
        KeyCode::Char('v') => {
            app.return_to_board_after_action = false;
            app.open_decisions();
        }
        _ => {}
    }
}

pub(crate) fn handle_create_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    // Ctrl+E composes the brief in $EDITOR. Intercept BEFORE the Char handler so
    // it isn't typed as a literal 'e' into the goal; `request_brief_edit` gates on
    // the Goal field, so pressing it elsewhere is a no-op. Inline single-line
    // typing is untouched (it arrives as a Char with no CONTROL modifier).
    if mods.contains(KeyModifiers::CONTROL)
        && matches!(code, KeyCode::Char('e') | KeyCode::Char('E'))
    {
        app.request_brief_edit();
        return;
    }
    // Esc/Enter need &mut App, so handle them before borrowing the form.
    match code {
        KeyCode::Esc => {
            // Back out of the candidate list FIRST. Esc means "undo the thing I just opened", and
            // losing a filled-in form to one stray press would be a bad trade for a picker nobody
            // asked to keep.
            if let UiMode::Creating(form) = &mut app.mode
                && form.clear_dir_pick()
            {
                return;
            }
            app.cancel_create_form();
            return;
        }
        KeyCode::Enter => {
            // A PICK IS A SUBLAYER with its own Enter and Esc: while one is live, `Enter` takes the
            // candidate and `Esc` backs out, and neither reaches the form. Submitting the form from
            // inside a list the human is still choosing in would create a session they had not
            // finished naming.
            if let UiMode::Creating(form) = &mut app.mode
                && form.accept_dir_pick()
            {
                return;
            }
            app.submit_create();
            return;
        }
        _ => {}
    }
    let UiMode::Creating(form) = &mut app.mode else {
        return;
    };
    let engine_before = form.engine;
    let decider_engine_before = form.decider_engine;
    match code {
        // TAB IS ALWAYS THE NEXT FIELD. It is the form's ONE unconditional exit, so no row can trap
        // the human — and the Directory row, which has a list and a ghost competing for keys, is
        // exactly the row that would. Taking something lives on keys that mean taking: `Enter` for
        // a picked candidate, `→` for the ghost.
        KeyCode::Tab => form.next_field(),
        KeyCode::BackTab => form.prev_field(),
        // `↑`/`↓` move the PICK while the Directory row is showing candidates, and move between
        // fields everywhere else — `move_dir_pick` reports which it did. Overloading these two is
        // safe only because `Tab`/`Shift+Tab` are not overloaded; the row's keybar says so.
        KeyCode::Down => {
            if !form.move_dir_pick(true) {
                form.next_field();
            }
        }
        KeyCode::Up => {
            if !form.move_dir_pick(false) {
                form.prev_field();
            }
        }
        // On a TEXT field (Directory/Goal) ←/→ move the caret; on every toggle field
        // (Engine/Model/Autonomy/Cadence/Decider/Decider Model) they step the value — the two Model
        // fields are now uniform ←→ steppers over `[(default)] ++ choices`, not comboboxes.
        KeyCode::Left => {
            if form.is_text_field() {
                form.caret_left();
            } else {
                form.adjust(false);
            }
        }
        // `→` at the end of the line accepts the ghost too, the way fish does it. It cannot
        // conflict: with the caret already at the end there is no rightward move to lose.
        KeyCode::Right if form.dir_ghost().is_some() => {
            form.accept_dir_ghost();
        }
        KeyCode::Right => {
            if form.is_text_field() {
                form.caret_right();
            } else {
                form.adjust(true);
            }
        }
        // Home/End/Delete edit the focused text field; no-ops on the toggles.
        KeyCode::Home => form.caret_home(),
        KeyCode::End => form.caret_end(),
        KeyCode::Delete => form.delete_forward(),
        KeyCode::Backspace => form.backspace(),
        // ONE source of truth for text-vs-toggle: `is_text_field` (the same predicate ←/→ use
        // above), so a field added or reordered can't make typing and caret motion disagree. A
        // text field takes the character; a toggle field cycles on space and ignores the rest.
        KeyCode::Char(c) => {
            if form.is_text_field() {
                form.type_char(c);
            } else if c == ' ' {
                form.adjust(true);
            }
        }
        _ => {}
    }
    // ONE recompute per key, after the key has been applied — not at each mutation site, because
    // typing, backspace, delete and a paste all reach the same buffer. `refresh_dir_suggestion`
    // returns immediately when the text did not change, so a caret move or a keystroke in another
    // field costs nothing and the renderer never has to read a directory.
    form.refresh_dir_completion();
    // The Engine toggle changes which models exist, so refresh the create form's cached
    // `model_choices` from the catalog whenever the engine just flipped — `models_for` needs
    // `&mut App` (it does the one-per-engine discovery), so it happens here, not on the form.
    // Only on an actual engine change, so ordinary keys stay allocation-free.
    let (engine_after, decider_engine_after) = match &app.mode {
        UiMode::Creating(form) => (form.engine, form.decider_engine),
        _ => return,
    };
    if engine_after != engine_before {
        let choices = app.models_for(engine_after).to_vec();
        if let UiMode::Creating(form) = &mut app.mode {
            form.model_choices = choices;
        }
    }
    // The Decider engine toggle (autopilot-only) likewise changes which decider models exist, so
    // refresh `decider_model_choices` from the catalog whenever the decider engine just flipped —
    // same reasoning as the worker engine above (`toggle_decider_engine` already cleared the picked
    // model), only on an actual change so ordinary keys stay allocation-free.
    if decider_engine_after != decider_engine_before {
        let choices = app.models_for(decider_engine_after).to_vec();
        if let UiMode::Creating(form) = &mut app.mode {
            form.decider_model_choices = choices;
        }
    }
}

/// A bracketed paste: one event carrying the whole block, delivered to whichever text
/// field is open — and to NOTHING otherwise.
///
/// TWO sanitizers, because the fields differ in kind:
///
/// - The one-line [`Field`]s (answer, rename, cadence, switcher, and the create form's rows) are
///   flattened with [`advise::sanitize_control_bytes`]: newlines and every other control byte become
///   spaces and whitespace runs collapse. That is the honest treatment for a one-row field — the
///   alternative is a string it cannot render and the caret cannot navigate.
/// - The three PROSE buffers (Message, goal, directive) KEEP THEIR LINES, which is most of the reason
///   they are buffers. They use [`tmux::sanitize_send_text`], the same cleaner the delivery path runs,
///   so what a paste puts on screen is exactly what the agent will receive.
///
/// The prose half is a fix, not a feature: these fields became `ratatui-textarea` buffers while this
/// function still flattened every paste, so the composer's own `insert_str` test passed (it inserts
/// directly) while a real bracketed paste still arrived as one long line.
///
/// In `Normal` mode a paste does NOTHING but say so. It must never be replayed as key
/// presses: `d`, `q` and `m` are all live there.
pub(crate) fn handle_paste(app: &mut App, raw: &str) {
    let prose = matches!(
        app.mode,
        UiMode::Sending { .. } | UiMode::EditingGoal { .. } | UiMode::EditingDirective { .. }
    );
    let text = if prose {
        tmux::sanitize_send_text(raw)
    } else {
        advise::sanitize_control_bytes(raw)
    };
    if text.is_empty() {
        return;
    }
    match &mut app.mode {
        UiMode::Answering { input, .. } => input.insert_str(&text),
        UiMode::EditingGoal { input, .. } => input.insert_str(&text),
        UiMode::EditingDirective { input, .. } => input.insert_str(&text),
        UiMode::Renaming { input, .. } => input.insert_str(&text),
        UiMode::Sending { input, .. } => input.insert_str(&text),
        UiMode::Switching { query, cursor, .. } => {
            query.insert_str(&text);
            *cursor = 0;
        }
        // One O(n) `insert_str` into the focused text field (toggles/model steppers ignore it), the
        // same path the four overlay fields above use — not a per-char loop, which would be O(n²).
        UiMode::Creating(form) => {
            form.paste(&text);
            // A pasted path is the most likely thing to complete, so the ghost is recomputed here
            // too — this is the one mutation that does not arrive through `handle_create_key`.
            form.refresh_dir_completion();
        }
        _ => {
            let n = text.chars().count();
            app.status = format!("pasted {n} chars — open a field first (/, R, g, i or s)");
        }
    }
}
