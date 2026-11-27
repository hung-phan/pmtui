//! The SESSIONS list: which section a row belongs to (and therefore how the list
//! sorts), the fixed-width columns a row is built from, and the list widget itself.
//! Grouped together because the widths, the group order and the row text have to
//! agree — a column budget that disagrees with the row that fills it is a truncated
//! glyph.

use crate::*;

/// Columns a SESSIONS row must have before it can afford the age column, measured on
/// the LIST PANE's inner width (not the frame's — that is the width a row actually gets).
///
/// The arithmetic: the identity line's fixed part is 2 columns (the glyph) and the age adds 4, so
/// 6 is the point at which the age can be shown beside a name of any width at all. It is the first
/// thing dropped, because it is context while everything else on the line is identity.
///
/// DERIVED from its parts rather than typed as a literal, and the row does not compare against
/// it directly: `project_row_line` admits each trailing item against the room actually left.
/// This survives as the pinned TOTAL — the width at which the whole row must still fit — and
/// `the_rows_trailing_widths_still_add_up_to_the_age_threshold` asserts the two agree.
#[cfg(test)]
pub(crate) const ROW_AGE_W: u16 = ROW_FIXED_W + ROW_AGE_COL_W;

/// The identity line's FIXED head, in columns: `"{glyph} "` — and that is all.
///
/// IT WAS 36 (2 glyph + 16 name + 14 status word + 4 tier tag), a stamp that drew every field at
/// every width. Measured against a real 52-column sidebar, that left the row six columns short of
/// the pane while a depth-2 child elided its own name. The status word, the tier tag, the cadence
/// and the engine now live on the row's SECOND line ([`project_row_detail`]), so the identity line
/// is the glyph, the name, and the age — one fixed field, one flexible, one trailing.
pub(crate) const ROW_FIXED_W: u16 = 2;

/// `" {:>3}"` — the age column, the identity line's one trailing item.
pub(crate) const ROW_AGE_COL_W: u16 = 4;

/// `" user"` — the human-attached chip, on the STATE line.
pub(crate) const ROW_CHAT_W: u16 = 5;

/// The state line's indent, in columns: it hangs under the NAME rather than under the glyph, so
/// the two lines read as one block and the state words still align down the list.
///
/// FOUR, measured on a real pane: the identity line's spans are preceded by the 2-column selection
/// gutter that `render_sessions` pushes, then its own 2-column glyph, so the name starts four
/// columns into the row. An indent of 2 put the state words under the GLYPH, two columns left of
/// the name, and read as a misaligned second line.
pub(crate) const ROW_DETAIL_INDENT: u16 = 4;

/// The selection gutter `render_sessions` pushes ahead of the identity line's spans — the `▸ ` mark
/// on the selected row, two spaces on every other.
///
/// The state line carries no gutter (its own indent replaces it), so it is handed
/// `content_w + ROW_GUTTER_W` as its budget. Without that it was two columns shorter than the line
/// above it and the right-aligned age stopped two columns inside the pane edge.
pub(crate) const ROW_GUTTER_W: u16 = 2;

/// How many lineage levels a row may be indented by.
///
/// TWO, and the `from <parent>` cue in the preview head carries anything deeper. Nothing bounds
/// depth any more (a session may coordinate its own children), but the indent is paid for out of
/// the 16-column name field — see [`ROW_INDENT_W`] — so a third level would start eating names to
/// draw a shape the preview already states in words.
pub(crate) const ROW_MAX_INDENT: usize = 2;

/// Columns one lineage level costs: `"\u{2514} "`, or two spaces under it.
///
/// TAKEN FROM THE NAME FIELD, never added to the row. [`ROW_FIXED_W`] is what every trailing item
/// is admitted against, and that admission is load-bearing — a regression there once shipped a
/// `(paused)` badge no human could see, and another spent the last columns on the age while
/// dropping the chip that said why nothing was advancing. An indent that widened the head would
/// move every one of those thresholds; this one moves none.
pub(crate) const ROW_INDENT_W: usize = 2;

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

