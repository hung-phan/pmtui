//! The per-session decision audit (`v`): a full-screen, read-only history of decider consults and
//! the legacy autopilot decisions that do not have a richer audit record.
//!
//! The two-line header stays fixed while the audit body tail-follows. Audit entries are expanded
//! rather than flattened into the legacy event feed: the question, deterministic policy, result,
//! reason, and duration remain visible together. A completed audit replaces the compatibility
//! `SupervisorResolved` event written at the same timestamp; all other decision events remain.

use crate::*;

/// Width of the legacy event kind column. `auto-answered` is the longest label.
const KIND_W: usize = 13;
/// Width of an expanded audit field label (`Duration` is the longest current label).
const FIELD_W: usize = 10;

enum DecisionRow<'a> {
    Audit(&'a job::DeciderRun),
    Legacy(&'a job::AutopilotEvent),
}

impl DecisionRow<'_> {
    /// Completion is when a finished audit becomes reviewable and is therefore its watermark time.
    fn at(&self) -> Epoch {
        match self {
            Self::Audit(run) => run.finished_at.unwrap_or(run.started_at),
            Self::Legacy(event) => event.at,
        }
    }
}

/// Full-screen decision audit for session `id`. `scroll` is lines UP from the tail, so zero keeps
/// the newest result at the bottom. Returns the body's maximum scroll for key-handling clamps.
pub(crate) fn render_decisions(
    f: &mut Frame,
    app: &App,
    scroll: usize,
    since: Option<Epoch>,
    id: &str,
    area: Rect,
) -> usize {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let project = app.projects.iter().find(|view| view.id == id);
    let runs = project
        .map(|view| view.decider_runs.as_slice())
        .unwrap_or(&[]);
    let events = project
        .map(|view| view.autopilot_events.as_slice())
        .unwrap_or(&[]);
    let advice = project.and_then(|view| view.advice_inflight.as_ref());
    let queue = project
        .map(|view| view.advice_queue.as_slice())
        .unwrap_or(&[]);

    // Advice sequence numbers may repeat after a daemon restart. Only the newest unfinished audit
    // with the same target is the live consult; an older Consulting record is interrupted.
    let active_run = advice
        .and_then(|parked| {
            runs.iter().rev().find(|run| {
                run.seq == parked.seq
                    && run.finished_at.is_none()
                    && matches!(&run.outcome, job::DeciderOutcome::Consulting)
                    && matches!(
                        (run.target, parked.pane_dialog),
                        (job::DeciderTarget::Marker, false) | (job::DeciderTarget::Dialog, true)
                    )
            })
        })
        .map(|run| (run.seq, run.started_at, run.target));

    let mut rows: Vec<DecisionRow<'_>> = runs.iter().map(DecisionRow::Audit).collect();
    rows.extend(
        events
            .iter()
            .filter(|event| event.kind.is_decision())
            .map(DecisionRow::Legacy),
    );
    rows.sort_by_key(DecisionRow::at);

    let block = Block::bordered()
        .border_style(attention::rule())
        .title(format!(
            " Audit · {id} · Turns  [Decisions] · {} ",
            rows.len()
        ))
        .title_style(attention::pane_title())
        .padding(Padding::horizontal(2));
    let inner = block.inner(body);
    f.render_widget(block, body);

    let header_h = inner.height.min(2);
    let [header, log] =
        Layout::vertical([Constraint::Length(header_h), Constraint::Min(0)]).areas(inner);
    if let Some(view) = project {
        f.render_widget(Paragraph::new(audit_header(view, header.width)), header);
    } else {
        f.render_widget(
            Paragraph::new(vec![
                Line::styled("decider unknown · idle", dim),
                Line::styled("no counters available", dim),
            ]),
            header,
        );
    }

    let width = log.width;
    let mut lines = Vec::new();
    if rows.is_empty() {
        lines.push(Line::raw(""));
        if queue.is_empty() {
            lines.push(Line::styled(
                format!(
                    "nothing yet - while {id} is on autopilot, decider consults and other \
                     decisions appear here."
                ),
                dim,
            ));
        }
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
                    width,
                    Style::default().fg(agent_manager::theme::accent()),
                ));
            }
            match row {
                DecisionRow::Audit(run) => {
                    let active = active_run.is_some_and(|(seq, started_at, target)| {
                        run.seq == seq && run.started_at == started_at && run.target == target
                    });
                    lines.extend(audit_lines(run, active, width));
                }
                DecisionRow::Legacy(event) => lines.extend(decision_lines(event, width)),
            }
        }
    }
    if !queue.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(section_line_styled(
            &format!("decision queue ({})", queue.len()),
            width,
            Style::default().fg(agent_manager::theme::accent()),
        ));
        let active_ids = advice
            .filter(|parked| !parked.pane_dialog)
            .map(|parked| parked.stop_ids.as_slice())
            .unwrap_or(&[]);
        let mut next_index = 0usize;
        for queued in queue {
            let active = active_ids.contains(&queued.stop_id);
            let label = if active {
                "Now".to_string()
            } else {
                next_index += 1;
                format!("Next {next_index}")
            };
            let question = if queued.draft.question.trim().is_empty() {
                stop_kind_label(queued.draft.kind).to_string()
            } else {
                queued.draft.question.clone()
            };
            lines.extend(labelled_lines(&label, &question, width));
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
        format!("  · \u{2191}{clamped_scroll}")
    };
    let hint = if footer.width >= 90 {
        format!(
            "Tab turns · Esc/q return · wheel or ↑/↓/j/k scroll · PgUp/PgDn page · Home top · End bottom{state}"
        )
    } else if footer.width >= 55 {
        format!("Tab turns · Esc/q return · wheel/j/k scroll · PgUp/PgDn · Home/End{state}")
    } else {
        format!("Tab · q return · wheel/j/k scroll{state}")
    };
    f.render_widget(Paragraph::new(Line::styled(hint, dim)), footer);

    max_scroll
}

