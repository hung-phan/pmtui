//! The visual vocabulary every pane shares: how big a modal overlay gets, the frame
//! it is drawn in, the section divider that heads a block, and the key chip. One
//! module because five overlays each rolling their own "centre a rect, clear it, wrap
//! it in a border" is exactly how they drifted into looking like five different
//! widgets.

use crate::*;

pub(crate) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [col] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    col
}

/// The widest an overlay may grow, however big the terminal is. A one-line text field
/// 140 columns wide is not more usable than one 96 wide, it just looks unfinished.
pub(crate) const OVERLAY_MAX_W: u16 = 96;

/// The chrome an overlay spends on itself: its two border rows. The title rides ON the top
/// border and the hint on the bottom one, so neither costs a row of its own.
pub(crate) const OVERLAY_CHROME_H: u16 = 2;

/// How many rows of buffer the inline goal and directive fields grow to before the library scrolls
/// them. They are content-driven up to this, so a one-line mandate does not open a tall empty window
/// and a long one does not have to be read four rows at a time.
pub(crate) const GOAL_EDIT_LINES: usize = 8;

/// How tall a frame must be to hold `rows` body rows, with a floor so a one-line modal
/// still reads as a window rather than a slot.
///
/// Height is CONTENT-DRIVEN on purpose, and getting that wrong is visible: a first cut
/// gave every overlay a flat 12 rows, so the goal field drew a three-line body inside nine
/// rows of nothing — which reads as unfinished, the very complaint the resize was meant to
/// answer. Bigger is not the same as better proportioned.
pub(crate) fn overlay_h(rows: usize) -> u16 {
    u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .saturating_add(OVERLAY_CHROME_H)
        .max(9)
}

/// Does this status line report a FAILURE? The ONE predicate both the keybar and the
/// status log colour red by, so the two surfaces can never disagree about what counts as
/// one. It matches the two ways this codebase phrases a failure: `"… failed"` (a driver
/// error echoed verbatim by `s`) and `"could not …"` (the wording most `self.status = …`
/// error arms use — e.g. lifecycle's "could not pause", autopilot's "could not change the
/// mode"). The keybar only tested `"failed"`, so a "could not …" status rendered GREEN —
/// the colour of success — whenever it was the newest line on a narrow terminal.
pub(crate) fn status_is_failure(status: &str) -> bool {
    status.contains("failed") || status.contains("could not")
}

/// The columns a full-width one-line BAR keeps clear on each side, so its text starts in the
/// column the panes below it use rather than flush against the frame.
///
/// The top bar was the only row on the dashboard that began at column 0. Everything else starts
/// its text at column 2 — a pane title (`SESSIONS`, `PAUSED 0`) sits one column past its corner,
/// and the keybar's first chip and the verdict's first name are each drawn one column in — while
/// the bar's reversed ` pmtui ` chip hung over the `┌` beneath it with `pmtui` a column to the left
/// of every other left edge (user: *"for the top bar pmtui, can you have padding left and right? i
/// think it doesn't align well with the panel bellow it"*). One column is the whole correction, and
/// it is spent on BOTH sides so the right-hand controls stop short of the frame as well.
pub(crate) const BAR_PAD_X: u16 = 1;

/// `area` inset by [`BAR_PAD_X`] on each side — what a bar draws into.
///
/// Returned UNCHANGED when the row is too narrow to spend the columns, because a bar that gives its
/// last columns to padding reports nothing at all, and the tiny-terminal sweeps expect a line.
pub(crate) fn bar_inset(area: Rect) -> Rect {
    if area.width <= 2 * BAR_PAD_X {
        return area;
    }
    Rect {
        x: area.x.saturating_add(BAR_PAD_X),
        width: area.width - 2 * BAR_PAD_X,
        ..area
    }
}

/// The columns [`bar_inset`] leaves inside `width`, so a caller holding only the row's width
/// reaches the same width-tier decisions as the bar that was drawn.
pub(crate) fn bar_width(width: u16) -> u16 {
    if width <= 2 * BAR_PAD_X {
        width
    } else {
        width - 2 * BAR_PAD_X
    }
}

