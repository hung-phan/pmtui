//! The INLINE fields a human edits a live session with: its rename (`R`), its cadence (`c`), its
//! goal (`g`) and its directive (`i`). One module because they share the frame and the note under it
//! saying what the save does, and because the goal and cadence fields double as the prompts `m`
//! chains on its way into Autopilot, where the title and the key line change together.
//!
//! The split inside it is the BUFFER: rename and cadence edit a one-line [`Field`] (an id, an
//! interval), while the goal and the directive edit a [`Composer`] — prose, in a file that may hold
//! several lines, so the field opens on what is on disk and scrolls rather than truncating.

use crate::*;

pub(crate) fn render_rename_field(
    f: &mut Frame,
    area: Rect,
    id: &str,
    current: Option<&str>,
    input: &Field,
) {
    let inner = draw_overlay_frame(
        f,
        area,
        64,
        overlay_h(5),
        &format!("Rename \u{b7} {}", truncate(id, 24)),
        agent_manager::theme::accent(),
        "enter save \u{b7} empty restores id \u{b7} esc cancel",
    );
    let width = usize::from(inner.width);
    let current = current.unwrap_or(id);
    let body = Text::from(vec![
        Line::styled(
            truncate(&format!("current: {current}"), width),
            attention::text_dim(),
        ),
        Line::raw(""),
        input_line(input, width.saturating_sub(3)),
        Line::raw(""),
        Line::styled(
            truncate(
                "display only; runtime id and terminal stay unchanged",
                width,
            ),
            attention::text_dim(),
        ),
    ]);
    f.render_widget(Paragraph::new(body), inner);
}

/// Inline cadence field (`c`) — how often pmd nudges this session.
pub(crate) fn render_cadence_field(
    f: &mut Frame,
    area: Rect,
    id: &str,
    current: u64,
    input: &Field,
    then_autopilot: bool,
) {
    // rows: current + blank + input + blank + the accepted-forms note.
    let inner = draw_overlay_frame(
        f,
        area,
        64,
        overlay_h(5),
        &if then_autopilot {
            // "check-in", the one word this UI uses for the interval — the field said "heartbeat" while
            // its own status line, the preview and the keybar all said something else.
            format!("Autopilot \u{b7} check-in for {}", truncate(id, 18))
        } else {
            format!("Cadence \u{b7} {}", truncate(id, 24))
        },
        agent_manager::theme::accent(),
        if then_autopilot {
            // Says what enter DOES (it starts the drive) and what an empty save means here,
            // which is "take the default" rather than the ordinary field's "change nothing".
            //
            // SHORT ENOUGH FOR A 64-COLUMN FRAME. The first version was 66 columns, and
            // `draw_overlay_frame` DROPS a hint it cannot draw whole — so it rendered nowhere,
            // exactly as the goal prompt's first hint did. Caught the same way: by looking at a
            // real pane. `the_autopilot_prompts_show_their_key_lines` now pins both.
            "enter = autopilot ON \u{b7} empty = default \u{b7} esc = Standard"
        } else {
            "enter save \u{b7} empty keeps it \u{b7} esc cancel"
        },
    );
    let text_w = usize::from(inner.width);
    let dim = Style::default().add_modifier(Modifier::DIM);
    // BOTH spellings of the current value: the human-readable one is what the status line
    // will echo back, and the raw seconds are what a human types to change it by a little.
    let body = Text::from(vec![
        Line::styled(
            truncate(
                &format!(
                    "checks in every {} ({}s)",
                    job_engine::human_cadence(current),
                    current
                ),
                text_w,
            ),
            dim,
        ),
        Line::raw(""),
        input_line(input, text_w.saturating_sub(3)),
        Line::raw(""),
        // The accepted FORMS, because a field that only says "cadence" invites `5 min`
        // and then rejects it. Bounds included: they are the other thing a human cannot
        // guess, and being clamped without warning reads as the field ignoring you.
        Line::styled(
            truncate(
                &format!(
                    "seconds, or 10m / 1h30m \u{b7} {}..{}",
                    job_engine::human_cadence(job_engine::CADENCE_MIN_S),
                    job_engine::human_cadence(job_engine::CADENCE_MAX_S)
                ),
                text_w,
            ),
            dim,
        ),
    ]);
    f.render_widget(Paragraph::new(body), inner);
}

