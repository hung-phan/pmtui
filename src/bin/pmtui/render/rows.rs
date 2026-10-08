//! The SESSIONS list: which section a row belongs to (and therefore how the list
//! sorts), the fixed-width columns a row is built from, and the list widget itself.
//! Grouped together because the widths, the group order and the row text have to
//! agree — a column budget that disagrees with the row that fills it is a truncated
//! glyph.

use crate::*;

/// Columns a SESSIONS row must have before it can afford the age column, measured on
/// the LIST PANE's inner width (not the frame's — that is the width a row actually gets).
///
/// The arithmetic: the row's fixed part is exactly 36 columns (2 glyph + 16 id + 14
/// posture + 4 tier chip) and the age adds 4, so 40 is the point at which the age can
/// be shown at all. It is the first thing dropped, because it is context while everything else
/// on the row is either identity or a call to action.
///
/// This was 44 while the severity badge was a TRAILING item reserving four columns of its own.
/// The badge was folded into the label, then removed outright at the user's request, so those
/// four columns came back and the age now survives four columns further down.
///
/// Consequence worth knowing rather than mistaking for a bug: [`render_body`]'s WIDE tier gives
/// the list a 44–52 column sidebar ([`sessions_width`]), so the age appears part-way up that
/// range rather than only at its cap.
///
/// DERIVED from its parts rather than typed as a literal, and the row does not compare against
/// it directly: `project_row_line` admits each trailing item against the room actually left.
/// This survives as the pinned TOTAL — the width at which the whole row must still fit — and
/// `the_rows_trailing_widths_still_add_up_to_the_age_threshold` asserts the two agree.
#[cfg(test)]
pub(crate) const ROW_AGE_W: u16 = ROW_FIXED_W + ROW_AGE_COL_W;

/// The SESSIONS row's FIXED head, in columns: `"{glyph} "` 2 + the label padded to 16
/// terminal cells ([`pad_cols`]) + `" {:<13}"` 14 + `" [{}]"` 4. Every row draws all of
/// it at every width; optional age and human-attached fields are admitted only when the
/// remaining width permits.
pub(crate) const ROW_FIXED_W: u16 = 36;

/// `" {:>3}"` — the age column.
pub(crate) const ROW_AGE_COL_W: u16 = 4;

/// `" chat"` — the live-chat chip.
pub(crate) const ROW_CHAT_W: u16 = 5;

/// Which section of the SESSIONS list a row belongs to.
///
/// Attention and unavailable terminals are view-level states around the actual operating mode;
/// the `[A]`/`[S]` tag remains visible in every group. Routine live rows use
/// [`agent_manager::daemon::pmd_drives_row`], the same ownership predicate as the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowGroup {
    /// The session is blocked on a human decision or intervention.
    NeedsYou,
    /// pmd drives it right now.
    Autopilot,
    /// Nothing drives it; the human does.
    Standard,
    /// The operator paused it, or its expected terminal is unavailable.
    Paused,
}

/// The sections, in the order they are laid out and sorted.
pub(crate) const ROW_GROUPS: [RowGroup; 4] = [
    RowGroup::NeedsYou,
    RowGroup::Autopilot,
    RowGroup::Standard,
    RowGroup::Paused,
];

/// An enabled row whose terminal is gone and that is not waiting on the human: it cannot be
/// working, so the row reads `offline` and sits with the paused rows. A Task card leads with the
/// same OFFLINE cue.
pub(crate) fn row_offline(v: &ProjectView) -> bool {
    v.enabled && !v.session_live && !v.posture.needs_attention()
}

/// Which section `v` belongs to. Pure over the view, so the sort and the render cannot disagree.
pub(crate) fn row_group(v: &ProjectView) -> RowGroup {
    if !v.enabled {
        // An explicit pause remains authoritative even if the frozen posture still needs attention.
        RowGroup::Paused
    } else if v.posture.needs_attention() {
        // Human-owned work outranks operating mode and terminal availability.
        RowGroup::NeedsYou
    } else if row_offline(v) {
        // Unavailable automation outranks routine mode; the `[A]`/`[S]` tag keeps the mode.
        RowGroup::Paused
    } else if agent_manager::daemon::pmd_drives_row(v.mode, v.tier) {
        RowGroup::Autopilot
    } else {
        RowGroup::Standard
    }
}

impl RowGroup {
    /// Sort key — the index in [`ROW_GROUPS`].
    pub(crate) fn order(self) -> usize {
        ROW_GROUPS
            .iter()
            .position(|g| *g == self)
            .unwrap_or(usize::MAX)
    }

