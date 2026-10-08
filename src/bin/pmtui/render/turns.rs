//! Deterministic per-session heartbeat trace for the `v` audit view.

use super::*;
use agent_manager::job::{
    AutopilotEvent, AutopilotEventKind, TurnNoReportReason, TurnOutcome, TurnTrace, TurnTrigger,
};

enum TurnRow<'a> {
    Turn(&'a TurnTrace),
    Event(&'a AutopilotEvent),
}

impl TurnRow<'_> {
    fn at(&self) -> Epoch {
        match self {
            Self::Turn(turn) => turn.started_at,
            Self::Event(event) => event.at,
        }
    }
}

pub(crate) fn render_turns(
    f: &mut Frame,
    app: &App,
    scroll: usize,
    since: Option<Epoch>,
    id: &str,
    area: Rect,
) -> usize {
    let dim = Style::default().add_modifier(Modifier::DIM);
    if area.width < 4 || area.height < 3 {
        f.render_widget(Paragraph::new("Audit · Turns"), area);
        return 0;
    }

    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let project = app.projects.iter().find(|view| view.id == id);
    let traces = project
        .map(|view| view.turn_trace.as_slice())
        .unwrap_or(&[]);
    let events = project
        .map(|view| view.autopilot_events.as_slice())
        .unwrap_or(&[]);
    let traced_from = traces.iter().map(|turn| turn.started_at).min();

    let mut rows = traces.iter().map(TurnRow::Turn).collect::<Vec<_>>();
    rows.extend(
        events
            .iter()
            .filter(|event| {
                !matches!(
                    event.kind,
                    AutopilotEventKind::Nudged | AutopilotEventKind::Reported(_)
                ) || traced_from.is_none_or(|started_at| event.at < started_at)
            })
            .map(TurnRow::Event),
    );
    rows.sort_by_key(TurnRow::at);

    let block = Block::bordered()
        .border_style(attention::rule())
        .title(format!(
            " Audit · {id} · [Turns]  Decisions · {} ",
            traces.len()
        ))
        .title_style(attention::pane_title())
        .padding(Padding::horizontal(2));
    let inner = block.inner(body);
    f.render_widget(block, body);

    let header_h = inner.height.min(2);
    let [header, log] =
        Layout::vertical([Constraint::Length(header_h), Constraint::Min(0)]).areas(inner);
    if let Some(view) = project {
        let current = format!(
            "{} · {}",
            view.posture.label(),
            if view.next_action.trim().is_empty() {
                "no next action recorded"
            } else {
                view.next_action.as_str()
            }
        );
        let pending = traces
            .iter()
            .filter(|turn| matches!(turn.outcome, TurnOutcome::AwaitingReport))
            .count();
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(clipped(&current, header.width)),
                Line::styled(
                    clipped(
                        &format!(
                            "{} turn{} · {pending} awaiting report",
                            traces.len(),
                            if traces.len() == 1 { "" } else { "s" }
                        ),
                        header.width,
                    ),
                    dim,
                ),
            ]),
            header,
        );
    }

    let mut lines = Vec::new();
    if rows.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("nothing yet - heartbeat turns for {id} appear here after pmd sends a nudge."),
            dim,
        ));
    } else {
        let boundary = since.unwrap_or(0);
        let new_n = if since.is_some() {
            rows.iter().filter(|row| row.at() > boundary).count()
        } else {
            0
        };
        let first_new = rows.len().saturating_sub(new_n);
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                lines.push(Line::raw(""));
            }
            if new_n > 0 && index == first_new {
                lines.push(section_line_styled(
                    &format!("new since {} ({new_n})", clock_label(boundary)),
                    log.width,
                    Style::default().fg(agent_manager::theme::accent()),
                ));
            }
            match row {
                TurnRow::Turn(turn) => lines.extend(turn_lines(turn, log.width)),
                TurnRow::Event(event) => lines.extend(event_lines(event, log.width)),
            }
        }
    }

    let visible = usize::from(log.height);
    let max_scroll = lines.len().saturating_sub(visible);
    let clamped_scroll = scroll.min(max_scroll);
    let end = lines.len().saturating_sub(clamped_scroll);
    let start = end.saturating_sub(visible);
    f.render_widget(Paragraph::new(Text::from(lines[start..end].to_vec())), log);

    let state = if clamped_scroll == 0 {
        String::new()
    } else {
        format!(" · ↑{clamped_scroll}")
    };
    f.render_widget(
        Paragraph::new(Line::styled(
            clipped(
                &format!(
                    "Tab decisions · Esc/q return · wheel/j/k scroll · PgUp/PgDn · Home/End{state}"
                ),
                footer.width,
            ),
            dim,
        )),
        footer,
    );
    max_scroll
}

