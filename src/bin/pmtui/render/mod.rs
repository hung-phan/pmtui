//! Drawing the screen. `render` is the one entry point: it short-circuits to the
//! full-screen wake view, refuses a terminal too small to be legible, then lays out
//! the status bar, the body and the keybar and draws whichever overlay the [`UiMode`]
//! asks for. The panes and the shared chrome live in the submodules beside it.
//!
//! ONE SEAM PER FILE: `status`, `rows`, `keybar` and `preview` are the dashboard's own
//! regions; `chrome` is the visual vocabulary they all share (overlay frames, section
//! rules, key chips); `stops` lays one stop out for both surfaces that show one; `wake` is
//! the full-screen follow view; and `answer`, `create`, `editing`, `sending`, `confirm` and
//! `help` are one file per [`UiMode`] overlay — so `render` stays a dispatcher, and each
//! overlay's geometry (the row budgets and column counts its comments justify) sits beside
//! the only code that spends it.

mod answer;
mod board;
mod chrome;
mod confirm;
mod create;
mod decisions;
mod editing;
mod help;
mod keybar;
mod model_picker;
mod preview;
mod rows;
mod sending;
mod settings;
mod status;
mod stops;
mod switcher;
mod turns;
mod wake;

pub(crate) use answer::*;
pub(crate) use board::*;
pub(crate) use chrome::*;
pub(crate) use confirm::*;
pub(crate) use create::*;
pub(crate) use decisions::*;
pub(crate) use editing::*;
pub(crate) use help::*;
pub(crate) use keybar::*;
pub(crate) use model_picker::*;
pub(crate) use preview::*;
pub(crate) use rows::*;
pub(crate) use sending::*;
pub(crate) use settings::*;
pub(crate) use status::*;
pub(crate) use stops::*;
pub(crate) use switcher::*;
pub(crate) use turns::*;
pub(crate) use wake::*;

use crate::*;

/// Responsive layout thresholds. Very narrow terminals show the list alone;
/// medium terminals stack list above detail; wide terminals use two columns.
pub(crate) const NARROW_W: u16 = 50;
pub(crate) const WIDE_W: u16 = 96;

/// The smallest terminal the dashboard will draw into. Below this even a bordered
/// list is illegible, so we render one honest centred line rather than a scrambled
/// frame. (The full-screen wake-follow view is line-based and stays usable at any
/// size, so it is deliberately NOT gated by this.)
pub(crate) const MIN_W: u16 = 24;
pub(crate) const MIN_H: u16 = 8;

