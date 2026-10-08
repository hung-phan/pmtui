//! Compact, read-only projection of the worker's optional continuity checkpoint.

use super::*;

const CHECKPOINT_MIN_HEIGHT: u16 = 24;
const CHECKPOINT_ACTIVITY_HEIGHT: u16 = 30;

pub(crate) fn preview_checkpoint_lines(
    checkpoint: Option<&state::WorkerCheckpoint>,
    inner_w: u16,
    inner_h: u16,
) -> Vec<Line<'static>> {
    let Some(checkpoint) = checkpoint.filter(|_| inner_h >= CHECKPOINT_MIN_HEIGHT && inner_w > 0)
    else {
        return Vec::new();
    };
    let active_activities = checkpoint
        .activities
        .iter()
        .filter(|activity| {
            matches!(
                activity.status,
                state::CheckpointActivityStatus::Running | state::CheckpointActivityStatus::Waiting
            )
        })
        .count();
    let mut lines = vec![
        section_line("checkpoint", inner_w),
        Line::raw(truncate(
            &format!(
                "checkpoint #{} · done {} · now {} · activities {} · blockers {}",
                checkpoint.seq,
                checkpoint.done.len(),
                checkpoint.in_progress.len(),
                active_activities,
                checkpoint.blockers.len()
            ),
            usize::from(inner_w),
        )),
    ];

    if let Some((label, text)) = checkpoint
        .blockers
        .first()
        .map(|text| ("note", text))
        .or_else(|| checkpoint.in_progress.first().map(|text| ("now", text)))
        .or_else(|| checkpoint.next.first().map(|text| ("next", text)))
    {
        lines.push(checkpoint_detail_line(label, text, inner_w));
    }

    if inner_h >= CHECKPOINT_ACTIVITY_HEIGHT
        && let Some(activity) = checkpoint.activities.first()
    {
        // Handles and output references may contain host paths or provider identifiers. They remain
        // available to the worker for reconciliation, but the default human preview shows only the
        // bounded activity name and status.
        let detail = format!("{} · {}", activity.id, activity.status.label());
        lines.push(checkpoint_detail_line("activity", &detail, inner_w));
    }
    lines
}

fn checkpoint_detail_line(label: &str, text: &str, width: u16) -> Line<'static> {
    let label = format!("{label}: ");
    let width = usize::from(width);
    let label_style = Style::default()
        .fg(agent_manager::theme::accent())
        .add_modifier(Modifier::BOLD);
    if label.chars().count() >= width {
        return Line::styled(truncate(label.trim_end(), width), label_style);
    }
    let value = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let value_width = width - label.chars().count();
    Line::from(vec![
        Span::styled(label, label_style),
        Span::raw(truncate(&value, value_width)),
    ])
}