fn audit_header(view: &ProjectView, width: u16) -> Vec<Line<'static>> {
    let engine = view.decider_engine.map(Engine::label).unwrap_or("unknown");
    let state = match view.advice_inflight.as_ref() {
        Some(parked) => {
            let current = usize::from(
                !parked.pane_dialog
                    && view
                        .advice_queue
                        .iter()
                        .any(|queued| parked.stop_ids.contains(&queued.stop_id)),
            );
            let next = view.advice_queue.len().saturating_sub(current);
            if next == 0 {
                format!("consulting #{}", parked.seq)
            } else {
                format!("consulting #{} · {next} next", parked.seq)
            }
        }
        None if !view.advice_queue.is_empty() => {
            format!("{} queued", view.advice_queue.len())
        }
        None => "idle".to_string(),
    };
    let configured_model = view.decider_model.as_deref().unwrap_or("(default)");
    let model = if engine.len() + configured_model.len() + state.len() + 4 <= usize::from(width) {
        configured_model
    } else {
        configured_model
            .rsplit('.')
            .next()
            .unwrap_or(configured_model)
    };
    let counters = view.decision_digest;

    vec![
        Line::from(vec![
            Span::styled(
                format!("{engine}/{model}"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" · "),
            Span::styled(state, Style::default().fg(agent_manager::theme::accent())),
        ]),
        Line::styled(
            if width >= 90 {
                format!(
                    "disposed {} · working {} · monitoring {} · auto {} · escalated {} · stalled {}",
                    counters.disposed,
                    counters.working,
                    counters.monitoring,
                    counters.auto_flow,
                    counters.escalated,
                    counters.stalled
                )
            } else {
                format!(
                    "all {} · work {} · watch {} · auto {} · ask {} · stuck {}",
                    counters.disposed,
                    counters.working,
                    counters.monitoring,
                    counters.auto_flow,
                    counters.escalated,
                    counters.stalled
                )
            },
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]
}

fn audit_lines(run: &job::DeciderRun, active: bool, width: u16) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let engine = run.engine.label();
    let model = run.model.as_deref().unwrap_or("(default)");
    let timestamp = format!("{}  ", clock_label(run.started_at));
    let title = if matches!(&run.outcome, job::DeciderOutcome::Skipped { .. }) {
        format!("preflight #{} · {}", run.seq, target_label(run.target))
    } else {
        format!("decider #{} · {}", run.seq, target_label(run.target))
    };
    let engine_model = format!("{engine}/{model}");
    let title_style = Style::default()
        .fg(outcome_tone(&run.outcome, active))
        .add_modifier(Modifier::BOLD);
    let mut lines =
        if timestamp.chars().count() + title.chars().count() + 3 + engine_model.chars().count()
            <= usize::from(width)
        {
            vec![Line::from(vec![
                Span::styled(timestamp, dim),
                Span::styled(format!("{title} · {engine_model}"), title_style),
            ])]
        } else {
            let mut rows = vec![Line::from(vec![
                Span::styled(timestamp, dim),
                Span::styled(title, title_style),
            ])];
            rows.extend(labelled_lines("Engine", &engine_model, width));
            rows
        };

    lines.extend(labelled_lines("Asked", &run.question, width));
    for (index, option) in run.options.iter().enumerate() {
        lines.extend(continuation_lines(
            &format!("{}) {}", index + 1, one_line(option)),
            width,
        ));
    }
    let legacy_reported = match &run.outcome {
        job::DeciderOutcome::Skipped {
            reported_kind,
            effect,
            ..
        } => (*reported_kind, *effect),
        _ => (None, None),
    };
    let reported_kind = run.reported_kind.or(legacy_reported.0);
    let effect = run.effect.or(legacy_reported.1);
    if reported_kind.is_some() || effect.is_some() {
        let reported = reported_kind.map(stop_kind_label).unwrap_or("unknown kind");
        let effect = effect
            .map(|value| value.summary())
            .unwrap_or_else(|| "effect unavailable".into());
        lines.extend(labelled_lines(
            "Reported",
            &format!("{reported} · {effect}"),
            width,
        ));
    }
    let disposition = match &run.outcome {
        job::DeciderOutcome::Resolved { .. } | job::DeciderOutcome::Recovered { .. } => "auto-flow",
        job::DeciderOutcome::Consulting if active => "awaiting verdict",
        _ => "escalate",
    };
    lines.extend(labelled_lines(
        "Policy",
        &format!(
            "{} · {} -> {} · {disposition}",
            stop_kind_label(run.policy.kind),
            risk_label(run.policy.labelled_risk),
            risk_label(run.policy.effective_risk)
        ),
        width,
    ));

    let (result, reason) = audit_result(&run.outcome, active);
    lines.extend(labelled_lines("Result", &result, width));
    lines.extend(labelled_lines("Reason", &reason, width));
    lines.extend(labelled_lines(
        "Duration",
        &duration_label(run, active),
        width,
    ));
    lines
}