/// A modal never sits flush against the terminal's edge: it keeps this much clear on the
/// right and below, so the frame reads as a window ON the screen rather than part of it.
///
/// These were the drop-shadow offsets. The shadow is GONE — it was the thing the user
/// disliked, not the border — but the breathing room it happened to reserve was worth
/// keeping on its own merits, and keeping the numbers keeps the geometry (and the tests
/// that pin it) unchanged.
pub(crate) const OVERLAY_MARGIN_X: u16 = 2;
pub(crate) const OVERLAY_MARGIN_Y: u16 = 1;

/// The width [`overlay_rect`] will grant, split out so a caller can know it BEFORE it draws.
///
/// The answer overlay needs exactly that: its height depends on how many rows the question
/// wraps to, which depends on the width. One function rather than two copies of the
/// arithmetic, because a private copy would drift and the symptom would be an overlay whose
/// height is right for a width it did not get.
pub(crate) fn overlay_w(area: Rect, want_w: u16) -> u16 {
    let roomy = (u32::from(area.width) * 45 / 100) as u16;
    want_w
        .max(roomy)
        .min(OVERLAY_MAX_W)
        .min(area.width.saturating_sub(OVERLAY_MARGIN_X))
        .max(1)
        .min(area.width)
}

/// Horizontal padding [`draw_overlay_frame`] applies at a given popup width — the other half
/// of what a caller needs to predict its own inner width.
pub(crate) fn overlay_pad(popup_w: u16) -> u16 {
    if popup_w >= 32 {
        2
    } else if popup_w >= 24 {
        1
    } else {
        0
    }
}

/// The columns of TEXT an overlay of `want_w` will get: the granted width less its two border
/// columns and its padding on both sides.
pub(crate) fn overlay_text_w(area: Rect, want_w: u16) -> usize {
    let w = overlay_w(area, want_w);
    usize::from(w.saturating_sub(2 + 2 * overlay_pad(w)))
}

/// An overlay's frame: how big it gets, given what it WANTS and what the terminal has.
///
/// Grows toward `want_w` and, on a roomy terminal, a little past it (45% of the width,
/// capped at [`OVERLAY_MAX_W`]) so a modal does not look like a postage stamp on a wide
/// screen. HEIGHT never grows past `want_h` — extra rows would be empty ones, and empty
/// space reads as unfinished rather than generous.
///
/// Every step is saturating and the result is clamped INTO `area`, so a 1x1 terminal
/// yields a 1x1 rect rather than a panic (pinned by the tiny-terminal sweeps).
pub(crate) fn overlay_rect(area: Rect, want_w: u16, want_h: u16) -> Rect {
    let w = overlay_w(area, want_w);
    let h = want_h
        .min(area.height.saturating_sub(OVERLAY_MARGIN_Y))
        .max(1)
        .min(area.height);
    centered_rect(w, h, area)
}

/// Draw a modal overlay's whole frame and return the INNER area its body goes in.
///
/// One function for all five overlays, because five copies of "centre a rect, `Clear` it,
/// wrap it in a bordered block" is how they drifted into looking like five different
/// widgets. What it gives them, and what the user was asking for when they said the
/// popups look "flat and small":
///
/// * **depth** — a ROUNDED BORDER in the accent colour over a RAISED SURFACE
///   ([`attention::surface`]). On Catppuccin Mocha that surface is `surface1` `#45475a`,
///   the colour the theme itself uses for popups, so the modal reads as a card with an
///   edge rather than as a hole punched in the dashboard.
///
///   There was briefly a drop shadow instead of the border. Both were tried on a real
///   pane; the border stayed and the shadow went, at the user's word. The SURFACE stays
///   either way — it is what makes the modal a card rather than a fenced-off region, and
///   it costs no rows;
/// * **size** — [`overlay_rect`], which grows on a roomy terminal;
/// * **a title CHIP** rather than a bare word: reverse-video in the accent colour, the
///   same treatment the header's brand and attention chips use, so the whole UI speaks
///   one visual language;
/// * **a hint on the bottom border** instead of inside the body, which buys the body a
///   row and stops the keys competing with the content for attention;
/// * **breathing room** — horizontal and vertical padding, dropped automatically when
///   the terminal is too small to spend rows on it (that is what keeps a text field's
///   input row inside the frame at `MIN_H`).
pub(crate) fn draw_overlay_frame(
    f: &mut Frame,
    area: Rect,
    want_w: u16,
    want_h: u16,
    title: &str,
    accent: Color,
    hint: &str,
) -> Rect {
    draw_overlay_frame_in(f, overlay_rect(area, want_w, want_h), title, accent, hint)
}