fn turn_lines(turn: &TurnTrace, width: u16) -> Vec<Line<'static>> {
    let trigger = match turn.trigger {
        TurnTrigger::Heartbeat {
            pending_context,
            marker_recovery,
        } => match (pending_context, marker_recovery) {
            (true, true) => "heartbeat · pending answer · marker recovery",
            (true, false) => "heartbeat · pending answer",
            (false, true) => "heartbeat · marker recovery",
            (false, false) => "heartbeat",
        },
    };
    let mut lines = vec![Line::raw(clipped(
        &format!(
            "{}  turn #{} · {}",
            clock_label(turn.started_at),
            turn.id,
            trigger
        ),
        width,
    ))];
    match &turn.outcome {
        TurnOutcome::AwaitingReport => lines.push(Line::styled(
            clipped(
                &format!(
                    "  awaiting report newer than marker #{}",
                    turn.marker_baseline
                ),
                width,
            ),
            Style::default().fg(agent_manager::theme::soft()),
        )),
        TurnOutcome::Reported {
            at,
            marker_seq,
            state,
            disposition,
            status,
            next_step,
            ..
        } => {
            lines.push(Line::raw(clipped(
                &format!(
                    "  report #{marker_seq} · {} → {} · {}",
                    wake_label(*state),
                    disposition_label(*disposition),
                    duration(*at - turn.started_at)
                ),
                width,
            )));
            if status.is_some() || next_step.is_some() {
                let detail_style = Style::default().add_modifier(Modifier::DIM);
                if let Some(status) = status {
                    lines.extend(wrapped_detail_lines("status", status, width, detail_style));
                }
                if let Some(next) = next_step {
                    lines.extend(wrapped_detail_lines("next", next, width, detail_style));
                }
            }
        }
        TurnOutcome::NoReport { at, reason } => lines.push(Line::styled(
            clipped(
                &format!(
                    "  no report · {} · {}",
                    no_report_label(*reason),
                    duration(*at - turn.started_at)
                ),
                width,
            ),
            Style::default().fg(agent_manager::theme::soft()),
        )),
    }
    lines
}

fn event_lines(event: &AutopilotEvent, width: u16) -> Vec<Line<'static>> {
    let count = if event.count > 1 {
        format!(" ×{}", event.count)
    } else {
        String::new()
    };
    let prefix = format!("{}  {}{count}", clock_label(event.at), event.kind.label());
    let style = Style::default().add_modifier(Modifier::DIM);
    match event
        .kind
        .detail()
        .filter(|detail| !detail.trim().is_empty())
    {
        Some(detail) => wrapped_prefixed_lines(&format!("{prefix} · "), &detail, width, style),
        None => vec![Line::styled(clipped(&prefix, width), style)],
    }
}

fn wrapped_detail_lines(label: &str, value: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    wrapped_prefixed_lines(&format!("  {label}: "), value, width, style)
}

fn wrapped_prefixed_lines(
    prefix: &str,
    value: &str,
    width: u16,
    style: Style,
) -> Vec<Line<'static>> {
    let total = usize::from(width);
    if total == 0 {
        return Vec::new();
    }
    let clean = value.replace(['\n', '\r'], " ").trim().to_string();
    let prefix_w = prefix.chars().count();
    if prefix_w >= total {
        let mut lines = vec![Line::styled(clipped(prefix, width), style)];
        lines.extend(
            wrap_all(&clean, total)
                .into_iter()
                .map(|row| Line::styled(row, style)),
        );
        return lines;
    }

    let wrapped = wrap_all(&clean, total - prefix_w);
    let mut lines = vec![Line::styled(
        format!("{prefix}{}", wrapped.first().cloned().unwrap_or_default()),
        style,
    )];
    for row in wrapped.iter().skip(1) {
        lines.push(Line::styled(
            format!("{}{row}", " ".repeat(prefix_w)),
            style,
        ));
    }
    lines
}

pub(crate) fn wake_label(state: job::WakeState) -> &'static str {
    match state {
        job::WakeState::Working => "working",
        job::WakeState::Monitoring => "monitoring",
        job::WakeState::Blocked => "blocked",
    }
}

pub(crate) fn disposition_label(disposition: job::TurnDisposition) -> &'static str {
    match disposition {
        job::TurnDisposition::Working => "working",
        job::TurnDisposition::Monitoring => "monitoring",
        job::TurnDisposition::Reviewing => "reviewing",
        job::TurnDisposition::AutoFlow => "auto-flow",
        job::TurnDisposition::Escalated => "needs you",
        job::TurnDisposition::Interrupted => "interrupted",
        job::TurnDisposition::Stalled => "stalled",
    }
}

pub(crate) fn no_report_label(reason: TurnNoReportReason) -> &'static str {
    match reason {
        TurnNoReportReason::Superseded => "superseded",
        TurnNoReportReason::Relaunched => "worker relaunched",
        TurnNoReportReason::TerminalUnavailable => "terminal unavailable",
    }
}

pub(crate) fn duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

pub(crate) fn clipped(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".into();
    }
    let mut clipped = text.chars().take(width - 1).collect::<String>();
    clipped.push('…');
    clipped
}
