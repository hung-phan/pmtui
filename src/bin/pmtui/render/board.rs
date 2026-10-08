//! Task-oriented view over managed sessions. The full work area belongs to
//! lifecycle lanes; the Session projection remains one `1` (or Tab) away.

use crate::*;

const BOARD_ALL_W: u16 = 150;
const BOARD_THREE_W: u16 = 105;
const BOARD_TWO_W: u16 = 72;
const CARD_ROWS: u16 = 9;

/// First visible item of a `visible`-wide window over `total` items. Like a list scroll offset it
/// keeps `previous` until `selected` falls outside the window, then moves just far enough to show
/// it — so an item already on screen never slides out from under the pointer when it is selected.
fn sticky_window_start(
    previous: usize,
    selected: Option<usize>,
    visible: usize,
    total: usize,
) -> usize {
    let visible = visible.max(1);
    let start = previous.min(total.saturating_sub(visible));
    match selected {
        Some(selected) if selected < start => selected,
        Some(selected) if selected >= start + visible => selected + 1 - visible,
        _ => start,
    }
}

/// Draw the Task view.
pub(crate) fn render_board(f: &mut Frame, app: &App, area: Rect) {
    if area.width < MIN_W || area.height < MIN_H {
        render_too_small(f, area);
        return;
    }
    let [status, board_body, keybar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    render_status_bar(f, app, status);
    app.panes.set(PaneRects {
        sessions: Rect::ZERO,
        ..PaneRects::default()
    });

    let selected_column = app
        .selected_view()
        .map(board_column)
        .unwrap_or(BoardColumn::NeedsYou);
    let visible = if board_body.width >= BOARD_ALL_W {
        5
    } else if board_body.width >= BOARD_THREE_W {
        3
    } else if board_body.width >= BOARD_TWO_W {
        2
    } else {
        1
    };
    let start = sticky_window_start(
        app.board_column_start.get(),
        Some(selected_column.index()),
        visible,
        BoardColumn::ALL.len(),
    );
    app.board_column_start.set(start);
    let columns = &BoardColumn::ALL[start..(start + visible).min(BoardColumn::ALL.len())];
    if app.board_detail_open {
        render_board_detail(f, app, board_body);
    } else {
        let mut constraints = Vec::with_capacity(columns.len().saturating_mul(2).saturating_sub(1));
        for index in 0..columns.len() {
            if index > 0 {
                constraints.push(Constraint::Length(1));
            }
            constraints.push(Constraint::Ratio(1, columns.len() as u32));
        }
        let areas = Layout::horizontal(constraints).split(board_body);
        for (column, area) in columns.iter().zip(areas.iter().step_by(2)) {
            render_board_column(f, app, *column, *area);
        }
    }

    // The keybar carries the transient status — the only surface that does, now that the log is a
    // file rather than a pane competing for these rows.
    render_keybar(f, app, keybar);
}

pub(crate) fn render_board_column(f: &mut Frame, app: &App, column: BoardColumn, area: Rect) {
    app.board_column_hits.borrow_mut().push((area, column));
    if area.width == 0 || area.height == 0 {
        return;
    }
    let members: Vec<usize> = app
        .projects
        .iter()
        .enumerate()
        .filter_map(|(index, view)| (board_column(view) == column).then_some(index))
        .collect();
    let color = board_column_color(column);
    let block = Block::bordered()
        .border_style(Style::default().fg(color))
        .title(Line::from(vec![
            Span::styled(
                format!(" {} ", column.label()),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", members.len()),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ]));
    let cards = block.inner(area);
    f.render_widget(block, area);
    if cards.width == 0 || cards.height == 0 {
        return;
    }
    if members.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(" no tasks", attention::text_dim())),
            cards,
        );
        return;
    }

    let capacity = usize::from(cards.height / CARD_ROWS).max(1);
    let selected_position = members.iter().position(|index| *index == app.selected);
    let mut offsets = app.board_lane_offsets.get();
    let first = sticky_window_start(
        offsets[column.index()],
        selected_position,
        capacity,
        members.len(),
    );
    offsets[column.index()] = first;
    app.board_lane_offsets.set(offsets);
    for (slot, index) in members.iter().skip(first).take(capacity).enumerate() {
        let y = cards.y + u16::try_from(slot).unwrap_or(u16::MAX) * CARD_ROWS;
        let card = Rect::new(cards.x, y, cards.width, CARD_ROWS.min(cards.bottom() - y));
        let view = &app.projects[*index];
        render_board_card(f, view, *index == app.selected, card);
        app.board_hits.borrow_mut().push((card, view.id.clone()));
    }
}

fn render_board_card(f: &mut Frame, view: &ProjectView, selected: bool, area: Rect) {
    let column = board_column(view);
    let (glyph, color) = match column {
        BoardColumn::NeedsYou => {
            let (glyph, fallback) = status_glyph(view);
            (glyph, attention::level(view).hue().unwrap_or(fallback))
        }
        BoardColumn::Working => ("●", board_column_color(column)),
        BoardColumn::Autopilot | BoardColumn::Pending | BoardColumn::Paused => {
            ("○", board_column_color(column))
        }
    };
    let mode = if view.tier == Some(Tier::Autopilot) {
        "auto"
    } else {
        "standard"
    };
    let border_style = Style::default().fg(color).add_modifier(if selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    });
    let card_id = truncate(view.label(), usize::from(area.width).saturating_sub(10));
    let block = Block::bordered()
        .border_style(border_style)
        .padding(Padding::horizontal(1))
        .style(if selected {
            attention::selection()
        } else {
            Style::default()
        })
        .title(Line::from(vec![
            Span::styled(
                if selected { " ▸ " } else { "   " },
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{glyph} "), Style::default().fg(color)),
            Span::styled(
                card_id.clone(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = usize::from(inner.width).max(1);
    let engine = view.engine.map(Engine::label).unwrap_or("agent");
    let engine_color = match view.engine {
        Some(Engine::Claude) => agent_manager::theme::brand(),
        Some(Engine::Codex) => agent_manager::theme::live(),
        None => attention::text(),
    };
    let mode_color = if view.tier == Some(Tier::Autopilot) {
        agent_manager::theme::accent()
    } else {
        agent_manager::theme::accent_alt()
    };
    let project = view
        .forked_from
        .as_deref()
        .map(|parent| format!("fork:{parent}"))
        .unwrap_or_else(|| {
            view.project_name
                .as_deref()
                .unwrap_or("project")
                .to_string()
        });
    let compact_metadata = format!("{engine} · {mode}");
    let show_project = text_cols(&project) + 3 + text_cols(&compact_metadata) <= width;
    let mut metadata = Vec::new();
    if show_project {
        metadata.push(Span::styled(project, attention::text_dim()));
        metadata.push(Span::styled(" · ", attention::text_dim()));
    }
    metadata.push(Span::styled(
        engine.to_string(),
        Style::default().fg(engine_color),
    ));
    metadata.push(Span::styled(" · ", attention::text_dim()));
    metadata.push(Span::styled(mode, Style::default().fg(mode_color)));
    let (cue, cue_detail, filled_cue) = board_card_cue(view, SystemClock.now());
    let next = board_card_next(view);
    let context = board_card_context(view, width);
    let [
        title_area,
        metadata_area,
        state_area,
        next_area,
        context_area,
    ] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(Line::styled(
            view.work_summary.as_deref().unwrap_or_else(|| view.label()),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .wrap(Wrap { trim: true }),
        title_area,
    );
    f.render_widget(Paragraph::new(Line::from(metadata)), metadata_area);
    f.render_widget(
        Paragraph::new(Line::from({
            let cue_style = Style::default().fg(color).add_modifier(if filled_cue {
                Modifier::REVERSED | Modifier::BOLD
            } else {
                Modifier::BOLD
            });
            let mut spans = vec![Span::styled(
                if filled_cue {
                    format!(" {cue} ")
                } else {
                    cue.to_string()
                },
                cue_style,
            )];
            if let Some(detail) = cue_detail {
                spans.push(Span::styled(
                    format!("  {detail}"),
                    Style::default().fg(color),
                ));
            }
            spans
        })),
        state_area,
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "NEXT ",
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(next, attention::text_dim()),
        ]))
        .wrap(Wrap { trim: true }),
        next_area,
    );
    f.render_widget(Paragraph::new(context), context_area);
}

/// The card's availability and lineage line. Terminal availability is card-level truth for every
/// enabled session — an Autopilot card with no terminal is not "between turns" — so the offline cue
/// comes first and in the Session list's offline red; lineage follows, dim, in what remains. Fork
/// lineage wins over spawn lineage when a row carries both (a fork of a spawned child copies its
/// source's `spawned_by`); spawn lineage names the parent by its display label.
fn board_card_context(view: &ProjectView, width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut room = width;
    if view.enabled && !view.session_live {
        let offline = truncate("terminal offline", room);
        room = room.saturating_sub(text_cols(&offline));
        spans.push(Span::styled(
            offline,
            Style::default().fg(agent_manager::theme::hard()),
        ));
    }
    let lineage = match (
        view.forked_from.as_deref(),
        view.spawned_by_label.as_deref(),
    ) {
        (Some(source), _) => Some(format!(
            "fork of {} · shared dir",
            truncate(source, width.saturating_sub(20))
        )),
        (None, Some(parent)) => Some(format!("from {parent}")),
        (None, None) => None,
    };
    if let Some(lineage) = lineage {
        let lead = if spans.is_empty() { "" } else { " · " };
        spans.push(Span::styled(
            truncate(&format!("{lead}{lineage}"), room),
            attention::text_dim(),
        ));
    }
    Line::from(spans)
}

fn board_column_color(column: BoardColumn) -> Color {
    match column {
        BoardColumn::NeedsYou => agent_manager::theme::soft(),
        BoardColumn::Working => agent_manager::theme::live(),
        BoardColumn::Autopilot => agent_manager::theme::accent(),
        BoardColumn::Pending => agent_manager::theme::accent_alt(),
        BoardColumn::Paused => agent_manager::theme::rule(),
    }
}

fn render_board_detail(f: &mut Frame, app: &App, board: Rect) {
    if board.width == 0 || board.height == 0 {
        return;
    }
    let detail = board;
    app.panes.set(PaneRects {
        sessions: Rect::ZERO,
        detail,
    });
    f.render_widget(Clear, detail);
    // `Clear` leaves the terminal's default pair; the detail pane is the dashboard's canvas, not a
    // floating card, so it gets the canvas pair back before anything draws on it.
    f.render_widget(Block::default().style(attention::canvas()), detail);
    render_detail_with_composer(f, app, detail);
}

pub(crate) fn board_card_cue(
    view: &ProjectView,
    now: Epoch,
) -> (&'static str, Option<String>, bool) {
    // No terminal means no turn cycle and nothing ready at a prompt. The Session list's own
    // predicate, so the two views name the same rows offline; a card needing you keeps its cue.
    if row_offline(view) {
        return ("OFFLINE", None, false);
    }
    match board_column(view) {
        BoardColumn::NeedsYou => {
            let label = if view.stops.iter().any(|stop| stop.kind == "confirm_done") {
                "READY TO CLOSE"
            } else {
                "ACTION REQUIRED"
            };
            let waiting = view
                .oldest_stop_since
                .filter(|_| !view.stops.is_empty())
                .map(|since| format!("waiting {}", age_label(Some(since), now)));
            (label, waiting, true)
        }
        BoardColumn::Pending => ("READY", None, true),
        BoardColumn::Working => ("IN PROGRESS", None, false),
        BoardColumn::Autopilot => ("BETWEEN TURNS", None, false),
        // A staged spawn sits with the paused cards, but its broker is launching it: nothing
        // paused it and Resume would be refused.
        BoardColumn::Paused if view.spawn_staged => ("STARTING…", None, false),
        BoardColumn::Paused => ("PAUSED", None, false),
    }
}

fn board_card_next(view: &ProjectView) -> String {
    if board_column(view) == BoardColumn::NeedsYou
        && let Some(question) = view
            .stops
            .iter()
            .map(|stop| stop.question.trim())
            .find(|question| !question.is_empty())
    {
        return question.to_string();
    }
    view.next_action.clone()
}