pub(crate) fn render(f: &mut Frame, app: &App) {
    app.row_hits.borrow_mut().clear();
    app.create_hits.borrow_mut().clear();
    app.key_hits.borrow_mut().clear();
    app.top_hits.borrow_mut().clear();
    app.switch_hits.borrow_mut().clear();
    app.settings_hits.borrow_mut().clear();
    app.board_hits.borrow_mut().clear();
    app.board_column_hits.borrow_mut().clear();
    app.preview_attach_hit.set(Rect::ZERO);
    app.composer_hit.set(Rect::ZERO);
    app.panes.set(PaneRects::default());
    // THE CANVAS IS THE THEME'S. Every cell gets `bg.base` before anything else draws, so a theme owns
    // the background and not just the ink — without it a light theme's foregrounds land on whatever
    // the terminal's background happens to be, which is unreadable rather than merely off.
    //
    // The cost, stated because it is a real one: an opaque background defeats terminal transparency
    // and blur. That is the trade a theme engine makes — the alternative is themes that only half
    // apply.
    // Both sides of the pair, and this is what themes every PLAIN span in the tree: ratatui patches a
    // cell's style rather than replacing it, so text drawn with no foreground of its own keeps the one
    // painted here. Without the `fg` those spans fell through to the terminal's default ink — white on
    // near-white under a light theme. A span that names its own hue still wins over it.
    f.render_widget(Block::default().style(attention::canvas()), f.area());
    // Full-screen readable wake-follow view: when active, draw ONLY the wake view
    // over the whole area and return (skip the dashboard + overlays entirely).
    if let UiMode::WakeView { id, paths, scroll } = &app.mode {
        let max = render_wake_view(f, id, paths, *scroll, f.area());
        app.scroll_max.set(max); // `&App` + Cell::set is fine
        return;
    }
    // The DECISION LANE is a full-screen view like the wake follow: draw it over the whole area and
    // return, so the dashboard + overlays are skipped. `scroll_max` clamps its key handling, exactly
    // as the wake view does. (`&App` + Cell::set is fine.)
    if let UiMode::Decisions {
        tab,
        scroll,
        since,
        id,
        ..
    } = &app.mode
    {
        let max = match tab {
            AuditTab::Turns => render_turns(f, app, *scroll, *since, id, f.area()),
            AuditTab::Decisions => render_decisions(f, app, *scroll, *since, id, f.area()),
        };
        app.scroll_max.set(max);
        return;
    }
    // A Board-origin overlay draws over the Task view and then replaces its keybar with the
    // overlay's own chips.
    if app.return_to_board_after_create {
        let area = f.area();
        let drew_board = match &app.mode {
            UiMode::Creating(form) => {
                render_board(f, app, area);
                render_create_for_app(f, area, app, form);
                true
            }
            UiMode::ConfirmCreateDir { dir, .. } => {
                render_board(f, app, area);
                render_confirm_create_dir(f, area, dir);
                true
            }
            _ => false,
        };
        if drew_board {
            render_board_overlay_keybar(f, app, area);
            return;
        }
    }
    if app.return_to_board_after_send && matches!(app.mode, UiMode::Sending { .. }) {
        render_board(f, app, f.area());
        return;
    }
    if app.return_to_board_after_answer
        && let UiMode::Answering {
            input,
            choice,
            scroll,
        } = &app.mode
    {
        let area = f.area();
        render_board(f, app, area);
        render_answer(f, app, area, input, *choice, *scroll);
        render_board_overlay_keybar(f, app, area);
        return;
    }
    if app.return_to_board_after_switch
        && let UiMode::Switching {
            query,
            cursor,
            items,
        } = &app.mode
    {
        let area = f.area();
        render_board(f, app, area);
        render_switcher(f, app, area, query, *cursor, items);
        render_board_overlay_keybar(f, app, area);
        return;
    }
    if app.return_to_board_after_action {
        let area = f.area();
        let drew_board = match &app.mode {
            UiMode::EditingGoal {
                id,
                input,
                then_autopilot,
                ..
            } => {
                render_board(f, app, area);
                render_goal_field(f, area, id, input, *then_autopilot);
                true
            }
            UiMode::EditingCadence {
                id,
                current,
                input,
                then_autopilot,
                ..
            } => {
                render_board(f, app, area);
                render_cadence_field(f, area, id, *current, input, *then_autopilot);
                true
            }
            UiMode::Confirming { id, what, .. } => {
                render_board(f, app, area);
                render_confirm(f, area, id, *what);
                true
            }
            UiMode::Renaming { id, current, input } => {
                render_board(f, app, area);
                render_rename_field(f, area, id, current.as_deref(), input);
                true
            }
            _ => false,
        };
        if drew_board {
            render_board_overlay_keybar(f, app, area);
            return;
        }
    }
    if matches!(app.mode, UiMode::Board) {
        render_board(f, app, f.area());
        return;
    }
    if let UiMode::Settings { cursor, open } = &app.mode {
        render_settings(f, app, f.area(), *cursor, *open);
        return;
    }
    let area = f.area();
    // Honest too-small state: one centred line instead of a scrambled frame.
    if area.width < MIN_W || area.height < MIN_H {
        render_too_small(f, area);
        return;
    }
    // agent-deck layout: a 1-line status bar, the body (SESSIONS list, plus the
    // PREVIEW when the terminal is wide enough for one), and a 1-line keybar.
    // `Min(0)` on the body means the two fixed 1-row bars can never starve it.
    // A one-line VERDICT above the body, naming WHICH sessions need you — shown ONLY when
    // something does. Silence is the default state: an all-clear fleet spends no row on it, so the
    // dashboard is unchanged until an escalation exists. The status bar already carries the COUNT
    // (` ✕ 2 need you `); this line carries the identities the count cannot, right where the eye
    // lands first. Below `NARROW_W` it still shows — knowing which session needs you matters at
    // every width — and it clips at the right edge like the status bar rather than wrapping.
    let (needs, _running, _idle, stuck) = counts(app);
    let show_verdict = needs + stuck > 0;
    let areas = dashboard_areas(area, show_verdict);

    render_status_bar(f, app, areas.status);
    if let Some(v) = areas.verdict {
        render_verdict_line(f, app, v);
    }
    // The body is the list and the preview, all of it. A ` STATUS ` pane used to take the bottom rows
    // of it to show the status log; the log is a file now (`status_log`), so those rows go back to the
    // sessions it was covering and the keybar is the one surface carrying the transient line.
    render_body(f, app, areas.body);
    // ---- overlays: one arm per mode, one file per arm ----------------------
    // A `match` rather than a chain of `if let`s, because exactly one of these can be
    // true and the shape should say so. Each arm hands off to the module beside this
    // one; nothing about an overlay's geometry is decided here.
    match &app.mode {
        UiMode::Answering {
            input,
            choice,
            scroll,
        } => render_answer(f, app, area, input, *choice, *scroll),
        UiMode::Creating(form) => render_create_for_app(f, area, app, form),
        UiMode::ConfirmCreateDir { dir, .. } => render_confirm_create_dir(f, area, dir),
        UiMode::EditingCadence {
            id,
            current,
            input,
            then_autopilot,
            ..
        } => render_cadence_field(f, area, id, *current, input, *then_autopilot),
        UiMode::EditingGoal {
            id,
            input,
            then_autopilot,
            ..
        } => render_goal_field(f, area, id, input, *then_autopilot),
        UiMode::EditingDirective { id, input, .. } => render_directive_field(f, area, id, input),
        UiMode::Renaming { id, current, input } => {
            render_rename_field(f, area, id, current.as_deref(), input)
        }
        UiMode::Switching {
            query,
            cursor,
            items,
        } => render_switcher(f, app, area, query, *cursor, items),
        // `?` key reference. Drawn like the other overlays, and the counterpart to the
        // ADAPTIVE keybar: whatever the bar drops for want of columns is listed here.
        // (`&App` + Cell::set is fine.)
        UiMode::Help { scroll } => {
            app.scroll_max
                .set(render_help(f, *scroll, area, &app.status_log))
        }
        UiMode::Confirming { id, what, .. } => render_confirm(f, area, id, *what),
        UiMode::ModelPicker {
            target,
            id,
            stage,
            engine,
            cursor,
            stored_model,
        } => render_model_picker(
            f,
            area,
            app,
            id,
            *target,
            *stage,
            *engine,
            *cursor,
            stored_model.as_deref(),
        ),
        // `WakeView` returned at the top of this function (it owns the whole screen), and
        // `Normal` is the dashboard with nothing over it.
        UiMode::Normal
        | UiMode::Board
        // `Settings` returned above with the other whole-screen views.
        | UiMode::Settings { .. }
        | UiMode::Sending { .. }
        | UiMode::WakeView { .. }
        | UiMode::Decisions { .. } => {}
    }
    // The keybar is the final visual layer because its click regions are already
    // published. On a minimum-size terminal a modal can overlap this row; drawing the
    // bar last keeps every active target visible instead of leaving hidden controls.
    render_keybar(f, app, areas.keybar);
}