/// How many of `row`'s ancestors are in `members` — its drawn indent level.
///
/// Pure over the slice it is handed, for the same reason [`row_group`] is: the arrangement and the
/// render must not disagree about which rows are children. An ancestor OUTSIDE the group does not
/// count, which is what keeps a needy child at the top of `NEEDS YOU` rather than indented under a
/// parent that is not there — the `from <parent>` cue still names it.
///
/// The walk is bounded by `members.len()`, so a lineage that somehow pointed in a circle costs one
/// pass rather than hanging the frame.
pub(crate) fn row_depth(views: &[ProjectView], members: &[usize], row: usize) -> usize {
    let mut depth = 0;
    let mut parent = views[row].spawned_by.as_deref();
    while let Some(id) = parent {
        let Some(&at) = members.iter().find(|&&i| views[i].id == id) else {
            break;
        };
        depth += 1;
        if depth > members.len() {
            break;
        }
        parent = views[at].spawned_by.as_deref();
    }
    depth
}

/// The indices of `views`, grouped as the list draws them and with each child directly after its
/// own parent.
///
/// LINEAGE IS NOT A CONTAINER HERE, and that is the whole point. The groups stay exactly as
/// [`row_order`] put them — urgency first — and this only settles the order WITHIN one group, so a
/// needy child whose parent is on Autopilot still sits at the top of `NEEDS YOU` instead of being
/// folded under a parent elsewhere. The product's one job is telling a human which agent needs
/// them, and a row that is hidden inside a parent is a row that failed to.
///
/// Within a group a parent is followed by its descendants, depth first, each in the order
/// [`row_order`] already gave them. A member whose parent is not in the group is a root here.
pub(crate) fn lineage_order(views: &[ProjectView]) -> Vec<usize> {
    let mut order = Vec::with_capacity(views.len());
    for group in ROW_GROUPS {
        let members: Vec<usize> = (0..views.len())
            .filter(|i| row_group(&views[*i]) == group)
            .collect();
        let mut placed = vec![false; members.len()];
        for (n, &i) in members.iter().enumerate() {
            if placed[n] || row_depth(views, &members, i) != 0 {
                continue;
            }
            placed[n] = true;
            order.push(i);
            push_descendants(
                views,
                &members,
                &views[i].id.clone(),
                &mut placed,
                &mut order,
            );
        }
        // Defensive, and it keeps the two orders equal in length: anything left unplaced (only
        // reachable from a lineage that points in a circle) keeps its sorted position at the end.
        for (n, &i) in members.iter().enumerate() {
            if !placed[n] {
                order.push(i);
            }
        }
    }
    order
}

/// Append every member whose parent is `parent`, then that member's own descendants.
fn push_descendants(
    views: &[ProjectView],
    members: &[usize],
    parent: &str,
    placed: &mut [bool],
    order: &mut Vec<usize>,
) {
    for (n, &i) in members.iter().enumerate() {
        if placed[n] || views[i].spawned_by.as_deref() != Some(parent) {
            continue;
        }
        placed[n] = true;
        order.push(i);
        push_descendants(views, members, &views[i].id.clone(), placed, order);
    }
}

