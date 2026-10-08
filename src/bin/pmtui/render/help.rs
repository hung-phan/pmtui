//! The `?` overlay: every row of [`BINDINGS`], unconditionally. It exists because
//! the keybar is ADAPTIVE and hides chips, so this is the safety net that keeps them
//! discoverable.

use crate::*;

/// Width of the help overlay's key column, in columns — wide enough for the longest
/// key in [`BINDINGS`] (`PgUp/PgDn`) so the description column stays aligned.
pub(crate) const HELP_KEY_W: usize = 10;

/// Preferred help-overlay width, clamped to the frame by [`render_help`]. Sized so the
/// description column fits the binding help text on a normal terminal: at 96, the inner
/// width is 94 and the fixed key column eats 12, leaving ~82 for descriptions (the
/// longest are trimmed to fit). It was 64, which truncated every description with `…`
/// even on a 140-column terminal — the help that explains the keys cut off the
/// explanation. `render_help` still clamps this to the frame, so a narrow terminal
/// degrades to `…` gracefully rather than overflowing.
pub(crate) const HELP_W: u16 = 96;

/// The help overlay's body: every [`BINDINGS`] row that carries a description,
/// grouped under its [`KeyGroup`] heading, as `key | description` pairs with a fixed
/// key column. `width` is the overlay's INNER width; descriptions are truncated to it
/// so no row can wrap and desync the scroll maths.
///
/// This is the single source of truth for "what keys exist": the adaptive keybar reads
/// the same table, so it can hide a chip without hiding the knowledge.
pub(crate) fn help_lines(width: usize) -> Vec<Line<'static>> {
    let head = Style::default()
        .fg(agent_manager::theme::accent())
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(agent_manager::theme::soft())
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);
    // `truncate` renders a lone "…" for a zero budget, which would still be one column
    // wider than the row allows; a zero budget means "no room at all".
    let clamp = |s: &str, n: usize| -> String {
        if n == 0 {
            String::new()
        } else {
            truncate(s, n)
        }
    };
    let mut out: Vec<Line> = Vec::new();
    for group in KEY_GROUPS {
        let rows: Vec<&Binding> = BINDINGS
            .iter()
            .filter(|b| b.group == group && !b.help.is_empty())
            .collect();
        if rows.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Line::raw(""));
        }
        out.push(Line::styled(clamp(group.heading(), width), head));
        for b in rows {
            // Both cells are clamped to `width`, so even an absurdly narrow overlay
            // yields rows that fit (the key column simply eats all of them).
            let key = clamp(&format!(" {:<HELP_KEY_W$} ", b.key), width);
            let desc = clamp(b.help, width.saturating_sub(text_cols(&key)));
            out.push(Line::from(vec![
                Span::styled(key, key_style),
                Span::styled(desc, dim),
            ]));
        }
    }
    out
}

/// The `?` key reference: an overlay listing EVERY binding, because the keybar is
/// adaptive and hides some. Same look as the Answer/Create overlays (bordered,
/// titled, cyan accent, basic ANSI colours only) and sized RELATIVE to the terminal,
/// clamped so it never exceeds the frame. Returns the max scroll offset (rows beyond
/// the viewport) so `handle_key` can clamp; every index is saturating/clamped, so a
/// 1-row frame degrades to an empty box rather than panicking.
pub(crate) fn render_help(f: &mut Frame, scroll: usize, area: Rect, log: &Path) -> usize {
    let w = HELP_W.min(area.width.saturating_sub(2)).max(1);
    let inner = usize::from(w).saturating_sub(2);
    // WHERE THE HISTORY IS, and FIRST — the body scrolls, so a path appended after fifty rows of
    // bindings is a path nobody sees. The status line is transient and the pane that used to hold the
    // log is gone, so "what did it say before?" is newly this overlay's question to answer.
    let mut rows = vec![
        Line::styled(
            truncate("STATUS LOG", inner.max(1)),
            Style::default()
                .fg(agent_manager::theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            truncate(&format!(" {}", log.display()), inner.max(1)),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Line::raw(""),
    ];
    rows.extend(help_lines(inner));
    // Content rows + the two borders.
    let want = u16::try_from(rows.len().saturating_add(2)).unwrap_or(u16::MAX);
    let h = want.min(area.height.saturating_sub(2)).max(1);
    let popup = centered_rect(w, h, area);
    f.render_widget(Clear, popup);

    let visible = usize::from(h).saturating_sub(2);
    let max_scroll = rows.len().saturating_sub(visible);
    let start = scroll.min(max_scroll);
    let end = start.saturating_add(visible).min(rows.len());
    let body: Vec<Line> = rows[start..end].to_vec();

    let hint = if max_scroll > 0 {
        format!(
            " j/k scroll ({}/{}) · any key closes ",
            start + 1,
            rows.len()
        )
    } else {
        " any key closes ".to_string()
    };
    let block = Block::bordered()
        // The card's own pair, like every other overlay: `Clear` above reset these cells to the
        // terminal's default, and the help overlay is the most-read text in the dashboard.
        .style(attention::surface())
        .border_style(Style::default().fg(agent_manager::theme::accent()))
        .title(" Keys ")
        .title_style(Style::default().add_modifier(Modifier::BOLD))
        .title_bottom(Line::styled(
            hint,
            Style::default().add_modifier(Modifier::DIM),
        ));
    // Deliberately NO .wrap(): one logical row == one screen row keeps the scroll
    // window exact (the same reason `render_wake_view` doesn't wrap).
    f.render_widget(Paragraph::new(Text::from(body)).block(block), popup);
    max_scroll
}