    /// The user's own word for this section.
    pub(crate) fn name(self) -> &'static str {
        match self {
            RowGroup::NeedsYou => "NEEDS YOU",
            RowGroup::Autopilot => "AUTOPILOT",
            RowGroup::Standard => "STANDARD",
            RowGroup::Paused => "PAUSED / OFFLINE",
        }
    }

    /// A short operational hint that recedes beside the group name and count.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            RowGroup::NeedsYou => "action needed",
            RowGroup::Autopilot => "pmd drives",
            RowGroup::Standard => "you drive",
            RowGroup::Paused => "not running",
        }
    }

    fn style(self) -> Style {
        match self {
            RowGroup::NeedsYou => Style::default()
                .fg(agent_manager::theme::soft())
                .add_modifier(Modifier::BOLD),
            RowGroup::Autopilot => attention::pane_title(),
            RowGroup::Standard => Style::default()
                .fg(attention::text())
                .add_modifier(Modifier::BOLD),
            RowGroup::Paused => Style::default()
                .fg(agent_manager::theme::rule())
                .add_modifier(Modifier::BOLD),
        }
    }

    /// The header line for a section holding `n` rows, fitted to `w` columns.
    ///
    /// A LADDER rather than a truncation, for the same reason the trailing row items are admitted
    /// whole: `AUTOPILOT (2) · pmd dri` is worse than `AUTOPILOT (2)`, because a clipped hint reads
    /// as a clipped row. The name+count is never dropped — at the pane's floor it always fits (the
    /// longest is 20 columns of a 36-column inner width).
    pub(crate) fn header(self, n: usize, w: u16) -> Line<'static> {
        let head = format!("{} ({n})", self.name());
        let tail = format!(" · {}", self.hint());
        let mut spans = vec![Span::styled(head.clone(), self.style())];
        if text_cols(&head) + text_cols(&tail) <= usize::from(w) {
            // The hint RECEDES: it restates in words what the name already said, and the rows
            // under it are what the eye should land on.
            spans.push(Span::styled(tail, attention::text_dim()));
        }
        Line::from(spans)
    }
}

/// The list's row order: SECTION first, then posture, display label, and stable id.
///
/// Section first is what keeps `j`/`k` walking down the list the way it is drawn.
/// [`render_sessions`] groups independently — it is pure over whatever vec it is handed, so it
/// cannot draw a duplicate header for an unsorted list — which makes this the only thing tying
/// keyboard order to screen order. `the_sort_and_the_screen_agree_on_row_order` asserts they do.
pub(crate) fn row_order(a: &ProjectView, b: &ProjectView) -> std::cmp::Ordering {
    row_group(a)
        .order()
        .cmp(&row_group(b).order())
        .then_with(|| a.posture.sort_rank().cmp(&b.posture.sort_rank()))
        .then_with(|| a.label().cmp(b.label()))
        .then_with(|| a.id.cmp(&b.id))
}

fn status_style(v: &ProjectView) -> Style {
    if !v.enabled || row_offline(v) {
        return attention::text_dim();
    }
    match attention::level(v) {
        attention::Level::Hard => Style::default()
            .fg(agent_manager::theme::hard())
            .add_modifier(Modifier::BOLD),
        attention::Level::Soft => Style::default()
            .fg(agent_manager::theme::soft())
            .add_modifier(Modifier::BOLD),
        attention::Level::Live => attention::text_live(),
        attention::Level::Calm | attention::Level::Off => attention::text_dim(),
    }
}

fn tier_style(tier: Option<Tier>) -> Style {
    match tier {
        Some(Tier::Autopilot) => Style::default()
            .fg(agent_manager::theme::accent_alt())
            .add_modifier(Modifier::BOLD),
        Some(Tier::Standard) => attention::text_dim(),
        None => Style::default().fg(agent_manager::theme::soft()),
    }
}

