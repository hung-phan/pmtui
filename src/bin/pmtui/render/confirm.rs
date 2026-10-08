//! The confirm overlay, for the two actions that END a running agent (`d` remove, `r`
//! restart). Both arms are prose a human reads before pressing `y`, and the column counts
//! in the notes below are why it is prose that must not be edited casually.

use crate::*;

/// Remove-confirmation overlay (only shown for a still-live session).
pub(crate) fn render_confirm(f: &mut Frame, area: Rect, id: &str, what: Confirmable) {
    // Red, and the only overlay whose accent is not its own idea: `attention`'s Hard
    // hue, because these are the modals that end a running agent.
    let (title, hint) = match what {
        Confirmable::Remove => ("Confirm remove", "y remove \u{b7} any other key cancels"),
        Confirmable::Restart => ("Confirm restart", "y restart \u{b7} any other key cancels"),
        Confirmable::Apply => ("Confirm apply", "y apply \u{b7} any other key cancels"),
    };
    let inner = draw_overlay_frame(
        f,
        area,
        64,
        // Four, not three: this overlay still `.wrap()`s its prose, and the "still
        // live" line runs to 54 columns against the 60 it gets — one wrap away from
        // pushing the last line out.
        overlay_h(4),
        title,
        agent_manager::theme::hard(),
        hint,
    );
    let body = Text::from(match what {
        Confirmable::Remove => vec![
            Line::styled(
                format!("Delete {id}?"),
                Style::default()
                    .fg(agent_manager::theme::hard())
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw("The agent is still live \u{2014} this kills its tmux session."),
            Line::styled(
                "(.project-state kept; source files untouched)",
                Style::default().add_modifier(Modifier::DIM),
            ),
        ],
        // Says what it COSTS (the turn in flight) and what it KEEPS (the
        // conversation), because "restart" alone tells you neither.
        Confirmable::Restart => vec![
            Line::styled(
                format!("Restart {id}'s agent?"),
                Style::default()
                    .fg(agent_manager::theme::hard())
                    .add_modifier(Modifier::BOLD),
            ),
            // Names the CHAT explicitly: it is a second claude the human may be in the middle
            // of, and it is not obvious that "restart the agent" reaches it. 58 columns against
            // the 60 the frame gives this body.
            Line::raw("Stops pmd, ends the agent + chat panes, restarts pmd."),
            // 57 columns against the 60 the frame gives it. The first draft ran to 61
            // and wrapped onto a second row mid-phrase ("a reply in flight is / lost"),
            // which a real render caught and no unit test would have.
            Line::styled(
                "(resumes the same conversation; a reply in flight is lost)",
                Style::default().add_modifier(Modifier::DIM),
            ),
        ],
        // The one confirm here that WRITES to the human's own files rather than ending an agent, so it
        // says which tree moves and that a conflict costs them nothing.
        Confirmable::Apply => vec![
            Line::styled(
                format!("Apply {id}'s commit to this checkout?"),
                Style::default()
                    .fg(agent_manager::theme::hard())
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw("Cherry-picks its commit onto the project's current branch."),
            Line::styled(
                "(a conflict is aborted; your checkout is left as it is)",
                Style::default().add_modifier(Modifier::DIM),
            ),
        ],
    });
    f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), inner);
}

/// "Create directory?" — the create-flow confirm shown when the form names a folder that does not
/// exist yet. CYAN like the create overlay it interrupts (not the red of the destructive
/// [`render_confirm`]), because this MAKES a folder rather than ending an agent, and here `y`/enter
/// confirm. The path shows its TAIL when too long, since the new folder's name — the part worth
/// checking before you make it — sits at the end.
pub(crate) fn render_confirm_create_dir(f: &mut Frame, area: Rect, dir: &str) {
    let inner = draw_overlay_frame(
        f,
        area,
        72,
        overlay_h(4),
        "Create directory?",
        agent_manager::theme::accent(),
        "y / enter create \u{b7} any other key goes back",
    );
    let text_w = usize::from(inner.width);
    let shown = if dir.chars().count() > text_w {
        let tail: String = dir
            .chars()
            .skip(dir.chars().count() - text_w.saturating_sub(1))
            .collect();
        format!("\u{2026}{tail}")
    } else {
        dir.to_string()
    };
    let body = Text::from(vec![
        Line::raw("This folder does not exist yet:"),
        Line::styled(
            shown,
            Style::default()
                .fg(agent_manager::theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "Create it and set up the session here?",
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), inner);
}