/// The keybar row of a Board-origin overlay: the overlay's chips plus the transient status, which
/// this bar always carries — there is no longer a pane that could hold it instead.
fn render_board_overlay_keybar(f: &mut Frame, app: &App, area: Rect) {
    let keybar = Rect::new(
        area.x,
        area.bottom().saturating_sub(1),
        area.width,
        area.height.min(1),
    );
    render_keybar(f, app, keybar);
}

struct DashboardAreas {
    status: Rect,
    verdict: Option<Rect>,
    body: Rect,
    keybar: Rect,
}

fn dashboard_areas(area: Rect, verdict: bool) -> DashboardAreas {
    let mut constraints = vec![Constraint::Length(1)];
    if verdict {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    constraints.push(Constraint::Length(1));
    let chunks = Layout::vertical(constraints).split(area);
    let mut index = 0;
    let status = chunks[index];
    index += 1;
    let verdict = verdict.then(|| {
        let result = chunks[index];
        index += 1;
        result
    });
    let body = chunks[index];
    index += 1;
    DashboardAreas {
        status,
        verdict,
        body,
        keybar: chunks[index],
    }
}

/// The dashboard body: narrow list-only, medium stacked, wide two-column.
pub(crate) fn render_body(f: &mut Frame, app: &App, area: Rect) {
    if area.width < NARROW_W {
        if matches!(app.mode, UiMode::Sending { .. }) {
            app.panes.set(PaneRects {
                detail: area,
                ..PaneRects::default()
            });
            render_detail_with_composer(f, app, area);
            return;
        }
        app.panes.set(PaneRects {
            sessions: area,
            ..PaneRects::default()
        });
        render_sessions(f, app, area);
        return;
    }
    if area.width < WIDE_W {
        let list_height = if matches!(app.mode, UiMode::Sending { .. }) {
            (area.height / 3).max(6).min(area.height)
        } else {
            stacked_sessions_height(&app.projects, area.height)
        };
        let [list, detail] =
            Layout::vertical([Constraint::Length(list_height), Constraint::Min(0)]).areas(area);
        app.panes.set(PaneRects {
            sessions: list,
            detail,
        });
        render_sessions(f, app, list);
        render_detail_with_composer(f, app, detail);
        return;
    }
    // Wide: SESSIONS on the left (~35%, clamped), PREVIEW fills the rest.
    let [left, preview] = Layout::horizontal([
        Constraint::Length(sessions_width(area.width)),
        Constraint::Min(0),
    ])
    .areas(area);
    // The left column is the list, all of it. The status log used to take its bottom rows as a
    // ` STATUS ` pane; it is a file now, so the list keeps the whole column.
    app.panes.set(PaneRects {
        sessions: left,
        detail: preview,
    });
    render_sessions(f, app, left);
    render_detail_with_composer(f, app, preview);
}

pub(crate) fn render_detail_with_composer(f: &mut Frame, app: &App, area: Rect) {
    const MIN_PREVIEW_ROWS: u16 = 16;
    const MIN_ACTIVE_PREVIEW_ROWS: u16 = 6;
    const MIN_COMPOSER_ROWS: u16 = 3;
    let active = matches!(app.mode, UiMode::Sending { .. });
    let show_composer = active
        || (app.selected_view().is_some()
            && area.height >= COMPOSER_ROWS.saturating_add(MIN_PREVIEW_ROWS));
    let [preview_body, composer] = if show_composer {
        let available = area.height.saturating_sub(MIN_ACTIVE_PREVIEW_ROWS);
        let composer_rows = if active && available >= MIN_COMPOSER_ROWS {
            available.min(COMPOSER_ROWS)
        } else {
            COMPOSER_ROWS.min(area.height)
        };
        Layout::vertical([Constraint::Min(0), Constraint::Length(composer_rows)]).areas(area)
    } else {
        [area, Rect::ZERO]
    };
    render_preview(f, app, preview_body);
    if composer.width > 0 {
        match &app.mode {
            UiMode::Sending { target, input } => {
                // The target, not the selection: the band names the session Enter delivers to.
                let label = app
                    .projects
                    .iter()
                    .find(|view| view.id == target.id)
                    .map_or(target.id.as_str(), ProjectView::label);
                render_send_field(f, composer, label, input);
            }
            _ => {
                if let Some(view) = app.selected_view() {
                    render_send_shelf(
                        f,
                        composer,
                        view.label(),
                        app.message_drafts.get(&view.id),
                        message_route(view),
                    );
                    app.composer_hit.set(composer);
                }
            }
        }
    }
}

/// One centred dim-yellow line stating the real size and the minimum — the honest
/// alternative to a frame whose panes have all collapsed to borders. Three rows are
/// reserved (with wrapping) so the message still reads on a very narrow terminal;
/// `Flex::Center` + `Wrap` keep a 1x1 area panic-free.
pub(crate) fn render_too_small(f: &mut Frame, area: Rect) {
    let msg = format!(
        "Terminal too small ({}x{}) · min {MIN_W}x{MIN_H}",
        area.width, area.height
    );
    let [row] = Layout::vertical([Constraint::Length(3)])
        .flex(Flex::Center)
        .areas(area);
    f.render_widget(
        Paragraph::new(
            Line::styled(
                msg,
                Style::default()
                    .fg(agent_manager::theme::soft())
                    .add_modifier(Modifier::DIM),
            )
            .centered(),
        )
        .wrap(Wrap { trim: true }),
        row,
    );
}
