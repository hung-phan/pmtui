//! The PREVIEW's `Stops` block: the decisions a human has to act on, built BEFORE the log
//! and so RESERVED out of its height. Its own file because the three caps that keep that
//! reservation honest — how many stops, how many rows each, and how much of the pane the
//! block may take at all — are the whole of its logic.

use crate::*;

/// At most this many stops are reserved in the PREVIEW's `Stops` tail; the rest
/// are summarised as `+N more` so a pile-up can never starve the log.
pub(crate) const PREVIEW_MAX_STOPS: usize = 5;

/// The rows of the PREVIEW's pinned `Stops` block, or an empty `Vec` when the row has no
/// open stop. Never grows past HALF of `inner`, because this block's height is taken out of
/// the transcript's.
pub(crate) fn preview_stop_block(
    v: &ProjectView,
    inner: Rect,
    lvl: attention::Level,
) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let inner_w = inner.width;
    // Color-coded by effective risk (Hard red, Medium yellow, Low default). Each
    // stop shows the agent's actual question + options, not just its kind — the
    // whole point of surfacing a decision. Three caps keep that free text from
    // stealing the transcript's rows, since this block's height is reserved out of
    // the log's: at most PREVIEW_MAX_STOPS stops, at most PREVIEW_MAX_STOP_LINES
    // rows each, and at most HALF the pane for the block as a whole. Anything
    // dropped by any of the three is counted in the section's `+N more` marker.
    // NOTHING TO SHOW ON A ROW NOTHING DRIVES. User: *"when autopilot is off, the answer panel still
    // shows. This isn't correct, since user will drive, we don't need to do anything here"*. Right, and
    // it was worse than clutter: `a`'s gate is `answer_reaches_the_agent`, so on an undriven row the
    // panel offered lettered options to a key that refuses — and nothing reads `answers.json` for that
    // row, so an answer would strand. The human drives such a session in the chat, where the agent's
    // question is already on screen.
    //
    // The daemon's OWN predicate, so this cannot disagree with the key: `pmd_drives_row` is true for
    // native `Auto` and interactive rows (which pmd drives at every tier), so this only ever hides the
    // block on a Standard agent-loop row — exactly the one where `a` says "autopilot off — press m".
    let driven = agent_manager::daemon::pmd_drives_row(v.mode, v.tier);
    let mut stops: Vec<Line> = Vec::new();
    if !v.stops.is_empty() && driven {
        let text_w = usize::from(inner_w).saturating_sub(2);
        // Half the pane, minus the section rule and a possible `+N more` row; at
        // least one row so a single stop's header always survives a tiny pane. A
        // stop squeezed by this budget degrades down a deliberate ladder — its rows
        // are ordered header, question, options, and only whole leading rows are
        // kept — so the worst case is the bare header line the preview showed before
        // this feature, never a garbled half-row.
        let row_budget = usize::from(inner.height / 2).saturating_sub(2).max(1);
        let mut shown = 0usize;
        let mut rows: Vec<Line> = Vec::new();
        let considered = v.stops.len().min(PREVIEW_MAX_STOPS);
        let mut left = row_budget;
        for (i, s) in v.stops.iter().take(PREVIEW_MAX_STOPS).enumerate() {
            // Reserve ONE row for every stop after this one, so a second decision can never be
            // swallowed whole by a verbose first one — the count in `+N more` is a poor
            // substitute for a row that names it. The first stop is the one being acted on, so
            // it gets everything that reservation leaves.
            let mine = left.saturating_sub(considered - i - 1);
            if mine == 0 {
                break; // out of budget — the rest fall into `+N more`
            }
            // A blank row BETWEEN stops, on the same terms as the one inside them: only when a
            // row is genuinely spare. Two decisions running flush together are harder to tell
            // apart than two paragraphs, because both start with a severity chip.
            let sep = usize::from(i > 0 && mine > 1);
            let entry = stop_preview_lines(s, text_w, mine - sep);
            if sep == 1 && !entry.is_empty() {
                rows.push(Line::raw(""));
                left = left.saturating_sub(1);
            }
            left = left.saturating_sub(entry.len());
            rows.extend(entry);
            shown += 1;
        }
        let hidden = v.stops.len().saturating_sub(shown);
        // The rule over the block a human must act on carries the row's severity. It was
        // `fg(Cyan)` — the same accent as the `Log` rule above a transcript nobody has to
        // answer — which is how "the one thing to act on" ended up the quietest region on
        // the screen, below the log, behind a dim line.
        // The rule NAMES THE KEY, because this block is a summary on purpose — its height is
        // reserved out of the log's, so it is capped at half the pane and its question and
        // options are truncated with `…`. That is the right trade for a glance, but a human
        // staring at a clipped decision had no signal that anything would show them the rest;
        // the user's report was *"it is not interactable or for me to be able to see the whole
        // set of commands"*, and half of that is this block not pointing at `a`.
        stops.push(section_line_styled(
            // Names what the key DOES, not `a answers` (which read as "a-answers", a noun). User:
            // *"It is confusing … update the message like Stops for answers"*.
            "stops \u{b7} press s to answer",
            inner_w,
            if lvl.is_filled() {
                lvl.label_style()
            } else {
                Style::default().fg(agent_manager::theme::accent())
            },
        ));
        stops.extend(rows);
        if hidden > 0 {
            stops.push(Line::styled(format!("  +{hidden} more"), dim));
        }
    }
    stops
}
