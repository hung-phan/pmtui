//! The Settings view: the dashboard's own preferences, as a place rather than a dialog.
//!
//! Reached by `0`, the third tab after `1 Sessions` and `2 Tasks` (user: *"make the tab 0 after
//! others tab and name it settings. we then can use it to config theme"*). It wears the same top bar,
//! body and keybar as every other view, because a view that framed itself differently would read as an
//! overlay.
//!
//! # A table of settings, not one setting's list
//!
//! One row per setting — name, current value, what it governs — and the values live in a DROPDOWN that
//! `Enter` drops from the row (user: *"we may have many settings in the future, so can you make a drop
//! down instead"*). The page therefore costs one row per setting however many there are, and the next
//! setting needs nothing here: it is a [`crate::settings::SettingKind`] variant.
//!
//! The dropdown is anchored under its own row's value rather than centred like the modal overlays, so
//! it reads as belonging to that setting; it flips ABOVE the row when there is no room below, which is
//! what keeps the last setting usable on a short terminal.

use crate::*;

/// Columns the name column takes, so every value lines up under the one above it.
const NAME_W: usize = 14;

/// Columns the value column takes, including the `▾` marker.
const VALUE_W: usize = 26;

/// Widest a dropdown gets. Beyond this a long name is truncated rather than allowed to cover the whole
/// view — the list is a choice, not a document.
const DROP_W: u16 = 40;

/// THE MOVING CURSOR, drawn rather than merely highlighted.
///
/// A selection BACKGROUND cannot be trusted to show: `bg.selection` and `bg.elevated` are the same
/// colour in Catppuccin Mocha (and it is one palette entry away in plenty of others), so inside an
/// elevated dropdown the highlighted row looked exactly like every other one — the user pressed `j`,
/// the cursor moved, and nothing on screen said so: *"the high light on the row item makes it looks
/// like it doesn't work"*. A caret plus the accent cannot collide with a background, so the cursor is
/// visible on every theme. The background highlight stays for the themes where it does read.
const CURSOR: &str = "▸ ";