/// Draw the shared modal card chrome INTO a caller-chosen `popup` rect and return the inner body
/// area. Split out of [`draw_overlay_frame`] so the create overlay can size itself LARGE (past
/// [`OVERLAY_MAX_W`]) via [`centered_rect`] while every other overlay keeps the shared width policy.
pub(crate) fn draw_overlay_frame_in(
    f: &mut Frame,
    popup: Rect,
    title: &str,
    accent: Color,
    hint: &str,
) -> Rect {
    f.render_widget(Clear, popup);
    // HORIZONTAL ONLY. `Block::inner` already reserves a row for a top title and a row
    // for a bottom one when there are no borders to hang them on
    // (`ratatui-widgets`'s `block.rs`: `borders.intersects(TOP) || has_title_at_position`),
    // so vertical padding here DOUBLE-COUNTS the chrome — it cost the answer overlay two
    // rows and evicted its input caret, which is exactly what
    // `answer_overlay_truncates_a_long_question_without_resizing_or_panicking` caught.
    let pad = Padding::horizontal(overlay_pad(popup.width));
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .style(attention::surface())
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
        )))
        .padding(pad);
    // The hint needs two border corners plus its own width; below that it is dropped
    // rather than drawn as a stub (`Block` would clip it mid-word).
    if !hint.is_empty() && usize::from(popup.width) >= text_cols(hint) + 6 {
        block = block.title_bottom(
            Line::styled(
                format!(" {hint} "),
                Style::default().add_modifier(Modifier::DIM),
            )
            .centered(),
        );
    }
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    inner
}

/// The smallest middle region the answer overlay asks for, so it still reads as a decision
/// screen on a short terminal even when the stop is one line with no options.
pub(crate) const ANSWER_MIN_MIDDLE_H: usize = 4;

/// Rows one PgUp/PgDn moves the answer overlay's question. Deliberately smaller than a full
/// screen: overlapping context is what makes paged prose readable rather than a flipbook.
pub(crate) const ANSWER_PAGE_ROWS: usize = 5;

/// A centered agent-deck section rule: `──────── Session ────────`, dim dashes
/// with the label in a subtle accent, sized exactly to `width` columns.
pub(crate) fn section_line(label: &str, width: u16) -> Line<'static> {
    section_line_styled(
        label,
        width,
        Style::default().fg(agent_manager::theme::accent()),
    )
}

/// [`section_line`] with the label's style chosen by the caller, so the `Stops` rule
/// can wear the row's own attention fill while `Log` stays a quiet accent.
///
/// The `" {label} "` FORMAT IS UNCHANGED and must stay that way — several tests assert
/// `" Stops "` and `" Log "` with their surrounding spaces.
pub(crate) fn section_line_styled(label: &str, width: u16, label_style: Style) -> Line<'static> {
    let w = width as usize;
    let label_text = format!(" {label} ");
    let label_len = label_text.chars().count();
    if w <= label_len {
        // No room for rules — just show the (accented) label, truncated by render.
        return Line::styled(label_text, label_style);
    }
    let dashes = w - label_len;
    let left = dashes / 2;
    let right = dashes - left;
    let dim = Style::default().fg(agent_manager::theme::rule());
    Line::from(vec![
        Span::styled("─".repeat(left), dim),
        Span::styled(label_text, label_style),
        Span::styled("─".repeat(right), dim),
    ])
}

/// One keybar chip: the key in a reversed/bold badge, then its dim label. With an
/// empty `label` the badge stands alone — that is the MEDIUM keybar tier, where
/// dropping labels is what buys the room to keep every key on screen.
pub(crate) fn keychip(key: &str, label: &str) -> Vec<Span<'static>> {
    let badge = Span::styled(
        format!(" {key} "),
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
    );
    if label.is_empty() {
        return vec![badge];
    }
    vec![
        badge,
        Span::styled(
            format!(" {label}"),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]
}
