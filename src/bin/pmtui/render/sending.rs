//! The `s` send field: one line of text on its way to a live agent's pane. Its own file
//! rather than a third arm of `editing`, because what it edits is not a session SETTING —
//! nothing is written to disk, the text becomes the agent's next turn.

use crate::*;

/// Text rows inside the composer. SIX, the height it has always had: this was written as the status
/// pane's content height, and that pane is gone (the status log is a file now), so the number it was
/// borrowing lives here rather than vanishing with it.
pub(crate) const COMPOSER_CONTENT_ROWS: u16 = 6;

/// Rows the composer occupies: [`COMPOSER_CONTENT_ROWS`] plus its two borders.
pub(crate) const COMPOSER_ROWS: u16 = COMPOSER_CONTENT_ROWS + 2;

/// Dormant composer shelf. It is visible whenever the selected workspace is on
/// screen, but it performs no input itself; pointer activation routes through `s`.
pub(crate) fn render_send_shelf(
    f: &mut Frame,
    area: Rect,
    id: &str,
    draft: Option<&Composer>,
    route: MessageRoute,
) {
    let title = format!(" Message \u{b7} {} ", truncate(id, 28));
    let block = Block::bordered()
        .border_type(BorderType::Plain)
        .border_style(attention::rule())
        .title(Line::styled(
            title,
            Style::default()
                .fg(agent_manager::theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    // The same route the `s` key takes, so the shelf never offers what the key refuses.
    let text = match (route, draft) {
        (MessageRoute::Answer, _) => {
            "\u{203a} press s or click to answer this decision".to_string()
        }
        (MessageRoute::IncompleteFork, _) => {
            "\u{203a} no conversation to message \u{b7} delete this fork with d".to_string()
        }
        (MessageRoute::SpawnStaged, _) => {
            "\u{203a} a spawn request is creating it \u{b7} message it once it starts".to_string()
        }
        // One line of preview for a buffer that may now hold several: the shelf says a draft is
        // waiting, and opening it is how you read it.
        (MessageRoute::Message, Some(draft)) if !draft.is_empty() => {
            format!("\u{203a} draft \u{b7} {}", draft.text().replace('\n', " "))
        }
        (MessageRoute::Message, Some(_) | None) => {
            "\u{203a} press s or click to message this session".to_string()
        }
    };
    f.render_widget(
        Paragraph::new(Line::styled(text, attention::text_dim())).wrap(Wrap { trim: false }),
        inner,
    );
}

/// Send field (`s`) — an inline dashboard band that leaves the session list and transcript
/// visible while the human composes the next turn. `label` is the target's display label,
/// the same one its row and the dormant shelf show, so the band names where Enter delivers.
pub(crate) fn render_send_field(f: &mut Frame, area: Rect, label: &str, input: &Composer) {
    let block = Block::bordered()
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(agent_manager::theme::rule()))
        .title(Line::styled(
            format!(" Message \u{b7} {} ", truncate(label, 28)),
            Style::default()
                .fg(agent_manager::theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // THE LIBRARY DRAWS THE TEXT. Wrapping, the caret across a wrapped row, and the scroll that keeps
    // it on screen were ~60 lines of arithmetic here; `ratatui-textarea` owns them now, which is the
    // whole reason the composer moved to it.
    f.render_widget(input.widget(), inner);
}