fn audit_result(outcome: &job::DeciderOutcome, active: bool) -> (String, String) {
    match outcome {
        job::DeciderOutcome::Consulting if active => (
            "consulting".into(),
            "the decider consult is still in flight".into(),
        ),
        job::DeciderOutcome::Consulting => (
            "interrupted".into(),
            "no matching in-flight consult remains".into(),
        ),
        job::DeciderOutcome::Skipped { reason, .. } => ("not called".into(), one_line(reason)),
        job::DeciderOutcome::Resolved { answer, reason } => (one_line(answer), one_line(reason)),
        job::DeciderOutcome::Refused { reason } => ("refused".into(), one_line(reason)),
        job::DeciderOutcome::Failed { reason } => ("failed".into(), one_line(reason)),
        job::DeciderOutcome::Recovered { answer } => (
            format!("recovered: {}", one_line(answer)),
            "recovered after the daemon restarted".into(),
        ),
        job::DeciderOutcome::Interrupted { reason } => ("interrupted".into(), one_line(reason)),
    }
}

fn outcome_tone(outcome: &job::DeciderOutcome, active: bool) -> Color {
    match outcome {
        job::DeciderOutcome::Resolved { .. } | job::DeciderOutcome::Recovered { .. } => {
            agent_manager::theme::live()
        }
        job::DeciderOutcome::Consulting if active => agent_manager::theme::accent(),
        job::DeciderOutcome::Skipped { .. } | job::DeciderOutcome::Refused { .. } => {
            agent_manager::theme::soft()
        }
        job::DeciderOutcome::Consulting
        | job::DeciderOutcome::Failed { .. }
        | job::DeciderOutcome::Interrupted { .. } => agent_manager::theme::hard(),
    }
}

fn duration_label(run: &job::DeciderRun, active: bool) -> String {
    match run.finished_at {
        Some(finished) => compact_duration(finished.saturating_sub(run.started_at)),
        None if active => format!(
            "{} in progress",
            compact_duration(SystemClock.now().saturating_sub(run.started_at))
        ),
        None => "unfinished".into(),
    }
}

