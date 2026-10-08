//! The ANSWER overlay — the moment the human actually decides. It is the biggest of the
//! overlays and the only one with a SCROLLING body, so its geometry is arithmetic rather
//! than a fixed frame: that is why it earns a file of its own beside the small fixed-size
//! fields.

use crate::*;

fn wrapped_option_rows(index: usize, option: &str, selected: bool, width: usize) -> Vec<String> {
    let marker = if selected { "▸ " } else { "  " };
    let prefix = format!("{marker}{} ", index + 1);
    let indent = " ".repeat(prefix.chars().count());
    let text_width = width.saturating_sub(prefix.chars().count()).max(1);
    let wrapped = wrap_all(option, text_width);
    if wrapped.is_empty() {
        return vec![prefix.trim_end().to_string()];
    }
    wrapped
        .into_iter()
        .enumerate()
        .map(|(row, text)| {
            if row == 0 {
                format!("{prefix}{text}")
            } else {
                format!("{indent}{text}")
            }
        })
        .collect()
}

/// Answer overlay — the selected row's first open stop, its options, and the field the
/// answer is typed into. `input`/`choice`/`scroll` come straight from
/// [`UiMode::Answering`]; publishes the scroll ceiling on `app.scroll_max` so the key
/// handler can clamp PgDn to what the frame actually granted.
pub(crate) fn render_answer(
    f: &mut Frame,
    app: &App,
    area: Rect,
    input: &Field,
    choice: usize,
    scroll: usize,
) {
    if let Some(v) = app.selected_view()
        && let Some(stop) = v.stops.first()
    {
        // This is the moment the human actually decides, so it gets the biggest frame of the
        // six and the only scrolling body: reading the question must not mean leaving the
        // screen for the marker file or the pane.
        //
        // The layout is THREE regions, and which of them may scroll is the whole design:
        //   - the header (`id (risk) · kind`) is PINNED to the top;
        //   - the middle (question + options) SCROLLS;
        //   - the input row is PINNED to the bottom.
        // Pinning the input is not cosmetic. This overlay previously clamped the question to
        // two lines precisely because anything taller pushed the caret out of the frame — so
        // the fix for "I cannot read the whole question" is not a taller body, it is a body
        // that cannot evict the thing you type into.
        //
        // Width is 88, up from 76: these questions are prose and the options are sentences,
        // and every column here buys a wrapped row back.
        // CONTENT-DRIVEN height, which needs the width first — the question's row count is a
        // function of how wide it gets to wrap. `overlay_text_w` reports exactly what
        // `draw_overlay_frame` is about to grant, so this is a prediction, not a guess.
        //
        // A first cut simply asked for three quarters of the terminal. It fit everything and
        // looked wrong: a 12-row body inside 22 rows, i.e. ten rows of nothing above the
        // caret — the same "flat and unfinished" complaint that drove `overlay_h` in the first
        // place. Bigger is not better proportioned.
        let text_w = overlay_text_w(area, 88);
        let question_rows = {
            let q = stop_product_text(stop);
            if q.is_empty() {
                0
            } else {
                wrap_all(q, text_w).len()
            }
        };
        let opt_rows = if stop.options.is_empty() {
            0
        } else {
            1 + stop
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| wrapped_option_rows(index, option, false, text_w).len())
                .sum::<usize>()
        };
        // header + blank + middle + scroll-note + input, with a floor so a one-line stop with
        // no options still reads as a decision screen. Capped at three quarters of the
        // terminal: past that a modal stops being a modal.
        let want_rows = (2 + question_rows.max(1) + opt_rows + 2)
            .max(2 + ANSWER_MIN_MIDDLE_H + 2)
            .min((usize::from(area.height) * 3 / 4).max(2 + ANSWER_MIN_MIDDLE_H + 2));
        let inner = draw_overlay_frame(
            f,
            area,
            88,
            overlay_h(want_rows),
            &format!("Answer \u{b7} {}", v.label()),
            agent_manager::theme::soft(),
            // The hint teaches the interaction, because a cursor you can move is not
            // discoverable from a static screenshot.
            "\u{2191}\u{2193} pick \u{b7} wheel/Pg scroll \u{b7} enter send \u{b7} type freely \u{b7} esc cancel",
        );
        // The prediction above must equal what was granted, or the wrap that sized the frame
        // is not the wrap being drawn into it. Trust the frame.
        let text_w = usize::from(inner.width);
        let dim = Style::default().fg(agent_manager::theme::rule());
        let header = Line::styled(
            truncate(
                &format!(
                    "{} ({}) · {}",
                    stop.id,
                    risk_str(stop.risk_class),
                    stop.kind
                ),
                text_w,
            ),
            Style::default().add_modifier(Modifier::BOLD),
        );

        // ---- the scrolling middle: the whole question, then one option per row ----
        let mut middle: Vec<Line> = Vec::new();
        // The agent's own question, or — for a stop the HARNESS synthesized, which carries no
        // draft — the product string for its kind, through the SAME `stop_product_text` the
        // `Stops` block uses. Still nothing to say (a pre-question ledger, an agent-authored
        // kind with no text): the kind on the header alone, as before.
        let question = stop_product_text(stop);
        if !question.is_empty() {
            // NOT `wrap_clamped`: nothing is clipped here any more. The full question is
            // wrapped and the region scrolls — which is the actual complaint being fixed.
            for row in wrap_all(question, text_w) {
                middle.push(Line::raw(row));
            }
        }
        let mut option_ranges = Vec::with_capacity(stop.options.len());
        if !stop.options.is_empty() {
            middle.push(Line::raw(""));
            let picked = choice.min(stop.options.len().saturating_sub(1));
            for (i, opt) in stop.options.iter().enumerate() {
                let on = i == picked && input.trim().is_empty();
                let start = middle.len();
                let style = if on {
                    Style::default()
                        .fg(agent_manager::theme::soft())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                // Every option wraps through the same unbounded text rule as the question. The
                // marker and number stay on the first row; continuation rows align under the text.
                for row in wrapped_option_rows(i, opt, on, text_w) {
                    middle.push(Line::styled(row, style));
                }
                option_ranges.push(start..middle.len());
            }
        }

        // ---- fit: header + blank + middle + scroll-note + input ----
        let middle_len = middle.len();
        let rows = usize::from(inner.height);
        let middle_h = rows.saturating_sub(4).max(1);
        // Option navigation requests cursor-follow with `usize::MAX`. Manual Page/mouse scrolling
        // uses an absolute top row and is never overridden, so every question/option row remains
        // reachable. If one option is taller than the viewport, follow its first row so the marker
        // stays visible; manual scrolling can then read the rest.
        let picked = choice.min(stop.options.len().saturating_sub(1));
        let cursor_range = option_ranges.get(picked).cloned().unwrap_or(0..0);
        let cursor_start = cursor_range.start;
        let cursor_end = cursor_range.end.saturating_sub(1);
        let max_top = middle_len.saturating_sub(middle_h);
        let follow_choice =
            scroll == usize::MAX && !option_ranges.is_empty() && input.trim().is_empty();
        let top = if follow_choice {
            if cursor_range.len() > middle_h {
                cursor_start.min(max_top)
            } else {
                cursor_end
                    .saturating_sub(middle_h.saturating_sub(1))
                    .min(cursor_start)
                    .min(max_top)
            }
        } else {
            scroll.min(max_top)
        };
        app.answer_scroll_top.set(top);
        let cursor_visible = cursor_start >= top && cursor_start < top.saturating_add(middle_h);
        let more_above = top > 0;
        let more_below = top + middle_h < middle_len;
        let selection_state =
            if !stop.options.is_empty() && input.trim().is_empty() && !cursor_visible {
                format!("selected option {}  ", picked + 1)
            } else {
                String::new()
            };

        let mut body = vec![header, Line::raw("")];
        body.extend(middle.into_iter().skip(top).take(middle_h));
        // Pad so the input stays on the LAST row whatever the content's height — a caret that
        // floats up under a short question reads as a different widget every time.
        while body.len() < rows.saturating_sub(2) {
            body.push(Line::raw(""));
        }
        // Says there is more to read, and how to reach it. Without this a scrolled region is
        // indistinguishable from a truncated one — which is the bug being fixed, wearing a
        // different hat.
        body.push(if more_above || more_below {
            Line::styled(
                truncate(
                    &format!(
                        "{}{}{}wheel or pgup/pgdn scrolls",
                        selection_state,
                        if more_above {
                            "\u{2191} more above  "
                        } else {
                            ""
                        },
                        if more_below {
                            "\u{2193} more below  "
                        } else {
                            ""
                        }
                    ),
                    text_w,
                ),
                dim,
            )
        } else {
            Line::raw("")
        });
        // The input row, windowed so the caret stays visible however long the answer runs —
        // and now MOVABLE with ←/→/Home/End, drawn by the shared `input_line`.
        body.push(input_line(input, text_w.saturating_sub(3)));
        // NO `.wrap()`. Every row above is already fitted to `text_w` — question and options by
        // `wrap_all`, the header by `truncate` — so a second wrapping pass could only spill one of
        // them onto an extra row and evict the pinned input caret. Geometry stays deterministic
        // because exactly one helper decides every line break.
        f.render_widget(Paragraph::new(Text::from(body)), inner);
        // Publish the scroll CEILING so PgDn cannot run past the end — the same
        // render-measures-it/handler-obeys-it contract the wake view uses. The render path is
        // the only place that knows `middle_h`, which depends on the frame the terminal
        // actually granted.
        app.scroll_max.set(max_top);
    }
}