/// Reorder `views` in place so the list reads as the tree it is. Called once per refresh, after
/// [`row_order`]; the keyboard walks this vec and the render filters it in this order, so screen
/// order and `j`/`k` cannot come apart.
pub(crate) fn arrange_lineage(views: &mut Vec<ProjectView>) {
    let order = lineage_order(views);
    let mut taken: Vec<Option<ProjectView>> = views.drain(..).map(Some).collect();
    for i in order {
        if let Some(view) = taken[i].take() {
            views.push(view);
        }
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
pub(crate) fn project_row_line(v: &ProjectView, content_w: u16, depth: usize) -> Line<'static> {
    let offline = row_offline(v);
    let (glyph, gcolor) = if offline {
        ("○", agent_manager::theme::hard())
    } else {
        status_glyph(v)
    };
    let loud = v.enabled && v.posture.needs_attention();
    // THE LINEAGE TIE COMES FIRST, so a child's whole row — glyph included — steps right of its
    // parent's. Its columns come OUT OF THE NAME FIELD rather than being added to the head, so
    // `ROW_FIXED_W` is unchanged and every trailing admission threshold stays where it was.
    let depth = depth.min(ROW_MAX_INDENT);
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
    if depth > 0 {
        let mut tie = " ".repeat((depth - 1) * ROW_INDENT_W);
        tie.push_str("\u{2514} ");
        spans.push(Span::styled(tie, attention::text_dim()));
    }
    spans.push(Span::styled(
        format!("{glyph} "),
        if loud {
            Style::default().fg(gcolor).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(gcolor)
        },
    ));
    // EVERY id is ordinary text. An autopilot row's id used to start cyan so the TachyonFX hue sweep
    // had saturation to rotate; the sweep is gone, so the colour was left saying "this row is driven"
    // — a job the Autopilot section, the `[A]` tag and the `control pmd` line already do, and one a hue
    // should not have, since the id is what tells you WHICH session (user: *"i see you use special
    // color for the autopilot text. just keey it normal"*).
    // Sized in terminal cells: a display name is user text, and a CJK or emoji label padded by
    // chars pushed the status label and tier tag past the pane edge.
    //
    // THE NAME IS THE FLEX FIELD, and now it is the only thing competing with the age. Measured at
    // both ends of the width band while four fields shared this line: at the 52-column cap the row
    // stopped SIX columns short of the pane, because every field was fixed — so the row was a
    // 36-column stamp floating in a 45-column field, the right edge was never reached, and the ages
    // did not line up down the list to be compared. Giving the slack to the name pins the age
    // against the right edge BY CONSTRUCTION (`2 + name + 4 == content_w`) rather than by a second
    // alignment rule that could disagree with the admission below.
    //
    // It also buys back what the row was short of. The name was capped at 16 columns for the
    // narrowest sidebar, and the lineage indent is paid OUT of it, so a depth-2 child elided
    // `autopilot-docs` to `autopilot-d…`. With the state words on their own line it gets the whole
    // width, and a name no longer has to be short to be read.
    // THE WHOLE LINE IS THE NAME. Nothing else is on it — the age went down to the state line with
    // the rest of the facts, so this line answers exactly one question and the name is never
    // shortened for anything.
    let id_field = pad_cols(
        v.label(),
        usize::from(content_w.saturating_sub(ROW_FIXED_W)).saturating_sub(depth * ROW_INDENT_W),
    );
    let id_color = attention::text();
    spans.push(Span::styled(
        id_field,
        if loud {
            Style::default().fg(id_color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(id_color)
        },
    ));
    Line::from(spans)
}

/// The row's SECOND line: what this session is doing, what drives it, how often, and on which
/// engine — the state cluster, dim, hanging under the name.
///
/// WHY IT IS A LINE AND NOT A COLUMN. These four facts shared the identity line as fixed fields, in
/// a 39–45 column sidebar, against a name that had 16. The state word and the tier tag are the two
/// the project's design record pins as always-visible, so they could not be shed to make room — the
/// only way to stop them competing with the thing that says WHICH session this is was to give them
/// a line of their own. That line already existed: every row owns a blank beneath it for spacing,
/// and the selected row's band already covers both.
///
/// `v.enabled == false` dims the whole row at the caller, so this builds no dim-specific style.
pub(crate) fn project_row_detail(
    v: &ProjectView,
    content_w: u16,
    depth: usize,
    now: Epoch,
) -> Line<'static> {
    let offline = row_offline(v);
    let indent =
        ROW_DETAIL_INDENT + u16::try_from(depth.min(ROW_MAX_INDENT) * ROW_INDENT_W).unwrap_or(0);
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ".repeat(usize::from(indent)))];
    let mut left = content_w.saturating_sub(indent);

    // THE STATE WORD IS NEVER SHED. It is the only place a paused, offline or starting row says so
    // — the real-tmux suite once caught a `(paused)` badge that no human would ever have seen,
    // because it sat where the width ran out. A staged spawn is disabled too, but nothing paused
    // it: the broker is launching it.
    //
    // PAUSED and OFFLINE outrank a stale routine posture for accuracy: a stopped or missing process
    // cannot currently be "working" even if that is the last posture in its ledger.
    let word = if v.spawn_staged {
        "starting…"
    } else if !v.enabled {
        "paused"
    } else if offline {
        "offline"
    } else {
        primary_status_label(v)
    };
    let word = truncate(word, usize::from(left).max(1));
    left = left.saturating_sub(u16::try_from(text_cols(&word)).unwrap_or(0));
    spans.push(Span::styled(word, status_style(v)));

    // THE TIER TAG IS NEVER SHED EITHER, and it is a pinned invariant rather than a judgement: it
    // stays visible in every group, because two of the four groups (NEEDS YOU, PAUSED/OFFLINE) mix
    // driven and human-driven rows and the section header cannot say which a row is.
    let tag = format!(" [{}]", tier_tag(v.tier));
    left = left.saturating_sub(u16::try_from(text_cols(&tag)).unwrap_or(0));
    spans.push(Span::styled(tag, tier_style(v.tier)));

    // From here everything is ADMITTED against the room left, in priority order. A live chat REPL
    // holds this session's conversation, which PARKS pmd's poll (`chat_lock::is_active`) — it is the
    // reason nothing is advancing, so it outranks both facts below. Not a posture change: the row's
    // posture is still whatever the ledger says.
    if v.human_attached && left >= ROW_CHAT_W {
        left -= ROW_CHAT_W;
        spans.push(Span::styled(
            " user",
            Style::default()
                .fg(agent_manager::theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
    }
    // THE CADENCE, ONLY FOR A ROW SOMETHING DRIVES. On a Standard row pmd never nudges, so a
    // heartbeat here would promise a wake nothing will send — the same rule the preview head keeps.
    // From the ledger first, which is the value `job_engine` consults.
    if agent_manager::daemon::pmd_drives_row(v.mode, v.tier) {
        let cadence = format!(
            " \u{b7} every {}",
            job_engine::human_cadence(v.cadence_s.unwrap_or(job_engine::DEFAULT_CADENCE_S))
        );
        if left >= u16::try_from(text_cols(&cadence)).unwrap_or(u16::MAX) {
            left -= u16::try_from(text_cols(&cadence)).unwrap_or(0);
            spans.push(Span::styled(cadence, attention::text_dim()));
        }
    }
    // The ENGINE last of the left cluster: it changes least and matters least, so it is the one
    // that goes when the sidebar is narrow.
    if let Some(engine) = v.engine {
        let label = format!(" \u{b7} {}", engine.label());
        if left >= u16::try_from(text_cols(&label)).unwrap_or(u16::MAX) + ROW_AGE_COL_W {
            left -= u16::try_from(text_cols(&label)).unwrap_or(0);
            spans.push(Span::styled(label, attention::text_dim()));
        }
    }
    // HOW LONG SINCE THIS SESSION DID ANYTHING (`ProjectView::last_activity`: the driver's
    // `observed_at`, or the agent-loop ledger's `updated_at`). It answers what the state word
    // cannot — "working" for 3 seconds and "working" for 3 hours are very different rows.
    //
    // RIGHT-ALIGNED TO THE PANE EDGE, which is the whole reason it is worth a position of its own:
    // the ages then form a column you can read down and compare, instead of ending wherever the
    // cluster before them happened to stop. The engine above yields its columns to it rather than
    // the other way round, because the age changes every tick and the engine never does.
    if left >= ROW_AGE_COL_W {
        spans.push(Span::raw(" ".repeat(usize::from(left - ROW_AGE_COL_W))));
        spans.push(Span::styled(
            format!(" {:>3}", age_label(v.last_activity, now)),
            attention::text_dim(),
        ));
    }
    Line::from(spans)
}

/// A fixed "now" for the row tests, far enough past the fixtures' `last_activity` that
/// the age column renders a stable value instead of tracking the wall clock.
#[cfg(test)]
pub(crate) const ROW_TEST_NOW: Epoch = 100;

/// WHAT THE ROW SAYS — both of its lines, joined by a newline, at a width wide enough for every
/// optional field. A row is two lines now, so a content assertion is about the pair; a test that
/// cares WHICH line a field is on reads [`project_row_line`] or [`project_row_detail`] directly.
#[cfg(test)]
pub(crate) fn project_row_text(v: &ProjectView) -> String {
    format!(
        "{}\n{}",
        project_row_text_at(v, 200),
        flatten(&project_row_detail(v, 200, 0, ROW_TEST_NOW))
    )
}

/// The IDENTITY line only, at an explicit pane width and clock — the seam the age-column width
/// tier is tested through.
#[cfg(test)]
pub(crate) fn project_row_text_at(v: &ProjectView, content_w: u16) -> String {
    flatten(&project_row_line(v, content_w, 0))
}

/// The STATE line only, at an explicit pane width and lineage depth.
#[cfg(test)]
pub(crate) fn project_row_detail_text_at(
    v: &ProjectView,
    content_w: u16,
    depth: usize,
    now: Epoch,
) -> String {
    flatten(&project_row_detail(v, content_w, depth, now))
}

#[cfg(test)]
fn flatten(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Every list item the SESSIONS pane would draw, plus its two borders — what the stacked tier asks
/// the pane for.
///
/// IT MUST AGREE WITH [`render_sessions`] ITEM FOR ITEM, or the stacked layout sizes the pane for a
/// list of a different length and the last rows fall off a pane that had room. The items are one
/// header per non-empty group plus THREE per project — the identity line, the state line under it,
/// and the blank that separates it from the next session (which is also the gap before the next
/// section's header).
pub(crate) fn sessions_full_height(projects: &[ProjectView]) -> u16 {
    let groups = ROW_GROUPS
        .iter()
        .filter(|group| projects.iter().any(|view| row_group(view) == **group))
        .count();
    let items = projects.len().saturating_mul(3).saturating_add(groups);
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
    for group in ROW_GROUPS {
        let members: Vec<usize> = (0..app.projects.len())
            .filter(|i| row_group(&app.projects[*i]) == group)
            .collect();
        if members.is_empty() {
            continue;
        }
        // NO SEPARATE SECTION BLANK. Every row owns the blank line under it (below), so the gap
        // before a header is the previous section's last row's own spacer — one blank, exactly as
        // when this was a rule of its own (user: *"we can be a bit nicer in spacing on our UI so it
        // doesn't feel cram"*). Folding the two rules into one is what makes the SELECTION uniform:
        // a spacer only some rows had gave the last row of each section a one-line band where every
        // other row had two.
        items.push(ListItem::new(group.header(members.len(), content_w)));
        item_to_proj.push(None); // a header is not selectable
        for &i in &members {
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
            // ITS OWN GROUP DECIDES THE INDENT. `row_depth` counts only ancestors inside
            // `members`, so a child whose parent sits in another section is drawn flush — and keeps
            // its `from <parent>` cue in the preview head, where lineage has always been a cue.
            spans.extend(
                project_row_line(v, content_w, row_depth(&app.projects, &members, i)).spans,
            );
            let mut item = ListItem::new(Line::from(spans));
            if !v.enabled {
                // Preserve the disabled-row dimming across the whole row.
                item = item.style(Style::default().add_modifier(Modifier::DIM));
            }
            items.push(item);
            item_to_proj.push(Some(i));
            // EVERY ROW OWNS THE LINE UNDER IT, which carries its state cluster
            // ([`project_row_detail`]). `List` has no item spacing, so this second line is its own
            // `ListItem`, and it maps to `None` — not selectable, not clickable, and `j`/`k` never
            // land on it. Uniform rather than between-rows-only, which is what makes every
            // selection the same height and doubles as the gap before the next section header.
            let mut spacer = ListItem::new(project_row_detail(
                v,
                content_w + ROW_GUTTER_W,
                row_depth(&app.projects, &members, i),
                now,
            ));
            // THE SELECTED ROW'S BAND INCLUDES ITS GAP. Spaced rows turned the selection into a
            // thin stripe with air on both sides; carrying the fill one row down makes it one block
            // with room in it.
            if i == app.selected {
                let mut style = attention::selection();
                if !v.enabled {
                    // A dim row's band is dim too, or the fill reads brighter than the row on it.
                    style = style.add_modifier(Modifier::DIM);
                }
                spacer = spacer.style(style);
            }
            items.push(spacer);
            item_to_proj.push(None);
            // AND A BLANK BETWEEN SESSIONS. The state line is the row's own second line, not air,
            // so without this the next row's name sits directly under the previous row's state and
            // the list reads as one block of text (user: *"space between rows too. It is too cramp
            // together"*). Deliberately OUTSIDE the selection band below: the band is the row's two
            // content lines, and a fill that carried into this blank would touch the next row.
            // It also serves as the gap before the next section's header, so the sections are
            // separated exactly once.
            items.push(ListItem::new(Line::raw("")));
            item_to_proj.push(None);
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
    // THE LIST STAYS TOP-ALIGNED. It was briefly centered in the pane, which is not what more room
    // per row was for: a list that starts at a different line depending on how many sessions exist
    // gives the eye nothing fixed to come back to, and `j` from the top row then begins in a
    // different place each time (user: *"why does it put everything in the center. i don't want
    // that"*).
    let inner_h = usize::from(area.height.saturating_sub(2));
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
    // nothing, so a click on a section header selects nothing — and neither does one on a row
    // spacer or on the centering pad, which are `None` for the same reason. `inner_h` (hoisted
    // above, where the pad is measured against it) drops 2 for the borders.
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
