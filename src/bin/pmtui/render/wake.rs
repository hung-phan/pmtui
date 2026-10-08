//! The full-screen wake-follow view: re-read the live wake's step log each tick and
//! render its transcript, with the wake state's own word and colour. Read-only, and
//! deliberately not gated by the dashboard's minimum size — it is line-based, so it
//! stays usable on any terminal.

use crate::*;

/// The dashboard word for a wake report's self-declared state.
pub(crate) fn wake_state_str(s: job::WakeState) -> &'static str {
    match s {
        job::WakeState::Working => "working",
        job::WakeState::Monitoring => "monitoring",
        job::WakeState::Blocked => "blocked",
    }
}

/// Full-screen READABLE follow view of the selected agent-loop row's live wake.
/// Re-reads `paths.step_log(latest_seq)` each render tick and renders it via
/// [`agent_manager::stream_json::render_transcript`], auto-following the tail
/// (`scroll == 0`) or showing a window scrolled UP by `scroll` lines. READ-ONLY and
/// panic-safe: a missing/unreadable log degrades to a placeholder, and every
/// height/index computation is `saturating_*` so a 1-row terminal can't panic.
/// Replaces the old raw tmux `watch()` attach.
pub(crate) fn render_wake_view(
    f: &mut Frame,
    id: &str,
    paths: &ProjectPaths,
    scroll: usize,
    area: Rect,
) -> usize {
    let dim = Style::default().add_modifier(Modifier::DIM);
    // Log body fills the screen; a single-row footer carries the key hints.
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

    // Visible inner height inside the block borders (top + bottom = 2 rows).
    let inner_height = body.height.saturating_sub(2);

    let seq = latest_step_seq(&paths.steps_dir());
    let title = match seq {
        Some(seq) => format!(" Wake: {id}  #{seq} "),
        None => format!(" Wake: {id} "),
    };
    let block = Block::bordered()
        .border_style(attention::rule())
        .title(title)
        .title_style(attention::pane_title());

    // Resolve the readable window of log lines, panic-safely. Also carry out the
    // max scroll (lines beyond the viewport) so `render` can clamp key handling.
    let (lines, max_scroll): (Vec<Line>, usize) = match seq {
        // No wakes on disk yet -> a dim placeholder; nothing to scroll.
        None => (vec![Line::styled("  no wakes yet", dim)], 0),
        Some(seq) => {
            let raw = std::fs::read_to_string(paths.step_log(seq)).unwrap_or_default();
            let rendered = agent_manager::stream_json::render_transcript(&raw);
            if rendered.is_empty() {
                (vec![Line::styled("  —", dim)], 0)
            } else {
                // Auto-following window: `scroll` = lines scrolled UP from the tail.
                let visible = inner_height as usize;
                let max_scroll = rendered.len().saturating_sub(visible);
                // Clamp for display so over-scroll sticks at the top.
                let scroll = scroll.min(max_scroll);
                let end = rendered.len().saturating_sub(scroll);
                let start = end.saturating_sub(visible);
                let window = rendered[start..end]
                    .iter()
                    .map(|l| Line::raw(l.clone()))
                    .collect();
                // Return the max BEFORE the display clamp.
                (window, max_scroll)
            }
        }
    };
    // Deliberately NO .wrap(): the scroll window is line-based (one logical line == one
    // screen row), so wrapping a wide line would over/under-fill the visible window and
    // desync the scroll math. Wide lines are clipped at the right edge instead.
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), body);

    // Footer: dim key hints + the current follow/scroll state.
    let state = if scroll == 0 {
        "  · following".to_string()
    } else {
        format!("  · scrolled ↑{scroll}")
    };
    let hint = format!("Esc/q return · ↑/↓ or j/k scroll · PgUp/PgDn page · End follow{state}");
    f.render_widget(Paragraph::new(Line::styled(hint, dim)), footer);

    max_scroll
}