/// A styled SESSIONS row: colored status glyph + id + a dim secondary label
/// (engine/liveness for interactive, posture for autonomous) + a tier chip + a compact
/// age + the human-attached signal. Disabled-row dimming is applied by the caller (whole-row).
///
/// `content_w` is the LIST pane's inner width (see [`ROW_AGE_W`]); `now` is passed in
/// rather than read from the clock here so the row stays a pure function of its inputs
/// and the age is unit-testable without a fake clock.
pub(crate) fn project_row_line(v: &ProjectView, content_w: u16, now: Epoch) -> Line<'static> {
    let offline = row_offline(v);
    let (glyph, gcolor) = if offline {
        ("○", agent_manager::theme::hard())
    } else {
        status_glyph(v)
    };
    let loud = v.enabled && v.posture.needs_attention();
    let mut spans = vec![Span::styled(
        format!("{glyph} "),
        if loud {
            Style::default().fg(gcolor).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(gcolor)
        },
    )];
    // EVERY id is ordinary text. An autopilot row's id used to start cyan so the TachyonFX hue sweep
    // had saturation to rotate; the sweep is gone, so the colour was left saying "this row is driven"
    // — a job the Autopilot section, the `[A]` tag and the `control pmd` line already do, and one a hue
    // should not have, since the id is what tells you WHICH session (user: *"i see you use special
    // color for the autopilot text. just keey it normal"*).
    // Sized in terminal cells: a display name is user text, and a CJK or emoji label padded by
    // chars pushed the status label and tier tag past the pane edge.
    let id_field = pad_cols(v.label(), 16);
    let id_color = attention::text();
    spans.push(Span::styled(
        id_field,
        if loud {
            Style::default().fg(id_color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(id_color)
        },
    ));
    // Secondary label: interactive projects are human-driven, so show tmux
    // liveness + engine, NOT the autonomous posture (which may be stale).
    //
    // PAUSED and OFFLINE outrank a stale routine posture for accuracy. A stopped or missing
    // process cannot currently be "working" even if that is the last posture in its ledger.
    //
    // This is also the only place the state can be SEEN. The trailing position (where the badge
    // used to live) is empty at the widths that matter: between 80 and ~125 terminal columns the
    // item width is exactly `ROW_FIXED_W`, so nothing after the tier tag is drawn at all — the
    // real-tmux suite caught a "(paused)" badge that no human would ever have seen. The label is
    // inside the fixed head, always drawn, and it costs zero extra columns.
    //
    // A staged spawn is disabled too, but nothing paused it: the spawn broker is launching it.
    let word = if v.spawn_staged {
        "starting…"
    } else if !v.enabled {
        "paused"
    } else if offline {
        "offline"
    } else {
        primary_status_label(v)
    };
    // Semantic foregrounds carry state without turning routine rows into filled banners. The
    // field remains exactly 14 columns, preserving every width and shedding threshold below.
    let label_text = format!(" {:<13}", truncate(word, 13));
    spans.push(Span::styled(label_text, status_style(v)));
    spans.push(Span::styled(
        format!(" [{}]", tier_tag(v.tier)),
        tier_style(v.tier),
    ));
    // How long since this session last did anything (`ProjectView::last_activity`: the
    // driver's `observed_at`, or the agent-loop ledger's `updated_at`). It was already
    // loaded and thrown away, and it answers the question the posture cannot — "working"
    // for 3 seconds and "working" for 3 hours are very different rows. Right-aligned to
    // 3 columns so the ages line up down the list and the badges after them do too.
    // The TRAILING chips are built FIRST so the age can be shed against them, even
    // though they are pushed last.
    //
    // This ordering is load-bearing, and getting it wrong shipped a regression: the row is
    // a `Vec<Span>` rendered with no wrap, so ratatui's `LineTruncator` cuts the TAIL.
    // With the age pushed ahead of these chips, an overflowing row kept the age and
    // dropped `chat` — i.e. it spent the last columns on a nice-to-have and discarded "a
    // human is holding this conversation, which is why nothing is advancing". That chip
    // exists because exactly that state once wedged a session and cost a live debugging
    // session to explain, so it outranks the age. Caught by
    // `chat_chip_says_the_poll_is_parked` (tests/integration/), which no unit test saw.
    //
    // ADMISSION, NOT TRUNCATION. Each trailing item is measured and either taken WHOLE
    // or left out, so neither the age nor the human-attached signal is half-drawn.
    //
    // Priority `chat > age`; display order `age, chat`. `chat` first because a live chat REPL
    // is the reason nothing is advancing — exactly that state once wedged a session and cost a
    // live debugging session to explain — while the age is context.
    //
    let room = content_w.saturating_sub(ROW_FIXED_W);
    let mut left = room;
    let show_attached = v.human_attached && left >= ROW_CHAT_W;
    if show_attached {
        left -= ROW_CHAT_W;
    }
    if left >= ROW_AGE_COL_W {
        spans.push(Span::styled(
            format!(" {:>3}", age_label(v.last_activity, now)),
            attention::text_dim(),
        ));
    }
    // A live chat REPL holds this session's conversation, which PARKS pmd's poll (see
    // `chat_lock::is_active`). Its own chip, NOT a posture change: the row's posture is
    // still whatever the ledger says, and this is the reason nothing is advancing it.
    if show_attached {
        spans.push(Span::styled(
            " user",
            Style::default()
                .fg(agent_manager::theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
    }
    // (A trailing " (disabled)" badge lived here until m36. Two things were wrong with it: it was
    // pushed with no admission check, so at 37–40 columns ratatui's tail truncator could leave
    // " (pau" — the exact half-draw the block above forbids for the severity badge — and at the
    // common width it was not drawn at all. The label says it now.)
    Line::from(spans)
}

/// A fixed "now" for the row tests, far enough past the fixtures' `last_activity` that
/// the age column renders a stable value instead of tracking the wall clock.
#[cfg(test)]
pub(crate) const ROW_TEST_NOW: Epoch = 100;

/// Plain-text flattening of [`project_row_line`] (tested for content, so what's
/// shown is what's asserted), at a width wide enough for every optional chip.
#[cfg(test)]
pub(crate) fn project_row_text(v: &ProjectView) -> String {
    project_row_text_at(v, 200, ROW_TEST_NOW)
}

/// [`project_row_text`] at an explicit pane width and clock — the seam the age-column
/// width tier is tested through.
#[cfg(test)]
pub(crate) fn project_row_text_at(v: &ProjectView, content_w: u16, now: Epoch) -> String {
    project_row_line(v, content_w, now)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect()
}

pub(crate) fn sessions_full_height(projects: &[ProjectView]) -> u16 {
    let groups = ROW_GROUPS
        .iter()
        .filter(|group| projects.iter().any(|view| row_group(view) == **group))
        .count();
    let items = projects
        .len()
        .saturating_add(groups)
        .saturating_add(groups.saturating_sub(1));
    u16::try_from(items).unwrap_or(u16::MAX).saturating_add(2)
}

pub(crate) fn stacked_sessions_height(projects: &[ProjectView], body_h: u16) -> u16 {
    let want = sessions_full_height(projects);
    let cap = body_h.saturating_sub(7).max(3);
    want.clamp(3, cap).min(body_h)
}

/// The SESSIONS list width in the WIDE tier: ~35% of the terminal, clamped to a
/// readable band so it neither crowds the preview nor shrinks to nothing, and never
/// wider than the terminal (so a tiny term still lays out without panicking).
///
/// The floor is what a full row actually needs — 2 (`▸ ` highlight symbol) + 2
/// (glyph) + 16 (id) + 14 (secondary label) + 4 (tier chip) + 2 (borders) + 4 (two columns
/// of inner padding each side) = 44 — so the tier chip stops being clipped off the right
/// edge. The extra 4 over the old 40 is the inner [`Padding`] the SESSIONS block now carries.
pub(crate) fn sessions_width(total: u16) -> u16 {
    let target = (u32::from(total) * 35 / 100) as u16;
    target.clamp(44, 52).min(total)
}

/// Left pane: the bordered ` SESSIONS ` list of styled project rows.
///
/// The rows are told the pane's INNER width — `area` minus the two border columns, the two
/// columns of inner [`Padding`] each side, and the two the `▸ ` selection marker takes (8 in
/// all) — because that, not the frame width, is what a row has to fit; the age column is shed
/// against it (see [`ROW_AGE_W`]). "Now" is read ONCE per frame, not per row, so every age on
/// screen is measured from the same instant.
pub(crate) fn render_sessions(f: &mut Frame, app: &App, area: Rect) {
    // 8 = 2 borders + 4 inner padding (two columns each side) + 2 selection marker.
    let content_w = area.width.saturating_sub(8);
    let now = SystemClock.now();
    // SECTIONED by attention, availability, then driver (see [`RowGroup`]). Grouping is done HERE, over
    // whatever order `app.projects` is in, rather than by trusting the vec to be sorted — a
    // render that assumed sorted input would draw a second `AUTOPILOT` header the first time
    // anything handed it an unsorted list, which is the kind of defect that only shows up on
    // screen. `refresh` sorts by the same key so keyboard order matches screen order.
    //
    // Headers are LIST ITEMS, not a separate widget, so they scroll with the rows and cost
    // nothing when a group is empty. They are never selectable: `app.selected` indexes
    // `projects`, and `sel_item` below maps it to the item index the header offsets produce, so
    // no navigation code has to know sections exist at all.
    let mut items: Vec<ListItem> = Vec::with_capacity(app.projects.len() + ROW_GROUPS.len());
    // Parallel to `items`: which `projects` index each list item is, or `None` for a header. Used
    // AFTER the render to turn the list's scroll offset into a screen-row → project-index map for
    // click-to-select (see `app.row_hits`).
    let mut item_to_proj: Vec<Option<usize>> = Vec::with_capacity(items.capacity());
    let mut sel_item = None;
    let mut drawn_a_group = false;
    for group in ROW_GROUPS {
        let members: Vec<usize> = (0..app.projects.len())
            .filter(|i| row_group(&app.projects[*i]) == group)
            .collect();
        if members.is_empty() {
            continue;
        }
        // A blank line BETWEEN sections (not before the first) so the list breathes instead of
        // stacking one section's header straight onto the previous section's rows (user: *"we can be
        // a bit nicer in spacing on our UI so it doesn't feel cram"*). A spacer is not selectable.
        if drawn_a_group {
            items.push(ListItem::new(Line::raw("")));
            item_to_proj.push(None);
        }
        drawn_a_group = true;
        items.push(ListItem::new(group.header(members.len(), content_w)));
        item_to_proj.push(None); // a header is not selectable
        for i in members {
            let v = &app.projects[i];
            if i == app.selected {
                sel_item = Some(items.len());
            }
            // Hand-draw the `▸ ` selection gutter as part of the ROW content, rather than the List's
            // `highlight_symbol` — because that symbol column is reserved for EVERY item, including
            // the non-selectable section headers, which then sit a marker-width further in than they
            // need (user: *"the … AUTOPILOT, STANDARD, and PAUSE … have too much padding left"*).
            // Drawing it here keeps rows indented by the marker while headers stay flush at the
            // pane's inner padding. `content_w` already reserves these 2 columns.
            let gutter = if i == app.selected {
                Span::styled(
                    attention::GUTTER,
                    Style::default().add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };
            let mut spans = vec![gutter];
            spans.extend(project_row_line(v, content_w, now).spans);
            let mut item = ListItem::new(Line::from(spans));
            if !v.enabled {
                // Preserve the disabled-row dimming across the whole row.
                item = item.style(Style::default().add_modifier(Modifier::DIM));
            }
            items.push(item);
            item_to_proj.push(Some(i));
        }
    }
    if items.is_empty() {
        // First run: the list is where sessions will appear, so it names the key that makes one.
        // The top controls that also say so yield on a very narrow terminal; this line does not.
        let long = "press n to create a session";
        let hint = if text_cols(long) <= usize::from(content_w.saturating_add(2)) {
            long
        } else {
            "n: new session"
        };
        items.push(ListItem::new(Line::styled(hint, attention::text_dim())));
        item_to_proj.push(None);
    }
    let list = List::new(items)
        .block(
            Block::bordered()
                // The frame recedes so the rows can carry the screen — drawn in the quiet rule
                // rather than the theme's full-strength foreground. No focus brightening: `j`/`k`
                // always move this list's selection (there is no other focus to be in), and the
                // selected ROW is the affordance for where they will land.
                .border_style(attention::rule())
                .title(" SESSIONS ")
                .title_style(attention::pane_title())
                // Two columns of horizontal breathing room each side so the rows don't jam against
                // the border (user's "less cram" spacing). This is why `content_w` subtracts 8 (2
                // border + 4 padding + 2 marker), not 4; there is no top padding, so the click-map
                // keeps the border-only +1.
                .padding(Padding::horizontal(2)),
        )
        // Selection remains separate from state: a raised row plus the `▸` gutter does not replace
        // the glyph or semantic foreground. The gutter is
        // hand-drawn into each ROW's spans above (NOT ratatui's `highlight_symbol`, which reserves a
        // marker column on every item including the non-selectable headers — the source of their
        // extra left indent). `content_w` reserves the gutter's 2 columns for the rows.
        .highlight_style(attention::selection());
    let mut ls = ListState::default();
    // The ITEM index, not `app.selected`: the headers above the selected row shift it down.
    ls.select(sel_item);
    f.render_stateful_widget(list, area, &mut ls);

    // CLICK-to-select: map each on-screen row to the project it drew, so a left-click can select it
    // (user: *"i think the UI is not clickable on the left panel. i want click to just select it"*).
    // `ls.offset()` is the first VISIBLE item after the render decided its scroll, so visible row `k`
    // shows item `offset + k` at screen row `area.y + 1 + k` — one item per row, offset by 1 for the
    // top border (the pane has horizontal padding only, no top padding). Header/blank items map to
    // nothing, so a click on a section header selects nothing. `inner_h` drops 2 for the borders.
    let inner_h = usize::from(area.height.saturating_sub(2));
    let offset = ls.offset();
    let mut hits: Vec<(u16, usize)> = Vec::with_capacity(inner_h);
    for k in 0..inner_h {
        match item_to_proj.get(offset + k) {
            Some(Some(pi)) => hits.push((area.y + 1 + k as u16, *pi)),
            Some(None) => {} // a section header
            None => break,   // past the last item
        }
    }
    *app.row_hits.borrow_mut() = hits;
}