fn compact_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

fn target_label(target: job::DeciderTarget) -> &'static str {
    match target {
        job::DeciderTarget::Marker => "marker",
        job::DeciderTarget::Dialog => "dialog",
    }
}

fn stop_kind_label(kind: agent_manager::pmstate::StopKind) -> &'static str {
    use agent_manager::pmstate::StopKind;
    match kind {
        StopKind::Publish => "publish",
        StopKind::Merge => "merge",
        StopKind::ConfirmDone => "confirm_done",
        StopKind::Ambiguity => "ambiguity",
        StopKind::Stuck => "stuck",
        StopKind::ExpertNeeded => "expert_needed",
        StopKind::WorkerStuck => "worker_stuck",
        StopKind::Capability => "capability",
    }
}

fn risk_label(risk: RiskClass) -> &'static str {
    match risk {
        RiskClass::Low => "low",
        RiskClass::Medium => "medium",
        RiskClass::Hard => "hard",
    }
}

fn labelled_lines(label: &str, value: &str, width: u16) -> Vec<Line<'static>> {
    let total = usize::from(width);
    if total == 0 {
        return Vec::new();
    }
    let clean = one_line(value);
    let prefix = format!("  {label:<FIELD_W$}");
    let prefix_w = prefix.chars().count();
    let label_style = Style::default()
        .fg(agent_manager::theme::accent())
        .add_modifier(Modifier::BOLD);

    if total <= prefix_w {
        let mut lines = vec![Line::styled(prefix, label_style)];
        lines.extend(wrapped_with_indent(&clean, total, 0));
        return lines;
    }

    let wrapped = wrap_all(&clean, total - prefix_w);
    let mut lines = vec![Line::from(vec![
        Span::styled(prefix, label_style),
        Span::raw(wrapped.first().cloned().unwrap_or_default()),
    ])];
    let indent = prefix_w.min(total.saturating_sub(1));
    for row in wrapped.iter().skip(1) {
        lines.push(Line::raw(format!("{}{}", " ".repeat(indent), row)));
    }
    lines
}

fn continuation_lines(value: &str, width: u16) -> Vec<Line<'static>> {
    let total = usize::from(width);
    if total == 0 {
        return Vec::new();
    }
    let indent = (2 + FIELD_W).min(total.saturating_sub(1));
    wrapped_with_indent(
        &one_line(value),
        total.saturating_sub(indent).max(1),
        indent,
    )
}

fn wrapped_with_indent(value: &str, available: usize, indent: usize) -> Vec<Line<'static>> {
    wrap_all(value, available.max(1))
        .into_iter()
        .map(|row| Line::raw(format!("{}{}", " ".repeat(indent), row)))
        .collect()
}

fn one_line(value: &str) -> String {
    value.replace(['\n', '\r'], " ").trim().to_string()
}

/// One legacy decision as `HH:MM:SS  kind   detail`, with hanging wrapped detail.
fn decision_lines(ev: &job::AutopilotEvent, width: u16) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let total = usize::from(width);
    let kind = format!("{:<KIND_W$}", ev.kind.label());
    let kind_style = Style::default()
        .fg(autopilot_tone(&ev.kind))
        .add_modifier(Modifier::BOLD);
    let ts = clock_label(ev.at);
    let count = if ev.count > 1 {
        format!("\u{d7}{} ", ev.count)
    } else {
        String::new()
    };
    let prefix_w = 10 + KIND_W + 1 + count.chars().count();
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(format!("{ts}  "), dim),
        Span::styled(format!("{kind} "), kind_style),
    ];
    if !count.is_empty() {
        spans.push(Span::styled(count, dim));
    }
    match ev.kind.detail() {
        Some(detail) if !detail.trim().is_empty() => {
            let one = one_line(&detail);
            let available = total.saturating_sub(prefix_w).max(1);
            let indent = prefix_w.min(total.saturating_sub(1));
            let wrapped = wrap_all(&one, available);
            spans.push(Span::raw(wrapped.first().cloned().unwrap_or_default()));
            let mut out = vec![Line::from(spans)];
            for row in wrapped.iter().skip(1) {
                out.push(Line::raw(format!("{}{}", " ".repeat(indent), row)));
            }
            out
        }
        _ => vec![Line::from(spans)],
    }
}
