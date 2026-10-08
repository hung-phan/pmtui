//! `/` quick switcher: one search field and a bounded list of existing sessions.

use crate::*;

fn tail_truncate(text: &str, width: usize) -> String {
    if Line::raw(text).width() <= width {
        return text.to_string();
    }
    let ellipsis = "…";
    let mut used = Line::raw(ellipsis).width();
    if used > width {
        return String::new();
    }
    let mut suffix = Vec::new();
    for ch in text.chars().rev() {
        let columns = Line::raw(ch.to_string()).width();
        if used.saturating_add(columns) > width {
            break;
        }
        suffix.push(ch);
        used = used.saturating_add(columns);
    }
    suffix.reverse();
    format!("{ellipsis}{}", suffix.into_iter().collect::<String>())
}

pub(crate) fn render_switcher(
    f: &mut Frame,
    app: &App,
    area: Rect,
    query: &Field,
    cursor: usize,
    items: &[SwitchItem],
) {
    let matches = switch_matches(items, query.as_str());
    let visible_hint = matches.len().clamp(1, 12);
    let inner = draw_overlay_frame(
        f,
        area,
        84,
        overlay_h(visible_hint + 2),
        "Switch session",
        agent_manager::theme::accent(),
        "type to filter \u{b7} enter select \u{b7} esc cancel",
    );
    let [search, count, results] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(input_line(
            query,
            usize::from(search.width).saturating_sub(3),
        )),
        search,
    );
    f.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "{} match{}",
                matches.len(),
                if matches.len() == 1 { "" } else { "es" }
            ),
            Style::default().fg(agent_manager::theme::rule()),
        )),
        count,
    );
    if matches.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                "No sessions match",
                Style::default().fg(agent_manager::theme::soft()),
            )),
            results,
        );
        return;
    }

    let visible = usize::from(results.height).max(1);
    let cursor = cursor.min(matches.len() - 1);
    let start = cursor
        .saturating_sub(visible.saturating_sub(1))
        .min(matches.len().saturating_sub(visible));
    let width = usize::from(results.width);
    for (row, result) in (start..matches.len()).take(visible).enumerate() {
        let Some(item) = matches.get(result).and_then(|index| items.get(*index)) else {
            continue;
        };
        let row_area = Rect::new(
            results.x,
            results.y.saturating_add(row as u16),
            results.width,
            1,
        );
        app.switch_hits.borrow_mut().push((row_area, result));
        let marker = if result == cursor { "\u{25b8} " } else { "  " };
        let identity = if item.label == item.id {
            item.id.clone()
        } else {
            format!("{} \u{b7} {}", item.label, item.id)
        };
        let prefix = format!("{marker}{identity} \u{b7} ");
        let root_budget = width.saturating_sub(Line::raw(&prefix).width());
        let text = if item.root.is_empty() {
            format!("{marker}{identity}")
        } else {
            format!("{prefix}{}", tail_truncate(&item.root, root_budget))
        };
        let style = if result == cursor {
            Style::default()
                .fg(agent_manager::theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        f.render_widget(Paragraph::new(Line::styled(text, style)), row_area);
    }
}