/// Draw the Settings view. `cursor` is the setting under the cursor; `open` is its dropdown's cursor
/// while the dropdown is down.
pub(crate) fn render_settings(
    f: &mut Frame,
    app: &App,
    area: Rect,
    cursor: usize,
    open: Option<usize>,
) {
    if area.width < MIN_W || area.height < MIN_H {
        render_too_small(f, area);
        return;
    }
    let [status, body, keybar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    render_status_bar(f, app, status);
    // No pane rects: nothing here scrolls under the pointer, and publishing a stale one would let a
    // wheel over Settings scroll a session list that is not on screen.
    app.panes.set(PaneRects::default());
    let rows = render_rows(f, app, body, cursor, open.is_some());
    if let Some(pick) = open {
        // INSIDE the frame, never over it: the block's bottom title is the path this view writes to,
        // and a dropdown that covered it would hide the answer to "where does this go?".
        let bounds = Rect {
            x: body.x.saturating_add(1),
            y: body.y.saturating_add(1),
            width: body.width.saturating_sub(2),
            height: body.height.saturating_sub(2),
        };
        render_dropdown(f, app, bounds, cursor, pick, &rows);
    }
    render_keybar(f, app, keybar);
}

/// The settings table. Returns each setting's `(row, value column x)` so the dropdown can anchor itself
/// under the value it changes.
fn render_rows(f: &mut Frame, app: &App, area: Rect, cursor: usize, open: bool) -> Vec<(u16, u16)> {
    let block = Block::bordered()
        .border_style(attention::rule())
        .title(" SETTINGS ")
        .title_style(attention::pane_title())
        .title_bottom(Line::styled(
            format!(" {} ", app.settings_path.display()),
            attention::text_dim(),
        ))
        .padding(Padding::horizontal(2));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        app.settings_hits.borrow_mut().clear();
        return Vec::new();
    }

    let [head, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                // Indented by the cursor column the rows spend, so the headings sit over their columns.
                Span::raw("  "),
                Span::styled(pad_cols("Setting", NAME_W), attention::pane_title()),
                Span::styled(pad_cols("Value", VALUE_W), attention::pane_title()),
                Span::styled("Enter opens the list", attention::text_dim()),
            ]),
            Line::raw(""),
        ]),
        head,
    );

    let settings = crate::settings::SettingKind::ALL;
    let items: Vec<ListItem> = settings
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            let row = app.setting_row(*kind);
            // The cursor is the CARET, not the background — see `CURSOR` on why a background alone
            // cannot be trusted to show. Dropped while the dropdown is down: the moving cursor is in
            // the list then, and two carets would disagree about which one `j` drives.
            let here = index == cursor && !open;
            ListItem::new(Line::from(vec![
                Span::styled(
                    if here { CURSOR } else { "  " },
                    Style::default()
                        .fg(agent_manager::theme::accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    pad_cols(kind.name(), NAME_W),
                    Style::default().fg(attention::text()),
                ),
                // The value in the dashboard's own accent: it is the answer this row exists to give.
                Span::styled(
                    pad_cols(&truncate(&row.value, VALUE_W - 3), VALUE_W - 2),
                    Style::default()
                        .fg(agent_manager::theme::accent())
                        .add_modifier(Modifier::BOLD),
                ),
                // `▾` says "there is a list under here", which is the whole affordance a keyboard
                // dropdown has.
                Span::styled("▾ ", Style::default().fg(agent_manager::theme::muted())),
                Span::styled(kind.summary(), attention::text_dim()),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(cursor.min(settings.len().saturating_sub(1))));
    f.render_stateful_widget(
        // While the dropdown is down the row keeps its place but not the cursor's emphasis: the moving
        // cursor is in the list, and two highlighted things would disagree about which one moves.
        List::new(items).highlight_style(if open {
            Style::default()
        } else {
            attention::selection()
        }),
        list,
        &mut state,
    );

    let anchor_x = list.x.saturating_add(u16::try_from(NAME_W).unwrap_or(0));
    let geometry: Vec<(u16, u16)> = (0..settings.len())
        .filter_map(|index| {
            let row = u16::try_from(index).ok()?;
            (row < list.height).then_some((list.y.saturating_add(row), anchor_x))
        })
        .collect();
    // CLICK-TO-OPEN for the rows — only while no dropdown is down, because the hit list is shared and
    // an open dropdown's options own it (see `event_dispatch`).
    if !open {
        *app.settings_hits.borrow_mut() = geometry
            .iter()
            .enumerate()
            .map(|(index, (y, _))| (*y, index))
            .collect();
    }
    geometry
}

/// The open dropdown, anchored under its setting's value.
fn render_dropdown(
    f: &mut Frame,
    app: &App,
    body: Rect,
    cursor: usize,
    pick: usize,
    rows: &[(u16, u16)],
) {
    let Some(kind) = crate::settings::SettingKind::ALL.get(cursor).copied() else {
        return;
    };
    let Some((row_y, x)) = rows.get(cursor).copied() else {
        return;
    };
    let setting = app.setting_row(kind);
    if setting.options.is_empty() {
        return;
    }

    // Below the row by default, above it when that is where the room is — a list that ran off the
    // bottom would hide the very options it exists to show.
    let want = u16::try_from(setting.options.len() + 2).unwrap_or(u16::MAX);
    let below = body.bottom().saturating_sub(row_y.saturating_add(1));
    let above = row_y.saturating_sub(body.y);
    let (y, height) = if want <= below || below >= above {
        (row_y.saturating_add(1), want.min(below).max(3))
    } else {
        let height = want.min(above).max(3);
        (row_y.saturating_sub(height), height)
    };
    let width = DROP_W.min(body.width.saturating_sub(2)).max(12);
    let popup = Rect::new(
        x.min(body.right().saturating_sub(width)),
        y,
        width,
        height.min(body.height),
    );

    // No bottom hint: the keybar below already reads `Enter Select · Esc Cancel · ↑↓/jk Move` for
    // exactly this state (`Scope::SettingsPick`), and a second copy inside the popup would both repeat
    // it and, at this width, be dropped as too long anyway.
    let inner = draw_overlay_frame_in(f, popup, kind.name(), agent_manager::theme::accent(), "");
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // TWO MARKERS, TWO QUESTIONS. `▸` is the moving cursor — "what would Enter set?" — and `●` is the
    // value in force — "what is set?". Both are drawn glyphs: one marker answering both questions is how
    // a picker starts to look like it is lying once you move off the current row, and a cursor carried
    // only by a background is how this one looked broken (see [`CURSOR`]).
    let text_w = usize::from(inner.width);
    // The note is a COLUMN, not a suffix: one width for every label lines each `dark`/`light` up, so
    // the eye scans a column instead of reading every row's tail.
    let note_w = setting
        .options
        .iter()
        .map(|option| text_cols(&option.note))
        .max()
        .unwrap_or(0);
    let label_w = text_w.saturating_sub(4 + note_w + 1);
    let items: Vec<ListItem> = setting
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let current = setting.current == Some(index);
            let here = index == pick.min(setting.options.len().saturating_sub(1));
            let label = pad_cols(&truncate(&option.label, label_w), label_w);
            let note = pad_cols(&option.note, note_w);
            ListItem::new(Line::from(vec![
                Span::styled(
                    if here { CURSOR } else { "  " },
                    Style::default()
                        .fg(agent_manager::theme::accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if current { "● " } else { "  " },
                    Style::default().fg(agent_manager::theme::live()),
                ),
                Span::styled(
                    label,
                    match (here, current) {
                        // The cursor row is the loud one, because it is the one a keypress acts on.
                        (true, _) => Style::default()
                            .fg(agent_manager::theme::accent())
                            .add_modifier(Modifier::BOLD),
                        (false, true) => Style::default()
                            .fg(agent_manager::theme::live())
                            .add_modifier(Modifier::BOLD),
                        (false, false) => Style::default().fg(attention::text()),
                    },
                ),
                Span::styled(note, attention::text_dim()),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(pick.min(setting.options.len().saturating_sub(1))));
    f.render_stateful_widget(
        List::new(items).highlight_style(attention::selection()),
        inner,
        &mut state,
    );

    // The OPTION rows own the hit list while the dropdown is down, published after the draw so
    // `ListState::offset` is the offset the widget actually scrolled to.
    *app.settings_hits.borrow_mut() = (0..inner.height)
        .filter_map(|row| {
            let index = state.offset().checked_add(usize::from(row))?;
            (index < setting.options.len()).then_some((inner.y.saturating_add(row), index))
        })
        .collect();
}