/// Inline goal field (`g`) — `brief.md`, edited in place.
///
/// The body is the BUFFER plus one note row. It used to be a read-only preview of the goal on disk
/// above a one-line caret, because a one-line field could not hold a multi-line mandate — so the
/// field opened EMPTY and the preview carried a "4 lines — ^E edits them all" apology. The buffer is
/// a `ratatui-textarea` seeded from disk now, so the preview and the apology are the same thing as
/// the field: what you see is what you are editing.
pub(crate) fn render_goal_field(
    f: &mut Frame,
    area: Rect,
    id: &str,
    input: &Composer,
    then_autopilot: bool,
) {
    // CONTENT-DRIVEN height that grows toward the goal being edited (up to [`GOAL_EDIT_LINES`] rows
    // of it), floored by `overlay_h`. Past that the library scrolls, keeping the caret on screen.
    //
    // The height `overlay_h` ASKS for is not always what the terminal GRANTS — on a short pane
    // `overlay_rect` clamps it — so the note is pinned to the BOTTOM of whatever inner height it gets
    // and the buffer takes the rest, yielding text rows (never the note) when space runs out.
    let want_rows = input.line_count().clamp(1, GOAL_EDIT_LINES) + 2;
    // The TITLE and the KEY LINE both change when this field is the way into Autopilot, because the
    // same overlay then does something the ordinary one does not: `enter` turns the drive on. An
    // unlabelled prompt would have a human press Enter to save a goal and silently hand pmd the
    // wheel — and `esc` has to name what it is declining, not just that it closes something.
    let (title, keys) = if then_autopilot {
        (
            format!("Autopilot \u{b7} goal for {}", truncate(id, 20)),
            // SHORT ENOUGH TO SURVIVE. `draw_overlay_frame` drops a hint it cannot draw whole, and
            // the first version of this line (with "empty keeps this goal") was 83 columns against a
            // 72-column frame — so it silently vanished, which a test caught only because it
            // asserted on the rendered screen.
            "enter = autopilot ON \u{b7} ^X^E $EDITOR \u{b7} esc stays Standard",
        )
    } else {
        (
            format!("Goal \u{b7} {}", truncate(id, 24)),
            "enter save \u{b7} ^J newline \u{b7} ^X^E $EDITOR \u{b7} esc cancel",
        )
    };
    let inner = draw_overlay_frame(
        f,
        area,
        72,
        overlay_h(want_rows),
        &title,
        agent_manager::theme::accent(),
        keys,
    );
    // WHAT the save does and WHEN it lands — the two questions this field always raised and never
    // answered. `JobScheduler::nudge` re-reads the brief every heartbeat, so nothing happens at the
    // instant of the write. The consequence, not the mechanism: the autopilot variant says this press
    // starts an autonomous drive, which the human should read first.
    let note = if then_autopilot {
        "writes brief.md, turns autopilot on \u{b7} pmd starts driving at the next check-in"
    } else {
        // SHORT ENOUGH FOR 68 COLUMNS, measured on a real 80-column pane: the first version said "the
        // agent sees it at its next check-in" and lost the tail to an ellipsis.
        "writes brief.md \u{b7} empty keeps it \u{b7} read at the next check-in"
    };
    draw_buffer_and_note(f, inner, input, note);
}

/// The body both prose fields draw: the buffer, then a dim note pinned to the BOTTOM row with a
/// separating blank above it when the granted height has room for one.
///
/// The note is pinned rather than laid out from the top because a short terminal must lose text rows
/// (the library scrolls, so the caret stays visible) instead of losing the line that says what Enter
/// will do to a file.
fn draw_buffer_and_note(f: &mut Frame, inner: Rect, input: &Composer, note: &str) {
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let has_blank = inner.height >= 3;
    let text_h = inner.height.saturating_sub(1 + u16::from(has_blank));
    if text_h > 0 {
        f.render_widget(
            input.widget(),
            Rect {
                height: text_h,
                ..inner
            },
        );
    }
    f.render_widget(
        Paragraph::new(Line::styled(
            truncate(note, usize::from(inner.width)),
            attention::text_dim(),
        )),
        Rect {
            y: inner.y + inner.height - 1,
            height: 1,
            ..inner
        },
    );
}

/// Inline directive field (`i`) — the RESTRICTIVE counterpart to [`render_goal_field`]. The same
/// seeded buffer and bottom-pinned note, with no autopilot variant (setting a directive never flips
/// the dial) and a distinct `^X^R` = rescind in its key line. Drawn in MAGENTA to read as a different
/// kind of setting from the cyan goal/cadence fields.
pub(crate) fn render_directive_field(f: &mut Frame, area: Rect, id: &str, input: &Composer) {
    let want_rows = input.line_count().clamp(1, GOAL_EDIT_LINES) + 2;
    let title = format!("Directive \u{b7} {}", truncate(id, 22));
    // SHORT ENOUGH to survive a 72-column frame (`draw_overlay_frame` drops a hint it cannot draw
    // whole). `^J` newline is the one key this line has no room for; the note below names the
    // outcomes instead, and `?` documents the readline map once for all three prose fields.
    let keys = "enter save \u{b7} ^X^E $EDITOR \u{b7} ^X^R rescind \u{b7} esc cancel";
    let inner = draw_overlay_frame(
        f,
        area,
        72,
        overlay_h(want_rows),
        &title,
        agent_manager::theme::brand(),
        keys,
    );
    // WHAT a save/empty-save/rescind each does — the three outcomes this field has, said in one line.
    // `directive.md` is re-read fresh at the decider's next consult, so nothing happens at the
    // instant of the write.
    draw_buffer_and_note(
        f,
        inner,
        input,
        "writes directive.md \u{b7} empty keeps it \u{b7} ^X^R rescinds",
    );
}
